//! Posts, media, and the small amount of time math the renderer needs.
//!
//! X sends `created_at` as UTC timestamps. Parsing and formatting them here
//! keeps a datetime crate out of the binary.

use serde::{Deserialize, Serialize};

/// One account returned by `GET /2/users/by`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub name: String,
    pub username: String,
    pub avatar: Option<String>,
}

/// A link painted over a UTF-16 range of [`Post::text`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    pub start: usize,
    pub end: usize,
    pub href: String,
    /// Visible label when it should differ from the sliced post text.
    pub label: Option<String>,
}

/// A photo, or a still frame for video and GIF posts.
///
/// `url` and `remote_url` are the remote image (the preview frame for video
/// and GIF). `local_path` is set after that image is stored under the data
/// directory. `video_url` is the original video or GIF file when X sent one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub remote_url: String,
    #[serde(default)]
    pub local_path: Option<String>,
    #[serde(default)]
    pub video_url: Option<String>,
    #[serde(default)]
    pub media_key: String,
    pub alt: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub fn safe_media_key(key: &str) -> bool {
    (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// One post, already reduced to the text and media we are willing to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    pub name: String,
    pub username: String,
    pub avatar: Option<String>,
    pub text: String,
    pub created_at: i64,
    pub entities: Vec<Entity>,
    pub media: Vec<Media>,
}

/// JSON shape of a post. Built when a tab is served.
#[derive(Debug, Serialize)]
pub struct PostJson<'a> {
    pub id: &'a str,
    pub username: &'a str,
    pub name: &'a str,
    pub avatar: Option<&'a str>,
    pub text: &'a str,
    pub created_at: String,
    pub url: String,
    pub media: Vec<MediaJson<'a>>,
}

#[derive(Debug, Serialize)]
pub struct MediaJson<'a> {
    #[serde(rename = "type")]
    pub kind: &'a str,
    pub url: String,
    pub remote_url: &'a str,
    pub local_path: Option<&'a str>,
    pub video_url: Option<&'a str>,
    pub alt: Option<&'a str>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub fn status_url(username: &str, id: &str) -> String {
    if is_handle(username) {
        format!("https://x.com/{username}/status/{id}")
    } else {
        format!("https://x.com/i/status/{id}")
    }
}

pub fn profile_url(username: &str) -> Option<String> {
    if is_handle(username) {
        Some(format!("https://x.com/{username}"))
    } else {
        None
    }
}

pub fn is_handle(username: &str) -> bool {
    let len = username.len();
    (1..=15).contains(&len)
        && username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub fn is_snowflake(id: &str) -> bool {
    !id.is_empty() && id.len() <= 30 && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// `2024-05-01T12:34:56.000Z` → unix seconds. Fractional seconds are ignored.
pub fn parse_rfc3339(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || (bytes[10] != b'T' && bytes[10] != b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year: i32 = std::str::from_utf8(&bytes[0..4]).ok()?.parse().ok()?;
    let month: u32 = std::str::from_utf8(&bytes[5..7]).ok()?.parse().ok()?;
    let day: u32 = std::str::from_utf8(&bytes[8..10]).ok()?.parse().ok()?;
    let hour: i64 = std::str::from_utf8(&bytes[11..13]).ok()?.parse().ok()?;
    let minute: i64 = std::str::from_utf8(&bytes[14..16]).ok()?.parse().ok()?;
    let second: i64 = std::str::from_utf8(&bytes[17..19]).ok()?.parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..61).contains(&second) {
        return None;
    }
    if !rfc3339_suffix(&value[19..]) {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    Some(days * 86400 + hour * 3600 + minute * 60 + second.min(59))
}

fn rfc3339_suffix(rest: &str) -> bool {
    if rest == "Z" || rest == "z" {
        return true;
    }
    let Some(digits) = rest.strip_prefix('.') else {
        return false;
    };
    let Some(digits) = digits
        .strip_suffix('Z')
        .or_else(|| digits.strip_suffix('z'))
    else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

pub fn format_rfc3339(secs: i64) -> String {
    if secs < 0 {
        return "1970-01-01T00:00:00Z".to_string();
    }
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

pub fn format_display(secs: i64) -> String {
    let rfc = format_rfc3339(secs);
    format!("{} {} UTC", &rfc[..10], &rfc[11..16])
}

fn month_len(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Howard Hinnant's civil-from-days, valid for dates on or after 1970-01-01.
fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > month_len(year, month)
    {
        return None;
    }
    let year = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = year / 400;
    let year_of_era = (year - era * 400) as u64;
    let month_prime = (if month > 2 { month - 3 } else { month + 9 }) as u64;
    let day_of_year = (153 * month_prime + 2) / 5 + day as u64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146097 + day_of_era as i64 - 719468)
}

fn civil_from_days(mut z: i64) -> (i32, u32, u32) {
    z += 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let day_of_era = (z - era * 146097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_and_known_timestamp() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            parse_rfc3339("2023-11-14T22:13:20.000Z"),
            Some(1_700_000_000)
        );
        assert_eq!(format_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_display(1_700_000_000), "2023-11-14 22:13 UTC");
    }

    #[test]
    fn leap_days_round_trip() {
        for (year, month, day) in [(1970, 1, 1), (2000, 2, 29), (2024, 2, 29), (2026, 10, 9)] {
            let days = days_from_civil(year, month, day).unwrap();
            assert_eq!(civil_from_days(days), (year, month, day));
        }
        assert_eq!(parse_rfc3339("2023-02-29T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("not a time"), None);
    }
}
