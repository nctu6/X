//! HTTP surface. A tab request may refresh that tab, then the handler
//! serves the rendered body. Health never calls X. Successful GETs of the
//! page and tab fragments add to the aggregate page-view count.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, Method, Response, StatusCode};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;

use crate::images::{resolve_media, ImageSource, CACHE_CONTROL as MEDIA_CACHE};
use crate::pageviews::{is_human_agent, PageViews};
use crate::render::{
    cache_control, render_fragment, render_health, render_page, render_tab_json, Encoded,
};
use crate::store::{PrepareError, Store};
use crate::xapi::XSource;

pub struct AppState<S, I> {
    pub store: Store<S, I>,
    pub views: PageViews,
}

impl<S, I> Clone for AppState<S, I> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            views: self.views.clone(),
        }
    }
}

pub fn router<S, I>(state: AppState<S, I>) -> Router
where
    S: XSource + 'static,
    I: ImageSource + 'static,
{
    let base = state.store.config().base_path.clone();
    let index = if base.is_empty() {
        "/".to_string()
    } else {
        format!("{base}/")
    };
    let health = format!("{base}/health");
    let popularity = format!("{base}/popularity.json");
    let tab = format!("{base}/tab/:tab");
    let api = format!("{base}/api/:tab");
    let media = format!("{base}/media/:account/:file");
    let mut router = Router::new()
        .route(&index, get(index_page))
        .route(&health, get(health_page))
        .route(
            &popularity,
            get(popularity_json).options(popularity_preflight),
        )
        .route(&tab, get(tab_fragment))
        .route(&api, get(tab_json))
        .route(&media, get(media_file));
    if !base.is_empty() {
        router = router.route(&base, get(slash));
    }
    router.fallback(not_found).with_state(state)
}

async fn index_page<S: XSource + 'static, I: ImageSource + 'static>(
    State(state): State<AppState<S, I>>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    let Some(first) = state.store.config().tabs.first().map(|tab| tab.id.clone()) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    let view = match state.store.prepare_tab(&first, false).await {
        Ok(view) => view,
        Err(PrepareError::UnknownTab) => return text(StatusCode::NOT_FOUND, "not found"),
    };
    let cache = cache_control(state.store.config().cache_max_age_secs);
    let page = render_page(state.store.config(), &view);
    let response = serve(&page, HTML, &headers, &cache, true);
    count_view(&state.views, &method, &headers, &response);
    response
}

async fn tab_fragment<S: XSource + 'static, I: ImageSource + 'static>(
    State(state): State<AppState<S, I>>,
    method: Method,
    Path(tab): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let view = match state.store.prepare_tab(&tab, false).await {
        Ok(view) => view,
        Err(PrepareError::UnknownTab) => return text(StatusCode::NOT_FOUND, "not found"),
    };
    let cache = cache_control(state.store.config().cache_max_age_secs);
    let body = render_fragment(state.store.config(), &view);
    let response = serve(&body, HTML, &headers, &cache, true);
    count_view(&state.views, &method, &headers, &response);
    response
}

/// A page view is a GET (not HEAD) from a browser, not a bot, that served
/// the HTML or confirmed the cached copy with `304`. The User-Agent is only
/// read here and never kept.
fn count_view(views: &PageViews, method: &Method, headers: &HeaderMap, response: &Response<Body>) {
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    if method == Method::GET
        && matches!(response.status(), StatusCode::OK | StatusCode::NOT_MODIFIED)
        && is_human_agent(agent)
    {
        views.record();
    }
}

/// Public and cross-origin so other sites can sort by it. A plain GET needs
/// no preflight; OPTIONS is answered anyway.
async fn popularity_json<S, I>(State(state): State<AppState<S, I>>) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, JSON)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CACHE_CONTROL, POPULARITY_CACHE)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(state.views.popularity_json()))
        .expect("popularity response")
}

async fn popularity_preflight() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
        .header(header::ACCESS_CONTROL_MAX_AGE, "86400")
        .header(header::CACHE_CONTROL, POPULARITY_CACHE)
        .body(Body::empty())
        .expect("preflight response")
}

async fn tab_json<S: XSource + 'static, I: ImageSource + 'static>(
    State(state): State<AppState<S, I>>,
    Path(tab): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let id = tab.strip_suffix(".json").unwrap_or(tab.as_str());
    let view = match state.store.prepare_tab(id, false).await {
        Ok(view) => view,
        Err(PrepareError::UnknownTab) => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, JSON)
                .header(header::CACHE_CONTROL, "no-store")
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                .body(Body::from(UNKNOWN_TAB))
                .expect("static response");
        }
    };
    let cache = cache_control(state.store.config().cache_max_age_secs);
    let base = state.store.config().base_path.clone();
    let body = render_tab_json(&base, &view);
    serve(&body, JSON, &headers, &cache, true)
}

async fn health_page<S, I>(State(state): State<AppState<S, I>>) -> Response<Body> {
    let body = render_health(state.store.config());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, JSON)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(body))
        .expect("health response")
}

async fn media_file<S, I>(
    State(state): State<AppState<S, I>>,
    Path((account, file)): Path<(String, String)>,
) -> Response<Body> {
    let dir = std::path::PathBuf::from(&state.store.config().data_dir);
    let Some(path) = resolve_media(&dir, &account, &file) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => return text(StatusCode::NOT_FOUND, "not found"),
    };
    let kind = match file.rsplit_once('.').map(|(_, ext)| ext) {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, kind)
        .header(header::CACHE_CONTROL, MEDIA_CACHE)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(bytes))
        .expect("media response")
}

async fn slash<S, I>(State(state): State<AppState<S, I>>) -> Response<Body> {
    let target = format!("{}/", state.store.config().base_path);
    Response::builder()
        .status(StatusCode::PERMANENT_REDIRECT)
        .header(header::LOCATION, target)
        .body(Body::empty())
        .expect("redirect")
}

async fn not_found() -> Response<Body> {
    text(StatusCode::NOT_FOUND, "not found")
}

fn text(status: StatusCode, body: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("text response")
}

fn serve(
    doc: &Encoded,
    content_type: &'static str,
    headers: &HeaderMap,
    cache_control: &str,
    security: bool,
) -> Response<Body> {
    let choice = choose_encoding(
        headers
            .get(header::ACCEPT_ENCODING)
            .and_then(|value| value.to_str().ok()),
    );
    let (body, etag, coding): (&Bytes, &str, Option<&str>) = match choice {
        Coding::Br => doc
            .br
            .as_ref()
            .map(|body| (body, doc.etag_br.as_str(), Some("br")))
            .unwrap_or((&doc.raw, doc.etag_raw.as_str(), None)),
        Coding::Gzip => doc
            .gzip
            .as_ref()
            .map(|body| (body, doc.etag_gzip.as_str(), Some("gzip")))
            .unwrap_or((&doc.raw, doc.etag_raw.as_str(), None)),
        Coding::Identity => (&doc.raw, doc.etag_raw.as_str(), None),
    };
    if if_none_match(headers, etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::VARY, "Accept-Encoding")
            .header(header::CACHE_CONTROL, cache_control)
            .body(Body::empty())
            .expect("304");
    }
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ETAG, etag)
        .header(header::VARY, "Accept-Encoding")
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::SERVER, "xfeed");
    if let Some(coding) = coding {
        builder = builder.header(header::CONTENT_ENCODING, coding);
    }
    if security {
        builder = builder
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
            .header(header::REFERRER_POLICY, "no-referrer")
            .header(header::CONTENT_SECURITY_POLICY, CSP);
    }
    builder.body(Body::from(body.clone())).expect("response")
}

const HTML: &str = "text/html; charset=utf-8";
const POPULARITY_CACHE: &str = "public, max-age=3600";
const JSON: &str = "application/json; charset=utf-8";
const UNKNOWN_TAB: &str = "{\"error\":\"unknown tab\"}";
const CSP: &str = "default-src 'none'; connect-src 'self'; img-src 'self' https://pbs.twimg.com https://ton.twimg.com; style-src 'unsafe-inline'; script-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coding {
    Br,
    Gzip,
    Identity,
}

fn choose_encoding(header: Option<&str>) -> Coding {
    let Some(header) = header else {
        return Coding::Identity;
    };
    let mut br = None;
    let mut gzip = None;
    let mut star = None;
    for part in header.split(',') {
        let mut pieces = part.trim().split(';');
        let name = pieces.next().unwrap_or("").trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let mut quality = 1000u16;
        for piece in pieces {
            if let Some(value) = piece.trim().strip_prefix("q=") {
                quality = parse_q(value);
            }
        }
        match name.as_str() {
            "br" => br = Some(quality),
            "gzip" => gzip = Some(quality),
            "*" => star = Some(quality),
            _ => {}
        }
    }
    let br_q = br.or(star).unwrap_or(0);
    let gzip_q = gzip.or(star).unwrap_or(0);
    if (br.is_some() || star.is_some()) && br_q > 0 && br_q >= gzip_q {
        return Coding::Br;
    }
    if gzip_q > 0 {
        Coding::Gzip
    } else {
        Coding::Identity
    }
}

fn parse_q(value: &str) -> u16 {
    let value = value.trim();
    if value == "1" || value == "1.0" || value == "1.00" || value == "1.000" {
        return 1000;
    }
    let mut parts = value.split('.');
    let whole: u16 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    if whole >= 1 {
        return 1000;
    }
    let frac = parts.next().unwrap_or("");
    let mut quality = 0u16;
    for (index, ch) in frac.chars().take(3).enumerate() {
        if !ch.is_ascii_digit() {
            return 0;
        }
        let place = 10u16.pow(2 - index as u32);
        quality += (ch as u16 - u16::from(b'0')) * place;
    }
    quality
}

fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get(header::IF_NONE_MATCH) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    text.split(',').any(|part| {
        let part = part.trim();
        part == "*" || part == etag
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::images::IgnoreImages;
    use crate::store::Clock;
    use crate::xapi::XError;
    use axum::body::Body;
    use axum::http::Request;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[test]
    fn negotiates_encoding() {
        assert_eq!(choose_encoding(None), Coding::Identity);
        assert_eq!(choose_encoding(Some("gzip")), Coding::Gzip);
        assert_eq!(choose_encoding(Some("br")), Coding::Br);
        assert_eq!(choose_encoding(Some("gzip, br")), Coding::Br);
        assert_eq!(choose_encoding(Some("br;q=0, gzip")), Coding::Gzip);
        assert_eq!(choose_encoding(Some("gzip;q=0.5, br;q=0.4")), Coding::Gzip);
        assert_eq!(choose_encoding(Some("*")), Coding::Br);
        assert_eq!(choose_encoding(Some("identity")), Coding::Identity);
        assert_eq!(choose_encoding(Some("br;q=0, gzip;q=0")), Coding::Identity);
    }

    struct Mock {
        calls: Arc<AtomicUsize>,
        paths: Arc<Mutex<Vec<String>>>,
    }

    impl XSource for Mock {
        fn fetch<'a>(
            &'a self,
            path: &'a str,
        ) -> impl std::future::Future<Output = Result<String, XError>> + Send + 'a {
            self.paths.lock().expect("paths").push(path.to_string());
            if path.contains("/tweets") {
                self.calls.fetch_add(1, Ordering::SeqCst);
            }
            let path = path.to_string();
            async move {
                if path.starts_with("/2/users/by") {
                    let (name, id) = if path.contains("github") {
                        ("github", "1")
                    } else {
                        ("example", "2")
                    };
                    return Ok(format!(
                        "{{\"data\":[{{\"id\":\"{id}\",\"name\":\"{name}\",\"username\":\"{name}\"}}]}}"
                    ));
                }
                let (id, text, created) = if path.contains("/users/1/") {
                    ("10", "from github", "2024-06-01T00:00:00Z")
                } else {
                    ("11", "from example", "2024-01-01T00:00:00Z")
                };
                if path.contains("since_id=") {
                    return Ok("{\"meta\":{\"result_count\":0}}".to_string());
                }
                Ok(format!(
                    "{{\"data\":[{{\"id\":\"{id}\",\"text\":\"{text}\",\"created_at\":\"{created}\",\"author_id\":\"{id}\"}}]}}"
                ))
            }
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xfeed-http-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    async fn call(
        app: Router,
        uri: &str,
        accept: Option<&str>,
        etag: Option<&str>,
    ) -> axum::http::Response<Body> {
        let mut builder = Request::builder().uri(uri);
        if let Some(accept) = accept {
            builder = builder.header(header::ACCEPT_ENCODING, accept);
        }
        if let Some(etag) = etag {
            builder = builder.header(header::IF_NONE_MATCH, etag);
        }
        app.oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn lazy_tabs_fetch_the_opened_account_once_per_slot() {
        let dir = temp_dir();
        let mock = Mock {
            calls: Arc::new(AtomicUsize::new(0)),
            paths: Arc::new(Mutex::new(Vec::new())),
        };
        let calls = Arc::clone(&mock.calls);
        let paths = Arc::clone(&mock.paths);
        let clock = Clock::system();
        clock.set(1_700_000_000);
        let config = Config::from_yaml(&format!(
            "base_path: \"/x\"\ncache_max_age_secs: 0\nx_bearer_token: \"super-secret-token-value\"\ndata_dir: \"{}\"\ntabs:\n  - label: News\n    accounts: [example]\n  - label: Tech\n    accounts: [github]\n",
            dir.display()
        ))
        .unwrap();
        let store = Store::open_with_clock(config, mock, IgnoreImages, clock.clone())
            .await
            .unwrap();
        let service = router(AppState {
            store: store.clone(),
            views: PageViews::in_memory(chrono_tz::Asia::Taipei),
        });

        let missing = call(service.clone(), "/", None, None).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let redirect = call(service.clone(), "/x", None, None).await;
        assert_eq!(redirect.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(redirect.headers().get(header::LOCATION).unwrap(), "/x/");

        let page = call(service.clone(), "/x/", None, None).await;
        assert_eq!(page.status(), StatusCode::OK);
        assert_eq!(
            page.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-cache"
        );
        assert!(page
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("connect-src 'self'"));
        let etag = page
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let body = axum::body::to_bytes(page.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("from example"));
        assert!(!html.contains("from github"));
        assert!(html.contains("id=\"tech\" hidden"));
        assert!(html.contains("Open this tab to load posts."));
        assert!(!html.contains("super-secret-token-value"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("/2/users/2/tweets") && !path.contains("since_id=")));
        assert!(!paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("/2/users/1/tweets")));

        let again = call(service.clone(), "/x/", None, None).await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let not_modified = call(service.clone(), "/x/", None, Some(&etag)).await;
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let empty = axum::body::to_bytes(not_modified.into_body(), 64)
            .await
            .unwrap();
        assert!(empty.is_empty());

        let health = call(service.clone(), "/x/health", None, None).await;
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let health_body = axum::body::to_bytes(health.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let health_json: serde_json::Value = serde_json::from_slice(&health_body).unwrap();
        assert_eq!(health_json["ok"], true);
        assert_eq!(health_json["timezone"], "Asia/Taipei");
        assert!(!health_body
            .windows(b"super-secret-token-value".len())
            .any(|window| window == b"super-secret-token-value"));

        let gzipped = call(service.clone(), "/x/", Some("gzip"), None).await;
        assert_eq!(
            gzipped
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(|value| value.as_bytes()),
            Some(&b"gzip"[..])
        );
        let bytes = axum::body::to_bytes(gzipped.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let fragment = call(service.clone(), "/x/tab/tech", None, None).await;
        assert_eq!(fragment.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(fragment.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let fragment_html = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(fragment_html.contains("from github"));
        assert!(!fragment_html.contains("<html"));
        assert!(!fragment_html.contains("super-secret-token-value"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let json = call(service.clone(), "/x/api/tech.json", None, None).await;
        assert_eq!(json.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let bytes = axum::body::to_bytes(json.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["label"], "Tech");
        assert_eq!(value["posts"][0]["text"], "from github");

        let unknown = call(service.clone(), "/x/api/nope", None, None).await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        clock.set(1_700_000_000 + 6 * 3600);
        let next = call(service.clone(), "/x/", None, Some(&etag)).await;
        assert_eq!(next.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("since_id=11")));
        assert_eq!(
            paths
                .lock()
                .unwrap()
                .iter()
                .filter(|path| path.contains("/2/users/1/tweets"))
                .count(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    struct PicApi {
        calls: Arc<AtomicUsize>,
    }

    impl XSource for PicApi {
        fn fetch<'a>(
            &'a self,
            path: &'a str,
        ) -> impl std::future::Future<Output = Result<String, XError>> + Send + 'a {
            if path.contains("/tweets") {
                self.calls.fetch_add(1, Ordering::SeqCst);
            }
            let path = path.to_string();
            async move {
                if path.starts_with("/2/users/by") {
                    return Ok(
                        "{\"data\":[{\"id\":\"2\",\"name\":\"example\",\"username\":\"example\"}]}"
                            .into(),
                    );
                }
                Ok(r#"{"data":[{"id":"11","text":"pic","created_at":"2024-01-01T00:00:00Z","author_id":"2","attachments":{"media_keys":["3_1","13_2"]}}],"includes":{"media":[{"media_key":"3_1","type":"photo","url":"https://pbs.twimg.com/media/abc.jpg","alt_text":"hill","width":8,"height":4},{"media_key":"13_2","type":"video","url":"https://video.twimg.com/ext_tw_video/clip.mp4","preview_image_url":"https://pbs.twimg.com/ext_tw_video_thumb/frame.jpg"}]}}"#.into())
            }
        }
    }

    #[derive(Clone)]
    struct Jpeg {
        calls: Arc<AtomicUsize>,
    }

    impl crate::images::ImageSource for Jpeg {
        fn get<'a>(
            &'a self,
            _url: &'a str,
        ) -> impl std::future::Future<Output = Result<crate::images::ImageBody, String>> + Send + 'a
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            async {
                Ok(crate::images::ImageBody {
                    bytes: vec![0xFF, 0xD8, 0xFF, 0x00],
                })
            }
        }
    }

    #[tokio::test]
    async fn local_media_is_served_immutable_and_is_not_an_api_call() {
        let dir = temp_dir();
        let api_calls = Arc::new(AtomicUsize::new(0));
        let image_calls = Arc::new(AtomicUsize::new(0));
        let config = Config::from_yaml(&format!(
            "base_path: \"/x\"\ndata_dir: \"{}\"\ntabs:\n  - label: News\n    accounts: [example]\n",
            dir.display()
        ))
        .unwrap();
        let store = Store::open_with_clock(
            config,
            PicApi {
                calls: Arc::clone(&api_calls),
            },
            Jpeg {
                calls: Arc::clone(&image_calls),
            },
            Clock::system(),
        )
        .await
        .unwrap();
        let service = router(AppState {
            store,
            views: PageViews::in_memory(chrono_tz::Asia::Taipei),
        });
        let page = call(service.clone(), "/x/", None, None).await;
        let html = String::from_utf8(
            axum::body::to_bytes(page.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("/x/media/example/3_1.jpg"));
        assert!(html.contains("/x/media/example/13_2.jpg"));
        assert!(html.contains("loading=\"lazy\" decoding=\"async\""));
        assert!(!html.contains("https://pbs.twimg.com/media/abc.jpg"));
        assert_eq!(api_calls.load(Ordering::SeqCst), 1);
        assert_eq!(image_calls.load(Ordering::SeqCst), 2);

        let image = call(service.clone(), "/x/media/example/3_1.jpg", None, None).await;
        assert_eq!(image.status(), StatusCode::OK);
        assert_eq!(
            image.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            image.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/jpeg"
        );
        let bytes = axum::body::to_bytes(image.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF]);

        let again = call(service.clone(), "/x/", None, None).await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(api_calls.load(Ordering::SeqCst), 1);
        assert_eq!(image_calls.load(Ordering::SeqCst), 2);

        let sneaky = call(service.clone(), "/x/media/example/..%2Fsecret", None, None).await;
        assert_eq!(sneaky.status(), StatusCode::NOT_FOUND);
        let missing = call(service.clone(), "/x/media/example/nope.txt", None, None).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&dir);
    }
    const BROWSER: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

    async fn request(
        app: Router,
        method: Method,
        uri: &str,
        agent: Option<&str>,
        etag: Option<&str>,
    ) -> Response<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(agent) = agent {
            builder = builder.header(header::USER_AGENT, agent);
        }
        if let Some(etag) = etag {
            builder = builder.header(header::IF_NONE_MATCH, etag);
        }
        app.oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn page_views_count_human_html_gets_only_and_feed_popularity_json() {
        let dir = temp_dir();
        let mock = Mock {
            calls: Arc::new(AtomicUsize::new(0)),
            paths: Arc::new(Mutex::new(Vec::new())),
        };
        let config = Config::from_yaml(&format!(
            "base_path: \"/x\"\ndata_dir: \"{}\"\ntabs:\n  - label: News\n    accounts: [example]\n",
            dir.display()
        ))
        .unwrap();
        let store = Store::open_with_clock(config, mock, IgnoreImages, Clock::system())
            .await
            .unwrap();
        let views = PageViews::in_memory(chrono_tz::Asia::Taipei);
        let service = router(AppState {
            store,
            views: views.clone(),
        });

        let page = request(service.clone(), Method::GET, "/x/", Some(BROWSER), None).await;
        assert_eq!(page.status(), StatusCode::OK);
        let etag = page
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(views.score(), 1);
        let fragment = request(
            service.clone(),
            Method::GET,
            "/x/tab/news",
            Some(BROWSER),
            None,
        )
        .await;
        assert_eq!(fragment.status(), StatusCode::OK);
        assert_eq!(views.score(), 2);
        let cached = request(
            service.clone(),
            Method::GET,
            "/x/",
            Some(BROWSER),
            Some(&etag),
        )
        .await;
        assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(views.score(), 3, "a revalidated page is still a view");

        for (method, uri) in [
            (Method::HEAD, "/x/"),
            (Method::HEAD, "/x/tab/news"),
            (Method::GET, "/x/tab/nope"),
            (Method::GET, "/x/api/news"),
            (Method::GET, "/x/health"),
            (Method::GET, "/x/popularity.json"),
            (Method::GET, "/x/media/example/1_1.jpg"),
            (Method::GET, "/x/missing"),
            (Method::OPTIONS, "/x/popularity.json"),
        ] {
            let _ = request(service.clone(), method.clone(), uri, Some(BROWSER), None).await;
            assert_eq!(views.score(), 3, "{method} {uri} must not count");
        }
        for agent in [
            None,
            Some(""),
            Some("curl/8.5.0"),
            Some("node"),
            Some("python-requests/2.32"),
            Some("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"),
            Some("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/130.0.0.0 Safari/537.36"),
        ] {
            for uri in ["/x/", "/x/tab/news"] {
                let response = request(service.clone(), Method::GET, uri, agent, None).await;
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(views.score(), 3, "{agent:?} on {uri} is not a human view");
            }
        }

        let response = request(
            service.clone(),
            Method::GET,
            "/x/popularity.json",
            Some("node"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json; charset=utf-8"
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "*"
        );
        assert_eq!(
            headers.get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=3600"
        );
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(
            text.starts_with(r#"{"version":1,"id":"x-feed","metric":"pageviews_30d","score":3,"window_days":30,"updated":""#),
            "{text}"
        );
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 6);
        assert!(json["score"].is_u64());
        let updated = json["updated"].as_str().unwrap();
        assert!(updated.ends_with('Z'));
        assert!(chrono::DateTime::parse_from_rfc3339(updated).is_ok());

        let preflight = request(
            service.clone(),
            Method::OPTIONS,
            "/x/popularity.json",
            None,
            None,
        )
        .await;
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            preflight
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "*"
        );
        assert!(preflight
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("GET"));

        let unprefixed = request(service, Method::GET, "/popularity.json", None, None).await;
        assert_eq!(
            unprefixed.status(),
            StatusCode::NOT_FOUND,
            "respects base_path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
