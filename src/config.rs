//! YAML configuration.
//!
//! The bearer token may live in this file (`x_bearer_token`). `X_BEARER_TOKEN`
//! overrides it when set. [`Config`]'s `Debug` impl redacts the token.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;

const API_DEFAULT: &str = "https://api.x.com";

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub listen: String,
    pub base_path: String,
    pub cache_max_age_secs: u64,
    pub posts_per_tab: usize,
    pub max_stored_posts: usize,
    pub max_image_bytes: u64,
    pub exclude_replies: bool,
    pub exclude_retweets: bool,
    pub api_base: String,
    pub data_dir: String,
    pub timezone: String,
    /// Shown after display times instead of the UTC offset, e.g. `Taiwan`.
    pub timezone_label: Option<String>,
    /// Token from the file. Not the env override. Redacted in `Debug`.
    pub x_bearer_token: String,
    pub tabs: Vec<Tab>,
    /// Optional site credits shown in the page footer.
    pub footer: Option<Footer>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("base_path", &self.base_path)
            .field("cache_max_age_secs", &self.cache_max_age_secs)
            .field("posts_per_tab", &self.posts_per_tab)
            .field("max_stored_posts", &self.max_stored_posts)
            .field("max_image_bytes", &self.max_image_bytes)
            .field("exclude_replies", &self.exclude_replies)
            .field("exclude_retweets", &self.exclude_retweets)
            .field("api_base", &self.api_base)
            .field("data_dir", &self.data_dir)
            .field("timezone", &self.timezone)
            .field("timezone_label", &self.timezone_label)
            .field("x_bearer_token", &"<redacted>")
            .field("tabs", &self.tabs)
            .field("footer", &self.footer)
            .finish()
    }
}

/// Credits in the page footer: an optional linked text, extra links, and a
/// plain-text note such as a disclaimer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    pub text: Option<String>,
    pub url: Option<String>,
    pub links: Vec<FooterLink>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub id: String,
    pub label: String,
    pub accounts: Vec<String>,
}

#[derive(Debug)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path)
            .map_err(|err| ConfigError::new(format!("read {}: {err}", path.display())))?;
        Self::from_yaml(&text).map_err(|err| ConfigError::new(format!("{}: {err}", path.display())))
    }

    pub fn from_yaml(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            serde_yaml::from_str(text).map_err(|err| ConfigError::new(format!("config: {err}")))?;
        raw.normalize()
    }

    /// File token, unless `from_env` is set and not blank. Never includes the token in errors.
    pub fn resolve_bearer_token(&self, from_env: Option<&str>) -> Result<String, ConfigError> {
        crate::config::resolve_bearer_token(&self.x_bearer_token, from_env)
    }

    /// The configured IANA zone. `timezone` is validated on load, so the UTC
    /// fallback only applies to a hand-built `Config`.
    pub fn tz(&self) -> chrono_tz::Tz {
        self.timezone.parse().unwrap_or(chrono_tz::UTC)
    }

    /// Zone and optional label used for every time shown on a page.
    pub fn display_zone(&self) -> crate::model::DisplayZone<'_> {
        crate::model::DisplayZone {
            tz: self.tz(),
            label: self.timezone_label.as_deref(),
        }
    }

    /// Account handles in first-seen order, without case-insensitive duplicates.
    pub fn handles(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for tab in &self.tabs {
            for account in &tab.accounts {
                if seen.insert(account.to_ascii_lowercase()) {
                    out.push(account.as_str());
                }
            }
        }
        out
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default = "default_listen")]
    listen: String,
    #[serde(default)]
    base_path: String,
    #[serde(default = "default_cache")]
    cache_max_age_secs: u64,
    #[serde(default = "default_posts")]
    posts_per_tab: usize,
    #[serde(default = "default_stored")]
    max_stored_posts: usize,
    #[serde(default = "default_image_bytes")]
    max_image_bytes: u64,
    #[serde(default = "default_true")]
    exclude_replies: bool,
    #[serde(default = "default_true")]
    exclude_retweets: bool,
    #[serde(default = "default_api")]
    api_base: String,
    #[serde(default = "default_data_dir")]
    data_dir: String,
    #[serde(default = "default_timezone")]
    timezone: String,
    #[serde(default)]
    timezone_label: Option<String>,
    #[serde(default)]
    x_bearer_token: String,
    tabs: Vec<RawTab>,
    #[serde(default)]
    footer: Option<RawFooter>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFooter {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    links: Vec<RawFooterLink>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFooterLink {
    label: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTab {
    label: String,
    accounts: Vec<String>,
}

fn default_listen() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_cache() -> u64 {
    0
}

fn default_stored() -> usize {
    400
}

fn default_image_bytes() -> u64 {
    8_000_000
}

fn default_data_dir() -> String {
    "data".to_string()
}

fn default_timezone() -> String {
    "Asia/Taipei".to_string()
}

fn default_posts() -> usize {
    20
}

fn default_true() -> bool {
    true
}

fn default_api() -> String {
    API_DEFAULT.to_string()
}

impl RawConfig {
    fn normalize(self) -> Result<Config, ConfigError> {
        if self.listen.trim().is_empty()
            || self.listen.len() > 128
            || self.listen.chars().any(char::is_whitespace)
        {
            return Err(ConfigError::new(
                "config: listen must be a host:port without spaces",
            ));
        }
        if self.cache_max_age_secs > 86_400 {
            return Err(ConfigError::new(
                "config: cache_max_age_secs must be between 0 and 86400",
            ));
        }
        if !(1..=100).contains(&self.posts_per_tab) {
            return Err(ConfigError::new(
                "config: posts_per_tab must be between 1 and 100",
            ));
        }
        if !(1..=5_000).contains(&self.max_stored_posts) {
            return Err(ConfigError::new(
                "config: max_stored_posts must be between 1 and 5000",
            ));
        }
        if self.max_image_bytes > 50_000_000 {
            return Err(ConfigError::new(
                "config: max_image_bytes must be between 0 and 50000000",
            ));
        }
        if self.max_stored_posts < self.posts_per_tab {
            return Err(ConfigError::new(
                "config: max_stored_posts must be at least posts_per_tab",
            ));
        }
        let data_dir = self.data_dir.trim().to_string();
        if data_dir.is_empty() || data_dir.contains('\0') {
            return Err(ConfigError::new("config: data_dir must be a path"));
        }
        let timezone = self.timezone.trim().to_string();
        if timezone.parse::<chrono_tz::Tz>().is_err() {
            return Err(ConfigError::new(format!(
                "config: timezone \"{timezone}\" is not an IANA time zone"
            )));
        }
        let timezone_label = match self.timezone_label.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(label) if label.chars().count() > 32 || label.chars().any(char::is_control) => {
                return Err(ConfigError::new(
                    "config: timezone_label must be at most 32 printable characters",
                ));
            }
            Some(label) => Some(label.to_string()),
        };
        if self.tabs.is_empty() {
            return Err(ConfigError::new("config: at least one tab is required"));
        }
        if self.tabs.len() > 30 {
            return Err(ConfigError::new("config: at most 30 tabs"));
        }

        let mut tabs = Vec::with_capacity(self.tabs.len());
        let mut ids = HashSet::new();
        let mut accounts_total = HashSet::new();
        for (index, tab) in self.tabs.into_iter().enumerate() {
            let label = tab.label.trim();
            if label.is_empty() || label.chars().count() > 40 {
                return Err(ConfigError::new(
                    "config: each tab label must be 1–40 characters",
                ));
            }
            if tab.accounts.is_empty() {
                return Err(ConfigError::new(format!(
                    "config: tab \"{label}\" needs at least one account"
                )));
            }
            if tab.accounts.len() > 25 {
                return Err(ConfigError::new(format!(
                    "config: tab \"{label}\" has more than 25 accounts"
                )));
            }
            let mut id = slug(label);
            if id.is_empty() {
                id = format!("tab-{}", index + 1);
            }
            if id.starts_with(|ch: char| ch.is_ascii_digit()) {
                id = format!("t-{id}");
            }
            if !ids.insert(id.clone()) {
                return Err(ConfigError::new(format!(
                    "config: two tabs share the id \"{id}\""
                )));
            }
            let mut accounts = Vec::new();
            let mut seen = HashSet::new();
            for account in tab.accounts {
                let handle = normalize_handle(&account).map_err(|err| {
                    ConfigError::new(format!("config: tab \"{label}\" account {err}"))
                })?;
                accounts_total.insert(handle.to_ascii_lowercase());
                if seen.insert(handle.to_ascii_lowercase()) {
                    accounts.push(handle);
                }
            }
            tabs.push(Tab {
                id,
                label: label.to_string(),
                accounts,
            });
        }
        if accounts_total.len() > 100 {
            return Err(ConfigError::new("config: at most 100 distinct accounts"));
        }

        let x_bearer_token = strip_bearer_prefix(self.x_bearer_token.trim()).to_string();
        if x_bearer_token.len() > 4096
            || x_bearer_token
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(ConfigError::new(
                "config: x_bearer_token must be a single line without spaces",
            ));
        }

        Ok(Config {
            listen: self.listen.trim().to_string(),
            base_path: normalize_base(&self.base_path)?,
            cache_max_age_secs: self.cache_max_age_secs,
            posts_per_tab: self.posts_per_tab,
            max_stored_posts: self.max_stored_posts,
            max_image_bytes: self.max_image_bytes,
            exclude_replies: self.exclude_replies,
            exclude_retweets: self.exclude_retweets,
            api_base: normalize_api(&self.api_base)?,
            data_dir,
            timezone,
            timezone_label,
            x_bearer_token,
            tabs,
            footer: match self.footer {
                Some(raw) => normalize_footer(raw)?,
                None => None,
            },
        })
    }
}

/// Trims the footer and drops it when nothing is left to show.
fn normalize_footer(raw: RawFooter) -> Result<Option<Footer>, ConfigError> {
    let text = match raw.text.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(text) if text.chars().count() > 200 || text.chars().any(char::is_control) => {
            return Err(ConfigError::new(
                "config: footer.text must be at most 200 printable characters",
            ));
        }
        Some(text) => Some(text.to_string()),
    };
    let url = match raw.url.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(url) => Some(normalize_link_url(url, "footer.url")?),
    };
    if raw.links.len() > 20 {
        return Err(ConfigError::new(
            "config: footer.links allows at most 20 links",
        ));
    }
    let mut links = Vec::with_capacity(raw.links.len());
    for link in raw.links {
        let label = link.label.trim();
        if label.is_empty() || label.chars().count() > 60 || label.chars().any(char::is_control) {
            return Err(ConfigError::new(
                "config: each footer link label must be 1–60 printable characters",
            ));
        }
        links.push(FooterLink {
            label: label.to_string(),
            url: normalize_link_url(link.url.trim(), "footer link url")?,
        });
    }
    let note = match raw.note.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(note) if note.chars().count() > 300 || note.chars().any(char::is_control) => {
            return Err(ConfigError::new(
                "config: footer.note must be at most 300 printable characters",
            ));
        }
        Some(note) => Some(note.to_string()),
    };
    // A bare url still gets visible text so the link is not empty.
    let text = text.or_else(|| url.clone());
    if text.is_none() && links.is_empty() && note.is_none() {
        return Ok(None);
    }
    Ok(Some(Footer {
        text,
        url,
        links,
        note,
    }))
}

/// Footer links must be absolute http(s) URLs with a host.
fn normalize_link_url(url: &str, field: &str) -> Result<String, ConfigError> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    let ok = match rest {
        Some(rest) => {
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            !host.is_empty()
                && url.len() <= 2048
                && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        None => false,
    };
    if ok {
        Ok(url.to_string())
    } else {
        Err(ConfigError::new(format!(
            "config: {field} must be an http:// or https:// URL"
        )))
    }
}

/// `from_env` wins when it contains a non-blank value. Errors do not echo the token.
pub fn resolve_bearer_token(
    from_file: &str,
    from_env: Option<&str>,
) -> Result<String, ConfigError> {
    let raw = match from_env {
        Some(value) if !value.trim().is_empty() => value,
        _ => from_file,
    };
    let token = strip_bearer_prefix(raw.trim());
    if token.is_empty() {
        return Err(ConfigError::new(
            "no X bearer token: set x_bearer_token in config.yml, or set X_BEARER_TOKEN",
        ));
    }
    if token.len() > 4096
        || token
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(ConfigError::new(
            "X bearer token must be a single line without spaces",
        ));
    }
    Ok(token.to_string())
}

fn strip_bearer_prefix(token: &str) -> &str {
    let prefix = "bearer ";
    if token.len() > prefix.len() && token[..prefix.len()].eq_ignore_ascii_case(prefix) {
        token[prefix.len()..].trim()
    } else {
        token
    }
}

fn normalize_handle(raw: &str) -> Result<String, String> {
    let name = raw.trim().strip_prefix('@').unwrap_or(raw.trim());
    if (1..=15).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        Ok(name.to_string())
    } else {
        Err(format!("\"{raw}\" is not an X username"))
    }
}

fn slug(label: &str) -> String {
    let mut out = String::new();
    let mut hyphen = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            hyphen = false;
        } else if !hyphen && !out.is_empty() {
            out.push('-');
            hyphen = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
        if out.ends_with('-') {
            out.pop();
        }
    }
    out
}

fn normalize_base(raw: &str) -> Result<String, ConfigError> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "/" {
        return Ok(String::new());
    }
    let trimmed = raw.trim_end_matches('/');
    if !trimmed.starts_with('/') || trimmed.starts_with("//") || trimmed.len() > 64 {
        return Err(ConfigError::new(
            "config: base_path must be empty or an absolute path like /x",
        ));
    }
    for segment in trimmed[1..].split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(ConfigError::new(
                "config: base_path has an empty or relative segment",
            ));
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
        {
            return Err(ConfigError::new(
                "config: base_path has a character that is not allowed",
            ));
        }
    }
    Ok(trimmed.to_string())
}

fn normalize_api(raw: &str) -> Result<String, ConfigError> {
    let raw = raw.trim().trim_end_matches('/');
    let https = raw.strip_prefix("https://");
    let http = raw.strip_prefix("http://");
    let (secure, rest) = match (https, http) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        _ => {
            return Err(ConfigError::new(
                "config: api_base must start with https:// or http://",
            ))
        }
    };
    if rest.is_empty()
        || rest.contains('/')
        || rest.contains('@')
        || rest.contains('\\')
        || rest.chars().any(char::is_whitespace)
    {
        return Err(ConfigError::new(
            "config: api_base must be an origin without a path or userinfo",
        ));
    }
    if !secure && !local_http_host(rest) {
        return Err(ConfigError::new(
            "config: plain http api_base is only allowed for localhost",
        ));
    }
    Ok(raw.to_string())
}

fn local_http_host(host: &str) -> bool {
    host == "localhost"
        || host.starts_with("localhost:")
        || host == "127.0.0.1"
        || host.starts_with("127.0.0.1:")
        || host == "[::1]"
        || host.starts_with("[::1]:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let config = Config::from_yaml(include_str!("../config.example.yml")).unwrap();
        assert_eq!(config.listen, "0.0.0.0:8181");
        assert_eq!(config.base_path, "");
        assert_eq!(config.timezone, "Asia/Taipei");
        assert_eq!(config.timezone_label.as_deref(), Some("Taiwan"));
        let footer = config.footer.as_ref().unwrap();
        assert_eq!(footer.url.as_deref(), Some("https://0x6.ai/"));
        assert_eq!(footer.links.len(), 4);
        assert!(footer
            .note
            .as_deref()
            .unwrap()
            .starts_with("Rights in posts"));
        assert_eq!(config.data_dir, "data");
        assert_eq!(config.posts_per_tab, 20);
        assert_eq!(config.max_image_bytes, 8_000_000);
        assert!(config.exclude_replies && config.exclude_retweets);
        assert_eq!(config.api_base, "https://api.x.com");
        assert_eq!(
            config
                .tabs
                .iter()
                .map(|tab| tab.id.as_str())
                .collect::<Vec<_>>(),
            vec!["news", "tech", "space"]
        );
        assert_eq!(
            config.handles(),
            vec!["BBCWorld", "Reuters", "github", "rustlang", "NASA", "SpaceX"]
        );
        assert!(
            config.x_bearer_token == "your-bearer-token-here",
            "config.example.yml should keep the placeholder token"
        );
        let shown = format!("{config:?}");
        assert!(!shown.contains("your-bearer-token-here"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn bearer_token_env_overrides_file_and_is_not_in_errors() {
        let config = Config::from_yaml(
            "x_bearer_token: \"file-token\"\ntabs:\n  - label: A\n    accounts: [abc]\n",
        )
        .unwrap();
        assert_eq!(
            resolve_bearer_token(&config.x_bearer_token, None).unwrap(),
            "file-token"
        );
        assert_eq!(
            resolve_bearer_token(&config.x_bearer_token, Some("  env-token  ")).unwrap(),
            "env-token"
        );
        assert_eq!(
            resolve_bearer_token(&config.x_bearer_token, Some("Bearer env-token")).unwrap(),
            "env-token"
        );
        assert_eq!(
            resolve_bearer_token(&config.x_bearer_token, Some("   ")).unwrap(),
            "file-token"
        );
        let missing = resolve_bearer_token("  ", None).unwrap_err();
        let message = missing.to_string();
        assert!(message.contains("x_bearer_token"));
        assert!(!message.contains("file-token"));
        let shown = format!("{config:?}");
        assert!(!shown.contains("file-token"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn defaults_and_normalization() {
        let config = Config::from_yaml(
            "tabs:\n  - label: \"24 Hours\"\n    accounts: [\"@RustLang\", \"rustlang\"]\n",
        )
        .unwrap();
        assert_eq!(config.listen, "0.0.0.0:8080");
        assert_eq!(config.base_path, "");
        assert_eq!(config.timezone, "Asia/Taipei");
        assert_eq!(config.data_dir, "data");
        assert_eq!(config.cache_max_age_secs, 0);
        assert_eq!(config.posts_per_tab, 20);
        assert_eq!(config.max_image_bytes, 8_000_000);
        assert_eq!(config.tabs[0].id, "t-24-hours");
        assert_eq!(config.tabs[0].accounts, vec!["RustLang".to_string()]);
        assert_eq!(config.api_base, API_DEFAULT);

        let config = Config::from_yaml(
            "base_path: \"/x/\"\napi_base: \"http://127.0.0.1:9\"\ntabs:\n  - label: 新聞\n    accounts: [abc]\n",
        )
        .unwrap();
        assert_eq!(config.base_path, "/x");
        assert_eq!(config.api_base, "http://127.0.0.1:9");
        assert_eq!(config.tabs[0].id, "tab-1");
    }

    #[test]
    fn rejects_bad_input() {
        let cases = [
            "listen: \"\"\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "timezone: \"Not/AZone\"\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "max_stored_posts: 1\nposts_per_tab: 20\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "max_image_bytes: 50000001\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "posts_per_tab: 0\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "posts_per_tab: 101\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "base_path: x\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "base_path: \"/x/../y\"\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "api_base: \"http://example.com\"\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "tabs:\n  - label: A\n    accounts: [\"bad name\"]\n",
            "tabs:\n  - label: A\n    accounts: [abc]\n  - label: \"a\"\n    accounts: [def]\n",
            "nope: true\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "x_bearer_token: \"has space\"\ntabs:\n  - label: A\n    accounts: [abc]\n",
            "listen: \"127.0.0.1:9\"\n",
        ];
        for case in cases {
            assert!(Config::from_yaml(case).is_err(), "{case}");
        }
    }

    #[test]
    fn timezone_label_is_optional_and_checked() {
        let base = "tabs:\n  - label: A\n    accounts: [abc]\n";
        let unset = Config::from_yaml(base).unwrap();
        assert_eq!(unset.timezone_label, None);
        let blank = Config::from_yaml(&format!("timezone_label: \"  \"\n{base}")).unwrap();
        assert_eq!(blank.timezone_label, None);
        let set = Config::from_yaml(&format!("timezone_label: \" Taiwan \"\n{base}")).unwrap();
        assert_eq!(set.timezone_label.as_deref(), Some("Taiwan"));
        assert!(
            Config::from_yaml(&format!("timezone_label: \"{}\"\n{base}", "x".repeat(33))).is_err()
        );
    }

    #[test]
    fn footer_is_optional_and_validated() {
        let base = "tabs:\n  - label: A\n    accounts: [abc]\n";
        assert_eq!(Config::from_yaml(base).unwrap().footer, None);
        let empty = Config::from_yaml(&format!("footer:\n  text: \" \"\n{base}")).unwrap();
        assert_eq!(empty.footer, None);

        let full = Config::from_yaml(&format!(
            "footer:\n  text: \" © Example \"\n  url: \"https://example.com/\"\n  links:\n    - {{ label: About, url: \"https://example.com/about.html\" }}\n{base}"
        ))
        .unwrap();
        let footer = full.footer.unwrap();
        assert_eq!(footer.text.as_deref(), Some("© Example"));
        assert_eq!(footer.url.as_deref(), Some("https://example.com/"));
        assert_eq!(
            footer.links,
            vec![FooterLink {
                label: "About".into(),
                url: "https://example.com/about.html".into()
            }]
        );

        assert_eq!(footer.note, None);
        let note_only = Config::from_yaml(&format!(
            "footer:\n  note: \" Rights <b>belong</b> to X. \"\n{base}"
        ))
        .unwrap()
        .footer
        .unwrap();
        assert_eq!(
            note_only.note.as_deref(),
            Some("Rights <b>belong</b> to X.")
        );
        assert_eq!(note_only.text, None);
        assert!(note_only.links.is_empty());
        let long = format!("footer:\n  note: \"{}\"\n{base}", "n".repeat(301));
        assert!(Config::from_yaml(&long).is_err());
        let max = format!("footer:\n  note: \"{}\"\n{base}", "貼".repeat(300));
        assert!(Config::from_yaml(&max).is_ok());

        let bare = Config::from_yaml(&format!("footer:\n  url: \"http://example.com\"\n{base}"))
            .unwrap()
            .footer
            .unwrap();
        assert_eq!(bare.text.as_deref(), Some("http://example.com"));

        for bad in [
            "footer:\n  url: \"javascript:alert(1)\"\n",
            "footer:\n  url: \"//example.com\"\n",
            "footer:\n  url: \"https://\"\n",
            "footer:\n  links:\n    - { label: X, url: \"data:text/html,hi\" }\n",
            "footer:\n  links:\n    - { label: \" \", url: \"https://example.com\" }\n",
            "footer:\n  links:\n    - { label: X, url: \"https://exa mple.com\" }\n",
            "footer:\n  colour: red\n",
        ] {
            assert!(Config::from_yaml(&format!("{bad}{base}")).is_err(), "{bad}");
        }
    }
}
