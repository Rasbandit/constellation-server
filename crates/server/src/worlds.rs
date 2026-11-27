//! Uploads of `.apworld` and player `.yaml` files, and seed generation from a chosen set of
//! them, over HTTP. Signed-in accounts only: every route but sign-up and login needs a session.
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

mod jobs;
mod legacy;
mod migrate;
mod uploads;
mod view;

use jobs::*;
pub(crate) use legacy::valid_module;
use legacy::*;
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
    /// With `--rooms`: the host's rooms folder, where a finished world's bundle goes as
    /// `<world id>.json`, and the public `host:port` its join address is under.
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

/// `status.engine` of a world the data-fill engine generates (a legacy world has none).
const ENGINE_DATA_ONLY: &str = "data-only";

/// How the data-only jobs (a fill, a manifest check) run: in a capped child process, always but
/// in the unit tests of this module, whose executable is not the server's.
#[derive(Debug, Clone)]
enum JobRunner {
    /// This executable's path, found once at startup (see `sandbox::exe_path`).
    Child(PathBuf),
    #[cfg(test)]
    InProcess,
}

type Reply = Result<Response, (StatusCode, String)>;

fn internal(e: impl core::fmt::Display) -> (StatusCode, String) {
    tracing::error!(error = %e, "worlds api");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error (see the server log)".to_owned())
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

    /// Fill the request `head` on the prepared world: on a pooled worker that stays up and may
    /// hold the world loaded, or without a pool as a `fill` job in a process of its own. The
    /// same seed either way, under the same caps and the same `fill_timeout_ms` kill.
    async fn fill(&self, head: &[u8], world: &PreparedWorld) -> Result<Vec<u8>, Failure> {
        if let Some(pool) = &self.pool {
            let job = crate::pool::Job {
                head,
                threads: fill_threads(),
                key: &world.key,
                file: &world.file,
            };
            return pool.fill(job).await;
        }
        let input = data_fill::job::frame(head, &world.file).map_err(Failure::Broken)?;
        self.run_job("fill", &input, Duration::from_millis(self.limits.fill_timeout_ms)).await
    }

    /// Run a data-only job (`prepare` or `fill`) on `input`; see [`JobRunner`].
    async fn run_job(
        &self,
        job: &str,
        input: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, Failure> {
        match &self.jobs {
            JobRunner::Child(exe) => run_job(exe, job, input, &self.limits, timeout).await,
            #[cfg(test)]
            JobRunner::InProcess => {
                let (job, input) = (job.to_owned(), input.to_vec());
                self.ran.lock().unwrap_or_else(PoisonError::into_inner).push(job.clone());
                tokio::task::spawn_blocking(move || {
                    data_fill::job::run(&job, &input).map_err(Failure::Refused)
                })
                .await
                .unwrap_or_else(|e| Err(Failure::Broken(e.to_string())))
            }
        }
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

async fn put_manifest(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery, body: Bytes) -> Reply {
    upload_manifest(&w, q.as_deref(), &body).await
}

/// Check `manifest` in a capped child process (the same engine that will fill it) and keep what
/// the check compiled: the found record and the prepared world's file (`data_fill::prepared`).
async fn prepare_manifest(w: &Worlds, manifest: &[u8]) -> Result<(Value, Vec<u8>), Failure> {
    let timeout = Duration::from_millis(w.limits.inspect_timeout_ms);
    let out = w.run_job("prepare", manifest, timeout).await?;
    let (found, file) = data_fill::job::unframe(&out).map_err(Failure::Broken)?;
    let found = serde_json::from_slice(found)
        .map_err(|e| Failure::Broken(format!("the manifest check printed bad JSON: {e}")))?;
    Ok((found, file.to_vec()))
}

/// Manifest `id`'s prepared world: the stored one, or, when that is stale or lost (prepared by
/// another build or layout, or its blob is missing or torn), prepared again from the stored
/// manifest and stored.
async fn prepared_world(w: &Worlds, id: &str) -> Result<PreparedWorld, String> {
    match w.manifests.prepared(id).await.map_err(|e| format!("{e:?}"))? {
        Some(stored) => Ok(stored),
        None => prepare_again(w, id).await,
    }
}

/// Prepare manifest `id`'s world from the stored manifest and store it in place of what was
/// stored for this engine.
async fn prepare_again(w: &Worlds, id: &str) -> Result<PreparedWorld, String> {
    let manifest = w.manifests.bytes(id).await.map_err(|e| format!("{e:?}"))?;
    let manifest = manifest.ok_or("the manifest is gone")?;
    tracing::info!(id, engine = w.manifests.engine, "preparing a world again");
    let (_, prepared) = prepare_manifest(w, &manifest).await.map_err(|e| e.to_string())?;
    w.manifests.put_prepared(id, prepared).await.map_err(|e| format!("{e:?}"))
}

/// Check a manifest in a capped child process (the same engine that will fill it), then store it
/// and its prepared world in the database. The same content again returns the stored record
/// without a check.
async fn upload_manifest(w: &Worlds, query: Option<&str>, body: &[u8]) -> Reply {
    let name = upload_name(query).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let cap = w.limits.manifest_max_bytes;
    if body.len() > cap {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, format!("larger than {cap} bytes")));
    }
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty upload".to_owned()));
    }
    let id = content_id(body);
    if let Some(stored) = w.manifests.record(&id).await.map_err(store_error)? {
        // A stored manifest whose bytes no longer hash to its id (a torn row) is not served as
        // stored: it falls through to the check and is written again, which heals it.
        match w.manifests.bytes(&id).await {
            Ok(Some(_)) => return Ok(json_reply(StatusCode::OK, &stored)),
            Ok(None) | Err(log::StoreError::Corrupt(_)) => {
                tracing::warn!(id, "stored manifest is missing or torn; storing it again");
            }
            Err(e) => return Err(store_error(e)),
        }
    }
    let Ok(_check) = w.checks.try_acquire() else {
        return Ok(busy(w, "too many manifests are being checked"));
    };
    let (found, prepared) = prepare_manifest(w, body).await.map_err(|e| match e {
        Failure::Refused(why) => (StatusCode::UNPROCESSABLE_ENTITY, why),
        Failure::Broken(why) => internal(why),
    })?;
    let stored = w.manifests.put(&name, body, &found).await.map_err(store_error)?;
    // What the check compiled is kept: a generation fills from it and compiles nothing. The
    // manifest is stored by now, and a generation prepares a world that has no stored form, so
    // failing to keep it is not the upload failing.
    if let Err(e) = w.manifests.put_prepared(&id, prepared).await {
        tracing::warn!(id, error = ?e, "the prepared world was not stored; a generation will");
    }
    tracing::info!(id, name, "manifest stored");
    Ok(json_reply(StatusCode::CREATED, &stored))
}

/// `GET /api/manifests?limit=&after=&game=`: a page of records ascending by id, `{items, next}`;
/// `next` is the `after` of the following page, null on the last.
async fn list_manifests(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery) -> Reply {
    let q = q.unwrap_or_default();
    let param = |key: &str| raw_param(&q, key).and_then(percent_decode);
    let limit = param("limit").and_then(|v| v.parse().ok());
    let bad = |e: log::StoreError| match e {
        log::StoreError::Corrupt(why) => (StatusCode::BAD_REQUEST, why),
        e @ log::StoreError::Unavailable => internal(format!("{e:?}")),
    };
    let (items, next) = w
        .manifests
        .list(param("game").as_deref(), param("after").as_deref(), limit)
        .await
        .map_err(bad)?;
    Ok(json_reply(StatusCode::OK, &json!({"items": items, "next": next})))
}

/// `POST /api/worlds` with `{"manifest": <id>, "yamls": [<id>...]}`: a data-only world, filled by
/// the engine in a capped child process (no world code runs). The player files are JSON
/// documents with a `name`, a `game` (the manifest's) and the world's options, pool and start
/// items (`data_fill::Instance`).
async fn create_data_only_world(
    w: &Arc<Worlds>,
    req: &Value,
    owner: Option<&str>,
    slot: JobSlot,
) -> Reply {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    if req.get("apworlds").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
        return Err(bad("a manifest world takes no legacy world files".to_owned()));
    }
    let manifest_id = req["manifest"].as_str().filter(|i| valid_id(i));
    let manifest_id = manifest_id.ok_or_else(|| bad("manifest: bad id".to_owned()))?;
    let manifest = w.manifests.record(manifest_id).await.map_err(store_error)?;
    let manifest = manifest.ok_or_else(|| bad(format!("manifest: no upload {manifest_id}")))?;
    let yamls = chosen(w, req, "yamls", Kind::Yaml).await?;
    if yamls.is_empty() {
        return Err(bad("choose at least one player file".to_owned()));
    }
    // A player file says which game it is for; the room plays the manifest's one.
    let game = manifest["game"].as_str().unwrap_or("");
    for y in &yamls {
        let games = (Kind::Yaml.spec().games)(y);
        if games.is_empty() || games.iter().any(|g| g.trim() != game) {
            let name = y["name"].as_str().unwrap_or("a player file");
            return Err(bad(format!("{name}: its game is not {game:?}, the manifest's")));
        }
    }
    let yaml_ids = ids(&yamls);
    let id = new_world_id(w, &[&vec![json!(manifest_id)], &yaml_ids]);
    let mut status = json!({
        "id": id, "status": "queued", "engine": ENGINE_DATA_ONLY,
        "manifest": manifest_id, "yamls": yaml_ids,
    });
    with_owner(&mut status, owner);
    queue(w, status, slot).await
}

/// A fill request's player: the slot's `name` and the player file (the world's instance for
/// this player, `data_fill::Instance`; unknown keys such as `game` are ignored by it).
#[derive(serde::Serialize)]
struct PlayerReq {
    name: String,
    instance: Value,
}

/// The players' files as fill request players. Real YAML player files are not read for manifest
/// worlds yet (see #114).
fn player_requests(files: &[(String, Vec<u8>)]) -> Result<Vec<PlayerReq>, String> {
    files
        .iter()
        .map(|(label, raw)| {
            let doc: Value = serde_json::from_slice(raw)
                .map_err(|e| format!("{label}: a manifest world reads JSON player files: {e}"))?;
            let name =
                doc["name"].as_str().ok_or(format!("{label}: the player file has no name"))?;
            Ok(PlayerReq { name: name.to_owned(), instance: doc.clone() })
        })
        .collect()
}

/// Threads a generation asks its fill for: written into the request ([`fill_head`]) and told
/// to the pool, which bounds a worker's CPU time by it. The one place both read.
const fn fill_threads() -> usize {
    config::GENERATION_WORKERS
}

/// The head of a generation's fill request (`data_fill::job::Fill`, JSON), which the prepared
/// world follows: on `config::GENERATION_WORKERS` threads, for a seed of at most
/// `seed_max_bytes`.
fn fill_head(
    seed_name: &str,
    files: &[(String, Vec<u8>)],
    (rng_seed, seed_max_bytes): (u64, u64),
) -> Result<Vec<u8>, String> {
    let head = data_fill::job::Fill {
        seed_name: seed_name.to_owned(),
        players: player_requests(files)?,
        rng_seed,
        workers: fill_threads(),
        algorithm: data_fill::room::Algorithm::Exact,
        seed_max_bytes,
    };
    serde_json::to_vec(&head).map_err(|e| e.to_string())
}

/// Each chosen player file from the store, with the name it was uploaded under (for errors).
async fn player_files(w: &Worlds, yamls: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut files = Vec::with_capacity(yamls.len());
    for id in yamls {
        let meta = w.upload_meta(Kind::Yaml, id).await?.ok_or(format!("no upload {id}"))?;
        let label = meta["name"].as_str().map_or_else(|| id.clone(), str::to_owned);
        files.push((label, w.upload_file(Kind::Yaml, id).await?));
    }
    Ok(files)
}

/// A manifest world: the engine fills it in a child process under the fill caps (address space,
/// CPU time, wall clock; a child past its time is killed), under the same `gen_concurrency`
/// permit, status file and publish path as a legacy world.
async fn generate_data_only(w: &Worlds, status: &Value) -> Result<(Loaded, Value), String> {
    let mut laps = Laps::start();
    let id = status["id"].as_str().unwrap_or("");
    let manifest = status["manifest"].as_str().unwrap_or("");
    let mut prepared = prepared_world(w, manifest).await?;
    let yamls: Vec<String> = status["yamls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|y| y.as_str().map(str::to_owned))
        .collect();
    // The seed is the world id's own hash: the same world id gives the same seed.
    let rng_seed =
        u64::from_le_bytes(Sha256::digest(id.as_bytes())[..8].try_into().unwrap_or([0; 8]));
    let seed_name = format!("data-only-{}", id.get(..12).unwrap_or(id));
    let files = player_files(w, &yamls).await?;
    laps.lap("store");
    let players = yamls.len();
    // Building the request is blocking work.
    let limits = w.limits;
    let head = tokio::task::spawn_blocking(move || {
        fill_head(&seed_name, &files, (rng_seed, limits.bundle_max_bytes))
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))?;
    laps.lap("request");
    // The job answers with the seed file itself; it names the game's package of the prepared
    // world.
    let mut first = true;
    let seed = loop {
        match w.fill(&head, &prepared).await {
            // A stored world this engine cannot load (it hashes to its key, so the store gave
            // it back) would fail every generation of its manifest: prepare it again, once.
            Err(Failure::Refused(why)) if first && why.contains(data_fill::job::UNLOADABLE) => {
                tracing::warn!(manifest, why, "the stored prepared world does not load");
                prepared = prepare_again(w, manifest).await?;
                first = false;
            }
            done => break done.map_err(|e| e.to_string())?,
        }
    };
    laps.lap("job");
    let loaded = tokio::task::spawn_blocking(move || {
        let game = data_fill::prepared::seed_game(&prepared)?.to_vec();
        crate::parse_seed(seed, vec![game], &limits)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))?;
    laps.lap("open");
    publish_loaded(w, loaded.clone()).await?;
    laps.lap("publish");
    // For the operator only. A requester is told no timings: how long the job took says whether
    // its world was already loaded in a pooled worker, that is, whether someone filled it lately.
    let ms = Value::Object(laps.ms.clone()).to_string();
    tracing::info!(world = %id, ms, "data-only generation timed");
    #[cfg(test)]
    {
        *w.laps.lock().unwrap_or_else(PoisonError::into_inner) = laps.ms;
    }
    let report = json!({"engine": ENGINE_DATA_ONLY, "rng_seed": rng_seed, "players": players});
    Ok((loaded, report))
}

/// Where a generation's wall time went: the milliseconds of each phase, for the server's log.
struct Laps {
    at: Instant,
    ms: serde_json::Map<String, Value>,
}

impl Laps {
    fn start() -> Self {
        Self { at: Instant::now(), ms: serde_json::Map::new() }
    }

    /// The phase since the last lap (or the start) ends now.
    fn lap(&mut self, phase: &str) {
        let now = Instant::now();
        // To a thousandth of a millisecond.
        let ms = ((now - self.at).as_secs_f64() * 1e6).round() / 1e3;
        self.ms.insert(phase.to_owned(), json!(ms));
        self.at = now;
    }
}

#[cfg(test)]
mod tests;
