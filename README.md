# X wire

A small Rust service that shows recent posts from X, one tab per group of accounts. Posts are stored as JSONL and served from memory. The X API is called only when someone opens a tab, and at most once per account in each six-hour slot.

The public site runs at the domain root, `https://x.0x6.ai/`.

## Run

`config.yml` holds the bearer token, so it is git-ignored. Start from the example:

```bash
cp config.example.yml config.yml
# Edit config.yml and replace x_bearer_token, or export an override.
export X_BEARER_TOKEN='...'   # optional; wins over config.yml when non-empty
cargo run --release -- config.yml
```

Open `http://127.0.0.1:8181/`. The first tab loads with that request. Other tabs load when you open them.

### Toolchain in `.venv`

The Rust toolchain lives inside the project, not in `~/.cargo` (`.venv/` is git-ignored):

```bash
export RUSTUP_HOME="$PWD/.venv/rustup" CARGO_HOME="$PWD/.venv/cargo"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable
export PATH="$CARGO_HOME/bin:$PATH"
```

### Production: `scripts/run.sh` in tmux

`scripts/run.sh` sets `RUSTUP_HOME`, `CARGO_HOME`, and `PATH` to the `.venv` toolchain, builds the release binary if sources changed, and runs `target/release/xfeed config.yml`. It exits with a hint if `config.yml` is missing. Run it in the tmux session `X`:

```bash
tmux new-session -d -s X /root/workspace/X/scripts/run.sh     # new session
tmux new-window -d -t X: -n xfeed /root/workspace/X/scripts/run.sh   # or a window in an existing session
```

To restart after pulling changes, press Ctrl-C in that window and run `scripts/run.sh` again (or `tmux respawn-pane -k -t X:xfeed /root/workspace/X/scripts/run.sh`).

`cargo run --release` builds a single binary, `target/release/xfeed`. The runtime inputs are the config file, the data directory it names, and optionally `X_BEARER_TOKEN`.

```bash
xfeed                       # serve config.yml
xfeed serve config.yml
xfeed update                # refresh every account, ignoring the slot limit
xfeed update --tab News
xfeed update --account NASA --config config.yml
./update_posts.sh           # same as `xfeed update`, after a release or debug build
./update_posts.sh --tab Tech
```

`update` prints `handle: stored N` or `handle: error: ...` and exits 1 if any account fails. It reads the same `config.yml`. A manual update counts as that slot's successful fetch, so the site will not call X again for that account until the next slot.

## Bearer token

Put the app-only bearer token in `config.yml`:

```yaml
x_bearer_token: "your-bearer-token-here"
```

The committed file uses that placeholder. Replace it with a token from the X developer portal.

If `X_BEARER_TOKEN` is set and not blank, it overrides the file. A blank or unset variable leaves the file value in place. A leading `Bearer ` is stripped. The process logs which source it used (`config.yml` or `X_BEARER_TOKEN`) and does not log the token. `Debug` output for the config prints `<redacted>`.

## Config

| Key | Default | Meaning |
| --- | --- | --- |
| `x_bearer_token` | empty | App-only bearer token. One line, no spaces. |
| `listen` | `0.0.0.0:8080` | Bind address. The example uses `0.0.0.0:8181`. |
| `base_path` | empty (root) | Public prefix. `/` or empty serves at the domain root (the example uses `/`). Use e.g. `/x` to mount under a path. |
| `cache_max_age_secs` | `0` | `0` sends `Cache-Control: private, no-cache`. Otherwise `private, max-age`. |
| `posts_per_tab` | `20` | Posts shown after merging that tab's accounts (1–100). |
| `max_stored_posts` | `400` | Posts kept in each account file (1–5000, at least `posts_per_tab`). |
| `max_image_bytes` | `8000000` | Largest image saved locally. `0` skips downloads and keeps the remote URL. |
| `exclude_replies` | `true` | Drop replies. |
| `exclude_retweets` | `true` | Drop reposts. |
| `api_base` | `https://api.x.com` | API origin. `http://` is allowed only for localhost. |
| `data_dir` | `data` | Directory of `{handle}.jsonl` and `{handle}.state.json`. |
| `timezone` | `Asia/Taipei` | IANA zone for the four daily slots and for displayed times. |
| `timezone_label` | unset | Text after displayed times, e.g. `Taiwan` gives `2026-10-09 10:46 Taiwan`. Unset shows the offset (`UTC+08:00`). The `<time datetime>` attribute and JSON stay UTC. |
| `tabs` | required | Each tab has a `label` and a list of usernames. |

Usernames may include a leading `@`. Labels become ids (`News` → `news`). A label that does not yield an ASCII slug becomes `tab-1`, `tab-2`, and so on.

## Slots and storage

Each local day is split into `00–06`, `06–12`, `12–18`, and `18–24` in `timezone`. The first request that opens a tab in a slot fetches that tab's accounts. Later requests in the same slot read the JSONL files. Concurrent requests for one account share a single in-flight call. A failed call does not consume the slot, so a later request may retry. The last successful time is stored next to the JSONL file, so a restart does not refetch.

User lookup is cached and is not the slot's successful call. New posts are requested with `since_id` set to the newest stored id. There is no background polling: an account is updated only when its tab is opened, or when `xfeed update` is run. The home page always checks the first tab. If the URL hash names another tab, the browser then requests that tab as well.

Posts are deduplicated by id. A newer copy replaces an older one, and the file keeps the newest `max_stored_posts`. A saved image path is kept when the same post is fetched again.

## Images

Each post's JSONL record stores the remote image URL, width, height, alt text, and, once downloaded, a path such as `media/bbcworld/3_1.jpg` under `data_dir`. Photos are saved from `url`. Videos and GIFs save `preview_image_url` and keep the original video URL. The file name is the media key plus an extension taken from the bytes, so a file that is already there is not downloaded again. Downloads run a few at a time, with a 10 second timeout, on a separate client. They are not X API calls and do not send the bearer token.

If a download fails, the page uses the remote URL. The next fetch or `xfeed update` tries the missing files again. `max_image_bytes` drops anything larger, and anything that is not a JPEG, PNG, GIF, or WebP.

The site serves those files at `{base}/media/{account}/{file}` with `Cache-Control: public, max-age=31536000, immutable`. The `<img>` tag uses the local copy (`loading="lazy" decoding="async"`, with width and height when X sent them).

```bash
xfeed prune                 # delete files under data/media that no post references
./update_posts.sh           # refresh posts and download their images
```

## Endpoints

With `base_path: /` (the default). With a prefix such as `/x`, every path below gains it, and `/x` redirects to `/x/`:

| Method | Path | Body |
| --- | --- | --- |
| GET | `/` | HTML for the first tab. This request may refresh that tab's accounts. |
| GET | `/tab/news` | HTML fragment for that tab. Opening a tab fetches this. |
| GET | `/api/news` | JSON for that tab. `/api/news.json` is the same. This also counts as opening the tab. |
| GET | `/media/bbcworld/3_1.jpg` | A saved image. Long-lived immutable cache. Does not call X. |
| GET | `/health` | `{"ok":true,"timezone":"Asia/Taipei","data_dir":"data"}`. Does not call X. |

HTML and JSON are gzipped or brotlied when that is smaller. Responses send `ETag` and `Vary: Accept-Encoding`. A matching `If-None-Match` returns `304` after the slot check, so a conditional request can still refresh a new slot. Health is `Cache-Control: no-store`.

Post text is HTML-escaped. Links, mentions, hashtags, and cashtags become anchors. Timestamps link to the post on X. Photos and video stills are shown only when the URL is on `pbs.twimg.com` or `ton.twimg.com`.

## Deploy behind nginx at `/x`

The live site uses a Cloudflare tunnel that forwards `x.0x6.ai` to `localhost:8181` with `base_path: /`, so no nginx is needed. To mount it under `/x` instead, run the binary on `127.0.0.1:8080` with `base_path: /x`. Forward `Accept-Encoding` and do not recompress, so the precompressed body is what the browser gets. `/x/tab/` and `/x/api/` use the same prefix.

```nginx
location = /x {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Accept-Encoding $http_accept_encoding;
    gzip off;
}

location /x/ {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Accept-Encoding $http_accept_encoding;
    gzip off;
}
```

`proxy_pass` has no trailing URI, so nginx keeps the `/x` prefix and the app routes it. `location /x/` does not match unrelated paths such as `/xtra`.

Point a probe at `http://127.0.0.1:8080/x/health`. It stays `200` while the process is up.

## Docker

```bash
docker build -t xfeed .
docker run --rm -p 8181:8181 \
  -e X_BEARER_TOKEN \
  -v "$PWD/config.yml:/etc/xfeed/config.yml:ro" \
  -v xfeed-data:/var/lib/xfeed \
  xfeed
```

The image listens on `0.0.0.0:8181` (from `config.example.yml`), reads `/etc/xfeed/config.yml`, and writes JSONL under `/var/lib/xfeed` (the example `data_dir: data` is relative to that workdir). Mount a config that contains `x_bearer_token`, or set `X_BEARER_TOKEN` to override the file. The image ships `config.example.yml` with the placeholder token, which will not authenticate until you replace it.

`docker run --rm ... xfeed update --tab News` refreshes that tab inside the container.

## Tests

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Tests parse `config.example.yml` (never the real `config.yml`), check slot boundaries, merge and dedupe JSONL, render HTML from mocked X JSON, and exercise single-flight fetches and HTTP with an in-process fake. They do not call the live API and do not need a real token.

## Layout

`xfeed` is one crate. `main` serves HTTP or runs `update`. `store` owns the JSONL files, the in-memory posts, and the once-per-slot gate. `images` downloads media and serves the prune walk. `slot` maps a timestamp onto the four local windows. `render` builds the home page, a tab fragment, JSON, gzip, and brotli. `http` selects the encoding, compares the ETag after the slot check, and serves saved images. `xapi` calls the X API v2 and parses posts.
