//! The built web UI (`--web-dir`), served on the room port from the folder `vite build` wrote,
//! with `.gz` and `.br` siblings made at build time (`web/scripts/precompress.mjs`). WebSocket
//! upgrades never get here: the router sends them to the room first.
//!
//! Rules: a file that exists is served (tower-http `ServeDir`: content type, `Last-Modified`,
//! ranges, the precompressed sibling the client accepts). A path that is not a file falls back to
//! `index.html` only for a GET or HEAD that accepts HTML and whose last segment has no extension
//! and is not under `/assets/` or `/api/` (the admin API is never on this port); anything else is a 404, so a broken asset link is never HTML.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tower::ServiceExt;
use tower_http::services::ServeDir;

/// Built assets carry a content hash in their name, so a browser may keep them for good.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Everything else (`index.html` above all) is checked on every load, so a new build shows at once.
const REVALIDATE: &str = "no-cache";
/// What the built app loads: its own scripts, styles and fetches (`/api`, the `/version` badge,
/// the event stream), images from itself or `data:`. Nothing may frame it.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
    font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
    frame-ancestors 'none'";

#[derive(Debug)]
pub(crate) struct WebDir {
    /// Canonical, so a symlink or `..` that leads out is caught by comparing with it.
    root: PathBuf,
    files: ServeDir,
}

impl WebDir {
    /// The folder of a build: it must hold `index.html`.
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        let root = dir.canonicalize().map_err(|e| format!("--web-dir {}: {e}", dir.display()))?;
        if !root.join("index.html").is_file() {
            return Err(format!("--web-dir {} has no index.html", dir.display()));
        }
        let files = ServeDir::new(&root).precompressed_gzip().precompressed_br();
        Ok(Self { root, files })
    }

    /// The answer to a GET or HEAD of `path`.
    pub(crate) async fn serve(&self, method: &Method, path: &str, h: &HeaderMap) -> Response {
        let target = if self.is_file(path).await {
            path
        } else if falls_back(method, path, h) {
            "/index.html"
        } else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut req = Request::builder().method(method).uri(target);
        for name in [header::ACCEPT_ENCODING, header::IF_MODIFIED_SINCE, header::RANGE] {
            if let Some(v) = h.get(&name) {
                req = req.header(name, v);
            }
        }
        let Ok(req) = req.body(Body::empty()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let Ok(res) = self.files.clone().oneshot(req).await;
        finish(res.map(Body::new), target, h)
    }

    /// `path` names a regular file inside the folder. The path is not percent-decoded, so `%2e%2e`
    /// is just a name that does not exist; `..` is refused here, and a symlink out is refused by
    /// the check on the resolved path.
    async fn is_file(&self, path: &str) -> bool {
        let mut file = self.root.clone();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if matches!(part, "." | "..") || part.contains(['\\', '\0']) {
                return false;
            }
            file.push(part);
        }
        match tokio::fs::canonicalize(&file).await {
            Ok(file) => file.starts_with(&self.root) && file.is_file(),
            Err(_) => false,
        }
    }
}

/// May this missing path be answered with the app? Only a page load: a GET or HEAD that accepts
/// HTML (no `Accept` counts), for a name without an extension outside `/assets/`.
fn falls_back(method: &Method, path: &str, h: &HeaderMap) -> bool {
    let accepts_html = h.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_none_or(|a| {
        a.split(',')
            .any(|t| matches!(t.split(';').next().map(str::trim), Some("text/html" | "*/*")))
    });
    let last = path.rsplit('/').next().unwrap_or_default();
    matches!(*method, Method::GET | Method::HEAD)
        && accepts_html
        && !last.contains('.')
        && ["/assets", "/api"].iter().all(|d| path != *d && !path.starts_with(&format!("{d}/")))
}

/// The headers every UI answer carries, and a weak `ETag` (size and mtime, per encoding) with
/// its `If-None-Match` check, which `ServeDir` does not do.
fn finish(mut res: Response, target: &str, req: &HeaderMap) -> Response {
    let hashed = target.starts_with("/assets/");
    let head = res.headers_mut();
    for (name, value) in [
        (header::CACHE_CONTROL, if hashed { IMMUTABLE } else { REVALIDATE }),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::VARY, "Accept-Encoding"),
    ] {
        head.insert(name, HeaderValue::from_static(value));
    }
    if res.status() != StatusCode::OK {
        return res;
    }
    let tag = etag(res.headers());
    if let Some(tag) = tag.and_then(|t| HeaderValue::from_str(&t).ok()) {
        let same = req.get(header::IF_NONE_MATCH).is_some_and(|m| m.as_bytes() == tag.as_bytes());
        res.headers_mut().insert(header::ETAG, tag);
        if same {
            *res.body_mut() = Body::empty();
            *res.status_mut() = StatusCode::NOT_MODIFIED;
            res.headers_mut().remove(header::CONTENT_LENGTH);
        }
    }
    res
}

fn etag(h: &HeaderMap) -> Option<String> {
    let len = h.get(header::CONTENT_LENGTH)?.to_str().ok()?;
    let modified = httpdate::parse_http_date(h.get(header::LAST_MODIFIED)?.to_str().ok()?).ok()?;
    let secs = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let enc = h.get(header::CONTENT_ENCODING).and_then(|e| e.to_str().ok()).unwrap_or("id");
    Some(format!("W/\"{len}-{secs:x}-{enc}\""))
}
