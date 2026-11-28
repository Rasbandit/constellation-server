//! Uploads of world files (`.apworld`, or a data-only manifest) and player files, and seed
//! generation from a chosen set of them, over HTTP. Signed-in accounts only: every route but
//! sign-up and login needs a session.
//!
//! This file holds the shared state, its routes and their gates. The rest is one module a job:
//! - [`uploads`]: the kinds of upload, their records and files in the store, id and name checks;
//! - [`legacy`]: the sidecar's path, with uploads inspected and worlds generated in the sandbox;
//! - [`engine`]: the data-only path, with manifests prepared and worlds filled by `data_fill`, in
//!   a capped child process or on a pooled worker ([`crate::pool`]);
//! - [`jobs`]: a generation's queue slot, status, heartbeat and sweep, and its run;
//! - [`publish`]: a finished generation's seed stored in the host and listed as a room;
//! - [`view`]: a world's status, seed and room, for the accounts that may see them;
//! - [`migrate`]: what a start moves in from the data folder of an older or stopped server.
//!
//! Storage:
//! - uploads live in the host's store (`world_uploads`, migration 0005), content-addressed by
//!   their sha256, each with its record: the upload's name and what the sidecar's inspect found,
//!   and its game ([`game_slug`]; a yaml of several games is `mixed`). Every server instance
//!   sees them. Uploads an older server kept as files (`apworlds/` and `yamls/`, by game or
//!   flat) are moved into the store at startup ([`Worlds::import_files`]);
//! - each generation's status lives in the store too (`world_jobs`, migration 0006). A queued
//!   or running one is marked alive every `gen_heartbeat_secs` by the process running it; one
//!   not marked for `gen_stale_secs` is failed by any instance's sweep, as its process stopped.
//!   Statuses an older server kept as `worlds/<id>/status.json` move into the store at startup;
//! - `worlds/<id>/` is a legacy generation's job folder: `apworlds/<module>.apworld` and `yamls/`
//!   (the chosen files, written from the store), removed when the job ends, and at startup
//!   when a stopped process left it. The server converts the sidecar's stdout (v1 JSON from
//!   `tools/ap-import`) to a seed (v2, `docs/seed-bundle-v2.md`) itself and stores it; no
//!   folder here is ever writable inside the sandbox.
//!
//! The flow follows Archipelago's web host (`WebHostLib/upload.py` checks uploads before it
//! keeps them; `WebHostLib/generate.py` generates one queued seed at a time from the stored
//! player files). Every call to the sidecar goes through [`crate::sandbox`].

use crate::accounts::{self, Authenticator, Identity, PasswordAuth};
use crate::host::{RoomsFolder, Waker};
use crate::manifests::{self, Manifests, PreparedWorld};
use crate::pool::Pool;
use crate::rooms::RoomKey;
use crate::sandbox::{AP, Failure, JOB, Mount, Sandbox, run_job};
use crate::store;
use crate::{Loaded, hex, parse_bundle};
use axum::Router;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use config::{Generation, Limits};
use log::{JobState, UploadKind, UploadRecord};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

mod engine;
mod jobs;
mod legacy;
mod migrate;
mod publish;
mod uploads;
mod view;

use engine::*;
use jobs::*;
pub(crate) use legacy::valid_module;
use legacy::*;
use publish::*;
use uploads::*;
use view::*;

/// The API's shared state.
pub(crate) struct Worlds {
    dir: PathBuf,
    sandbox: Sandbox,
    limits: Limits,
    /// Generations running at once (`gen_concurrency`).
    one_gen: Arc<Semaphore>,
    /// Generations queued or running (`gen_queue_max`): a new one past it is refused as busy.
    queued: Arc<AtomicUsize>,
    /// Manifest checks running, each a child process (`manifest_check_concurrency`).
    checks: Semaphore,
    /// Uploaded manifests: in the database, with a bounded cache.
    manifests: Manifests,
    /// The host's store: uploaded world and player files and the generations' statuses.
    store: store::Db,
    /// How a manifest fill or check runs.
    jobs: JobRunner,
    /// The fill workers that stay up (`fill_pool_workers`); without them each fill is a job of
    /// its own through `jobs`.
    pool: Option<Pool>,
    /// The jobs run so far, in order.
    #[cfg(test)]
    ran: std::sync::Mutex<Vec<String>>,
    /// The phase timings of the last data-only generation (the server only logs them).
    #[cfg(test)]
    laps: std::sync::Mutex<serde_json::Map<String, Value>>,
    /// Uploads and generation requests per client address (`admin_write_burst`).
    write_rate: edge_compat::RateTable<IpAddr>,
    /// Room view streams open at once (`view_streams`).
    view_streams: Arc<Semaphore>,
    /// Seed names this process put in the rooms folder (the host may not have listed them yet).
    published: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// Makes temporary folder names and world ids unique within the process.
    seq: AtomicU64,
    /// With `--rooms`: the host's rooms folder, where a finished world is listed as a room (its
    /// seed is in the host's store, never a file), and the public `host:port` (or `ws(s)://host:port`
    /// URL) its join address is under.
    rooms: Option<(RoomsFolder, String)>,
    /// The host's store, where every finished world's seed and packages go.
    importer: Option<Waker>,
    /// Sign-up, login and sessions, on the host's store (set by [`Worlds::router`]).
    accounts: Option<Arc<PasswordAuth>>,
    /// Behind a reverse proxy: the header carrying the client address (`--trusted-proxy-header`).
    /// Off by default, because any client could send it.
    proxy_header: Option<HeaderName>,
    /// The generator checkout's version (`__version__` in its `Utils.py`): what a
    /// catalogue version is checked with. None when it cannot be read.
    generator: Option<String>,
    /// The catalogue's version checks running in this process.
    catalogue_checks: crate::checks::Checks,
}

type Reply = Result<Response, (StatusCode, String)>;

fn internal(e: impl core::fmt::Display) -> (StatusCode, String) {
    tracing::error!(error = %e, "worlds api");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error (see the server log)".to_owned())
}

/// A failed check as a reply: what the check refused is the uploader's to read (422), anything
/// else is a 500 that hides its detail.
fn failure_reply(e: Failure) -> (StatusCode, String) {
    match e {
        Failure::Refused(why) => (StatusCode::UNPROCESSABLE_ENTITY, why),
        Failure::Broken(why) => internal(why),
    }
}

/// Run work that blocks and can fail off the runtime's threads; a panic in it is its error.
/// One of four ways `worlds` does this, apart only in what a panic becomes: see #220.
async fn try_blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(e.to_string()))
}

/// A failed store call as a 500 that hides its detail (it is in the log).
fn store_error(e: log::StoreError) -> (StatusCode, String) {
    internal(store_failed(&e))
}

/// A failed store call as the text the upload and job paths return.
fn store_failed(e: &log::StoreError) -> String {
    format!("the store: {e:?}")
}

pub(crate) fn json_reply(status: StatusCode, v: &Value) -> Response {
    json_text(status, v.to_string())
}

fn json_text(status: StatusCode, text: String) -> Response {
    let mut r = (status, text).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

/// Make the data folders; the routes come from [`Worlds::router`].
///
/// # Errors
/// When the data folder cannot be made or read.
pub(crate) fn open(dir: &Path, paths: Generation, limits: Limits) -> Result<Worlds, String> {
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    // Uploads a previous process was inspecting when it stopped.
    if dir.join("tmp").exists() {
        std::fs::remove_dir_all(dir.join("tmp")).map_err(io)?;
    }
    for sub in ["worlds", "tmp"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(io)?;
    }
    let mem = Arc::new(log::MemLogStore::new());
    let generator = generator_version(&paths.ap_dir);
    if generator.is_none() {
        tracing::warn!(
            ap = %paths.ap_dir.display(),
            "no __version__ in the checkout's Utils.py: catalogue versions will fail their checks"
        );
    }
    let exe = crate::sandbox::exe_path()?;
    Ok(Worlds {
        dir: dir.to_owned(),
        sandbox: Sandbox::new(paths, limits, dir)?,
        limits,
        one_gen: Arc::new(Semaphore::new(limits.gen_concurrency.max(1))),
        queued: Arc::new(AtomicUsize::new(0)),
        checks: Semaphore::new(limits.manifest_check_concurrency.max(1)),
        // Until [`Worlds::router`] gives it the host's store.
        manifests: Manifests::new(store::Db::Mem(Arc::clone(&mem)), &limits),
        store: store::Db::Mem(mem),
        pool: Pool::new(exe.clone(), limits),
        #[cfg(test)]
        laps: std::sync::Mutex::default(),
        jobs: JobRunner::Child(exe),
        #[cfg(test)]
        ran: std::sync::Mutex::default(),
        write_rate: edge_compat::RateTable::new(
            limits.admin_write_burst,
            limits.admin_write_refill_milli_per_sec,
            limits.rate_table_max_keys,
        ),
        view_streams: Arc::new(Semaphore::new(limits.view_streams)),
        published: std::sync::Mutex::default(),
        seq: AtomicU64::new(0),
        rooms: None,
        importer: None,
        accounts: None,
        proxy_header: None,
        generator,
        catalogue_checks: crate::checks::Checks::new(&limits),
    })
}

/// The checkout's version: `__version__ = "<v>"` in its `Utils.py`.
fn generator_version(ap: &Path) -> Option<String> {
    let utils = std::fs::read_to_string(ap.join("Utils.py")).ok()?;
    utils.lines().find_map(|line| {
        let rest = line.strip_prefix("__version__")?.trim_start().strip_prefix('=')?.trim();
        let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let inner = rest.strip_prefix(quote)?;
        let v = &inner[..inner.find(quote)?];
        (!v.is_empty()).then(|| v.to_owned())
    })
}

impl core::fmt::Debug for Worlds {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Worlds").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Worlds {
    /// The host's store.
    pub(crate) const fn db(&self) -> &store::Db {
        &self.store
    }

    /// The server's limits.
    pub(crate) const fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The generator version catalogue checks run with.
    pub(crate) fn generator(&self) -> Option<&str> {
        self.generator.as_deref()
    }

    pub(crate) const fn checks(&self) -> &crate::checks::Checks {
        &self.catalogue_checks
    }

    /// Wait for a generation slot (`gen_concurrency`), in the one line players' worlds wait in.
    pub(crate) async fn generation_turn(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        self.one_gen.acquire().await.ok()
    }

    /// The routes, rooted at `/api` (every path under it needs a session, but sign-up and login), on the host's store
    /// `db`, after the files an older server kept are moved into it. With `rooms` (the host's
    /// rooms folder and the public `host:port`), finished worlds are hosted as rooms.
    /// `proxy_header` names the header a trusted reverse proxy puts the client address in.
    pub(crate) async fn router(
        mut self,
        importer: Waker,
        rooms: Option<(RoomsFolder, String)>,
        proxy_header: Option<HeaderName>,
        db: store::Db,
    ) -> Router {
        self.store = db.clone();
        self.manifests = Manifests::new(db.clone(), &self.limits);
        self.import_files(self.store.is_durable()).await;
        self.import_jobs(self.store.is_durable()).await;
        self.rooms = rooms;
        self.importer = Some(importer);
        self.accounts = Some(Arc::new(PasswordAuth::new(db, self.limits, proxy_header.clone())));
        self.proxy_header = proxy_header;
        let state = Arc::new(self);
        tokio::spawn(sweep(Arc::downgrade(&state)));
        tokio::spawn(crate::checks::resume(Arc::clone(&state)));
        routes(state)
    }
}

fn routes(state: Arc<Worlds>) -> Router {
    let l = state.limits;
    let accounts = state.accounts.clone();
    // Each upload kind is cut off at its own cap while the body is read, not after it is held.
    let capped = |cap: usize| DefaultBodyLimit::max(cap);
    Router::new()
        .route(
            "/api/apworlds",
            get(list_apworlds).merge(put(put_apworld).layer(capped(l.apworld_max_bytes))),
        )
        .route("/api/yamls", get(list_yamls).merge(put(put_yaml).layer(capped(l.yaml_max_bytes))))
        .route(
            "/api/manifests",
            get(list_manifests).merge(put(put_manifest).layer(capped(l.manifest_max_bytes))),
        )
        .merge(crate::room_admins::routes())
        .merge(crate::catalogue::routes())
        .route("/api/worlds", post(create_world))
        .route("/api/worlds/{id}", get(world_status))
        .route("/api/worlds/{id}/room", get(room_summary))
        .route("/api/worlds/{id}/bundle", get(world_bundle))
        .route("/api/worlds/{id}/room/events", get(room_events))
        // Anything else under /api needs a session too: 401 without one, never the web UI's 404.
        .route("/api", any(StatusCode::NOT_FOUND))
        .route("/api/", any(StatusCode::NOT_FOUND))
        .route("/api/{*rest}", any(StatusCode::NOT_FOUND))
        .route_layer(axum::middleware::from_fn_with_state(Arc::clone(&state), limit_writes))
        .route_layer(axum::middleware::from_fn_with_state(Arc::clone(&state), auth))
        // After the gate, so it is open: the web UI asks whether to offer sign-up.
        .route("/api/auth", get(auth_mode))
        // The world requests are small JSON.
        .layer(capped(l.yaml_max_bytes))
        .with_state(state)
        // Open: sign-up, login, logout and who-am-I carry no session (their own limits apply).
        .merge(accounts.map_or_else(Router::new, accounts::routes))
}

/// `GET /api/auth`: `{"signup_open": bool}`, so the UI knows whether to offer sign-up.
async fn auth_mode(State(w): State<Arc<Worlds>>) -> impl IntoResponse {
    let body = format!("{{\"signup_open\":{}}}", w.limits.signup_open);
    ([(header::CONTENT_TYPE, "application/json")], body)
}

/// The one gate for `/api`: a valid session cookie puts the [`Identity`] in the request, anything
/// else is a bare 401. Sign-up, login, logout, who-am-I and `/api/auth` are routed outside it.
async fn auth(State(w): State<Arc<Worlds>>, mut req: Request, next: Next) -> Response {
    let Some(a) = &w.accounts else { return StatusCode::UNAUTHORIZED.into_response() };
    let Some(who) = a.identify(req.headers()).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    req.extensions_mut().insert(who);
    next.run(req).await
}

/// A client address may upload and request generations `admin_write_burst` times in a burst,
/// then `admin_write_refill_milli_per_sec` per second; past it, 429 until its allowance refills.
/// Runs behind the session check, so only a signed-in account is counted.
async fn limit_writes(State(w): State<Arc<Worlds>>, req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::PUT | Method::POST) {
        return next.run(req).await;
    }
    let ip = client_ip(&req, w.proxy_header.as_ref());
    if w.write_rate.take(ip, std::time::Instant::now()) {
        return next.run(req).await;
    }
    let refill = u64::from(w.limits.admin_write_refill_milli_per_sec).max(1);
    let mut res = StatusCode::TOO_MANY_REQUESTS.into_response();
    res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(1000_u64.div_ceil(refill)));
    res
}

/// The answer when the server has no room for more work: 503 and when to retry.
fn busy(w: &Worlds, what: &str) -> Response {
    let wait = (w.limits.fill_timeout_ms / 1000).max(1);
    let mut r =
        (StatusCode::SERVICE_UNAVAILABLE, format!("busy: {what}; retry later")).into_response();
    r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait));
    r
}

/// `POST /api/worlds`: take a place in the queue, then hand the request to its path, the
/// data-only engine for one that names a manifest and the sidecar for any other.
async fn create_world(State(w): State<Arc<Worlds>>, who: Identity, body: Bytes) -> Reply {
    let Some(slot) = w.admit() else { return Ok(busy(&w, "too many generations are queued")) };
    let req: Value = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad JSON: {e}")))?;
    let owner = Some(who.subject.as_str());
    if req.get("manifest").is_some() {
        create_data_only_world(&w, &req, owner, slot).await
    } else {
        create_legacy_world(&w, &req, owner, slot).await
    }
}

/// The address failures are counted under: the last entry of the last line of the trusted proxy
/// header (what the proxy itself appended; earlier lines and entries are the client's to forge),
/// else the peer's. An IPv6 address counts as its /64, since one host owns a whole /64.
pub(crate) fn client_ip(req: &Request, proxy_header: Option<&HeaderName>) -> IpAddr {
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

/// The raw (still percent-encoded) value of query parameter `key`.
pub(crate) fn raw_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|p| p.strip_prefix(key)?.strip_prefix('='))
}

pub(crate) fn percent_decode(s: &str) -> Option<String> {
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

#[cfg(test)]
mod tests;
