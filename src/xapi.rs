//! X API v2 client and response parsing.
//!
//! Entity `start`/`end` values are UTF-16 code units, matching the API (a
//! single emoji such as U+1F44B occupies two units). Ranges are clamped to
//! scalar boundaries so a bad index cannot slice inside a character.

use std::collections::HashMap;
use std::future::Future;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::model::{
    is_handle, is_snowflake, parse_rfc3339, safe_media_key, Entity, Media, Post, User,
};

#[derive(Debug, Clone)]
pub enum XError {
    RateLimited { retry_after_secs: u64 },
    Status { code: u16, detail: String },
    Network(String),
}

impl std::fmt::Display for XError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XError::RateLimited { retry_after_secs } => {
                write!(f, "rate limited, retry in {retry_after_secs}s")
            }
            XError::Status { code, detail } => write!(f, "HTTP {code}: {detail}"),
            XError::Network(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for XError {}

pub fn short_error(err: &XError) -> String {
    match err {
        XError::RateLimited { .. } => "rate limited".to_string(),
        XError::Status { code, detail } if *code == 401 || *code == 403 => {
            let _ = detail;
            "bearer token was rejected".to_string()
        }
        XError::Status { detail, .. } => detail.clone(),
        XError::Network(_) => "network error".to_string(),
    }
}

/// Something that can `GET` a path on the API origin.
pub trait XSource: Send + Sync {
    fn fetch<'a>(
        &'a self,
        path: &'a str,
    ) -> impl Future<Output = Result<String, XError>> + Send + 'a;
}

#[derive(Clone)]
pub struct XClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl XClient {
    pub fn new(token: String, base: String) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent("xfeed/0.1")
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .tcp_nodelay(true)
            .pool_max_idle_per_host(4)
            .build()
            .map_err(|err| format!("http client: {err}"))?;
        Ok(Self { http, base, token })
    }
}

impl XSource for XClient {
    fn fetch<'a>(
        &'a self,
        path: &'a str,
    ) -> impl Future<Output = Result<String, XError>> + Send + 'a {
        let url = format!("{}{}", self.base, path);
        let token = self.token.clone();
        let http = self.http.clone();
        async move {
            let response = http
                .get(&url)
                .bearer_auth(token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|err| XError::Network(err.to_string()))?;
            let status = response.status();
            if status.as_u16() == 429 {
                let retry = retry_after(response.headers(), now_unix());
                return Err(XError::RateLimited {
                    retry_after_secs: retry,
                });
            }
            // Read the body before branching so a non-success still consumes it.
            let body = response
                .text()
                .await
                .map_err(|err| XError::Network(err.to_string()))?;
            if !status.is_success() {
                return Err(XError::Status {
                    code: status.as_u16(),
                    detail: summarize_status(status.as_u16(), &body),
                });
            }
            Ok(body)
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn retry_after(headers: &reqwest::header::HeaderMap, now: i64) -> u64 {
    if let Some(value) = header_str(headers, "retry-after") {
        if let Ok(secs) = value.parse::<u64>() {
            return secs.clamp(1, 3600);
        }
    }
    if let Some(value) = header_str(headers, "x-rate-limit-reset") {
        if let Ok(reset) = value.parse::<i64>() {
            let delta = reset - now;
            if delta > 0 {
                return (delta as u64).clamp(1, 3600);
            }
        }
    }
    60
}

fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn summarize_status(code: u16, body: &str) -> String {
    if code == 401 || code == 403 {
        return "bearer token was rejected".to_string();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let detail = value
            .get("detail")
            .and_then(|item| item.as_str())
            .or_else(|| value.get("title").and_then(|item| item.as_str()))
            .or_else(|| {
                value
                    .pointer("/errors/0/detail")
                    .and_then(|item| item.as_str())
            });
        if let Some(detail) = detail {
            return clean_detail(detail);
        }
    }
    format!("HTTP {code}")
}

fn clean_detail(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if out.len() >= 160 {
            break;
        }
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        "request failed".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn users_path(names: &[String]) -> String {
    format!(
        "/2/users/by?usernames={}&user.fields=id,name,username,profile_image_url",
        names.join(",")
    )
}

pub fn tweets_path(
    id: &str,
    max_results: usize,
    exclude_replies: bool,
    exclude_retweets: bool,
    since_id: Option<&str>,
) -> String {
    let exclude = match (exclude_replies, exclude_retweets) {
        (true, true) => "&exclude=replies,retweets",
        (true, false) => "&exclude=replies",
        (false, true) => "&exclude=retweets",
        (false, false) => "",
    };
    let since = match since_id {
        Some(since_id) if is_snowflake(since_id) => format!("&since_id={since_id}"),
        _ => String::new(),
    };
    format!(
        "/2/users/{id}/tweets?max_results={max_results}&tweet.fields=author_id,created_at,entities,attachments,note_tweet,display_text_range&expansions=attachments.media_keys,author_id&media.fields=media_key,type,url,preview_image_url,alt_text,width,height&user.fields=id,name,username,profile_image_url{exclude}{since}"
    )
}

pub fn requested_posts(posts_per_tab: usize) -> usize {
    posts_per_tab.clamp(5, 100)
}

#[derive(Debug)]
pub struct UserBatch {
    pub users: Vec<User>,
    pub missing: Vec<String>,
}

pub fn parse_users(body: &str) -> Result<UserBatch, String> {
    let parsed: UsersBody = serde_json::from_str(body).map_err(|err| format!("users: {err}"))?;
    let mut users = Vec::new();
    for user in parsed.data.unwrap_or_default() {
        if !is_snowflake(&user.id) || !is_handle(&user.username) {
            continue;
        }
        users.push(User {
            id: user.id,
            name: user.name,
            username: user.username,
            avatar: user.profile_image_url.filter(|url| allowed_twimg(url)),
        });
    }
    let missing = parsed
        .errors
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| item.value)
        .map(|value| value.trim().trim_start_matches('@').to_string())
        .filter(|value| is_handle(value))
        .collect();
    Ok(UserBatch { users, missing })
}

pub fn parse_user_tweets(body: &str, fallback: &User) -> Result<Vec<Post>, String> {
    let parsed: TweetsBody = serde_json::from_str(body).map_err(|err| format!("tweets: {err}"))?;
    if parsed.data.is_none() {
        if let Some(errors) = parsed.errors.as_ref() {
            if !errors.is_empty() {
                let detail = errors
                    .iter()
                    .filter_map(|item| item.detail.as_deref().or(item.title.as_deref()))
                    .next()
                    .unwrap_or("request failed");
                return Err(clean_detail(detail));
            }
        }
        return Ok(Vec::new());
    }

    let includes = parsed.includes.unwrap_or_default();
    let users: HashMap<&str, &UserDto> = includes
        .users
        .iter()
        .map(|user| (user.id.as_str(), user))
        .collect();
    let media: HashMap<&str, &MediaDto> = includes
        .media
        .iter()
        .map(|item| (item.media_key.as_str(), item))
        .collect();

    let mut posts = Vec::new();
    for tweet in parsed.data.unwrap_or_default() {
        if let Some(post) = tweet_to_post(tweet, fallback, &users, &media) {
            posts.push(post);
        }
    }
    Ok(posts)
}

fn tweet_to_post(
    tweet: TweetDto,
    fallback: &User,
    users: &HashMap<&str, &UserDto>,
    media: &HashMap<&str, &MediaDto>,
) -> Option<Post> {
    if !is_snowflake(&tweet.id) {
        return None;
    }
    let created_at = parse_rfc3339(tweet.created_at.as_deref()?)?;
    let author = tweet
        .author_id
        .as_deref()
        .and_then(|id| users.get(id))
        .map(|user| User {
            id: user.id.clone(),
            name: user.name.clone(),
            username: user.username.clone(),
            avatar: user
                .profile_image_url
                .as_deref()
                .filter(|url| allowed_twimg(url))
                .map(str::to_string),
        })
        .unwrap_or_else(|| fallback.clone());

    let (mut text, mut entities, from_note) = if let Some(note) = tweet.note_tweet {
        if !note.text.is_empty() {
            (note.text, note.entities.unwrap_or_default(), true)
        } else {
            (tweet.text, tweet.entities.unwrap_or_default(), false)
        }
    } else {
        (tweet.text, tweet.entities.unwrap_or_default(), false)
    };
    if !from_note {
        if let Some(range) = tweet.display_text_range {
            if range.len() == 2 && range[1] >= range[0] {
                let (sliced, shifted) = slice_utf16(&text, &entities, range[0], range[1]);
                text = sliced;
                entities = shifted;
            }
        }
    }
    if text.chars().count() > 25_000 {
        text = text.chars().take(25_000).collect();
    }
    let entities = entities_from(&entities);
    let (text, entities) = finalize_text(&text, entities);

    let mut images = Vec::new();
    if let Some(keys) = tweet.attachments.and_then(|item| item.media_keys) {
        for key in keys {
            let Some(item) = media.get(key.as_str()) else {
                continue;
            };
            let image_url = match item.kind.as_str() {
                "photo" => item.url.as_deref().or(item.preview_image_url.as_deref()),
                _ => item.preview_image_url.as_deref(),
            };
            let Some(image_url) = image_url.filter(|url| allowed_twimg(url)) else {
                continue;
            };
            let video_url = match item.kind.as_str() {
                "video" | "animated_gif" => item
                    .url
                    .as_deref()
                    .filter(|url| allowed_video(url))
                    .map(str::to_string),
                _ => None,
            };
            images.push(Media {
                kind: media_kind(&item.kind),
                url: image_url.to_string(),
                remote_url: image_url.to_string(),
                local_path: None,
                video_url,
                media_key: if safe_media_key(&item.media_key) {
                    item.media_key.clone()
                } else {
                    String::new()
                },
                alt: item.alt_text.as_deref().map(clip_alt),
                width: item.width.filter(|value| (1..20_000).contains(value)),
                height: item.height.filter(|value| (1..20_000).contains(value)),
            });
            if images.len() == 4 {
                break;
            }
        }
    }

    Some(Post {
        id: tweet.id,
        name: author.name,
        username: author.username,
        avatar: author.avatar,
        text,
        created_at,
        entities,
        media: images,
    })
}

fn media_kind(kind: &str) -> String {
    if !kind.is_empty()
        && kind.len() <= 20
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        kind.to_string()
    } else {
        "media".to_string()
    }
}

fn clip_alt(value: &str) -> String {
    value.chars().take(300).collect()
}

fn entities_from(entities: &EntitiesDto) -> Vec<RawEntity> {
    let mut out = Vec::new();
    for url in &entities.urls {
        if url.media_key.is_some() {
            out.push(RawEntity {
                start: url.start,
                end: url.end,
                data: RawData::Hidden,
            });
            continue;
        }
        // If X names an expanded target, that is the navigation. Do not fall
        // back to the t.co wrapper when the target is not http(s).
        let href = if let Some(expanded) = url.expanded_url.as_deref() {
            if safe_http_url(expanded) {
                Some(expanded)
            } else {
                None
            }
        } else {
            url.url
                .as_deref()
                .filter(|candidate| safe_http_url(candidate))
        };
        let Some(href) = href else {
            continue;
        };
        let label = url.display_url.clone().filter(|label| !label.is_empty());
        out.push(RawEntity {
            start: url.start,
            end: url.end,
            data: RawData::Link {
                href: href.to_string(),
                label,
            },
        });
    }
    for mention in &entities.mentions {
        if is_handle(&mention.username) {
            out.push(RawEntity {
                start: mention.start,
                end: mention.end,
                data: RawData::Link {
                    href: format!("https://x.com/{}", mention.username),
                    label: None,
                },
            });
        }
    }
    for tag in &entities.hashtags {
        if valid_tag(&tag.tag) {
            out.push(RawEntity {
                start: tag.start,
                end: tag.end,
                data: RawData::Link {
                    href: format!("https://x.com/hashtag/{}", path_encode(&tag.tag)),
                    label: None,
                },
            });
        }
    }
    for tag in &entities.cashtags {
        if valid_tag(&tag.tag) {
            out.push(RawEntity {
                start: tag.start,
                end: tag.end,
                data: RawData::Link {
                    href: format!("https://x.com/search?q=%24{}", path_encode(&tag.tag)),
                    label: None,
                },
            });
        }
    }
    out
}

fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 80
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn path_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

enum RawData {
    Hidden,
    Link { href: String, label: Option<String> },
}

struct RawEntity {
    start: usize,
    end: usize,
    data: RawData,
}

fn slice_utf16(
    text: &str,
    entities: &EntitiesDto,
    start: usize,
    end: usize,
) -> (String, EntitiesDto) {
    let total = utf16_len(text);
    let start = start.min(total);
    let end = end.min(total).max(start);
    let sliced = utf16_slice(text, start, end).to_string();
    let mut shifted = entities.clone();
    let keep = |entity_start: usize, entity_end: usize| {
        entity_start >= start && entity_end <= end && entity_start < entity_end
    };
    shifted.hashtags.retain(|item| keep(item.start, item.end));
    shifted.mentions.retain(|item| keep(item.start, item.end));
    shifted.urls.retain(|item| keep(item.start, item.end));
    shifted.cashtags.retain(|item| keep(item.start, item.end));
    for item in &mut shifted.hashtags {
        item.start -= start;
        item.end -= start;
    }
    for item in &mut shifted.mentions {
        item.start -= start;
        item.end -= start;
    }
    for item in &mut shifted.urls {
        item.start -= start;
        item.end -= start;
    }
    for item in &mut shifted.cashtags {
        item.start -= start;
        item.end -= start;
    }
    (sliced, shifted)
}

/// Drop hidden (media permalink) ranges and remap the remaining links.
fn finalize_text(text: &str, entities: Vec<RawEntity>) -> (String, Vec<Entity>) {
    let total = utf16_len(text);
    let mut hidden = Vec::new();
    for entity in &entities {
        if matches!(entity.data, RawData::Hidden) && entity.start < entity.end {
            hidden.push((entity.start.min(total), entity.end.min(total)));
        }
    }
    hidden.sort_unstable();
    hidden = merge_ranges(hidden);
    if let Some((start, end)) = hidden.last().copied() {
        if utf16_only_ws(text, end, total) {
            let trimmed = trim_ws_before(text, start);
            hidden.pop();
            hidden.push((trimmed, total));
            hidden = merge_ranges(hidden);
        }
    }

    let mut map = vec![0usize; total + 1];
    let mut new_text = String::with_capacity(text.len());
    let mut new_units = 0usize;
    let mut old_units = 0usize;
    for ch in text.chars() {
        let len = ch.len_utf16();
        let hidden_char = ranges_overlap(&hidden, old_units, old_units + len);
        for step in 0..len {
            map[old_units + step] = new_units;
        }
        if !hidden_char {
            new_text.push(ch);
            new_units += len;
        }
        old_units += len;
    }
    map[total] = new_units;

    let mut links = Vec::new();
    for entity in entities {
        let RawData::Link { href, label } = entity.data else {
            continue;
        };
        if entity.start >= entity.end || entity.end > total {
            continue;
        }
        if ranges_overlap(&hidden, entity.start, entity.end) {
            continue;
        }
        let start = map[entity.start];
        let end = map[entity.end];
        if start < end {
            links.push(Entity {
                start,
                end,
                href,
                label,
            });
        }
    }
    (new_text, links)
}

fn merge_ranges(ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = out.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        out.push((start, end));
    }
    out
}

fn ranges_overlap(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    ranges
        .iter()
        .any(|(left, right)| start < *right && end > *left)
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

fn utf16_slice(text: &str, start: usize, end: usize) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    for ch in text.chars() {
        let len = ch.len_utf16();
        if units >= end {
            break;
        }
        if units >= start && units + len <= end {
            out.push(ch);
        }
        units += len;
    }
    out
}

fn utf16_only_ws(text: &str, start: usize, end: usize) -> bool {
    let mut units = 0usize;
    for ch in text.chars() {
        let len = ch.len_utf16();
        if units >= end {
            break;
        }
        if units >= start && !ch.is_whitespace() {
            return false;
        }
        units += len;
    }
    true
}

fn trim_ws_before(text: &str, start: usize) -> usize {
    let mut units = 0usize;
    let mut last_solid = 0usize;
    for ch in text.chars() {
        let len = ch.len_utf16();
        if units >= start {
            break;
        }
        if !ch.is_whitespace() {
            last_solid = units + len;
        }
        units += len;
    }
    last_solid
}

fn allowed_video(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    if host != "video.twimg.com" && host != "pbs.twimg.com" && host != "ton.twimg.com" {
        return false;
    }
    !path.is_empty() && !path.contains("..") && safe_graphic(url)
}

pub fn allowed_twimg(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    if host != "pbs.twimg.com" && host != "ton.twimg.com" {
        return false;
    }
    if path.is_empty() || path.contains("..") {
        return false;
    }
    safe_graphic(url)
}

pub fn safe_http_url(url: &str) -> bool {
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    if rest.is_empty() || rest.starts_with('/') || rest.starts_with('.') {
        return false;
    }
    safe_graphic(url)
}

fn safe_graphic(url: &str) -> bool {
    url.bytes().all(|byte| {
        byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\'' | b'<' | b'>' | b'\\' | b'`')
    })
}

#[derive(Debug, Deserialize)]
struct UsersBody {
    data: Option<Vec<UserDto>>,
    errors: Option<Vec<ApiError>>,
}

#[derive(Debug, Deserialize)]
struct UserDto {
    id: String,
    name: String,
    username: String,
    profile_image_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    value: Option<String>,
    title: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TweetsBody {
    data: Option<Vec<TweetDto>>,
    includes: Option<Includes>,
    errors: Option<Vec<ApiError>>,
}

#[derive(Debug, Default, Deserialize)]
struct Includes {
    #[serde(default)]
    users: Vec<UserDto>,
    #[serde(default)]
    media: Vec<MediaDto>,
}

#[derive(Debug, Deserialize)]
struct TweetDto {
    id: String,
    text: String,
    created_at: Option<String>,
    author_id: Option<String>,
    entities: Option<EntitiesDto>,
    attachments: Option<Attachments>,
    note_tweet: Option<NoteTweet>,
    display_text_range: Option<Vec<usize>>,
}

#[derive(Debug, Deserialize)]
struct NoteTweet {
    text: String,
    entities: Option<EntitiesDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct EntitiesDto {
    #[serde(default)]
    hashtags: Vec<TagEntity>,
    #[serde(default)]
    mentions: Vec<MentionEntity>,
    #[serde(default)]
    urls: Vec<UrlEntity>,
    #[serde(default)]
    cashtags: Vec<TagEntity>,
}

#[derive(Debug, Clone, Deserialize)]
struct TagEntity {
    start: usize,
    end: usize,
    tag: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MentionEntity {
    start: usize,
    end: usize,
    username: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UrlEntity {
    start: usize,
    end: usize,
    url: Option<String>,
    expanded_url: Option<String>,
    display_url: Option<String>,
    media_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Attachments {
    media_keys: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct MediaDto {
    media_key: String,
    #[serde(rename = "type")]
    kind: String,
    url: Option<String>,
    preview_image_url: Option<String>,
    alt_text: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback() -> User {
        User {
            id: "42".to_string(),
            name: "<script>".to_string(),
            username: "example".to_string(),
            avatar: None,
        }
    }

    #[test]
    fn parses_users_and_drops_bad_avatars() {
        let body = r#"{
            "data": [{
                "id": "42",
                "name": "Example",
                "username": "example",
                "profile_image_url": "https://pbs.twimg.com/profile_images/a_normal.jpg"
            }, {
                "id": "7",
                "name": "Evil",
                "username": "evil",
                "profile_image_url": "https://evil.example/a.jpg"
            }],
            "errors": [{"value": "nope", "title": "Not Found Error", "detail": "missing"}]
        }"#;
        let batch = parse_users(body).unwrap();
        assert_eq!(batch.users.len(), 2);
        assert!(batch.users[0].avatar.is_some());
        assert!(batch.users[1].avatar.is_none());
        assert_eq!(batch.missing, vec!["nope".to_string()]);
        assert!(parse_users("nope").is_err());
    }

    #[test]
    fn parses_timeline_links_media_notes_and_emoji() {
        let text = "Hello <script> @alice #Rust https://t.co/aaaaaaaaaa";
        let mention = text.find("@alice").unwrap();
        let hash = text.find("#Rust").unwrap();
        let url_at = text.find("https://t.co/aaaaaaaaaa").unwrap();
        let look = "Look https://t.co/bbbbbbbbbb";
        let wave = "hi 👋 #Rust";
        let wave_hash = utf16_len("hi 👋 ");
        let body = serde_json::json!({
            "data": [
                {
                    "id": "100",
                    "author_id": "42",
                    "text": text,
                    "created_at": "2024-05-01T12:34:56.000Z",
                    "entities": {
                        "mentions": [{"start": mention, "end": mention + "@alice".len(), "username": "alice"}],
                        "hashtags": [{"start": hash, "end": hash + "#Rust".len(), "tag": "Rust"}],
                        "urls": [{
                            "start": url_at,
                            "end": url_at + "https://t.co/aaaaaaaaaa".len(),
                            "url": "https://t.co/aaaaaaaaaa",
                            "expanded_url": "https://example.com/story?a=1&b=2",
                            "display_url": "example.com/story"
                        }]
                    }
                },
                {
                    "id": "101",
                    "author_id": "42",
                    "text": look,
                    "created_at": "2024-05-02T00:00:00Z",
                    "display_text_range": [0, 4],
                    "entities": {
                        "urls": [{
                            "start": 5,
                            "end": look.len(),
                            "url": "https://t.co/bbbbbbbbbb",
                            "expanded_url": "https://x.com/example/status/101/photo/1",
                            "display_url": "pic.x.com/bbbb",
                            "media_key": "3_1"
                        }]
                    },
                    "attachments": {"media_keys": ["3_1", "13_2", "7_3"]}
                },
                {
                    "id": "102",
                    "author_id": "42",
                    "text": "short",
                    "created_at": "2024-05-03T00:00:00Z",
                    "note_tweet": {
                        "text": "long form <b>post</b>",
                        "entities": {"mentions": []}
                    }
                },
                {
                    "id": "103",
                    "author_id": "42",
                    "text": wave,
                    "created_at": "2024-05-04T00:00:00Z",
                    "entities": {
                        "hashtags": [{"start": wave_hash, "end": utf16_len(wave), "tag": "Rust"}]
                    }
                },
                {
                    "id": "104",
                    "author_id": "42",
                    "text": "see https://t.co/cccccccccc",
                    "created_at": "2024-05-05T00:00:00Z",
                    "entities": {
                        "urls": [{
                            "start": 4,
                            "end": "see https://t.co/cccccccccc".len(),
                            "url": "https://t.co/cccccccccc",
                            "expanded_url": "javascript:alert(1)",
                            "display_url": "javascript:alert(1)"
                        }]
                    }
                }
            ],
            "includes": {
                "users": [{
                    "id": "42",
                    "name": "<script>",
                    "username": "example",
                    "profile_image_url": "https://pbs.twimg.com/profile_images/a_normal.jpg"
                }],
                "media": [
                    {
                        "media_key": "3_1",
                        "type": "photo",
                        "url": "https://pbs.twimg.com/media/abc.jpg",
                        "alt_text": "hill \"><script>",
                        "width": 800,
                        "height": 600
                    },
                    {
                        "media_key": "13_2",
                        "type": "video",
                        "url": "https://video.twimg.com/ext_tw_video/clip.mp4",
                        "preview_image_url": "https://pbs.twimg.com/ext_tw_video_thumb/frame.jpg"
                    },
                    {
                        "media_key": "7_3",
                        "type": "photo",
                        "url": "https://evil.example/a.jpg"
                    }
                ]
            }
        })
        .to_string();

        let posts = parse_user_tweets(&body, &fallback()).unwrap();
        assert_eq!(posts.len(), 5);

        let hello = &posts[0];
        assert_eq!(
            hello.created_at,
            crate::model::parse_rfc3339("2024-05-01T12:34:56.000Z").unwrap()
        );
        assert_eq!(hello.entities.len(), 3);
        assert_eq!(hello.entities[0].href, "https://example.com/story?a=1&b=2");
        assert_eq!(
            hello.entities[0].label.as_deref(),
            Some("example.com/story")
        );
        assert!(hello.entities[1].href.ends_with("/alice"));
        assert!(hello.entities[2].href.ends_with("/hashtag/Rust"));

        let photo = &posts[1];
        assert_eq!(photo.text, "Look");
        assert!(!photo.text.contains("t.co"));
        assert_eq!(photo.media.len(), 2);
        assert_eq!(photo.media[0].url, "https://pbs.twimg.com/media/abc.jpg");
        assert_eq!(photo.media[0].media_key, "3_1");
        assert!(photo.media[0].video_url.is_none());
        assert_eq!(photo.media[0].width, Some(800));
        assert!(photo.media[1].url.contains("frame.jpg"));
        assert_eq!(
            photo.media[1].video_url.as_deref(),
            Some("https://video.twimg.com/ext_tw_video/clip.mp4")
        );

        assert_eq!(posts[2].text, "long form <b>post</b>");
        assert!(posts[3].text.contains('👋'));
        assert_eq!(posts[3].entities.len(), 1);
        assert!(posts[3].entities[0].href.ends_with("/hashtag/Rust"));
        assert!(
            posts[4].entities.is_empty(),
            "javascript: urls are not linked"
        );

        assert!(parse_user_tweets("{}", &fallback()).unwrap().is_empty());
        assert!(parse_user_tweets("nope", &fallback()).is_err());
    }

    #[test]
    fn tweet_paths_match_the_api() {
        let all = tweets_path("9", 5, true, true, None);
        assert!(all.contains("/2/users/9/tweets?"));
        assert!(all.contains("max_results=5"));
        assert!(all.contains("exclude=replies,retweets"));
        assert!(!all.contains("since_id="));
        assert!(all.contains("attachments.media_keys"));
        assert!(
            all.contains("media.fields=media_key,type,url,preview_image_url,alt_text,width,height")
        );
        assert!(tweets_path("9", 20, false, true, Some("100")).contains("since_id=100"));
        assert!(tweets_path("9", 20, false, true, None).contains("exclude=retweets"));
        assert!(!tweets_path("9", 20, false, true, None).contains("replies"));
        assert!(!tweets_path("9", 20, false, false, None).contains("exclude="));
        assert_eq!(requested_posts(1), 5);
        assert_eq!(requested_posts(20), 20);
        assert_eq!(
            users_path(&["alice".to_string(), "bob".to_string()]),
            "/2/users/by?usernames=alice,bob&user.fields=id,name,username,profile_image_url"
        );
    }
}
