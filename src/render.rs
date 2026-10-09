//! HTML and JSON for one tab, plus gzip and brotli.
//!
//! The home page embeds the first tab. Other tabs are empty until the
//! browser asks for `/tab/{id}`. Bodies are compressed when that is smaller.

use std::io::{Read, Write};

use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::Serialize;

use crate::config::Config;
use crate::images::display_src;
use crate::model::{
    format_local, format_rfc3339, profile_url, status_url, DisplayZone, MediaJson, Post, PostJson,
};
use crate::store::TabView;

#[derive(Clone)]
pub struct Encoded {
    pub raw: Bytes,
    pub gzip: Option<Bytes>,
    pub br: Option<Bytes>,
    pub etag_raw: String,
    pub etag_gzip: String,
    pub etag_br: String,
}

pub fn cache_control(secs: u64) -> String {
    if secs == 0 {
        "private, no-cache".to_string()
    } else {
        format!("private, max-age={secs}")
    }
}

pub fn render_page(config: &Config, view: &TabView) -> Encoded {
    encode_body(page_html(config, view).into_bytes())
}

pub fn render_fragment(config: &Config, view: &TabView) -> Encoded {
    encode_body(fragment_html(&config.base_path, config.display_zone(), view).into_bytes())
}

pub fn render_tab_json(base: &str, view: &TabView) -> Encoded {
    encode_body(tab_json_bytes(base, view))
}

pub fn render_health(config: &Config) -> Vec<u8> {
    serde_json::to_vec(&HealthJson {
        ok: true,
        timezone: &config.timezone,
        data_dir: &config.data_dir,
    })
    .unwrap_or_else(|_| b"{}".to_vec())
}

#[derive(Serialize)]
struct HealthJson<'a> {
    ok: bool,
    timezone: &'a str,
    data_dir: &'a str,
}

#[derive(Serialize)]
struct TabJson<'a> {
    id: &'a str,
    label: &'a str,
    updated_at: Option<String>,
    stale: bool,
    error: Option<&'a str>,
    posts: Vec<PostJson<'a>>,
}

fn tab_json_bytes(base: &str, view: &TabView) -> Vec<u8> {
    let posts = view
        .posts
        .iter()
        .map(|post| PostJson {
            id: &post.id,
            username: &post.username,
            name: &post.name,
            avatar: post.avatar.as_deref(),
            text: &post.text,
            created_at: format_rfc3339(post.created_at),
            url: status_url(&post.username, &post.id),
            media: post
                .media
                .iter()
                .map(|item| MediaJson {
                    kind: &item.kind,
                    url: display_src(base, item),
                    remote_url: if item.remote_url.is_empty() {
                        &item.url
                    } else {
                        &item.remote_url
                    },
                    local_path: item.local_path.as_deref(),
                    video_url: item.video_url.as_deref(),
                    alt: item.alt.as_deref(),
                    width: item.width,
                    height: item.height,
                })
                .collect(),
        })
        .collect();
    serde_json::to_vec(&TabJson {
        id: &view.id,
        label: &view.label,
        updated_at: view.updated_at.map(format_rfc3339),
        stale: view.message.is_some(),
        error: view.message.as_deref(),
        posts,
    })
    .unwrap_or_else(|_| b"{}".to_vec())
}

fn page_html(config: &Config, view: &TabView) -> String {
    let mut html = String::with_capacity(4096 + view.posts.len() * 480);
    html.push_str("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<meta name=\"color-scheme\" content=\"light dark\">");
    html.push_str("<meta name=\"description\" content=\"Recent posts from X.\">");
    html.push_str("<link rel=\"icon\" href=\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Crect width='16' height='16' rx='2' fill='%23b8421a'/%3E%3Cpath stroke='white' stroke-width='1.6' d='M4.5 4.5l7 7M11.5 4.5l-7 7'/%3E%3C/svg%3E\">");
    html.push_str("<title>X</title><style>");
    html.push_str(CSS);
    html.push_str("</style></head><body data-base=\"");
    push_esc(&mut html, &config.base_path);
    html.push_str("\" data-first=\"");
    push_esc(&mut html, &view.id);
    html.push_str("\"><header><a class=\"brand\" href=\"#");
    html.push_str(&view.id);
    html.push_str("\">X</a><p class=\"status\">");
    match view.updated_at {
        Some(stamp) => {
            html.push_str("Updated ");
            push_time(&mut html, stamp, config.display_zone());
        }
        None => html.push_str("Not updated yet"),
    }
    html.push_str("</p></header><nav aria-label=\"Tabs\">");
    for tab in &config.tabs {
        html.push_str("<a href=\"#");
        html.push_str(&tab.id);
        html.push('"');
        if tab.id == view.id {
            html.push_str(" aria-current=\"page\"");
        }
        html.push('>');
        push_esc(&mut html, &tab.label);
        html.push_str("</a>");
    }
    html.push_str("</nav><main>");
    for tab in &config.tabs {
        html.push_str("<section class=\"panel\" id=\"");
        html.push_str(&tab.id);
        html.push('"');
        if tab.id != view.id {
            html.push_str(" hidden");
        }
        html.push('>');
        if tab.id == view.id {
            html.push_str(&fragment_html(
                &config.base_path,
                config.display_zone(),
                view,
            ));
        } else {
            html.push_str("<p class=\"empty\">Open this tab to load posts.</p>");
        }
        html.push_str("</section>");
    }
    html.push_str("</main><footer><a href=\"");
    let health = if config.base_path.is_empty() {
        "/health".to_string()
    } else {
        format!("{}/health", config.base_path)
    };
    push_esc(&mut html, &health);
    html.push_str("\">health</a></footer><script>");
    html.push_str(SCRIPT);
    html.push_str("</script></body></html>");
    html
}

/// `<time datetime="UTC ISO">local wall clock</time>`.
fn push_time(html: &mut String, secs: i64, zone: DisplayZone<'_>) {
    html.push_str("<time datetime=\"");
    html.push_str(&format_rfc3339(secs));
    html.push_str("\">");
    push_esc(html, &format_local(secs, zone));
    html.push_str("</time>");
}

fn fragment_html(base: &str, zone: DisplayZone<'_>, view: &TabView) -> String {
    let mut html = String::with_capacity(256 + view.posts.len() * 480);
    if let Some(message) = &view.message {
        html.push_str("<p class=\"banner\" role=\"status\">");
        push_esc(&mut html, message);
        html.push_str("</p>");
    }
    if let Some(stamp) = view.updated_at {
        html.push_str("<p class=\"meta\">Updated ");
        push_time(&mut html, stamp, zone);
        html.push_str("</p>");
    }
    if view.posts.is_empty() {
        html.push_str("<p class=\"empty\">No recent posts.</p>");
    }
    for post in &view.posts {
        push_post(&mut html, post, base, zone);
    }
    html
}

fn push_post(html: &mut String, post: &Post, base: &str, zone: DisplayZone<'_>) {
    html.push_str("<article><div class=\"by\">");
    if let Some(avatar) = &post.avatar {
        html.push_str("<img class=\"avatar\" alt=\"\" width=\"36\" height=\"36\" src=\"");
        push_esc(html, avatar);
        html.push_str("\">");
    }
    if let Some(profile) = profile_url(&post.username) {
        html.push_str("<a class=\"name\" href=\"");
        push_esc(html, &profile);
        html.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">");
        push_esc(html, &post.name);
        html.push_str("</a><a class=\"handle\" href=\"");
        push_esc(html, &profile);
        html.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">@");
        push_esc(html, &post.username);
        html.push_str("</a>");
    } else {
        html.push_str("<span class=\"name\">");
        push_esc(html, &post.name);
        html.push_str("</span>");
    }
    let permalink = status_url(&post.username, &post.id);
    html.push_str("<a class=\"when\" href=\"");
    push_esc(html, &permalink);
    html.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">");
    push_time(html, post.created_at, zone);
    html.push_str("</a></div>");
    if !post.text.is_empty() {
        html.push_str("<p class=\"text\">");
        push_linked(html, &post.text, &post.entities);
        html.push_str("</p>");
    }
    if !post.media.is_empty() {
        html.push_str("<div class=\"media");
        if post.media.len() > 1 {
            html.push_str(" multi");
        }
        html.push_str("\">");
        for item in &post.media {
            html.push_str("<a href=\"");
            push_esc(html, &permalink);
            html.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\"><img loading=\"lazy\" decoding=\"async\" src=\"");
            let src = display_src(base, item);
            push_esc(html, &src);
            html.push('"');
            if let Some(alt) = &item.alt {
                html.push_str(" alt=\"");
                push_esc(html, alt);
                html.push('"');
            } else {
                html.push_str(" alt=\"\"");
            }
            if let Some(width) = item.width {
                html.push_str(&format!(" width=\"{width}\""));
            }
            if let Some(height) = item.height {
                html.push_str(&format!(" height=\"{height}\""));
            }
            html.push_str("></a>");
        }
        html.push_str("</div>");
    }
    html.push_str("</article>");
}

/// Paint entities over UTF-16 ranges. Indices are already clamped to `text`.
fn push_linked(html: &mut String, text: &str, entities: &[crate::model::Entity]) {
    let total = text.chars().map(char::len_utf16).sum::<usize>();
    let mut ordered: Vec<&crate::model::Entity> = entities
        .iter()
        .filter(|entity| entity.start < entity.end && entity.end <= total)
        .collect();
    ordered.sort_by(|left, right| left.start.cmp(&right.start).then(right.end.cmp(&left.end)));
    let mut cursor = 0usize;
    let mut units = 0usize;
    let mut entity_index = 0usize;
    let chars: Vec<(usize, char)> = {
        let mut list = Vec::new();
        let mut at = 0usize;
        for ch in text.chars() {
            list.push((at, ch));
            at += ch.len_utf16();
        }
        list
    };
    let mut char_index = 0usize;
    while char_index < chars.len() || entity_index < ordered.len() {
        if entity_index < ordered.len() && ordered[entity_index].start < cursor {
            entity_index += 1;
            continue;
        }
        if entity_index < ordered.len() && ordered[entity_index].start == cursor {
            let entity = ordered[entity_index];
            let label = match &entity.label {
                Some(label) => label.clone(),
                None => utf16_slice(text, entity.start, entity.end),
            };
            html.push_str("<a href=\"");
            push_esc(html, &entity.href);
            html.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">");
            push_esc(html, &label);
            html.push_str("</a>");
            cursor = entity.end;
            entity_index += 1;
            while char_index < chars.len() && chars[char_index].0 < cursor {
                char_index += 1;
            }
            units = cursor;
            continue;
        }
        if char_index >= chars.len() {
            break;
        }
        let (at, ch) = chars[char_index];
        if at >= units {
            push_esc_char(html, ch);
            units = at + ch.len_utf16();
        }
        char_index += 1;
        cursor = units;
    }
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

fn push_esc(out: &mut String, value: &str) {
    for ch in value.chars() {
        push_esc_char(out, ch);
    }
}

fn push_esc_char(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        _ => out.push(ch),
    }
}

pub(crate) fn encode_body(raw: Vec<u8>) -> Encoded {
    let hash = fnv1a64(&raw);
    let gzip = gzip_bytes(&raw).filter(|body| body.len() < raw.len());
    let br = brotli_bytes(&raw).filter(|body| body.len() < raw.len());
    Encoded {
        etag_raw: format!("\"{hash:016x}\""),
        etag_gzip: format!("\"{hash:016x}-gzip\""),
        etag_br: format!("\"{hash:016x}-br\""),
        raw: Bytes::from(raw),
        gzip: gzip.map(Bytes::from),
        br: br.map(Bytes::from),
    }
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn gzip_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}

fn brotli_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut reader = brotli::CompressorReader::new(data, 4096, 5, 22);
    reader.read_to_end(&mut out).ok()?;
    Some(out)
}

const CSS: &str = "\
:root{--bg:#f6f4ef;--ink:#1c1915;--muted:#6b645b;--line:#e4dfd6;--accent:#b8421a;--banner:#f3e1d4;--card:#fffdf9}\
@media (prefers-color-scheme:dark){:root{--bg:#141311;--ink:#f3efe7;--muted:#a39c92;--line:#2c2925;--accent:#f0a080;--banner:#3a2a22;--card:#1c1b18}}\
*{box-sizing:border-box}body{margin:0 auto;max-width:38rem;padding:1.1rem 1rem 2.5rem;background:var(--bg);color:var(--ink);\
font:15px/1.45 ui-sans-serif,system-ui,-apple-system,\"Segoe UI\",Roboto,Helvetica,Arial,sans-serif}\
a{color:inherit}header{display:flex;align-items:baseline;justify-content:space-between;gap:1rem;margin-bottom:.8rem}\
.brand{font-weight:720;letter-spacing:.04em;text-decoration:none;font-size:1.15rem}\
.status,.meta{margin:0;color:var(--muted);font-variant-numeric:tabular-nums;font-size:.85rem}\
.meta{margin:.2rem 0 .4rem}\
.banner{margin:0 0 .9rem;padding:.55rem .7rem;background:var(--banner);border-radius:6px}\
nav{display:flex;gap:.15rem;position:sticky;top:0;background:var(--bg);border-bottom:1px solid var(--line);margin:0 -1rem .4rem;padding:0 1rem}\
nav a{padding:.65rem .75rem;color:var(--muted);text-decoration:none}\
nav a:hover{color:var(--ink)}\
nav a[aria-current=page]{color:var(--ink);box-shadow:inset 0 -2px 0 var(--accent)}\
.panel{display:block}.panel[hidden]{display:none}\
article{padding:.95rem 0;border-bottom:1px solid var(--line)}\
.by{display:flex;flex-wrap:wrap;align-items:baseline;gap:.35rem .5rem}\
.avatar{width:36px;height:36px;border-radius:50%;object-fit:cover;background:var(--line)}\
.name{font-weight:650;text-decoration:none}.handle,.when{color:var(--muted);text-decoration:none;font-size:.85rem}\
.when{margin-left:auto;font-variant-numeric:tabular-nums}\
.handle:hover,.when:hover,.name:hover{text-decoration:underline}\
.text{margin:.45rem 0 0;white-space:pre-wrap;overflow-wrap:anywhere}\
.text a{color:var(--accent)}\
.media{display:grid;gap:.4rem;margin-top:.6rem}.media.multi{grid-template-columns:1fr 1fr}\
.media img{width:100%;height:auto;max-height:280px;object-fit:cover;border-radius:6px;background:var(--line)}\
.empty{color:var(--muted)}footer{margin-top:1.2rem;color:var(--muted);font-size:.8rem}\
footer a{color:var(--muted)}a:focus-visible{outline:2px solid var(--accent);outline-offset:2px}\
::selection{background:#f0c2b0;color:#1c1915}\
";

const SCRIPT: &str = r##"(function(){
var root=document.body.getAttribute("data-base")||"";
var first=document.body.getAttribute("data-first");
var links=document.querySelectorAll("nav a");
function mark(id){var i,a;for(i=0;i<links.length;i++){a=links[i];if(a.getAttribute("href")==="#"+id){a.setAttribute("aria-current","page");document.title=a.textContent+" · X";}else a.removeAttribute("aria-current");}}
function show(id){var nodes=document.querySelectorAll("main .panel"),i;for(i=0;i<nodes.length;i++)nodes[i].hidden=nodes[i].id!==id;mark(id);}
function load(id){show(id);var el=document.getElementById(id);if(!el)return;history.replaceState(null,"",location.pathname+location.search+"#"+id);fetch(root+"/tab/"+encodeURIComponent(id),{cache:"no-store",headers:{accept:"text/html"}}).then(function(r){if(!r.ok)throw new Error("bad");return r.text();}).then(function(html){el.innerHTML=html;}).catch(function(){el.innerHTML="<p class=\"empty\">Could not load this tab.</p>";});}
document.querySelector("nav").addEventListener("click",function(ev){var t=ev.target,a=t.closest?t.closest("a"):null;if(!a)return;ev.preventDefault();load(a.getAttribute("href").slice(1));});
var hash=location.hash.slice(1);
if(hash&&hash!==first&&document.getElementById(hash))load(hash);else mark(first);
})();"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::User;
    use crate::xapi::parse_user_tweets;

    fn config() -> Config {
        Config::from_yaml(
            "base_path: \"/x\"\nx_bearer_token: \"super-secret-token-value\"\nposts_per_tab: 20\ntabs:\n  - label: News\n    accounts: [example]\n  - label: Tech\n    accounts: [github]\n",
        )
        .unwrap()
    }

    fn fixture() -> String {
        let text = "Hello <script> @alice #Rust https://t.co/aaaaaaaaaa";
        let mention = text.find("@alice").unwrap();
        let hash = text.find("#Rust").unwrap();
        let url_at = text.find("https://t.co/aaaaaaaaaa").unwrap();
        serde_json::json!({
            "data": [{
                "id": "100",
                "author_id": "42",
                "text": text,
                "created_at": "2024-05-01T12:34:56.000Z",
                "entities": {
                    "mentions": [{"start": mention, "end": mention + 6, "username": "alice"}],
                    "hashtags": [{"start": hash, "end": hash + 5, "tag": "Rust"}],
                    "urls": [{
                        "start": url_at,
                        "end": text.len(),
                        "url": "https://t.co/aaaaaaaaaa",
                        "expanded_url": "https://example.com/story?a=1&b=2",
                        "display_url": "example.com/story"
                    }]
                },
                "attachments": {"media_keys": ["3_1"]}
            }],
            "includes": {
                "users": [{
                    "id": "42",
                    "name": "</a><script>alert(1)</script>",
                    "username": "example",
                    "profile_image_url": "https://pbs.twimg.com/profile_images/a_normal.jpg"
                }],
                "media": [{
                    "media_key": "3_1",
                    "type": "photo",
                    "url": "https://pbs.twimg.com/media/abc.jpg",
                    "alt_text": "hill \"><script>",
                    "width": 800,
                    "height": 600
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn renders_mocked_x_response() {
        let user = User {
            id: "42".into(),
            name: "fallback".into(),
            username: "example".into(),
            avatar: None,
        };
        let mut posts = parse_user_tweets(&fixture(), &user).unwrap();
        for post in &mut posts {
            for media in &mut post.media {
                if media.media_key == "3_1" {
                    media.local_path = Some("media/example/3_1.jpg".into());
                }
            }
        }
        let config = config();
        let view = TabView {
            id: "news".into(),
            label: "News".into(),
            posts,
            updated_at: Some(1_700_000_000),
            message: Some("Showing saved posts. @example: rate limited".into()),
        };
        let page = render_page(&config, &view);
        let html = String::from_utf8(page.raw.to_vec()).unwrap();
        assert!(html.contains("example.com/story"));
        assert!(html.contains("https://example.com/story?a=1&amp;b=2"));
        assert!(html.contains("https://x.com/alice"));
        assert!(html.contains("https://x.com/hashtag/Rust"));
        assert!(html.contains("https://x.com/example/status/100"));
        assert!(html
            .contains("<time datetime=\"2024-05-01T12:34:56Z\">2024-05-01 20:34 UTC+08:00</time>"));
        assert!(html.contains(
            "Updated <time datetime=\"2023-11-14T22:13:20Z\">2023-11-15 06:13 UTC+08:00</time>"
        ));
        assert!(!html.contains("12:34 UTC"));
        assert!(html.contains("/x/media/example/3_1.jpg"));
        assert!(html.contains("loading=\"lazy\" decoding=\"async\""));
        assert!(!html.contains("https://pbs.twimg.com/media/abc.jpg"));
        assert!(html.contains("width=\"800\""));
        assert!(html.contains("alt=\"hill &quot;&gt;&lt;script&gt;\""));
        assert!(html.contains("&lt;/a&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("<img onerror"));
        assert!(html.contains("Showing saved posts."));
        assert!(html.contains("href=\"#news\""));
        assert!(html.contains("href=\"/x/health\""));
        assert!(html.contains("id=\"news\""));
        assert!(html.contains("id=\"tech\" hidden"));
        assert!(html.contains("Open this tab to load posts."));
        assert!(html.contains("data-base=\"/x\""));
        assert!(html.contains("data-first=\"news\""));
        assert!(html.contains("/tab/"));
        assert!(!html.contains("from github"));
        assert!(!html.contains("super-secret-token-value"));

        let fragment = render_fragment(&config, &view);
        let fragment_html = String::from_utf8(fragment.raw.to_vec()).unwrap();
        assert!(fragment_html.contains("2024-05-01 20:34 UTC+08:00"));
        assert!(fragment_html.contains("2023-11-15 06:13 UTC+08:00"));

        let labeled = Config {
            timezone_label: Some("Taiwan".into()),
            ..config.clone()
        };
        let labeled_html =
            String::from_utf8(render_fragment(&labeled, &view).raw.to_vec()).unwrap();
        assert!(labeled_html
            .contains("<time datetime=\"2024-05-01T12:34:56Z\">2024-05-01 20:34 Taiwan</time>"));
        assert!(labeled_html.contains(
            "Updated <time datetime=\"2023-11-14T22:13:20Z\">2023-11-15 06:13 Taiwan</time>"
        ));
        assert!(!labeled_html.contains("UTC+08:00"));
        assert!(fragment_html.contains("Showing saved posts."));
        assert!(fragment_html.contains("https://x.com/example/status/100"));
        assert!(!fragment_html.contains("<html"));
        assert!(!fragment_html.contains("super-secret-token-value"));

        let json = render_tab_json("/x", &view);
        assert!(!std::str::from_utf8(&json.raw)
            .unwrap()
            .contains("super-secret-token-value"));
        let value: serde_json::Value = serde_json::from_slice(&json.raw).unwrap();
        assert_eq!(value["id"], "news");
        assert_eq!(value["stale"], true);
        assert_eq!(value["posts"][0]["username"], "example");
        assert_eq!(value["posts"][0]["url"], "https://x.com/example/status/100");
        assert_eq!(value["posts"][0]["media"][0]["type"], "photo");
        let photo = &value["posts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|post| {
                post["media"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
            })
            .unwrap()["media"];
        assert_eq!(photo[0]["url"], "/x/media/example/3_1.jpg");
        assert_eq!(
            photo[0]["remote_url"],
            "https://pbs.twimg.com/media/abc.jpg"
        );
        assert_eq!(photo[0]["local_path"], "media/example/3_1.jpg");
        assert!(value["posts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("@alice"));

        assert!(page.gzip.is_some());
        assert!(page.br.is_some());
        let gzip = page.gzip.clone().unwrap();
        let mut decoder = flate2::read::GzDecoder::new(gzip.as_ref());
        let mut plain = Vec::new();
        decoder.read_to_end(&mut plain).unwrap();
        assert_eq!(plain, page.raw.as_ref());

        let brotli = page.br.clone().unwrap();
        let mut decoder = brotli::Decompressor::new(brotli.as_ref(), 4096);
        let mut plain = Vec::new();
        decoder.read_to_end(&mut plain).unwrap();
        assert_eq!(plain, page.raw.as_ref());

        let health = render_health(&config);
        assert!(!std::str::from_utf8(&health)
            .unwrap()
            .contains("super-secret-token-value"));
        let health: serde_json::Value = serde_json::from_slice(&health).unwrap();
        assert_eq!(health["ok"], true);
        assert_eq!(health["timezone"], "Asia/Taipei");
        assert_eq!(health["data_dir"], "data");
    }
}
