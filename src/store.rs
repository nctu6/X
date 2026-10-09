//! JSONL storage and the once-per-slot fetch gate.
//!
//! Each account is `{data_dir}/{handle}.jsonl` plus a sidecar state file.
//! A successful fetch records the time. Later requests in that same local
//! slot read the file and do not call X. A failed fetch does not record
//! success, so a later request in the slot may retry. Concurrent requests
//! for one account share a single in-flight call.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex};

use crate::config::{Config, Tab};
use crate::images::{cache_images, ImageClient, ImageSource};
use crate::model::{is_snowflake, Media, Post, User};
use crate::slot::{parse_timezone, same_slot};
use crate::xapi::{
    parse_user_tweets, parse_users, requested_posts, short_error, tweets_path, users_path, XSource,
};

#[derive(Clone, Debug)]
pub struct Clock {
    fixed: Arc<std::sync::Mutex<Option<i64>>>,
}

impl Clock {
    pub fn system() -> Self {
        Self {
            fixed: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn set(&self, unix: i64) {
        *self.fixed.lock().expect("clock") = Some(unix);
    }

    pub fn now(&self) -> i64 {
        self.fixed.lock().expect("clock").unwrap_or_else(now_unix)
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub struct Store<S, I> {
    inner: Arc<Inner<S, I>>,
}

impl<S, I> Clone for Store<S, I> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct Inner<S, I> {
    config: Config,
    tz: chrono_tz::Tz,
    dir: PathBuf,
    source: S,
    clock: Clock,
    images: I,
    accounts: Mutex<HashMap<String, AccountMem>>,
    /// Synchronous so a cancelled leader can drop its entry without awaiting.
    inflight: std::sync::Mutex<HashMap<String, watch::Receiver<Notice>>>,
}

#[derive(Clone)]
struct Notice {
    done: bool,
    error: Option<String>,
}

/// Removes the in-flight entry even when the request is cancelled.
struct Leader<S, I> {
    store: Store<S, I>,
    key: String,
    sender: Option<watch::Sender<Notice>>,
}

impl<S, I> Drop for Leader<S, I> {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Notice {
                done: true,
                error: Some("fetch cancelled".into()),
            });
        }
        let mut flights = self
            .store
            .inner
            .inflight
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        flights.remove(&self.key);
    }
}

struct AccountMem {
    handle: String,
    user_id: Option<String>,
    name: Option<String>,
    avatar: Option<String>,
    posts: Vec<Post>,
    last_success: Option<i64>,
    newest_id: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AccountState {
    last_success_unix: Option<i64>,
    newest_id: Option<String>,
    user_id: Option<String>,
    name: Option<String>,
    avatar: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TabView {
    pub id: String,
    pub label: String,
    pub posts: Vec<Post>,
    pub updated_at: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug)]
pub enum PrepareError {
    UnknownTab,
}

#[derive(Debug, Clone)]
pub struct UpdateReport {
    pub handle: String,
    pub stored: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum UpdateTarget {
    All,
    Tab(String),
    Account(String),
}

impl<S, I> Store<S, I> {
    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn clock(&self) -> &Clock {
        &self.inner.clock
    }

    pub fn tab(&self, name: &str) -> Option<&Tab> {
        self.inner
            .config
            .tabs
            .iter()
            .find(|tab| tab.id.eq_ignore_ascii_case(name) || tab.label.eq_ignore_ascii_case(name))
    }
}

impl<S: XSource + 'static> Store<S, ImageClient> {
    pub async fn open(config: Config, source: S) -> Result<Self, String> {
        let images = ImageClient::new(config.max_image_bytes)?;
        Self::open_with_clock(config, source, images, Clock::system()).await
    }
}

impl<S: XSource + 'static, I: ImageSource + 'static> Store<S, I> {
    pub async fn open_with_clock(
        config: Config,
        source: S,
        images: I,
        clock: Clock,
    ) -> Result<Self, String> {
        let tz = parse_timezone(&config.timezone)?;
        let dir = PathBuf::from(&config.data_dir);
        std::fs::create_dir_all(&dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
        let mut accounts = HashMap::new();
        for handle in config.handles() {
            let key = handle.to_ascii_lowercase();
            accounts.insert(key, load_account(&dir, handle)?);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                tz,
                dir,
                source,
                clock,
                images,
                accounts: Mutex::new(accounts),
                inflight: std::sync::Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Load one tab, fetching each account only when this slot has no success yet.
    pub async fn prepare_tab(&self, name: &str, force: bool) -> Result<TabView, PrepareError> {
        let Some(tab) = self.tab(name).cloned() else {
            return Err(PrepareError::UnknownTab);
        };
        let mut notes = Vec::new();
        for handle in &tab.accounts {
            if let Err(err) = self.ensure(handle, force).await {
                notes.push(format!("@{handle}: {err}"));
            }
        }
        let guard = self.inner.accounts.lock().await;
        let mut posts = Vec::new();
        let mut updated = None;
        let mut saved_notes = Vec::new();
        for handle in &tab.accounts {
            let key = handle.to_ascii_lowercase();
            let Some(account) = guard.get(&key) else {
                continue;
            };
            posts.extend(account.posts.iter().cloned());
            if let Some(stamp) = account.last_success {
                updated = Some(updated.map_or(stamp, |prior: i64| prior.max(stamp)));
            }
            if let Some(err) = &account.last_error {
                saved_notes.push(format!("@{}: {err}", account.handle));
            }
        }
        drop(guard);
        posts = merge_posts(posts, Vec::new(), self.inner.config.posts_per_tab);
        if notes.is_empty() {
            notes = saved_notes;
        }
        let message = if notes.is_empty() {
            None
        } else if updated.is_some() {
            Some(format!("Showing saved posts. {}", notes.join(" ")))
        } else {
            Some(notes.join(" "))
        };
        Ok(TabView {
            id: tab.id,
            label: tab.label,
            posts,
            updated_at: updated,
            message,
        })
    }

    pub async fn update(&self, target: UpdateTarget) -> Result<Vec<UpdateReport>, String> {
        let handles = self.handles_for(target)?;
        let mut reports = Vec::new();
        for handle in handles {
            let error = self.ensure(&handle, true).await.err();
            let stored = {
                let guard = self.inner.accounts.lock().await;
                guard
                    .get(&handle.to_ascii_lowercase())
                    .map(|account| account.posts.len())
                    .unwrap_or(0)
            };
            reports.push(UpdateReport {
                handle,
                stored,
                error,
            });
        }
        Ok(reports)
    }

    fn handles_for(&self, target: UpdateTarget) -> Result<Vec<String>, String> {
        match target {
            UpdateTarget::All => Ok(self
                .inner
                .config
                .handles()
                .into_iter()
                .map(str::to_string)
                .collect()),
            UpdateTarget::Tab(name) => {
                let tab = self
                    .tab(&name)
                    .ok_or_else(|| format!("unknown tab \"{name}\""))?;
                Ok(tab.accounts.clone())
            }
            UpdateTarget::Account(name) => {
                let key = name.trim().trim_start_matches('@').to_ascii_lowercase();
                let handle = self
                    .inner
                    .config
                    .handles()
                    .into_iter()
                    .find(|handle| handle.eq_ignore_ascii_case(&key))
                    .ok_or_else(|| format!("unknown account \"{name}\""))?;
                Ok(vec![handle.to_string()])
            }
        }
    }

    async fn ensure(&self, handle: &str, force: bool) -> Result<(), String> {
        let key = handle.to_ascii_lowercase();
        let now = self.inner.clock.now();
        if !force && self.account_fresh(&key, now).await {
            return Ok(());
        }

        let (sender, mut receiver) = {
            let mut flights = self
                .inner
                .inflight
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            if let Some(existing) = flights.get(&key) {
                (None, existing.clone())
            } else {
                let (sender, receiver) = watch::channel(Notice {
                    done: false,
                    error: None,
                });
                flights.insert(key.clone(), receiver.clone());
                (Some(sender), receiver)
            }
        };
        let Some(sender) = sender else {
            loop {
                if receiver.borrow().done {
                    break;
                }
                if receiver.changed().await.is_err() {
                    break;
                }
            }
            return match receiver.borrow().error.clone() {
                Some(err) => Err(err),
                None => Ok(()),
            };
        };

        let mut leader = Leader {
            store: self.clone(),
            key: key.clone(),
            sender: Some(sender),
        };
        // Another caller may have finished between the fresh check and leadership.
        if !force && self.account_fresh(&key, now).await {
            if let Some(sender) = leader.sender.take() {
                let _ = sender.send(Notice {
                    done: true,
                    error: None,
                });
            }
            return Ok(());
        }
        let result = self.fetch_account(&key).await;
        if let Some(sender) = leader.sender.take() {
            let _ = sender.send(Notice {
                done: true,
                error: result.as_ref().err().cloned(),
            });
        }
        result
    }

    async fn account_fresh(&self, key: &str, now: i64) -> bool {
        let guard = self.inner.accounts.lock().await;
        guard
            .get(key)
            .and_then(|account| account.last_success)
            .is_some_and(|stamp| same_slot(stamp, now, self.inner.tz))
    }

    async fn fetch_account(&self, key: &str) -> Result<(), String> {
        let handle = {
            let guard = self.inner.accounts.lock().await;
            guard
                .get(key)
                .map(|account| account.handle.clone())
                .unwrap_or_else(|| key.to_string())
        };
        let user_id = {
            let guard = self.inner.accounts.lock().await;
            guard.get(key).and_then(|account| account.user_id.clone())
        };
        let user_id = if let Some(user_id) = user_id {
            user_id
        } else {
            match self.lookup_user(&handle).await {
                Ok(Some(user)) => {
                    let id = user.id.clone();
                    self.remember_user(key, &user).await?;
                    id
                }
                Ok(None) => {
                    self.mark(
                        key,
                        Some(self.inner.clock.now()),
                        Some("account not found".into()),
                    )
                    .await?;
                    self.cache_saved(key).await;
                    return Err("account not found".into());
                }
                Err(err) => {
                    self.mark(key, None, Some(err.clone())).await?;
                    self.cache_saved(key).await;
                    return Err(err);
                }
            }
        };
        let since = {
            let guard = self.inner.accounts.lock().await;
            guard.get(key).and_then(|account| account.newest_id.clone())
        };
        let path = tweets_path(
            &user_id,
            requested_posts(self.inner.config.posts_per_tab),
            self.inner.config.exclude_replies,
            self.inner.config.exclude_retweets,
            since.as_deref(),
        );
        let body = match self.inner.source.fetch(&path).await {
            Ok(body) => body,
            Err(err) => {
                let message = short_error(&err);
                tracing::warn!(account = %handle, error = %message, "fetch failed");
                self.mark(key, None, Some(message.clone())).await?;
                self.cache_saved(key).await;
                return Err(message);
            }
        };
        let fallback = self.fallback_user(key, &user_id, &handle).await;
        let posts = match parse_user_tweets(&body, &fallback) {
            Ok(posts) => posts,
            Err(err) => {
                self.mark(key, None, Some(err.clone())).await?;
                self.cache_saved(key).await;
                return Err(err);
            }
        };
        let added = posts.len();
        self.commit(key, posts).await?;
        tracing::info!(account = %handle, added, "fetched");
        Ok(())
    }

    async fn lookup_user(&self, handle: &str) -> Result<Option<User>, String> {
        let path = users_path(&[handle.to_string()]);
        let body = self
            .inner
            .source
            .fetch(&path)
            .await
            .map_err(|err| short_error(&err))?;
        let batch = parse_users(&body)?;
        Ok(batch
            .users
            .into_iter()
            .find(|user| user.username.eq_ignore_ascii_case(handle)))
    }

    async fn remember_user(&self, key: &str, user: &User) -> Result<(), String> {
        let mut guard = self.inner.accounts.lock().await;
        let account = guard
            .get_mut(key)
            .ok_or_else(|| "unknown account".to_string())?;
        account.user_id = Some(user.id.clone());
        account.name = Some(user.name.clone());
        account.avatar.clone_from(&user.avatar);
        account.handle = user.username.clone();
        persist(account, &self.inner.dir)
    }

    async fn fallback_user(&self, key: &str, user_id: &str, handle: &str) -> User {
        let guard = self.inner.accounts.lock().await;
        let account = guard.get(key);
        User {
            id: user_id.to_string(),
            name: account
                .and_then(|account| account.name.clone())
                .unwrap_or_else(|| handle.to_string()),
            username: account
                .map(|account| account.handle.clone())
                .unwrap_or_else(|| handle.to_string()),
            avatar: account.and_then(|account| account.avatar.clone()),
        }
    }

    async fn commit(&self, key: &str, incoming: Vec<Post>) -> Result<(), String> {
        let (handle, mut merged) = {
            let guard = self.inner.accounts.lock().await;
            let account = guard
                .get(key)
                .ok_or_else(|| "unknown account".to_string())?;
            let merged = merge_posts(
                account.posts.clone(),
                incoming,
                self.inner.config.max_stored_posts,
            );
            (account.handle.clone(), merged)
        };
        self.cache_posts(&handle, &mut merged).await;
        let mut guard = self.inner.accounts.lock().await;
        let account = guard
            .get_mut(key)
            .ok_or_else(|| "unknown account".to_string())?;
        account.newest_id = newest_id(&merged);
        account.posts = merged;
        account.last_success = Some(self.inner.clock.now());
        account.last_error = None;
        persist(account, &self.inner.dir)
    }

    async fn cache_saved(&self, key: &str) {
        let (handle, mut posts) = {
            let guard = self.inner.accounts.lock().await;
            let Some(account) = guard.get(key) else {
                return;
            };
            (account.handle.clone(), account.posts.clone())
        };
        if posts.is_empty() {
            return;
        }
        self.cache_posts(&handle, &mut posts).await;
        let mut guard = self.inner.accounts.lock().await;
        let Some(account) = guard.get_mut(key) else {
            return;
        };
        account.posts = posts;
        if let Err(err) = persist(account, &self.inner.dir) {
            tracing::warn!(error = %err, "could not store images");
        }
    }

    async fn cache_posts(&self, handle: &str, posts: &mut [Post]) {
        cache_images(
            &self.inner.dir,
            handle,
            posts,
            &self.inner.images,
            self.inner.config.max_image_bytes,
        )
        .await;
    }

    async fn mark(
        &self,
        key: &str,
        success: Option<i64>,
        error: Option<String>,
    ) -> Result<(), String> {
        let mut guard = self.inner.accounts.lock().await;
        let account = guard
            .get_mut(key)
            .ok_or_else(|| "unknown account".to_string())?;
        if let Some(stamp) = success {
            account.last_success = Some(stamp);
        }
        account.last_error = error;
        persist(account, &self.inner.dir)
    }
}

pub fn merge_posts(existing: Vec<Post>, incoming: Vec<Post>, limit: usize) -> Vec<Post> {
    let mut by_id: HashMap<String, Post> = HashMap::new();
    for post in existing {
        by_id.insert(post.id.clone(), post);
    }
    for mut post in incoming {
        if let Some(prev) = by_id.get(&post.id) {
            post.media = merge_media(&prev.media, post.media);
        }
        by_id.insert(post.id.clone(), post);
    }
    let mut posts: Vec<Post> = by_id.into_values().collect();
    posts.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| id_num(&right.id).cmp(&id_num(&left.id)))
    });
    let mut seen = HashSet::new();
    posts.retain(|post| seen.insert(post.id.clone()));
    if limit > 0 && posts.len() > limit {
        posts.truncate(limit);
    }
    posts
}

fn merge_media(previous: &[Media], incoming: Vec<Media>) -> Vec<Media> {
    incoming
        .into_iter()
        .map(|mut item| {
            if item.local_path.is_none() {
                if let Some(prev) = previous.iter().find(|prev| {
                    (!item.media_key.is_empty() && prev.media_key == item.media_key)
                        || (!item.url.is_empty() && prev.url == item.url)
                }) {
                    item.local_path.clone_from(&prev.local_path);
                }
            }
            item
        })
        .collect()
}

fn newest_id(posts: &[Post]) -> Option<String> {
    posts
        .iter()
        .filter(|post| is_snowflake(&post.id))
        .max_by_key(|post| id_num(&post.id))
        .map(|post| post.id.clone())
}

fn id_num(id: &str) -> u128 {
    id.parse().unwrap_or(0)
}

fn load_account(dir: &Path, handle: &str) -> Result<AccountMem, String> {
    let key = handle.to_ascii_lowercase();
    let posts = read_jsonl(&jsonl_path(dir, &key))?;
    let state = read_state(&state_path(dir, &key))?;
    let newest_id = state.newest_id.clone().or_else(|| newest_id(&posts));
    Ok(AccountMem {
        handle: handle.to_string(),
        user_id: state.user_id,
        name: state.name,
        avatar: state.avatar,
        posts,
        last_success: state.last_success_unix,
        newest_id,
        last_error: state.last_error,
    })
}

fn persist(account: &AccountMem, dir: &Path) -> Result<(), String> {
    let key = account.handle.to_ascii_lowercase();
    write_jsonl(&jsonl_path(dir, &key), &account.posts)?;
    let state = AccountState {
        last_success_unix: account.last_success,
        newest_id: account.newest_id.clone(),
        user_id: account.user_id.clone(),
        name: account.name.clone(),
        avatar: account.avatar.clone(),
        last_error: account.last_error.clone(),
    };
    let bytes = serde_json::to_vec(&state).map_err(|err| format!("state: {err}"))?;
    write_atomic(&state_path(dir, &key), &bytes)
}

fn jsonl_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.jsonl"))
}

fn state_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.state.json"))
}

fn read_jsonl(path: &Path) -> Result<Vec<Post>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("read {}: {err}", path.display())),
    };
    let mut posts = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Post>(line) {
            Ok(post) if is_snowflake(&post.id) => posts.push(post),
            Ok(_) => tracing::warn!(path = %path.display(), line = index + 1, "skipped a post"),
            Err(err) => {
                tracing::warn!(path = %path.display(), line = index + 1, error = %err, "skipped a post")
            }
        }
    }
    Ok(merge_posts(posts, Vec::new(), usize::MAX))
}

fn read_state(path: &Path) -> Result<AccountState, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| format!("state: {err}")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AccountState::default()),
        Err(err) => Err(format!("read {}: {err}", path.display())),
    }
}

fn write_jsonl(path: &Path, posts: &[Post]) -> Result<(), String> {
    let mut body = Vec::new();
    for post in posts {
        serde_json::to_writer(&mut body, post).map_err(|err| format!("jsonl: {err}"))?;
        body.push(b'\n');
    }
    write_atomic(path, &body)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| format!("rename {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::IgnoreImages;
    use crate::xapi::XError;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Mock {
        calls: Arc<AtomicUsize>,
        paths: Arc<std::sync::Mutex<Vec<String>>>,
        fail_tweets: Arc<AtomicUsize>,
        /// When set, the first tweet request waits until `release` is signaled.
        hold_first: Arc<AtomicBool>,
        release: Arc<tokio::sync::Notify>,
    }

    impl Mock {
        fn new(fail_tweets: usize) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                paths: Arc::new(std::sync::Mutex::new(Vec::new())),
                fail_tweets: Arc::new(AtomicUsize::new(fail_tweets)),
                hold_first: Arc::new(AtomicBool::new(false)),
                release: Arc::new(tokio::sync::Notify::new()),
            }
        }
    }

    impl XSource for Mock {
        fn fetch<'a>(
            &'a self,
            path: &'a str,
        ) -> impl std::future::Future<Output = Result<String, XError>> + Send + 'a {
            self.paths.lock().expect("paths").push(path.to_string());
            let fail = path.contains("/tweets") && self.fail_tweets.load(Ordering::SeqCst) > 0;
            if fail {
                self.fail_tweets.fetch_sub(1, Ordering::SeqCst);
            }
            let hold = path.contains("/tweets")
                && self.calls.fetch_add(1, Ordering::SeqCst) == 0
                && self.hold_first.load(Ordering::SeqCst);
            let release = Arc::clone(&self.release);
            let path = path.to_string();
            async move {
                if path.starts_with("/2/users/by") {
                    let name = if path.contains("bob") { "bob" } else { "alice" };
                    let id = if name == "bob" { "1" } else { "2" };
                    return Ok(format!(
                        "{{\"data\":[{{\"id\":\"{id}\",\"name\":\"{name}\",\"username\":\"{name}\"}}]}}"
                    ));
                }
                if fail {
                    return Err(XError::RateLimited {
                        retry_after_secs: 30,
                    });
                }
                if hold {
                    release.notified().await;
                }
                let (id, text, created) = if path.contains("/users/1/") {
                    ("10", "from bob", "2024-06-01T00:00:00Z")
                } else {
                    ("11", "from alice", "2024-01-01T00:00:00Z")
                };
                if path.contains("since_id=") {
                    return Ok("{\"meta\":{\"result_count\":0}}".to_string());
                }
                Ok(format!(
                    "{{\"data\":[{{\"id\":\"{id}\",\"text\":\"{text}\",\"created_at\":\"{created}\",\"author_id\":\"1\"}}]}}"
                ))
            }
        }
    }

    fn config(dir: &Path) -> Config {
        Config::from_yaml(&format!(
            "data_dir: \"{}\"\nposts_per_tab: 10\nmax_stored_posts: 10\ntabs:\n  - label: News\n    accounts: [alice]\n  - label: Tech\n    accounts: [bob]\n",
            dir.display()
        ))
        .unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xfeed-{name}-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn jsonl_merge_dedups_and_prefers_the_newer_copy() {
        let old = Post {
            id: "1".into(),
            name: "A".into(),
            username: "a".into(),
            avatar: None,
            text: "old".into(),
            created_at: 10,
            entities: Vec::new(),
            media: Vec::new(),
        };
        let mut newer = old.clone();
        newer.text = "new".into();
        newer.created_at = 20;
        let other = Post {
            id: "2".into(),
            name: "B".into(),
            username: "b".into(),
            avatar: None,
            text: "other".into(),
            created_at: 15,
            entities: Vec::new(),
            media: Vec::new(),
        };
        let merged = merge_posts(vec![old.clone(), other.clone()], vec![newer.clone()], 10);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "1");
        assert_eq!(merged[0].text, "new");
        assert_eq!(merged[1].id, "2");
        let capped = merge_posts(vec![other, newer], Vec::new(), 1);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].id, "1");
        assert_eq!(capped[0].text, "new");

        let mut saved = old.clone();
        saved.media.push(Media {
            kind: "photo".into(),
            url: "https://pbs.twimg.com/media/abc.jpg".into(),
            remote_url: "https://pbs.twimg.com/media/abc.jpg".into(),
            local_path: Some("media/a/3_1.jpg".into()),
            video_url: None,
            media_key: "3_1".into(),
            alt: None,
            width: Some(8),
            height: Some(4),
        });
        let mut incoming = saved.clone();
        incoming.media[0].local_path = None;
        incoming.text = "newer".to_string();
        let merged = merge_posts(vec![saved], vec![incoming], 10);
        assert_eq!(merged[0].text, "newer");
        assert_eq!(
            merged[0].media[0].local_path.as_deref(),
            Some("media/a/3_1.jpg")
        );
    }

    #[tokio::test]
    async fn one_success_per_slot_single_flight_and_restart() {
        let dir = temp_dir("slot");
        let mock = Mock::new(0);
        mock.hold_first.store(true, Ordering::SeqCst);
        let calls = Arc::clone(&mock.calls);
        let paths = Arc::clone(&mock.paths);
        let release = Arc::clone(&mock.release);
        let clock = Clock::system();
        let tz = parse_timezone("Asia/Taipei").unwrap();
        let morning = chrono_tz::Asia::Taipei
            .with_ymd_and_hms(2026, 10, 9, 1, 0, 0)
            .unwrap()
            .timestamp();
        clock.set(morning);
        let store = Store::open_with_clock(config(&dir), mock, IgnoreImages, clock.clone())
            .await
            .unwrap();
        let left = tokio::spawn({
            let store = store.clone();
            async move { store.prepare_tab("news", false).await }
        });
        let right = tokio::spawn({
            let store = store.clone();
            async move { store.prepare_tab("News", false).await }
        });
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.notify_one();
        let (left, right) = tokio::join!(left, right);
        assert!(left
            .unwrap()
            .unwrap()
            .posts
            .iter()
            .any(|post| post.text == "from alice"));
        assert!(right
            .unwrap()
            .unwrap()
            .posts
            .iter()
            .any(|post| post.text == "from alice"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("/2/users/by")));
        assert!(paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("/2/users/2/tweets") && !path.contains("since_id=")));

        let again = store.prepare_tab("news", false).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(again.posts.iter().any(|post| post.text == "from alice"));

        let tech_before = calls.load(Ordering::SeqCst);
        store.prepare_tab("tech", false).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), tech_before + 1);
        assert!(paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("/2/users/1/tweets")));

        let afternoon = chrono_tz::Asia::Taipei
            .with_ymd_and_hms(2026, 10, 9, 7, 0, 0)
            .unwrap()
            .timestamp();
        assert!(!same_slot(morning, afternoon, tz));
        clock.set(afternoon);
        store.prepare_tab("news", false).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), tech_before + 2);
        assert!(paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("since_id=11")));

        let lines = std::fs::read_to_string(dir.join("alice.jsonl")).unwrap();
        let ids: Vec<_> = lines
            .lines()
            .map(|line| serde_json::from_str::<Post>(line).unwrap().id)
            .collect();
        assert_eq!(ids, vec!["11".to_string()]);

        let reopened = Store::open_with_clock(config(&dir), Mock::new(0), IgnoreImages, {
            let clock = Clock::system();
            clock.set(afternoon);
            clock
        })
        .await
        .unwrap();
        let quiet = Arc::clone(&reopened.inner.source.calls);
        reopened.prepare_tab("news", false).await.unwrap();
        assert_eq!(quiet.load(Ordering::SeqCst), 0);

        let forced = reopened
            .update(UpdateTarget::Account("alice".into()))
            .await
            .unwrap();
        assert!(forced[0].error.is_none());
        assert_eq!(quiet.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_fetch_can_be_retried_in_the_same_slot() {
        let dir = temp_dir("retry");
        let mock = Mock::new(1);
        let calls = Arc::clone(&mock.calls);
        let clock = Clock::system();
        clock.set(1_700_000_000);
        let store = Store::open_with_clock(config(&dir), mock, IgnoreImages, clock)
            .await
            .unwrap();
        let first = store.prepare_tab("news", false).await.unwrap();
        assert!(first.message.unwrap().contains("rate limited"));
        assert!(first.posts.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let second = store.prepare_tab("news", false).await.unwrap();
        assert!(second.posts.iter().any(|post| post.text == "from alice"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        store.prepare_tab("news", false).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
