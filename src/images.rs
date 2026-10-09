//! Local copies of post images.
//!
//! Photos are downloaded from `pbs.twimg.com`. Videos and GIFs keep their
//! original file URL and store the preview image instead. A file named
//! `media/{account}/{media_key}.{ext}` is not downloaded again. Image
//! requests use their own client, so they are not X API calls.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

use crate::model::{is_handle, safe_media_key, Media, Post};
use crate::xapi::allowed_twimg;

pub const FETCH_LIMIT: usize = 4;
pub const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

#[derive(Clone)]
pub struct ImageBody {
    pub bytes: Vec<u8>,
}

pub trait ImageSource: Clone + Send + Sync + 'static {
    fn get<'a>(
        &'a self,
        url: &'a str,
    ) -> impl Future<Output = Result<ImageBody, String>> + Send + 'a;
}

#[derive(Clone)]
pub struct ImageClient {
    http: reqwest::Client,
    max_bytes: u64,
}

impl ImageClient {
    pub fn new(max_bytes: u64) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent("xfeed/0.1")
            .timeout(std::time::Duration::from_secs(10))
            .connect_timeout(std::time::Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .tcp_nodelay(true)
            .build()
            .map_err(|err| format!("image client: {err}"))?;
        Ok(Self { http, max_bytes })
    }
}

impl ImageSource for ImageClient {
    fn get<'a>(
        &'a self,
        url: &'a str,
    ) -> impl Future<Output = Result<ImageBody, String>> + Send + 'a {
        let http = self.http.clone();
        let max_bytes = self.max_bytes;
        async move {
            if !allowed_twimg(url) {
                return Err("image host is not allowed".into());
            }
            let response = http
                .get(url)
                .header(reqwest::header::ACCEPT, "image/*")
                .send()
                .await
                .map_err(|_| "image download failed".to_string())?;
            if !response.status().is_success() {
                return Err(format!("image download failed ({})", response.status()));
            }
            if response.content_length().is_some_and(|len| len > max_bytes) {
                return Err("image is larger than max_image_bytes".into());
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|_| "image download failed".to_string())?;
            if bytes.len() as u64 > max_bytes {
                return Err("image is larger than max_image_bytes".into());
            }
            Ok(ImageBody {
                bytes: bytes.to_vec(),
            })
        }
    }
}

/// Used when a test does not exercise image downloads.
#[derive(Clone, Default)]
pub struct IgnoreImages;

impl ImageSource for IgnoreImages {
    async fn get(&self, _url: &str) -> Result<ImageBody, String> {
        Err("no image client".into())
    }
}

/// Download images that are not already on disk and record their relative paths.
pub async fn cache_images<I: ImageSource>(
    dir: &Path,
    account: &str,
    posts: &mut [Post],
    images: &I,
    max_bytes: u64,
) {
    if max_bytes == 0 || posts.is_empty() {
        return;
    }
    let account = account.trim().trim_start_matches('@').to_ascii_lowercase();
    if !is_handle(&account) {
        return;
    }
    for post in posts.iter_mut() {
        for media in &mut post.media {
            if let Some(rel) = existing_rel(dir, &account, &media.media_key) {
                media.local_path = Some(rel);
            } else {
                media.local_path = None;
            }
        }
    }
    let mut jobs = Vec::new();
    let mut seen = HashSet::new();
    for post in posts.iter() {
        for media in &post.media {
            if media.local_path.is_some() || !safe_media_key(&media.media_key) {
                continue;
            }
            let url = remote_image_url(media);
            if !allowed_twimg(url) || !seen.insert(media.media_key.clone()) {
                continue;
            }
            jobs.push((media.media_key.clone(), url.to_string()));
        }
    }
    if jobs.is_empty() {
        return;
    }

    let permits = Arc::new(Semaphore::new(FETCH_LIMIT));
    let mut set = JoinSet::new();
    for (key, url) in jobs {
        let images = images.clone();
        let permits = Arc::clone(&permits);
        let dir = dir.to_path_buf();
        let account = account.clone();
        set.spawn(async move {
            let Ok(_permit) = acquire(permits).await else {
                return None;
            };
            match images.get(&url).await {
                Ok(body) => match write_image(&dir, &account, &key, &body.bytes, max_bytes) {
                    Ok(rel) => Some((key, rel)),
                    Err(err) => {
                        tracing::warn!(media = %key, error = %err, "image not saved");
                        None
                    }
                },
                Err(err) => {
                    tracing::warn!(media = %key, error = %err, "image download failed");
                    None
                }
            }
        });
    }
    while let Some(joined) = set.join_next().await {
        let Ok(Some((key, rel))) = joined else {
            continue;
        };
        for post in posts.iter_mut() {
            for media in &mut post.media {
                if media.media_key == key {
                    media.local_path = Some(rel.clone());
                }
            }
        }
    }
}

async fn acquire(
    permits: Arc<Semaphore>,
) -> Result<OwnedSemaphorePermit, tokio::sync::AcquireError> {
    permits.acquire_owned().await
}

pub fn display_src(base: &str, item: &Media) -> String {
    if let Some(path) = item.local_path.as_deref() {
        if is_public_media_path(path) {
            if base.is_empty() {
                return format!("/{path}");
            }
            return format!("{base}/{path}");
        }
    }
    item.url.clone()
}

pub fn is_public_media_path(path: &str) -> bool {
    let mut parts = path.split('/');
    if parts.next() != Some("media") {
        return false;
    }
    let Some(account) = parts.next() else {
        return false;
    };
    let Some(file) = parts.next() else {
        return false;
    };
    parts.next().is_none() && is_handle(account) && safe_media_file(file)
}

pub fn resolve_media(data_dir: &Path, account: &str, file: &str) -> Option<PathBuf> {
    if !is_handle(account) || !safe_media_file(file) {
        return None;
    }
    let account = account.to_ascii_lowercase();
    let root = data_dir.join("media");
    let path = root.join(&account).join(file);
    let canon = path.canonicalize().ok()?;
    let root_canon = root.canonicalize().ok()?;
    if canon.starts_with(&root_canon) {
        Some(canon)
    } else {
        None
    }
}

pub struct PruneReport {
    pub removed: usize,
    pub kept: usize,
}

/// Delete files under `{data_dir}/media` that no JSONL post references.
pub fn prune_media(data_dir: &Path) -> Result<PruneReport, String> {
    let mut keep = HashSet::new();
    if data_dir.is_dir() {
        for entry in std::fs::read_dir(data_dir)
            .map_err(|err| format!("read {}: {err}", data_dir.display()))?
        {
            let entry = entry.map_err(|err| format!("read {}: {err}", data_dir.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.ends_with(".jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path())
                .map_err(|err| format!("read {}: {err}", entry.path().display()))?;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(post) = serde_json::from_str::<Post>(line) else {
                    continue;
                };
                for media in post.media {
                    if let Some(path) = media.local_path {
                        if is_public_media_path(&path) {
                            keep.insert(path);
                        }
                    }
                }
            }
        }
    }
    let root = data_dir.join("media");
    if !root.is_dir() {
        return Ok(PruneReport {
            removed: 0,
            kept: 0,
        });
    }
    let mut removed = 0usize;
    let mut kept = 0usize;
    let accounts =
        std::fs::read_dir(&root).map_err(|err| format!("read {}: {err}", root.display()))?;
    for account in accounts {
        let account = account.map_err(|err| format!("read {}: {err}", root.display()))?;
        if !account
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        let Some(account_name) = account.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let files = std::fs::read_dir(account.path())
            .map_err(|err| format!("read {}: {err}", account.path().display()))?;
        for file in files {
            let file = file.map_err(|err| format!("read {}: {err}", account.path().display()))?;
            let kind = file.file_type().map_err(|err| format!("stat: {err}"))?;
            if !kind.is_file() {
                continue;
            }
            let Some(name) = file.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let rel = format!("media/{account_name}/{name}");
            if keep.contains(&rel) && is_public_media_path(&rel) {
                kept += 1;
                continue;
            }
            std::fs::remove_file(file.path())
                .map_err(|err| format!("remove {}: {err}", file.path().display()))?;
            removed += 1;
        }
        let leftover = std::fs::read_dir(account.path())
            .map_err(|err| format!("read {}: {err}", account.path().display()))?;
        if leftover.count() == 0 {
            let _ = std::fs::remove_dir(account.path());
        }
    }
    Ok(PruneReport { removed, kept })
}

fn remote_image_url(media: &Media) -> &str {
    if !media.remote_url.is_empty() {
        &media.remote_url
    } else {
        &media.url
    }
}

fn existing_rel(dir: &Path, account: &str, key: &str) -> Option<String> {
    if !safe_media_key(key) {
        return None;
    }
    for ext in ["jpg", "jpeg", "png", "webp", "gif"] {
        let rel = format!("media/{account}/{key}.{ext}");
        if dir.join(&rel).is_file() {
            return Some(rel);
        }
    }
    None
}

pub fn safe_media_file(file: &str) -> bool {
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif") && safe_media_key(stem)
}

fn write_image(
    dir: &Path,
    account: &str,
    key: &str,
    bytes: &[u8],
    max_bytes: u64,
) -> Result<String, String> {
    if bytes.len() as u64 > max_bytes {
        return Err("image is larger than max_image_bytes".into());
    }
    let Some(ext) = image_ext(bytes) else {
        return Err("image is not a jpeg, png, gif, or webp".into());
    };
    if let Some(rel) = existing_rel(dir, account, key) {
        return Ok(rel);
    }
    let folder = dir.join("media").join(account);
    std::fs::create_dir_all(&folder)
        .map_err(|err| format!("create {}: {err}", folder.display()))?;
    let name = format!("{key}.{ext}");
    let dest = folder.join(&name);
    let tmp = folder.join(format!(".{key}.{ext}.tmp"));
    std::fs::write(&tmp, bytes).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|err| format!("rename {}: {err}", dest.display()))?;
    Ok(format!("media/{account}/{name}"))
}

fn image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0x00, 0x01];

    fn post(key: &str, url: &str) -> Post {
        Post {
            id: "1".into(),
            name: "A".into(),
            username: "alice".into(),
            avatar: None,
            text: "pic".into(),
            created_at: 1,
            entities: Vec::new(),
            media: vec![Media {
                kind: "photo".into(),
                url: url.into(),
                remote_url: url.into(),
                local_path: None,
                video_url: None,
                media_key: key.into(),
                alt: Some("hill".into()),
                width: Some(8),
                height: Some(4),
            }],
        }
    }

    #[derive(Clone)]
    struct Mock {
        calls: Arc<AtomicUsize>,
        current: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        fail: Arc<AtomicUsize>,
    }

    impl ImageSource for Mock {
        fn get<'a>(
            &'a self,
            url: &'a str,
        ) -> impl Future<Output = Result<ImageBody, String>> + Send + 'a {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let fail = url.contains("frame") && self.fail.load(Ordering::SeqCst) > 0;
            if fail {
                self.fail.fetch_sub(1, Ordering::SeqCst);
            }
            let current = Arc::clone(&self.current);
            let max_in_flight = Arc::clone(&self.max_in_flight);
            async move {
                let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                current.fetch_sub(1, Ordering::SeqCst);
                if fail {
                    Err("download failed".into())
                } else if url.contains("evil") {
                    Ok(ImageBody {
                        bytes: b"not an image".to_vec(),
                    })
                } else {
                    Ok(ImageBody {
                        bytes: JPEG.to_vec(),
                    })
                }
            }
        }
    }

    #[tokio::test]
    async fn skips_files_that_already_exist_and_limits_concurrency() {
        let dir = std::env::temp_dir().join(format!("xfeed-img-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("media/alice")).unwrap();
        std::fs::write(dir.join("media/alice/3_9.jpg"), JPEG).unwrap();
        let mock = Mock {
            calls: Arc::new(AtomicUsize::new(0)),
            current: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
            fail: Arc::new(AtomicUsize::new(1)),
        };
        let mut posts = vec![
            post("3_9", "https://pbs.twimg.com/media/already.jpg"),
            post("3_1", "https://pbs.twimg.com/media/one.jpg"),
            post("3_2", "https://pbs.twimg.com/media/two.jpg"),
            post("3_3", "https://pbs.twimg.com/media/three.jpg"),
            post("3_4", "https://pbs.twimg.com/media/four.jpg"),
            post("3_5", "https://pbs.twimg.com/media/five.jpg"),
            post("13_2", "https://pbs.twimg.com/ext_tw_video_thumb/frame.jpg"),
            post("7_3", "https://pbs.twimg.com/media/evil.jpg"),
        ];
        // The helper builds one post per call; give them distinct ids so they stay separate.
        for (index, post) in posts.iter_mut().enumerate() {
            post.id = index.to_string();
        }
        cache_images(&dir, "alice", &mut posts, &mock, 1_000).await;
        assert!(posts[0].media[0].local_path.as_deref() == Some("media/alice/3_9.jpg"));
        assert_eq!(
            posts[1].media[0].local_path.as_deref(),
            Some("media/alice/3_1.jpg")
        );
        assert!(dir.join("media/alice/3_1.jpg").is_file());
        assert!(
            posts[6].media[0].local_path.is_none(),
            "failed preview is retried later"
        );
        assert!(
            posts[7].media[0].local_path.is_none(),
            "non-images are not stored"
        );
        let calls = mock.calls.load(Ordering::SeqCst);
        assert_eq!(calls, 7, "the file already on disk is not downloaded");
        assert!(mock.max_in_flight.load(Ordering::SeqCst) <= FETCH_LIMIT);
        assert!(mock.max_in_flight.load(Ordering::SeqCst) >= 2);

        cache_images(&dir, "alice", &mut posts, &mock, 1_000).await;
        assert_eq!(
            mock.calls.load(Ordering::SeqCst),
            calls + 2,
            "missing files are downloaded again, saved files are not"
        );
        assert_eq!(
            posts[6].media[0].local_path.as_deref(),
            Some("media/alice/13_2.jpg")
        );

        let orphan = dir.join("media/alice/9_9.jpg");
        std::fs::write(&orphan, JPEG).unwrap();
        let line = serde_json::to_string(&posts[1]).unwrap();
        std::fs::write(dir.join("alice.jsonl"), format!("{line}\n")).unwrap();
        let report = prune_media(&dir).unwrap();
        assert!(!orphan.exists());
        assert!(dir.join("media/alice/3_1.jpg").is_file());
        assert!(report.removed >= 1);
        assert!(report.kept >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn display_src_prefers_a_local_path() {
        let mut item = post("3_1", "https://pbs.twimg.com/media/abc.jpg")
            .media
            .remove(0);
        assert_eq!(
            display_src("/x", &item),
            "https://pbs.twimg.com/media/abc.jpg"
        );
        item.local_path = Some("media/alice/3_1.jpg".into());
        assert_eq!(display_src("/x", &item), "/x/media/alice/3_1.jpg");
        assert_eq!(display_src("", &item), "/media/alice/3_1.jpg");
        item.local_path = Some("../secret".into());
        assert_eq!(
            display_src("/x", &item),
            "https://pbs.twimg.com/media/abc.jpg"
        );
    }
}
