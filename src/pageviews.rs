//! Aggregate page-view counts behind `/popularity.json` (0x6.ai popularity
//! spec v1).
//!
//! Only one number per local day is kept: no IP addresses, user agents,
//! paths, or anything else about a visitor. The User-Agent is looked at once,
//! at request time, to skip bots, and is never stored. Recording a view is one
//! atomic add; the mutex is taken only when the local day rolls over and on
//! flush.
//!
//! The published score is a daily snapshot: it covers the 30 complete local
//! days before today and is computed once per local date (at startup and on
//! the first request after midnight). Within a day every response is the
//! same body with the same `updated` time.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use bytes::Bytes;

use chrono::{DateTime, Days, NaiveDate, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// File under `data_dir` that holds the per-day totals.
pub const FILE_NAME: &str = "pageviews.json";
/// Complete local days summed into the score: the 30 days before today.
pub const WINDOW_DAYS: u64 = 30;
/// Days kept in the file, today included. Older days are dropped on flush.
pub const KEEP_DAYS: u64 = 60;
/// Schema version of `/popularity.json`.
pub const VERSION: u32 = 1;
/// This work's id in 0x6.ai's apps.json.
pub const ID: &str = "x-feed";
pub const METRIC: &str = "pageviews_30d";

/// Lower-case User-Agent fragments that mark a bot, crawler, preview
/// fetcher, monitor, or scripted client.
const BOT_TOKENS: &[&str] = &[
    "bot",
    "crawl",
    "spider",
    "slurp",
    "scrape",
    "archiver",
    "compatible;",
    "headless",
    "preview",
    "inspection",
    "lighthouse",
    "pagespeed",
    "monitor",
    "uptime",
    "pingdom",
    "check",
    "probe",
    "scan",
    "curl",
    "wget",
    "python",
    "go-http",
    "java/",
    "okhttp",
    "httpclient",
    "http-client",
    "libwww",
    "perl",
    "ruby",
    "php",
    "axios",
    "node",
    "undici",
    "fetch",
    "postman",
    "insomnia",
    "phantomjs",
    "selenium",
    "puppeteer",
    "playwright",
    "facebookexternalhit",
    "embedly",
    "whatsapp",
    "telegram",
    "discord",
    "slack",
    "skype",
    "vkshare",
    "quora",
    "pinterest",
    "redditbot",
    "feed",
    "rss",
];

/// True for a browser a person is likely using: the User-Agent is present,
/// starts with `Mozilla/` as every mainstream browser's does, and contains no
/// bot token. Decided per request; nothing about the agent is kept.
pub fn is_human_agent(user_agent: Option<&str>) -> bool {
    let Some(agent) = user_agent.map(str::trim) else {
        return false;
    };
    if agent.len() > 512 || !agent.starts_with("Mozilla/") {
        return false;
    }
    let lower = agent.to_ascii_lowercase();
    !BOT_TOKENS.iter().any(|token| lower.contains(token))
}

/// Sentinel for "use the system clock".
const SYSTEM_TIME: i64 = i64::MIN;

#[derive(Clone)]
pub struct PageViews {
    inner: Arc<Inner>,
}

struct Inner {
    tz: Tz,
    path: Option<PathBuf>,
    /// Views not yet folded into `State::days`. They all belong to `State::day`.
    pending: AtomicU64,
    /// Unix second at which `State::day` ends. A view at or after it rolls the day.
    day_end: AtomicI64,
    /// Fixed clock for tests, or `SYSTEM_TIME`.
    fixed_now: AtomicI64,
    state: Mutex<State>,
    /// Published `/popularity.json` body and the local date it was made for.
    snapshot: RwLock<Option<Snapshot>>,
}

struct Snapshot {
    day: NaiveDate,
    body: Bytes,
}

struct State {
    day: NaiveDate,
    days: BTreeMap<NaiveDate, u64>,
    dirty: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct DiskFormat {
    /// `YYYY-MM-DD` in the configured time zone → views that day.
    days: BTreeMap<String, u64>,
}

/// Field order is the wire order required by the spec.
#[derive(Serialize)]
struct Popularity<'a> {
    version: u32,
    id: &'a str,
    metric: &'a str,
    score: u64,
    window_days: u64,
    updated: String,
}

impl PageViews {
    /// Counts in memory only. Used by tests and when no file is wanted.
    pub fn in_memory(tz: Tz) -> Self {
        Self::build(tz, None, BTreeMap::new())
    }

    /// Loads `path` if it exists. A file that does not parse is moved aside
    /// to `pageviews.json.corrupt` so it is not overwritten, and counting
    /// starts from zero.
    pub fn load(path: PathBuf, tz: Tz) -> Result<Self, String> {
        let days = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<DiskFormat>(&bytes) {
                Ok(disk) => disk
                    .days
                    .into_iter()
                    .filter_map(|(day, count)| {
                        NaiveDate::parse_from_str(&day, "%Y-%m-%d")
                            .ok()
                            .map(|day| (day, count))
                    })
                    .collect(),
                Err(err) => {
                    let aside = path.with_extension("json.corrupt");
                    tracing::warn!(file = %path.display(), "page views file unreadable ({err}); moved to {}", aside.display());
                    std::fs::rename(&path, &aside)
                        .map_err(|err| format!("rename {}: {err}", path.display()))?;
                    BTreeMap::new()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => return Err(format!("read {}: {err}", path.display())),
        };
        Ok(Self::build(tz, Some(path), days))
    }

    fn build(tz: Tz, path: Option<PathBuf>, days: BTreeMap<NaiveDate, u64>) -> Self {
        let now = system_now();
        let day = local_date(now, tz);
        let views = Self {
            inner: Arc::new(Inner {
                tz,
                path,
                pending: AtomicU64::new(0),
                day_end: AtomicI64::new(day_end(day, tz, now)),
                fixed_now: AtomicI64::new(SYSTEM_TIME),
                state: Mutex::new(State {
                    day,
                    days,
                    dirty: false,
                }),
                snapshot: RwLock::new(None),
            }),
        };
        // The startup snapshot.
        views.popularity_json();
        views
    }

    /// Pins the clock (tests). The next view re-checks the day and the next
    /// read makes a new snapshot.
    pub fn set_now(&self, unix: i64) {
        self.inner.fixed_now.store(unix, Ordering::Relaxed);
        self.inner.day_end.store(i64::MIN, Ordering::Relaxed);
        *self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(|err| err.into_inner()) = None;
    }

    fn now(&self) -> i64 {
        match self.inner.fixed_now.load(Ordering::Relaxed) {
            SYSTEM_TIME => system_now(),
            fixed => fixed,
        }
    }

    /// Counts one page view.
    pub fn record(&self) {
        let now = self.now();
        if now >= self.inner.day_end.load(Ordering::Relaxed) {
            self.roll(now);
        }
        self.inner.pending.fetch_add(1, Ordering::Relaxed);
    }

    fn roll(&self, now: i64) {
        let mut state = self.lock();
        if now < self.inner.day_end.load(Ordering::Relaxed) {
            return;
        }
        let moved = self.inner.pending.swap(0, Ordering::Relaxed);
        fold(&mut state, moved);
        let today = local_date(now, self.inner.tz);
        state.day = today;
        self.inner
            .day_end
            .store(day_end(today, self.inner.tz, now), Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    /// Views counted so far on the current local day (live, not published).
    pub fn views_today(&self) -> u64 {
        let today = local_date(self.now(), self.inner.tz);
        self.sum_days(today, today)
    }

    /// Views in the `WINDOW_DAYS` complete local days before the day of `now`.
    fn score_at(&self, now: i64) -> u64 {
        let today = local_date(now, self.inner.tz);
        let (Some(from), Some(to)) = (
            today.checked_sub_days(Days::new(WINDOW_DAYS)),
            today.checked_sub_days(Days::new(1)),
        ) else {
            return 0;
        };
        self.sum_days(from, to)
    }

    fn sum_days(&self, from: NaiveDate, to: NaiveDate) -> u64 {
        let state = self.lock();
        let pending = self.inner.pending.load(Ordering::Relaxed);
        let mut total: u64 = state
            .days
            .range(from..=to)
            .map(|(_, count)| *count)
            .fold(0, u64::saturating_add);
        if (from..=to).contains(&state.day) {
            total = total.saturating_add(pending);
        }
        total
    }

    /// Body of `/popularity.json`: today's snapshot, made on the first call
    /// of each local date.
    pub fn popularity_json(&self) -> Bytes {
        let now = self.now();
        let today = local_date(now, self.inner.tz);
        if let Some(snapshot) = self
            .inner
            .snapshot
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .as_ref()
            .filter(|snapshot| snapshot.day == today)
        {
            return snapshot.body.clone();
        }
        let mut slot = self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(snapshot) = slot.as_ref().filter(|snapshot| snapshot.day == today) {
            return snapshot.body.clone();
        }
        let updated = DateTime::<Utc>::from_timestamp(now, 0)
            .unwrap_or_default()
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let body = Bytes::from(
            serde_json::to_vec(&Popularity {
                version: VERSION,
                id: ID,
                metric: METRIC,
                score: self.score_at(now),
                window_days: WINDOW_DAYS,
                updated,
            })
            .unwrap_or_else(|_| b"{}".to_vec()),
        );
        *slot = Some(Snapshot {
            day: today,
            body: body.clone(),
        });
        body
    }

    /// Folds pending views, drops days older than `KEEP_DAYS`, and writes the
    /// file with tmp + rename when anything changed. Returns whether it wrote.
    pub fn flush(&self) -> Result<bool, String> {
        let Some(path) = self.inner.path.as_deref() else {
            return Ok(false);
        };
        let today = local_date(self.now(), self.inner.tz);
        let body = {
            let mut state = self.lock();
            let moved = self.inner.pending.swap(0, Ordering::Relaxed);
            fold(&mut state, moved);
            if let Some(cutoff) = today.checked_sub_days(Days::new(KEEP_DAYS - 1)) {
                let before = state.days.len();
                state.days.retain(|day, _| *day >= cutoff);
                if state.days.len() != before {
                    state.dirty = true;
                }
            }
            if !state.dirty {
                return Ok(false);
            }
            state.dirty = false;
            let disk = DiskFormat {
                days: state
                    .days
                    .iter()
                    .map(|(day, count)| (day.format("%Y-%m-%d").to_string(), *count))
                    .collect(),
            };
            serde_json::to_vec_pretty(&disk).map_err(|err| err.to_string())?
        };
        if let Err(err) = write_atomic(path, &body) {
            self.lock().dirty = true;
            return Err(err);
        }
        Ok(true)
    }
}

fn fold(state: &mut State, moved: u64) {
    if moved > 0 {
        let day = state.day;
        let slot = state.days.entry(day).or_insert(0);
        *slot = slot.saturating_add(moved);
        state.dirty = true;
    }
}

fn system_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn local_date(unix: i64, tz: Tz) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(unix, 0)
        .unwrap_or_default()
        .with_timezone(&tz)
        .date_naive()
}

/// First second of the local day after `day`. Falls back to an hour from
/// `now` if that midnight does not exist in the zone.
fn day_end(day: NaiveDate, tz: Tz, now: i64) -> i64 {
    day.succ_opt()
        .and_then(|next| next.and_hms_opt(0, 0, 0))
        .and_then(|midnight| tz.from_local_datetime(&midnight).earliest())
        .map(|start| start.timestamp())
        .unwrap_or(now + 3600)
}

fn write_atomic(path: &Path, body: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| format!("mkdir {}: {err}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file =
        std::fs::File::create(&tmp).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    file.write_all(body)
        .and_then(|_| file.sync_all())
        .map_err(|err| format!("write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| format!("rename {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taipei() -> Tz {
        "Asia/Taipei".parse().unwrap()
    }

    fn at(day: u32, hour: u32) -> i64 {
        taipei()
            .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
            .unwrap()
            .timestamp()
    }

    fn temp_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xfeed-views-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join(FILE_NAME)
    }

    #[test]
    fn days_follow_taipei_midnight() {
        let views = PageViews::in_memory(taipei());
        views.set_now(at(20, 23));
        views.record();
        views.set_now(at(21, 0));
        views.record();
        views.record();
        let state = views.lock();
        let pending = views.inner.pending.load(Ordering::Relaxed);
        assert_eq!(state.day, NaiveDate::from_ymd_opt(2026, 10, 21).unwrap());
        assert_eq!(
            state
                .days
                .get(&NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()),
            Some(&1)
        );
        assert_eq!(pending, 2);
    }

    fn body(views: &PageViews) -> String {
        String::from_utf8(views.popularity_json().to_vec()).unwrap()
    }

    #[test]
    fn score_sums_the_thirty_complete_days_before_today() {
        let views = PageViews::in_memory(taipei());
        let day = |month, day| {
            taipei()
                .with_ymd_and_hms(2026, month, day, 12, 0, 0)
                .unwrap()
                .timestamp()
        };
        // For 2026-10-21 the window is 2026-09-21 ..= 2026-10-20.
        for (when, count) in [
            (day(9, 20), 7),
            (day(9, 21), 5),
            (day(10, 1), 3),
            (day(10, 20), 2),
            (day(10, 21), 1),
        ] {
            views.set_now(when);
            for _ in 0..count {
                views.record();
            }
        }
        assert_eq!(views.score_at(at(21, 23)), 10);
        assert_eq!(views.score_at(at(22, 1)), 6, "09-21 left, 10-21 joined");
        assert_eq!(views.views_today(), 1);
    }

    #[test]
    fn popularity_json_matches_spec_v1_exactly() {
        let views = PageViews::in_memory(taipei());
        views.set_now(at(21, 9));
        assert_eq!(
            body(&views),
            r#"{"version":1,"id":"x-feed","metric":"pageviews_30d","score":0,"window_days":30,"updated":"2026-10-21T01:00:00Z"}"#,
            "no data yet publishes score 0"
        );
        let json: serde_json::Value = serde_json::from_str(&body(&views)).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["id", "metric", "score", "updated", "version", "window_days"]
        );
        assert!(json["score"].is_u64());
        assert!(json["updated"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn snapshot_is_fixed_within_a_day_and_renews_after_midnight() {
        let views = PageViews::in_memory(taipei());
        views.set_now(at(20, 10));
        for _ in 0..4 {
            views.record();
        }
        views.set_now(at(21, 0) + 30);
        let first = body(&views);
        assert_eq!(
            first,
            r#"{"version":1,"id":"x-feed","metric":"pageviews_30d","score":4,"window_days":30,"updated":"2026-10-20T16:00:30Z"}"#
        );
        // Later the same Taipei day: more views, same published body.
        views.inner.fixed_now.store(at(21, 23), Ordering::Relaxed);
        for _ in 0..3 {
            views.record();
        }
        assert_eq!(views.views_today(), 3);
        assert_eq!(body(&views), first);
        // First request after the next midnight makes a new snapshot.
        views.inner.fixed_now.store(at(22, 8), Ordering::Relaxed);
        assert_eq!(
            body(&views),
            r#"{"version":1,"id":"x-feed","metric":"pageviews_30d","score":7,"window_days":30,"updated":"2026-10-22T00:00:00Z"}"#
        );
    }

    #[test]
    fn bots_and_scripts_are_not_humans() {
        let humans = [
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0",
            "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36",
        ];
        for agent in humans {
            assert!(is_human_agent(Some(agent)), "{agent}");
        }
        let bots = [
            "",
            "   ",
            "curl/8.5.0",
            "Wget/1.21",
            "python-requests/2.32",
            "Go-http-client/2.0",
            "node",
            "undici",
            "node-fetch/1.0",
            "axios/1.7",
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)",
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2)",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/130.0.0.0 Safari/537.36",
            "facebookexternalhit/1.1 (+http://www.facebook.com/externalhit_uatext.php)",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_10_1) AppleWebKit/600.2.5 (KHTML, like Gecko) Version/8.0.2 Safari/600.2.5 (Applebot/0.1)",
            "Mozilla/5.0 (X11; Linux x86_64) Chrome-Lighthouse",
            "Twitterbot/1.0",
            "Slackbot-LinkExpanding 1.0 (+https://api.slack.com/robots)",
            "Mozilla/5.0 (compatible; UptimeRobot/2.0; http://www.uptimerobot.com/)",
        ];
        for agent in bots {
            assert!(!is_human_agent(Some(agent)), "{agent:?}");
        }
        assert!(!is_human_agent(None));
    }

    #[test]
    fn flush_persists_reloads_and_drops_old_days() {
        let path = temp_file();
        let views = PageViews::load(path.clone(), taipei()).unwrap();
        assert!(!views.flush().unwrap(), "nothing to write yet");
        let old = taipei()
            .with_ymd_and_hms(2026, 8, 1, 12, 0, 0)
            .unwrap()
            .timestamp();
        views.set_now(old);
        views.record();
        views.set_now(at(21, 9));
        views.record();
        views.record();
        assert!(views.flush().unwrap());
        assert!(!views.flush().unwrap(), "unchanged");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"2026-10-21\": 2"));
        assert!(!text.contains("2026-08-01"), "older than 60 days");
        assert!(!path.with_extension("json.tmp").exists());

        let again = PageViews::load(path.clone(), taipei()).unwrap();
        again.set_now(at(21, 10));
        again.record();
        assert_eq!(again.views_today(), 3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unreadable_file_is_kept_aside() {
        let path = temp_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        let views = PageViews::load(path.clone(), taipei()).unwrap();
        assert_eq!(views.views_today(), 0);
        assert!(body(&views).contains(r#""score":0,"#));
        assert!(path.with_extension("json.corrupt").exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
