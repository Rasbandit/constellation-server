//! Uploads of `.apworld` and player `.yaml` files, and seed generation from a chosen set of
//! them, over HTTP. Admin only: every route needs `Authorization: Bearer <token>`.
//!
//! Storage is plain files under the data folder:
//! - `apworlds/<game>/<sha256>.apworld` and `yamls/<game>/<sha256>.yaml`, content-addressed,
//!   each with a `<sha256>.json` holding the upload's name and what the sidecar's inspect found.
//!   `<game>` is the inspected game name made safe by [`game_slug`] (a yaml of several games
//!   goes under `mixed`). The id stays the sha256, so a lookup scans the game folders; files
//!   from before the game folders sit directly in `apworlds/` and `yamls/` and are read too;
//! - `worlds/<id>/` per generation: `apworlds/<module>.apworld` and `yamls/` (the chosen
//!   files), `status.json`, and on success `bundle.seed` (seed file v2) with its game packages
//!   in `packages/` (`docs/seed-bundle-v2.md`). The server converts the sidecar's stdout (v1
//!   JSON from `tools/ap-import`) to v2 itself; no folder here is ever writable inside the
//!   sandbox.
//!
//! The flow follows Archipelago's web host (`WebHostLib/upload.py` checks uploads before it
//! keeps them; `WebHostLib/generate.py` generates one queued seed at a time from the stored
//! player files). Every call to the sidecar goes through [`crate::sandbox`].

use crate::host::{RoomsFolder, Waker};
use crate::rooms::RoomKey;
use crate::sandbox::{AP, Failure, JOB, Mount, Sandbox};
use crate::{Loaded, hex, parse_bundle};
use axum::Router;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use config::{Generation, Limits};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;
use tokio::sync::Semaphore;

/// The API's shared state.
pub(crate) struct Worlds {
    dir: PathBuf,
    /// SHA-256 of the admin token, or None: open mode, no auth (compared in constant time, see [`authorized`]).
    token: Option<[u8; 32]>,
    sandbox: Sandbox,
    limits: Limits,
    /// One generation at a time.
    one_gen: Semaphore,
    /// Room view streams open at once (`view_streams`).
    view_streams: Arc<Semaphore>,
    /// Seed names this process put in the rooms folder (the host may not have listed them yet).
    published: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// Makes temporary folder names and world ids unique within the process.
    seq: AtomicU64,
    /// With `--rooms`: the host's rooms folder, where a finished world's bundle goes as
    /// `<world id>.json`, and the public `host:port` (or `ws(s)://host:port` URL) its join address is under.
    rooms: Option<(RoomsFolder, String)>,
    /// The host's store, where every finished world's seed and packages go.
    importer: Option<Waker>,
    /// Wrong tokens per client address (`admin_auth_fail_burst`).
    auth_fails: edge_compat::RateTable<IpAddr>,
    /// Behind a reverse proxy: the header carrying the client address (`--trusted-proxy-header`).
    /// Off by default, because any client could send it.
    proxy_header: Option<HeaderName>,
}

/// What stores a kind of upload: its folder and file extension under the data folder, and which
/// game names its record holds. Files go to `<data dir>/<folder>/<game>/<sha256>.<ext>` for every
/// kind alike, so a new kind is one entry here plus its route (game manifests: see #101).
struct KindSpec {
    folder: &'static str,
    ext: &'static str,
    /// The game names in a record (untrusted; [`game_folder`] makes the folder from them).
    games: fn(&Value) -> Vec<&str>,
}

const KINDS: [KindSpec; 2] = [
    KindSpec {
        folder: "apworlds",
        ext: "apworld",
        games: |meta| meta["game"].as_str().into_iter().collect(),
    },
    KindSpec {
        folder: "yamls",
        ext: "yaml",
        games: |meta| {
            let players = meta["players"].as_array().map_or(&[][..], Vec::as_slice);
            players.iter().filter_map(|p| p["game"].as_str()).collect()
        },
    },
];

/// The kinds of upload; the discriminant indexes [`KINDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Apworld = 0,
    Yaml = 1,
}

impl Kind {
    const fn spec(self) -> &'static KindSpec {
        &KINDS[self as usize]
    }
    const fn folder(self) -> &'static str {
        self.spec().folder
    }
    const fn ext(self) -> &'static str {
        self.spec().ext
    }
    const fn cap(self, l: &Limits) -> usize {
        match self {
            Self::Apworld => l.apworld_max_bytes,
            Self::Yaml => l.yaml_max_bytes,
        }
    }
}

/// Longest game folder name.
const MAX_SLUG: usize = 64;
/// The folder of a yaml that names several games.
const MIXED: &str = "mixed";
/// The folder when an upload names no usable game.
const UNKNOWN_GAME: &str = "unknown";

/// A game name from an upload (untrusted) as one folder name: lowercase `a-z0-9_-` only, so
/// never a `.`, a separator or an empty name, and at most [`MAX_SLUG`] long.
fn game_slug(game: &str) -> String {
    let mut slug = String::new();
    for c in game.chars().flat_map(char::to_lowercase) {
        let c = if matches!(c, 'a'..='z' | '0'..='9' | '_') { c } else { '-' };
        if c != '-' || !(slug.is_empty() || slug.ends_with('-')) {
            slug.push(c);
        }
    }
    slug.truncate(MAX_SLUG);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { UNKNOWN_GAME.to_owned() } else { slug.to_owned() }
}

/// The game folder for an upload's record: its one game, or [`MIXED`] for several.
fn game_folder(kind: Kind, meta: &Value) -> String {
    let mut games = (kind.spec().games)(meta);
    games.sort_unstable();
    games.dedup();
    match games[..] {
        [] => UNKNOWN_GAME.to_owned(),
        [one] => game_slug(one),
        _ => MIXED.to_owned(),
    }
}

/// The folder holding upload `id`'s files: its game folder, or the old flat folder. `id` must
/// pass [`valid_id`].
fn record_dir(w: &Worlds, kind: Kind, id: &str) -> Option<PathBuf> {
    let store = w.dir.join(kind.folder());
    let json = format!("{id}.json");
    let games = std::fs::read_dir(&store).ok()?.filter_map(Result::ok).map(|e| e.path());
    std::iter::once(store.clone()).chain(games).find(|d| d.join(&json).is_file())
}

type Reply = Result<Response, (StatusCode, String)>;

fn internal(e: impl core::fmt::Display) -> (StatusCode, String) {
    tracing::error!(error = %e, "worlds api");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error (see the server log)".to_owned())
}

fn json_reply(status: StatusCode, v: &Value) -> Response {
    json_text(status, v.to_string())
}

fn json_text(status: StatusCode, text: String) -> Response {
    let mut r = (status, text).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

/// Make the data folders and mark jobs a previous process left unfinished as failed; the
/// routes come from [`Worlds::router`].
///
/// # Errors
/// When the data folder cannot be made or read.
pub(crate) fn open(
    dir: &Path,
    token: Option<&str>,
    paths: Generation,
    limits: Limits,
) -> Result<Worlds, String> {
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    // Uploads a previous process was inspecting when it stopped.
    if dir.join("tmp").exists() {
        std::fs::remove_dir_all(dir.join("tmp")).map_err(io)?;
    }
    for sub in ["apworlds", "yamls", "worlds", "tmp"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(io)?;
    }
    recover(&dir.join("worlds"))?;
    Ok(Worlds {
        dir: dir.to_owned(),
        token: token.map(|t| Sha256::digest(t.as_bytes()).into()),
        sandbox: Sandbox::new(paths, limits, dir)?,
        limits,
        one_gen: Semaphore::new(1),
        view_streams: Arc::new(Semaphore::new(limits.view_streams)),
        published: std::sync::Mutex::default(),
        seq: AtomicU64::new(0),
        rooms: None,
        importer: None,
        auth_fails: edge_compat::RateTable::new(
            limits.admin_auth_fail_burst,
            limits.admin_auth_fail_refill_milli_per_sec,
            limits.rate_table_max_keys,
        ),
        proxy_header: None,
    })
}

impl core::fmt::Debug for Worlds {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Worlds").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Worlds {
    /// Reserve `seed` for a world about to enter the rooms folder. The host lists one room per
    /// seed name, so a second world with a seed it already hosts would silently share the first
    /// one's room: refused instead.
    fn claim_seed(&self, seed: &str) -> Result<(), String> {
        let mut published = self.published.lock().unwrap_or_else(PoisonError::into_inner);
        let listed = self.rooms.as_ref().is_some_and(|(folder, _)| folder.key_of(seed).is_some());
        if listed || !published.insert(seed.to_owned()) {
            return Err(format!(
                "seed {seed:?} is already hosted as a room on this server; generate again for a \
                 new seed"
            ));
        }
        Ok(())
    }

    /// The routes, rooted at `/api` (every path under it needs the token). With `rooms` (the
    /// host's rooms folder and the public `host:port`), finished worlds are hosted as rooms.
    /// `proxy_header` names the header a trusted reverse proxy puts the client address in.
    pub(crate) fn router(
        mut self,
        importer: Waker,
        rooms: Option<(RoomsFolder, String)>,
        proxy_header: Option<HeaderName>,
    ) -> Router {
        self.rooms = rooms;
        self.importer = Some(importer);
        self.proxy_header = proxy_header;
        routes(Arc::new(self))
    }
}

fn routes(state: Arc<Worlds>) -> Router {
    let body_cap = state.limits.apworld_max_bytes.max(state.limits.yaml_max_bytes);
    Router::new()
        .route("/api/apworlds", get(list_apworlds).put(put_apworld))
        .route("/api/yamls", get(list_yamls).put(put_yaml))
        .route("/api/worlds", post(create_world))
        .route("/api/worlds/{id}", get(world_status))
        .route("/api/worlds/{id}/room", get(room_summary))
        .route("/api/worlds/{id}/bundle", get(world_bundle))
        .route("/api/worlds/{id}/room/events", get(room_events))
        // Anything else under /api is the token's too: 401 without it, never the web UI's 404.
        .route("/api", any(StatusCode::NOT_FOUND))
        .route("/api/", any(StatusCode::NOT_FOUND))
        .route("/api/{*rest}", any(StatusCode::NOT_FOUND))
        .route_layer(axum::middleware::from_fn_with_state(Arc::clone(&state), auth))
        // After the gate, so it is open: the web UI asks whether to prompt for a token.
        .route("/api/auth", get(auth_mode))
        .layer(DefaultBodyLimit::max(body_cap))
        .with_state(state)
}

/// `GET /api/auth`: `{"token_required": bool}`, so the UI knows whether to ask for a token.
async fn auth_mode(State(w): State<Arc<Worlds>>) -> impl IntoResponse {
    let body = format!("{{\"token_required\":{}}}", w.token.is_some());
    ([(header::CONTENT_TYPE, "application/json")], body)
}

/// A wrong token is a bare 401 (no detail for a scanner). A client address that sends more than
/// `admin_auth_fail_burst` of them gets 429 until its bucket refills. A right token is never
/// counted or refused, so nobody can lock the admin out by guessing from the same address (all
/// clients share the proxy's address unless `--trusted-proxy-header` is set).
async fn auth(State(w): State<Arc<Worlds>>, req: Request, next: Next) -> Response {
    // Open mode (no token configured): everyone is admin, nothing is counted.
    let Some(want) = &w.token else { return next.run(req).await };
    if authorized(req.headers(), want) {
        return next.run(req).await;
    }
    let ip = client_ip(&req, w.proxy_header.as_ref());
    if w.auth_fails.take(ip, std::time::Instant::now()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let refill = u64::from(w.limits.admin_auth_fail_refill_milli_per_sec).max(1);
    let wait = 1000_u64.div_ceil(refill).max(1);
    let mut res = StatusCode::TOO_MANY_REQUESTS.into_response();
    res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait));
    res
}

/// The address failures are counted under: the last entry of the last line of the trusted proxy
/// header (what the proxy itself appended; earlier lines and entries are the client's to forge),
/// else the peer's. An IPv6 address counts as its /64, since one host owns a whole /64.
fn client_ip(req: &Request, proxy_header: Option<&HeaderName>) -> IpAddr {
    let ip = proxy_header
        .and_then(|name| req.headers().get_all(name).iter().next_back())
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(|v| v.trim().parse().ok())
        .or_else(|| req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()))
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    match ip {
        IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & (u128::MAX << 64)).into()),
        v4 => v4,
    }
}

/// `Authorization: Bearer <token>` whose SHA-256 equals `want`. Both sides are hashed first, so
/// the comparison takes the same time for every token, whatever its length or content.
fn authorized(headers: &HeaderMap, want: &[u8; 32]) -> bool {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    else {
        return false;
    };
    let got: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    got.iter().zip(want).fold(0_u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// Ids are the lowercase hex of a SHA-256: checked before any id reaches a path.
fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The content address of an upload.
fn content_id(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The `name` query parameter, percent-decoded, checked to be a plain file name. It is only
/// shown back (storage uses the content address).
fn upload_name(query: Option<&str>) -> Result<String, String> {
    let raw = query
        .unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("name="))
        .ok_or("the name query parameter is required")?;
    let name = percent_decode(raw).ok_or("the name is not valid percent-encoded UTF-8")?;
    if name.is_empty() || name.len() > 255 || name == "." || name == ".." {
        return Err("the name must be 1 to 255 bytes".to_owned());
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err("the name must be a plain file name".to_owned());
    }
    Ok(name)
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.bytes();
    while let Some(b) = it.next() {
        out.push(match b {
            b'%' => {
                let hi = char::from(it.next()?).to_digit(16)?;
                let lo = char::from(it.next()?).to_digit(16)?;
                u8::try_from(hi * 16 + lo).ok()?
            }
            b'+' => b' ',
            b => b,
        });
    }
    String::from_utf8(out).ok()
}

/// Write `bytes` to `path` through a temporary file and a rename, so a reader never sees half.
/// The temporary name is unique, so two writers of one path never share it.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

async fn put_apworld(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery, body: Bytes) -> Reply {
    upload(&w, Kind::Apworld, q.as_deref(), &body).await
}

async fn put_yaml(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery, body: Bytes) -> Reply {
    upload(&w, Kind::Yaml, q.as_deref(), &body).await
}

/// Check, inspect in the sandbox, then store an upload. The same content again returns the
/// stored record.
async fn upload(w: &Worlds, kind: Kind, query: Option<&str>, body: &[u8]) -> Reply {
    let name = upload_name(query).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let cap = kind.cap(&w.limits);
    if body.len() > cap {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, format!("larger than {cap} bytes")));
    }
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty upload".to_owned()));
    }
    let id = content_id(body);
    if let Some(meta) =
        record_dir(w, kind, &id).and_then(|d| read_json(&d.join(format!("{id}.json"))))
    {
        return Ok(json_reply(StatusCode::OK, &meta));
    }
    let n = w.seq.fetch_add(1, Ordering::Relaxed);
    let tmp = w.dir.join("tmp").join(format!("{id}-{}-{n}", std::process::id()));
    let result = inspect_and_store(w, kind, &name, &id, body, &tmp).await;
    if let Err(e) = std::fs::remove_dir_all(&tmp) {
        tracing::warn!(error = %e, dir = %tmp.display(), "temporary upload folder left behind");
    }
    let meta = result?;
    tracing::info!(kind = kind.folder(), id, name, "upload stored");
    Ok(json_reply(StatusCode::CREATED, &meta))
}

async fn inspect_and_store(
    w: &Worlds,
    kind: Kind,
    name: &str,
    id: &str,
    body: &[u8],
    tmp: &Path,
) -> Result<Value, (StatusCode, String)> {
    let file = format!("upload.{}", kind.ext());
    std::fs::create_dir_all(tmp).map_err(internal)?;
    std::fs::write(tmp.join(&file), body).map_err(internal)?;
    let inside = format!("{JOB}/{file}");
    let (unpacked, entries) =
        (w.limits.apworld_max_unpacked_bytes.to_string(), w.limits.apworld_max_entries.to_string());
    let args: Vec<&str> = match kind {
        Kind::Apworld => {
            vec!["inspect-apworld", &inside, "--max-unpacked", &unpacked, "--max-entries", &entries]
        }
        Kind::Yaml => vec!["inspect-yaml", &inside],
    };
    let timeout = Duration::from_millis(w.limits.inspect_timeout_ms);
    let found = w
        .sandbox
        .run(false, &[Mount::Ro(tmp.to_owned(), JOB.to_owned())], &args, timeout)
        .await
        .map_err(|e| match e {
            Failure::Refused(why) => (StatusCode::UNPROCESSABLE_ENTITY, why),
            Failure::Broken(why) => internal(why),
        })?;
    let found: Value = serde_json::from_slice(&found)
        .map_err(|e| internal(format!("sidecar printed bad JSON: {e}")))?;
    let mut meta = match (kind, found) {
        (Kind::Apworld, Value::Object(o)) => {
            if !o.get("module").and_then(Value::as_str).is_some_and(valid_module) {
                return Err((StatusCode::UNPROCESSABLE_ENTITY, "bad module name".to_owned()));
            }
            Value::Object(o)
        }
        (Kind::Yaml, players @ Value::Array(_)) => json!({"players": players}),
        (_, other) => return Err(internal(format!("unexpected inspect result {other}"))),
    };
    // The server's own fields last, so the sidecar can never set them.
    for (k, v) in [("id", json!(id)), ("name", json!(name)), ("bytes", json!(body.len()))] {
        meta[k] = v;
    }
    let store = w.dir.join(kind.folder()).join(game_folder(kind, &meta));
    std::fs::create_dir_all(&store).map_err(internal)?;
    std::fs::rename(tmp.join(&file), store.join(format!("{id}.{}", kind.ext())))
        .map_err(internal)?;
    write_atomic(&store.join(format!("{id}.json")), meta.to_string().as_bytes())
        .map_err(internal)?;
    Ok(meta)
}

/// What Archipelago imports as `worlds.<module>`; also the file name in `custom_worlds`, so it
/// must never hold a path separator (the sidecar checks the same).
fn valid_module(m: &str) -> bool {
    let mut c = m.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

async fn list_apworlds(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Apworld)
}

async fn list_yamls(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Yaml)
}

fn list(w: &Worlds, kind: Kind) -> Reply {
    let store = w.dir.join(kind.folder());
    let games =
        std::fs::read_dir(&store).map_err(internal)?.filter_map(Result::ok).map(|e| e.path());
    let mut all: Vec<Value> = std::iter::once(store.clone())
        .chain(games.filter(|p| p.is_dir()))
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read_json(&e.path()))
        .collect();
    let key = |v: &Value| (v["name"].as_str().unwrap_or("").to_owned(), v["id"].to_string());
    all.sort_by_key(key);
    Ok(json_reply(StatusCode::OK, &Value::Array(all)))
}

/// The ids in `body[field]`: each valid, stored, and listed once.
fn chosen(w: &Worlds, body: &Value, field: &str, kind: Kind) -> Result<Vec<Value>, String> {
    let ids = body.get(field).and_then(Value::as_array).ok_or(format!("{field} must be a list"))?;
    let mut metas = Vec::with_capacity(ids.len());
    for id in ids {
        let id = id.as_str().filter(|i| valid_id(i)).ok_or(format!("{field}: bad id {id}"))?;
        if metas.iter().any(|m: &Value| m["id"] == id) {
            return Err(format!("{field}: {id} is listed twice"));
        }
        let meta = record_dir(w, kind, id)
            .and_then(|d| read_json(&d.join(format!("{id}.json"))))
            .ok_or(format!("{field}: no upload {id}"))?;
        metas.push(meta);
    }
    Ok(metas)
}

/// Lay out a world folder: the chosen files (apworlds named by module, as Archipelago loads
/// them).
fn prepare(w: &Worlds, world: &Path, apworlds: &[Value], yamls: &[Value]) -> Result<(), String> {
    let e = |e: std::io::Error| e.to_string();
    for sub in ["apworlds", "yamls"] {
        std::fs::create_dir_all(world.join(sub)).map_err(e)?;
    }
    for (kind, metas) in [(Kind::Apworld, apworlds), (Kind::Yaml, yamls)] {
        for m in metas {
            let id = m["id"].as_str().unwrap_or("");
            let stem = if kind == Kind::Apworld { m["module"].as_str().unwrap_or("") } else { id };
            let from = record_dir(w, kind, id)
                .ok_or(format!("no upload {id}"))?
                .join(format!("{id}.{}", kind.ext()));
            let to = world.join(kind.folder()).join(format!("{stem}.{}", kind.ext()));
            if std::fs::hard_link(&from, &to).is_err() {
                std::fs::copy(&from, &to).map_err(e)?;
            }
        }
    }
    Ok(())
}

async fn create_world(State(w): State<Arc<Worlds>>, body: Bytes) -> Reply {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let req: Value = serde_json::from_slice(&body).map_err(|e| bad(format!("bad JSON: {e}")))?;
    let apworlds = chosen(&w, &req, "apworlds", Kind::Apworld).map_err(bad)?;
    let yamls = chosen(&w, &req, "yamls", Kind::Yaml).map_err(bad)?;
    if yamls.is_empty() {
        return Err(bad("choose at least one yaml".to_owned()));
    }
    let mut modules: Vec<&str> = apworlds.iter().filter_map(|m| m["module"].as_str()).collect();
    modules.sort_unstable();
    if modules.windows(2).any(|p| p.first() == p.get(1)) {
        return Err(bad("two apworlds have the same module name".to_owned()));
    }
    let ids = |ms: &[Value]| ms.iter().map(|m| m["id"].clone()).collect::<Vec<_>>();
    let (apworld_ids, yaml_ids) = (ids(&apworlds), ids(&yamls));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = w.seq.fetch_add(1, Ordering::Relaxed);
    let seed = format!("{apworld_ids:?}{yaml_ids:?}{nanos}-{}-{n}", std::process::id());
    let id = content_id(seed.as_bytes());
    let world = w.dir.join("worlds").join(&id);
    prepare(&w, &world, &apworlds, &yamls).map_err(internal)?;
    let status = json!({"id": id, "status": "queued", "apworlds": apworld_ids, "yamls": yaml_ids});
    write_status(&world, &status).map_err(internal)?;
    tracing::info!(world = %id, "generation queued");
    tokio::spawn(generate(Arc::clone(&w), world, status));
    Ok(json_reply(StatusCode::ACCEPTED, &json!({"id": id, "status": "queued"})))
}

fn write_status(world: &Path, status: &Value) -> std::io::Result<()> {
    write_atomic(&world.join("status.json"), status.to_string().as_bytes())
}

/// What `ap_gen.py generate` prints. The bundle stays raw text (no tree of values in memory);
/// `load_bundle` parses it into its typed form.
#[derive(Debug, serde::Deserialize)]
struct GenOut {
    report: Value,
    bundle: Box<serde_json::value::RawValue>,
}

impl GenOut {
    fn parse(stdout: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(stdout)
            .map_err(|e| format!("the sidecar's result is malformed: {e}"))
    }
}

/// Run one queued generation (one at a time) and record how it ended.
async fn generate(w: Arc<Worlds>, world: PathBuf, mut status: Value) {
    let Ok(_permit) = w.one_gen.acquire().await else { return };
    let id = status["id"].as_str().unwrap_or("").to_owned();
    status["status"] = json!("running");
    if let Err(e) = write_status(&world, &status) {
        tracing::error!(error = %e, "status write failed");
    }
    tracing::info!(world = %id, "generation running");
    let (custom, yamls, out) =
        (format!("{AP}/custom_worlds"), format!("{JOB}/yamls"), format!("{JOB}/out"));
    let mounts = [
        Mount::Ro(world.join("apworlds"), custom.clone()),
        Mount::Ro(world.join("yamls"), yamls.clone()),
        Mount::Scratch(out.clone()),
    ];
    let args = ["generate", "--ap", AP, "--apworlds", &custom, "--yamls", &yamls, "--out", &out];
    let timeout = Duration::from_millis(w.limits.gen_timeout_ms);
    let result = match w.sandbox.run(true, &mounts, &args, timeout).await {
        Ok(stdout) => publish(&w, stdout).await,
        Err(e) => Err(e.to_string()),
    };
    // The job's copies of its inputs; the uploads they came from stay in the data folder.
    for sub in ["apworlds", "yamls"] {
        if let Err(e) = std::fs::remove_dir_all(world.join(sub)) {
            tracing::warn!(world = %id, error = %e, "job folder not cleaned up");
        }
    }
    let done = result.is_ok();
    match result {
        Ok((loaded, report)) => {
            status["status"] = json!("done");
            status["seed_name"] = json!(loaded.bundle.seed_name());
            status["slots"] = json!(loaded.bundle.slots().len());
            status["bundle_hash"] = json!(hex(&loaded.hash));
            status["generation"] = report;
        }
        Err(e) => {
            status["status"] = json!("failed");
            status["error"] = json!(e);
        }
    }
    if let Err(e) = write_status(&world, &status) {
        tracing::error!(error = %e, "status write failed");
    }
    // Logged after the status is on disk: tests and operators wait on these lines.
    if done {
        tracing::info!(world = %id, "generation done");
    } else {
        tracing::warn!(world = %id, "generation failed");
    }
}

/// Import the sidecar's v1 bundle (encoded to v2 off the runtime) into the host's store: its
/// seed and packages, never a file. With `--rooms` it is also listed as a room, after its seed
/// name is claimed.
async fn publish(w: &Worlds, stdout: Vec<u8>) -> Result<(Loaded, Value), String> {
    let limits = w.limits;
    let parsed = tokio::task::spawn_blocking(move || {
        let out = GenOut::parse(&stdout)?;
        parse_bundle(out.bundle.get().as_bytes(), &limits).map(|loaded| (loaded, out.report))
    });
    let (loaded, report) = parsed.await.unwrap_or_else(|e| Err(e.to_string()))?;
    // The world's code chose the seed name: a hosted one is refused, listed or not.
    let list = w.rooms.is_some();
    w.claim_seed(loaded.bundle.seed_name())?;
    let importer = w.importer.as_ref().ok_or("no host to store the seed in")?;
    importer.import(loaded.clone(), list).await?;
    Ok((loaded, report))
}

/// The listed room of a finished world (`--rooms` only): its rooms folder, key and join address
/// (`{host}:{port}/{key}`). Found by the seed name in the world's status; the host lists each
/// seed once.
fn hosted<'a>(w: &'a Worlds, status: &Value) -> Option<(&'a RoomsFolder, RoomKey, String)> {
    let (folder, public) = w.rooms.as_ref()?;
    let key = folder.key_of(status["seed_name"].as_str()?)?;
    let address = key.address(public);
    Some((folder, key, address))
}

fn checked_id(id: &str) -> Result<(), (StatusCode, String)> {
    if valid_id(id) { Ok(()) } else { Err((StatusCode::BAD_REQUEST, "bad id".to_owned())) }
}

fn read_status(w: &Worlds, id: &str) -> Result<Value, (StatusCode, String)> {
    checked_id(id)?;
    read_json(&w.dir.join("worlds").join(id).join("status.json"))
        .ok_or((StatusCode::NOT_FOUND, "no such world".to_owned()))
}

/// The status on disk, plus `room_key` and `address` once the host lists the world's room.
async fn world_status(State(w): State<Arc<Worlds>>, UrlPath(id): UrlPath<String>) -> Reply {
    let mut status = read_status(&w, &id)?;
    if let Some((_, key, address)) = hosted(&w, &status) {
        status["room_key"] = json!(key.as_str());
        status["address"] = json!(address);
    }
    Ok(json_reply(StatusCode::OK, &status))
}

fn no_room() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "this world does not run as a room on this server".to_owned())
}

/// The room is listed but could not answer now (it failed to load, or stopped): try again.
fn unavailable(why: impl core::fmt::Display) -> (StatusCode, String) {
    tracing::warn!(%why, "room view unavailable");
    (StatusCode::SERVICE_UNAVAILABLE, "the room is not available right now; try again".to_owned())
}

/// The world's room now ([`edge_compat::CompatRoom::summary`]), with its key and address. A
/// cold room is loaded to answer, as a connection would load it.
/// A finished world's seed, decoded from the store to its v1 JSON: the human view of what the
/// server keeps as binary (`docs/seed-bundle-v2.md`).
async fn world_bundle(State(w): State<Arc<Worlds>>, UrlPath(id): UrlPath<String>) -> Reply {
    let status = read_status(&w, &id)?;
    let seed =
        status["seed_name"].as_str().ok_or((StatusCode::NOT_FOUND, "not done".to_owned()))?;
    let hash = status["bundle_hash"].as_str().unwrap_or_default().to_owned();
    let importer = w.importer.as_ref().ok_or_else(no_room)?;
    let v1 = importer.export(crate::room_id(seed), hash).await.map_err(unavailable)?;
    Ok(json_text(StatusCode::OK, v1))
}

async fn room_summary(State(w): State<Arc<Worlds>>, UrlPath(id): UrlPath<String>) -> Reply {
    let status = read_status(&w, &id)?;
    let (folder, key, address) = hosted(&w, &status).ok_or_else(no_room)?;
    let (room, _visit) = folder.enter(key.clone()).await.map_err(unavailable)?;
    let mut summary = room.summary().await.map_err(|e| unavailable(format!("{e:?}")))?;
    summary["room_key"] = json!(key.as_str());
    summary["address"] = json!(address);
    Ok(json_reply(StatusCode::OK, &summary))
}

/// The world's room events as server-sent events, resuming after `Last-Event-ID`
/// ([`crate::room_view::events`]). The stream counts as a visit, so the room stays in memory
/// while someone watches it; at most `view_streams` are open at once.
async fn room_events(
    State(w): State<Arc<Worlds>>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Reply {
    let status = read_status(&w, &id)?;
    let (folder, key, _) = hosted(&w, &status).ok_or_else(no_room)?;
    let permit = Arc::clone(&w.view_streams).try_acquire_owned().map_err(|_| {
        (StatusCode::SERVICE_UNAVAILABLE, "too many room views are open; try again".to_owned())
    })?;
    let (room, visit) = folder.enter(key).await.map_err(unavailable)?;
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok()?.parse().ok())
        .map_or(proto::Seq(0), proto::Seq);
    let held = crate::room_view::Held { _visit: visit, _permit: permit };
    crate::room_view::events(room, held, after, w.limits.view_history_events)
        .await
        .map_err(|e| unavailable(format!("{e:?}")))
}

/// A job a previous process left queued or running will never finish: mark it failed.
fn recover(worlds: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(worlds).map_err(|e| format!("{}: {e}", worlds.display()))?;
    for world in entries.filter_map(Result::ok).map(|e| e.path()) {
        let Some(mut status) = read_json(&world.join("status.json")) else { continue };
        if matches!(status["status"].as_str(), Some("queued" | "running")) {
            status["status"] = json!("failed");
            status["error"] = json!("the server restarted before this generation finished");
            write_status(&world, &status).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_64_lowercase_hex_digits_are_an_id() {
        let good = "a".repeat(64);
        assert!(valid_id(&good));
        assert!(valid_id(&content_id(b"")));
        for bad in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            format!("{}/", "a".repeat(63)),
            format!("..{}", "a".repeat(62)),
            "g".repeat(64),
        ] {
            assert!(!valid_id(&bad), "{bad:?}");
        }
    }

    /// Expected value: the published SHA-256 of "abc" (FIPS 180-2 appendix B.1).
    #[test]
    fn the_content_address_is_the_hex_sha256_of_the_bytes() {
        assert_eq!(
            content_id(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(content_id(b"abc"), content_id(b"abx"));
    }

    fn headers(auth: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_str(auth).unwrap());
        h
    }

    #[test]
    fn an_ipv6_client_is_counted_by_its_64() {
        let req = |ip: &str| {
            let mut r = Request::new(axum::body::Body::empty());
            r.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 1)));
            r
        };
        assert_eq!(
            client_ip(&req("2001:db8:1:2::1"), None),
            client_ip(&req("2001:db8:1:2:ffff::9"), None)
        );
        assert_ne!(
            client_ip(&req("2001:db8:1:2::1"), None),
            client_ip(&req("2001:db8:1:3::1"), None)
        );
        assert_eq!(client_ip(&req("10.0.0.7"), None), "10.0.0.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn only_the_exact_bearer_token_is_authorized() {
        let want: [u8; 32] = Sha256::digest(b"s3cret").into();
        assert!(authorized(&headers("Bearer s3cret"), &want));
        for bad in [
            "Bearer s3cre",
            "Bearer s3cret ",
            "Bearer S3cret",
            "bearer s3cret",
            "s3cret",
            "Bearer ",
            "Basic s3cret",
        ] {
            assert!(!authorized(&headers(bad), &want), "{bad:?}");
        }
        assert!(!authorized(&HeaderMap::new(), &want));
    }

    /// Two bytes of the hash off by the same bit: an XOR fold would cancel them out.
    #[test]
    fn a_token_whose_hash_differs_in_two_bytes_alike_is_refused() {
        let mut want: [u8; 32] = Sha256::digest(b"s3cret").into();
        want[0] ^= 1;
        want[1] ^= 1;
        assert!(!authorized(&headers("Bearer s3cret"), &want));
    }

    #[test]
    fn an_internal_error_is_a_500_that_hides_its_detail() {
        let (status, text) = internal("/srv/data/secret.json: permission denied");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!text.is_empty() && !text.contains("secret"), "{text}");
    }

    #[test]
    fn an_unavailable_room_is_a_503_that_asks_to_try_again() {
        let (status, text) = unavailable("room task stopped");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(text, "the room is not available right now; try again");
    }

    /// Shows the data folder only: never the token's hash, the sandbox or the limits.
    #[test]
    fn the_state_shows_its_folder_and_not_the_token() {
        let root = temp("debug");
        let w = worlds(&root, Limits::DESIGN);
        let shown = format!("{w:?}");
        assert_eq!(shown, format!("Worlds {{ dir: {:?}, .. }}", root.join("data")));
        assert!(!shown.contains("token") && !shown.contains(&hex(&w.token.unwrap())), "{shown}");
    }

    /// The API's state on a fake checkout under `root`, data in `root/data`.
    fn worlds(root: &Path, limits: Limits) -> Worlds {
        let paths = crate::sandbox::tests::layout(root);
        open(&root.join("data"), Some("token"), paths, limits).unwrap()
    }

    /// Store `body` under `data/<folder>` as an earlier upload did, with record `meta` plus
    /// its id; returns the record.
    fn stored(root: &Path, folder: &str, ext: &str, body: &[u8], mut meta: Value) -> Value {
        let id = content_id(body);
        meta["id"] = json!(id);
        let dir = root.join("data").join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{id}.{ext}")), body).unwrap();
        std::fs::write(dir.join(format!("{id}.json")), meta.to_string()).unwrap();
        meta
    }

    async fn json_of(r: Response) -> Value {
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
        serde_json::from_slice(&axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap())
            .unwrap()
    }

    /// An upload already stored is answered from its record, so this never reaches the
    /// sandbox: what is left to see is the size check in front of it.
    #[tokio::test]
    async fn an_upload_of_exactly_its_cap_is_taken_and_one_byte_more_is_refused() {
        let root = temp("cap");
        let w = worlds(&root, Limits { apworld_max_bytes: 8, yaml_max_bytes: 4, ..Limits::DESIGN });
        for (kind, folder, ext, cap) in
            [(Kind::Apworld, "apworlds", "apworld", 8), (Kind::Yaml, "yamls", "yaml", 4)]
        {
            let body = vec![b'x'; cap];
            let meta = stored(&root, folder, ext, &body, json!({"name": "a"}));
            let r = upload(&w, kind, Some("name=a"), &body).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK, "{folder}");
            assert_eq!(json_of(r).await, meta);
            let over = upload(&w, kind, Some("name=a"), &vec![b'x'; cap + 1]).await.unwrap_err();
            assert_eq!(over.0, StatusCode::PAYLOAD_TOO_LARGE, "{folder}");
        }
    }

    #[tokio::test]
    async fn a_list_holds_each_stored_record_once() {
        let root = temp("list");
        let w = worlds(&root, Limits::DESIGN);
        let meta = stored(&root, "yamls", "yaml", b"name: a", json!({"name": "a.yaml"}));
        let r = list(&w, Kind::Yaml).unwrap();
        assert_eq!(json_of(r).await, json!([meta]));
        assert_eq!(json_of(list(&w, Kind::Apworld).unwrap()).await, json!([]));
    }

    #[test]
    fn chosen_ids_must_each_be_stored_and_listed_once() {
        let root = temp("chosen");
        let w = worlds(&root, Limits::DESIGN);
        let a = stored(&root, "yamls", "yaml", b"a", json!({"name": "a"}));
        let b = stored(&root, "yamls", "yaml", b"b", json!({"name": "b"}));
        let pick = |ids: Value| chosen(&w, &json!({ "yamls": ids }), "yamls", Kind::Yaml);
        assert_eq!(pick(json!([a["id"], b["id"]])).unwrap(), [a.clone(), b]);
        assert!(pick(json!([a["id"], a["id"]])).unwrap_err().contains("listed twice"));
        assert!(pick(json!([content_id(b"c")])).unwrap_err().contains("no upload"));
        assert!(pick(json!(["../x"])).unwrap_err().contains("bad id"));
        assert!(pick(json!("x")).unwrap_err().contains("must be a list"));
        let none = chosen(&w, &json!({}), "apworlds", Kind::Apworld).unwrap_err();
        assert!(none.contains("apworlds must be a list"), "{none}");
    }

    #[test]
    fn a_world_folder_holds_the_chosen_files_apworlds_named_by_module() {
        let root = temp("prepare");
        let w = worlds(&root, Limits::DESIGN);
        let apworld = stored(&root, "apworlds", "apworld", b"zip", json!({"module": "clique"}));
        let yaml = stored(&root, "yamls", "yaml", b"name: a", json!({}));
        let world = root.join("data/worlds/w");
        prepare(&w, &world, &[apworld], std::slice::from_ref(&yaml)).unwrap();
        let names = |sub: &str| -> Vec<String> {
            let entries = std::fs::read_dir(world.join(sub)).unwrap().filter_map(Result::ok);
            entries.map(|e| e.file_name().to_string_lossy().into_owned()).collect()
        };
        assert_eq!(names("apworlds"), ["clique.apworld"]);
        assert_eq!(std::fs::read(world.join("apworlds/clique.apworld")).unwrap(), b"zip");
        let id = yaml["id"].as_str().unwrap();
        assert_eq!(names("yamls"), [format!("{id}.yaml")]);
        assert_eq!(std::fs::read(world.join(format!("yamls/{id}.yaml"))).unwrap(), b"name: a");
    }

    #[tokio::test]
    async fn two_apworlds_with_one_module_name_are_refused_before_anything_runs() {
        let root = temp("same-module");
        let w = Arc::new(worlds(&root, Limits::DESIGN));
        let one = stored(&root, "apworlds", "apworld", b"1", json!({"module": "clique"}));
        let two = stored(&root, "apworlds", "apworld", b"2", json!({"module": "clique"}));
        let yaml = stored(&root, "yamls", "yaml", b"y", json!({}));
        let req = json!({"apworlds": [one["id"], two["id"]], "yamls": [yaml["id"]]});
        let e = create_world(State(w), Bytes::from(req.to_string())).await.unwrap_err();
        assert_eq!(e, (StatusCode::BAD_REQUEST, "two apworlds have the same module name".into()));
        let worlds_dir = std::fs::read_dir(root.join("data/worlds")).unwrap();
        assert_eq!(worlds_dir.count(), 0, "no world folder was made");
    }

    #[test]
    fn a_world_status_needs_a_valid_id_and_an_existing_world() {
        let root = temp("status");
        let w = worlds(&root, Limits::DESIGN);
        assert_eq!(read_status(&w, "../../token").unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(read_status(&w, &content_id(b"none")).unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn upload_names_are_decoded_and_must_be_plain_file_names() {
        assert_eq!(upload_name(Some("name=Majoras%20Mask.yaml")).unwrap(), "Majoras Mask.yaml");
        assert_eq!(upload_name(Some("x=1&name=a+b.yaml")).unwrap(), "a b.yaml");
        assert_eq!(upload_name(Some("name=%C3%A9.yaml")).unwrap(), "é.yaml");
        for bad in [None, Some(""), Some("name="), Some("name=a%2Fb"), Some("name=..")] {
            assert!(upload_name(bad).is_err(), "{bad:?}");
        }
        for bad in ["name=%2", "name=%zz", "name=%FF", "name=a%00b", "name=a%5Cb"] {
            assert!(upload_name(Some(bad)).is_err(), "{bad:?}");
        }
        assert!(upload_name(Some(&format!("name={}", "a".repeat(256)))).is_err());
        assert!(upload_name(Some(&format!("name={}", "a".repeat(255)))).is_ok());
    }

    #[test]
    fn module_names_are_python_identifiers() {
        for good in ["clique", "mm_2ship", "_x", "A1"] {
            assert!(valid_module(good), "{good}");
        }
        for bad in ["", "1a", "a-b", "a/b", "..", "a.b", "é"] {
            assert!(!valid_module(bad), "{bad}");
        }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("worlds-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&d));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_generation_result_must_be_an_object_with_a_report_and_a_bundle() {
        let ok = GenOut::parse(br#"{"report": {"seconds": 1}, "bundle": {"a": [1, 2]}}"#).unwrap();
        assert_eq!((ok.report, ok.bundle.get()), (json!({"seconds": 1}), r#"{"a": [1, 2]}"#));
        for bad in [&b"[]"[..], b"\"x\"", b"{}", br#"{"report": 1}"#, b"", b"{nope"] {
            let e = GenOut::parse(bad).unwrap_err();
            assert!(e.contains("malformed"), "{e}");
        }
    }

    #[test]
    fn concurrent_writers_of_one_path_never_collide() {
        let dir = temp("atomic");
        let path = dir.join("status.json");
        let writers: Vec<_> = (0..16)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    (0..50).try_for_each(|_| write_atomic(&path, format!("{i}").as_bytes()))
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap().unwrap();
        }
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");
    }

    #[test]
    fn a_status_survives_a_restart_and_unfinished_jobs_become_failed() {
        let worlds = temp("recover");
        let mk = |id: &str, status: &str| {
            let w = worlds.join(id);
            std::fs::create_dir_all(&w).unwrap();
            write_status(&w, &json!({"id": id, "status": status, "bundle": "/b"})).unwrap();
        };
        for (id, s) in [("q", "queued"), ("r", "running"), ("d", "done"), ("f", "failed")] {
            mk(id, s);
        }
        std::fs::create_dir_all(worlds.join("empty")).unwrap();
        recover(&worlds).unwrap();
        let read = |id: &str| read_json(&worlds.join(id).join("status.json")).unwrap();
        for id in ["q", "r"] {
            assert_eq!(read(id)["status"], "failed", "{id}");
            assert!(read(id)["error"].as_str().unwrap().contains("restarted"));
        }
        assert_eq!(read("d"), json!({"id": "d", "status": "done", "bundle": "/b"}));
        assert_eq!(read("f")["status"], "failed");
        assert!(!worlds.join("q/status.tmp").exists(), "the write is atomic");
    }
    #[test]
    fn a_game_name_becomes_a_safe_short_folder_name() {
        assert_eq!(game_slug("../../x"), "x");
        assert_eq!(game_slug("Majora's Mask"), "majora-s-mask");
        assert_eq!(game_slug(".."), UNKNOWN_GAME);
        assert_eq!(game_slug(""), UNKNOWN_GAME);
        assert_eq!(game_slug("a/b\\c.d"), "a-b-c-d");
        assert_eq!(game_slug(&"x".repeat(500)).len(), MAX_SLUG);
        for game in ["../../etc", "/abs", "a\0b", "é", "..\\.."] {
            let s = game_slug(game);
            assert!(
                !s.is_empty() && s.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
            );
            assert!(!s.starts_with('-'), "{s}");
        }
    }

    /// A record of `kind` naming `games`, shaped as that kind's inspect result.
    fn record(kind: Kind, games: &[&str]) -> Value {
        match kind {
            Kind::Apworld => json!({"game": games.first()}),
            Kind::Yaml => {
                json!({"players": games.iter().map(|g| json!({"game": g})).collect::<Vec<_>>()})
            }
        }
    }

    /// Every kind in the table: its game folder is safe, and it is the same rule for all.
    #[test]
    fn every_kind_stores_under_a_safe_game_folder_and_several_games_go_under_mixed() {
        for (i, kind) in [Kind::Apworld, Kind::Yaml].into_iter().enumerate() {
            assert_eq!(kind as usize, i, "Kind must index KINDS");
            assert_eq!(game_folder(kind, &record(kind, &["Clique"])), "clique");
            assert_eq!(game_folder(kind, &record(kind, &[])), UNKNOWN_GAME);
            assert_eq!(game_folder(kind, &record(kind, &["../../x"])), "x");
            for hostile in ["..", "/abs", "a/../b", "", "\\..\\x", "a\0b"] {
                let folder = game_folder(kind, &record(kind, &[hostile]));
                let path = Path::new("/data").join(kind.folder()).join(&folder);
                assert!(
                    path.starts_with(format!("/data/{}", kind.folder())),
                    "{hostile}: {path:?}"
                );
                assert_eq!(path.components().count(), 4, "{hostile}: {path:?}");
            }
        }
        assert_eq!(KINDS.len(), 2);
        let yaml = |games: &[&str]| game_folder(Kind::Yaml, &record(Kind::Yaml, games));
        assert_eq!(yaml(&["Clique", "Clique"]), "clique");
        assert_eq!(yaml(&["Clique", "Other"]), MIXED);
    }

    /// Uploads by game, and old flat ones, are all found by id, listed, and survive a restart.
    #[tokio::test]
    async fn an_id_is_found_in_a_game_folder_or_the_old_flat_one_also_after_a_restart() {
        let root = temp("by-game");
        let w = worlds(&root, Limits::DESIGN);
        let flat = stored(&root, "yamls", "yaml", b"flat", json!({"name": "flat"}));
        let grouped = stored(&root, "yamls/clique", "yaml", b"g1", json!({"name": "g1"}));
        let same_game = stored(&root, "yamls/clique", "yaml", b"g2", json!({"name": "g2"}));
        drop(w);
        let w = worlds(&root, Limits::DESIGN);
        let ids = json!([flat["id"], grouped["id"], same_game["id"]]);
        let got = chosen(&w, &json!({ "yamls": ids }), "yamls", Kind::Yaml).unwrap();
        assert_eq!(got, [flat.clone(), grouped.clone(), same_game.clone()]);
        let listed = json_of(list(&w, Kind::Yaml).unwrap()).await;
        assert_eq!(listed, json!([flat, grouped, same_game]));
        let dir = record_dir(&w, Kind::Yaml, grouped["id"].as_str().unwrap()).unwrap();
        assert_eq!(dir, root.join("data/yamls/clique"));
        assert!(record_dir(&w, Kind::Yaml, &"0".repeat(64)).is_none());
    }
}
