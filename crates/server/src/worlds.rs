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

mod migrate;
mod uploads;

use uploads::*;

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

/// One generation queued or running, counted against `gen_queue_max` until it is dropped.
struct JobSlot(Arc<AtomicUsize>);

impl Drop for JobSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
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

    /// Run one step of a catalogue check in the sandbox on `apworld` (see [`crate::checks`]),
    /// in a folder of its own under `tmp/` (cleared at startup) that goes when it ends.
    pub(crate) async fn sandboxed_check(
        &self,
        step: &crate::checks::Step,
        apworld: &[u8],
    ) -> Result<Vec<u8>, Failure> {
        use crate::checks::Step;
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join("tmp").join(format!("check-{}-{n}", std::process::id()));
        let broken = |e: std::io::Error| Failure::Broken(format!("check folder: {e}"));
        let ran = async {
            let inside = format!("{JOB}/upload.apworld");
            let (unpacked, entries) = (
                self.limits.apworld_max_unpacked_bytes.to_string(),
                self.limits.apworld_max_entries.to_string(),
            );
            let inspect = Duration::from_millis(self.limits.inspect_timeout_ms);
            match step {
                Step::Inspect | Step::Scan => {
                    tokio::fs::create_dir_all(&tmp).await.map_err(broken)?;
                    tokio::fs::write(tmp.join("upload.apworld"), apworld).await.map_err(broken)?;
                    let cmd =
                        if *step == Step::Inspect { "inspect-apworld" } else { "scan-apworld" };
                    let args =
                        [cmd, &inside, "--max-unpacked", &unpacked, "--max-entries", &entries];
                    let mounts = [Mount::Ro(tmp.clone(), JOB.to_owned())];
                    self.sandbox.run(false, &mounts, &args, inspect).await
                }
                Step::Seed { module, seed } => {
                    if !valid_module(module) {
                        return Err(Failure::Refused(format!("bad module name {module:?}")));
                    }
                    let worlds = tmp.join("apworlds");
                    tokio::fs::create_dir_all(&worlds).await.map_err(broken)?;
                    let file = worlds.join(format!("{module}.apworld"));
                    tokio::fs::write(file, apworld).await.map_err(broken)?;
                    let (custom, out) = (format!("{AP}/custom_worlds"), format!("{JOB}/out"));
                    let seed = seed.to_string();
                    let args = [
                        "check",
                        "--ap",
                        AP,
                        "--apworlds",
                        &custom,
                        "--module",
                        module,
                        "--seed",
                        &seed,
                        "--out",
                        &out,
                    ];
                    let mounts = [Mount::Ro(worlds, custom.clone()), Mount::Scratch(out.clone())];
                    let timeout = Duration::from_millis(self.limits.gen_timeout_ms);
                    self.sandbox.run(true, &mounts, &args, timeout).await
                }
            }
        };
        let result = ran.await;
        if let Err(e) = tokio::fs::remove_dir_all(&tmp).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, dir = %tmp.display(), "check folder left behind");
        }
        result
    }

    /// Count a generation against `gen_queue_max`, or `None` when that many are already queued
    /// or running.
    fn admit(&self) -> Option<JobSlot> {
        let before = self.queued.fetch_add(1, Ordering::AcqRel);
        let slot = JobSlot(Arc::clone(&self.queued));
        (before < self.limits.gen_queue_max).then_some(slot)
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

impl Worlds {
    /// Write a status to the store; its `id` and `status` fields say which job and where it is.
    ///
    /// # Errors
    /// A status without them, or the store failing.
    async fn save_status(&self, status: &Value) -> Result<(), String> {
        let (id, state, text) = job_row(status)?;
        self.store.job_put(id, state, &text).await.map_err(|e| store_failed(&e))
    }

    /// [`Self::save_status`] for a job's last status, tried again with a growing wait while
    /// the store is down: a finished job left `running` would be failed later by the sweep.
    async fn save_final(&self, status: &Value) -> Result<(), String> {
        let mut wait = FINAL_WRITE_WAIT;
        for attempt in 1..FINAL_WRITE_TRIES {
            let Err(e) = self.save_status(status).await else { return Ok(()) };
            tracing::warn!(attempt, error = %e, "final status write failed; trying again");
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
        self.save_status(status).await
    }

    /// Fail every queued or running job not marked alive for `gen_lost_secs`: the server
    /// running it is gone. A job its server marks alive meanwhile is left alone.
    async fn fail_stale(&self) {
        let older = Duration::from_secs(self.limits.gen_lost_secs);
        let error = "the server running this generation stopped answering";
        match self.store.job_fail_all_stale(older, error).await {
            Ok(ids) => ids.iter().for_each(|id| {
                tracing::warn!(world = %id, "generation failed: its server stopped");
            }),
            Err(e) => tracing::warn!(error = ?e, "could not fail stopped generations"),
        }
    }
}

/// A status's job id, state and text, as the store keeps them.
fn job_row(status: &Value) -> Result<(&str, JobState, String), String> {
    let id = status["id"].as_str().filter(|i| valid_id(i)).ok_or("a status with a bad id")?;
    let state = status["status"].as_str().and_then(JobState::parse);
    Ok((id, state.ok_or("a status with a bad state")?, status.to_string()))
}

/// Writes of a job's last status: tries, and the wait after the first, doubling (a cost trade:
/// about 15 s in all, one heartbeat, rides out a store restart).
const FINAL_WRITE_TRIES: u32 = 6;
const FINAL_WRITE_WAIT: Duration = Duration::from_millis(500);

/// What a job folder holds, for its log line: each file under it with its size, in name order,
/// at most `most` named. Links are named, never followed.
fn folder_contents(dir: &Path, most: usize) -> String {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.filter_map(Result::ok) {
            let (path, kind) = (e.path(), e.file_type());
            let name = path.strip_prefix(dir).unwrap_or(&path).display().to_string();
            match kind {
                Ok(k) if k.is_dir() => stack.push(path),
                Ok(k) if k.is_file() => {
                    let bytes = e.metadata().map_or(0, |m| m.len());
                    found.push(format!("{name} ({bytes} bytes)"));
                }
                _ => found.push(format!("{name} (not a file)")),
            }
        }
    }
    found.sort();
    let more = found.len().saturating_sub(most);
    found.truncate(most);
    match (found.is_empty(), more) {
        (true, _) => "nothing".to_owned(),
        (false, 0) => found.join(", "),
        (false, n) => format!("{}, and {n} more", found.join(", ")),
    }
}

/// Fail lost generations every `gen_heartbeat_secs` (the first time at once), until the API is
/// gone.
async fn sweep(w: std::sync::Weak<Worlds>) {
    let every = Duration::from_secs(w.upgrade().map_or(1, |w| w.limits.gen_heartbeat_secs).max(1));
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(w) = w.upgrade() else { return };
        w.fail_stale().await;
    }
}

/// A task aborted when this is dropped: a job's heartbeat stops with its job, even when the
/// job panics, so a job that died is never kept alive.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Mark job `id` alive every `gen_heartbeat_secs`, until aborted when the job ends.
async fn heartbeat(w: Arc<Worlds>, id: String) {
    let every = Duration::from_secs(w.limits.gen_heartbeat_secs.max(1));
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = w.store.job_touch(&id).await {
            tracing::warn!(world = %id, error = ?e, "could not mark a generation alive");
        }
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

async fn put_apworld(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery, body: Bytes) -> Reply {
    upload(&w, Kind::Apworld, q.as_deref(), &body).await
}

async fn put_yaml(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery, body: Bytes) -> Reply {
    upload(&w, Kind::Yaml, q.as_deref(), &body).await
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
    if let Some(meta) = w.intact_upload(kind, &id).await.map_err(internal)? {
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
    let mut meta = inspect_in_sandbox(w, kind, &file, tmp).await?;
    // The server's own fields last, so the sidecar can never set them.
    for (k, v) in [("id", json!(id)), ("name", json!(name)), ("bytes", json!(body.len()))] {
        meta[k] = v;
    }
    let key = manifests::key_of(id).ok_or_else(|| internal("an upload id that is not hex"))?;
    let game = game_folder(kind, &meta);
    let row = UploadRecord { kind: kind.store(), id: key, game, meta: meta.to_string() };
    w.store.upload_put(&row, body).await.map_err(store_error)?;
    Ok(meta)
}

/// What the sidecar finds in an `.apworld` or a player `.yaml` (the file is `tmp/<file>`).
async fn inspect_in_sandbox(
    w: &Worlds,
    kind: Kind,
    file: &str,
    tmp: &Path,
) -> Result<Value, (StatusCode, String)> {
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
    match (kind, found) {
        (Kind::Apworld, Value::Object(o)) => {
            if !o.get("module").and_then(Value::as_str).is_some_and(valid_module) {
                return Err((StatusCode::UNPROCESSABLE_ENTITY, "bad module name".to_owned()));
            }
            Ok(Value::Object(o))
        }
        (Kind::Yaml, players @ Value::Array(_)) => Ok(json!({"players": players})),
        (_, other) => Err(internal(format!("unexpected inspect result {other}"))),
    }
}

/// What Archipelago imports as `worlds.<module>`; also the file name in `custom_worlds`, so it
/// must never hold a path separator (the sidecar checks the same).
pub(crate) fn valid_module(m: &str) -> bool {
    let mut c = m.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

async fn list_apworlds(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Apworld).await
}

async fn list_yamls(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Yaml).await
}

async fn list(w: &Worlds, kind: Kind) -> Reply {
    let records = w.store.upload_list(kind.store()).await.map_err(store_error)?;
    let mut all: Vec<Value> = records.iter().filter_map(|r| serde_json::from_str(r).ok()).collect();
    let key = |v: &Value| (v["name"].as_str().unwrap_or("").to_owned(), v["id"].to_string());
    all.sort_by_key(key);
    Ok(json_reply(StatusCode::OK, &Value::Array(all)))
}

/// Lay out a world folder: the chosen files, written from the store (apworlds named by
/// module, as the generator loads them).
async fn prepare(
    w: &Worlds,
    world: &Path,
    apworlds: &[Value],
    yamls: &[Value],
) -> Result<(), String> {
    // Written off the runtime's threads: an apworld may be up to its cap.
    let blocking = async |f: Box<dyn FnOnce() -> std::io::Result<()> + Send>| {
        let done = tokio::task::spawn_blocking(f).await;
        done.map_err(|e| format!("the write stopped: {e}"))?.map_err(|e| e.to_string())
    };
    let subs = ["apworlds", "yamls"].map(|sub| world.join(sub));
    blocking(Box::new(move || subs.iter().try_for_each(std::fs::create_dir_all))).await?;
    for (kind, metas) in [(Kind::Apworld, apworlds), (Kind::Yaml, yamls)] {
        for m in metas {
            let id = m["id"].as_str().unwrap_or("");
            let stem = if kind == Kind::Apworld { m["module"].as_str().unwrap_or("") } else { id };
            let file = w.upload_file(kind, id).await?;
            let to = world.join(kind.folder()).join(format!("{stem}.{}", kind.ext()));
            blocking(Box::new(move || std::fs::write(&to, file))).await?;
        }
    }
    Ok(())
}

async fn create_world(State(w): State<Arc<Worlds>>, who: Identity, body: Bytes) -> Reply {
    let Some(slot) = w.admit() else { return Ok(busy(&w, "too many generations are queued")) };
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let req: Value = serde_json::from_slice(&body).map_err(|e| bad(format!("bad JSON: {e}")))?;
    if req.get("manifest").is_some() {
        return create_data_only_world(&w, &req, Some(who.subject.as_str()), slot).await;
    }
    let apworlds = chosen(&w, &req, "apworlds", Kind::Apworld).await?;
    let yamls = chosen(&w, &req, "yamls", Kind::Yaml).await?;
    if yamls.is_empty() {
        return Err(bad("choose at least one yaml".to_owned()));
    }
    let mut modules: Vec<&str> = apworlds.iter().filter_map(|m| m["module"].as_str()).collect();
    modules.sort_unstable();
    if modules.windows(2).any(|p| p.first() == p.get(1)) {
        return Err(bad("two apworlds have the same module name".to_owned()));
    }
    let (apworld_ids, yaml_ids) = (ids(&apworlds), ids(&yamls));
    let id = new_world_id(&w, &[&apworld_ids, &yaml_ids]);
    let world = w.dir.join("worlds").join(&id);
    if let Err(e) = prepare(&w, &world, &apworlds, &yamls).await {
        drop(std::fs::remove_dir_all(&world));
        return Err(internal(e));
    }
    let mut status =
        json!({"id": id, "status": "queued", "apworlds": apworld_ids, "yamls": yaml_ids});
    with_owner(&mut status, Some(who.subject.as_str()));
    queue(&w, status, slot).await
}

/// Note who asked for the world, so its room is recorded as theirs once it is listed
/// ([`claim_room`]).
fn with_owner(status: &mut Value, owner: Option<&str>) {
    if let Some(o) = owner {
        status["owner"] = json!(o);
    }
}

fn ids(metas: &[Value]) -> Vec<Value> {
    metas.iter().map(|m| m["id"].clone()).collect()
}

/// A world id no other world has: a hash of the chosen ids, the time and a counter.
fn new_world_id(w: &Worlds, chosen: &[&Vec<Value>]) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = w.seq.fetch_add(1, Ordering::Relaxed);
    content_id(format!("{chosen:?}{nanos}-{}-{n}", std::process::id()).as_bytes())
}

/// Make the job's folder, write the queued status and start the job: the one place every kind
/// of world is queued. The folder (a legacy job's copies of its inputs are in it) is made for
/// every job, so a restart finds it and fails the job at once; it is removed when the job ends.
async fn queue(w: &Arc<Worlds>, status: Value, slot: JobSlot) -> Reply {
    let id = status["id"].clone();
    let world = w.dir.join("worlds").join(id.as_str().unwrap_or_default());
    let made = tokio::fs::create_dir_all(&world).await.map_err(|e| e.to_string());
    let saved = match made {
        Ok(()) => w.save_status(&status).await,
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        drop(tokio::fs::remove_dir_all(&world).await);
        return Err(internal(e));
    }
    tracing::info!(world = %id, "generation queued");
    tokio::spawn(generate(Arc::clone(w), world, status, slot));
    Ok(json_reply(StatusCode::ACCEPTED, &json!({"id": id, "status": "queued"})))
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

/// Run one queued generation (one at a time), marked alive while it waits and runs, and
/// record how it ended.
async fn generate(w: Arc<Worlds>, world: PathBuf, status: Value, slot: JobSlot) {
    let id = status["id"].as_str().unwrap_or("").to_owned();
    let beat = AbortOnDrop(tokio::spawn(heartbeat(Arc::clone(&w), id.clone())));
    run_generation(&w, &world, status, slot).await;
    drop(beat);
    // Only copies of uploads (the store keeps them) and nothing the generator made, but its
    // log line says what it held, should a job ever need looking into.
    let most = w.limits.job_log_files_max;
    let removed = tokio::task::spawn_blocking(move || {
        let held = folder_contents(&world, most);
        (std::fs::remove_dir_all(&world), held)
    });
    match removed.await {
        Ok((Ok(()), held)) => tracing::info!(world = %id, held, "job folder removed"),
        Ok((Err(e), held)) => {
            tracing::warn!(world = %id, held, error = %e, "job folder not cleaned up");
        }
        Err(e) => tracing::warn!(world = %id, error = %e, "job folder not cleaned up"),
    }
}

async fn run_generation(w: &Worlds, world: &Path, mut status: Value, _slot: JobSlot) {
    let Ok(_permit) = w.one_gen.acquire().await else { return };
    let id = status["id"].as_str().unwrap_or("").to_owned();
    status["status"] = json!("running");
    if let Err(e) = w.save_status(&status).await {
        tracing::error!(error = %e, "status write failed");
    }
    tracing::info!(world = %id, "generation running");
    let result = if status["engine"] == ENGINE_DATA_ONLY {
        generate_data_only(w, &status).await
    } else {
        generate_sandboxed(w, world).await
    };
    let done = result.is_ok();
    match result {
        Ok((loaded, report)) => {
            status["status"] = json!("done");
            status["seed_name"] = json!(loaded.bundle.seed_name());
            status["slots"] = json!(loaded.bundle.slots().len());
            status["bundle_hash"] = json!(hex(&loaded.hash));
            status["generation"] = report;
            claim_room(w, &status).await;
        }
        Err(e) => {
            status["status"] = json!("failed");
            status["error"] = json!(e);
        }
    }
    if let Err(e) = w.save_final(&status).await {
        tracing::error!(error = %e, "status write failed");
    }
    // Logged after the status is stored: tests and operators wait on these lines. A failure's
    // line holds its error (the sidecar's output within `gen_error_max_bytes`), as its status
    // does.
    if done {
        tracing::info!(world = %id, "generation done");
    } else {
        let error = status["error"].as_str().unwrap_or("");
        tracing::warn!(world = %id, error, "generation failed");
    }
}

/// A legacy world: the sidecar generates it in the sandbox from the chosen files.
async fn generate_sandboxed(w: &Worlds, world: &Path) -> Result<(Loaded, Value), String> {
    let (custom, yamls, out) =
        (format!("{AP}/custom_worlds"), format!("{JOB}/yamls"), format!("{JOB}/out"));
    let mounts = [
        Mount::Ro(world.join("apworlds"), custom.clone()),
        Mount::Ro(world.join("yamls"), yamls.clone()),
        Mount::Scratch(out.clone()),
    ];
    let args = ["generate", "--ap", AP, "--apworlds", &custom, "--yamls", &yamls, "--out", &out];
    let timeout = Duration::from_millis(w.limits.gen_timeout_ms);
    // The job folder (its copies of the inputs) goes when the job ends ([`generate`]).
    match w.sandbox.run(true, &mounts, &args, timeout).await {
        Ok(stdout) => publish(w, stdout).await,
        Err(e) => Err(e.to_string()),
    }
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
    publish_loaded(w, loaded.clone()).await?;
    Ok((loaded, report))
}

/// Store a generated seed in the host's store and list it as a room (with `--rooms`), after its
/// seed name is claimed. The world's generator chose the name: a hosted one is refused, listed
/// or not.
async fn publish_loaded(w: &Worlds, loaded: Loaded) -> Result<(), String> {
    let list = w.rooms.is_some();
    w.claim_seed(loaded.bundle.seed_name())?;
    let importer = w.importer.as_ref().ok_or("no host to store the seed in")?;
    importer.import(loaded, list).await?;
    Ok(())
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

/// Record the finished world's listed room under the account that asked for it, so `GET /api/rooms` finds it. A failure is logged: the
/// world is made, only its listing under that account is missing.
async fn claim_room(w: &Worlds, status: &Value) {
    let (Some((_, key, _)), Some(seed)) = (hosted(w, status), status["seed_name"].as_str()) else {
        return;
    };
    let info = log::OwnedRoom {
        world_id: status["id"].as_str().map(str::to_owned),
        seed_name: seed.to_owned(),
        room_key: key.as_str().to_owned(),
        owner: None,
        age_secs: 0,
    };
    let owner = status["owner"].as_str();
    if let Err(e) = w.store.room_claim(crate::room_id(seed), owner, &info).await {
        tracing::error!(error = ?e, world = ?status["id"], "recording the room's owner failed");
    }
}

fn checked_id(id: &str) -> Result<(), (StatusCode, String)> {
    if valid_id(id) { Ok(()) } else { Err((StatusCode::BAD_REQUEST, "bad id".to_owned())) }
}

/// Job `id`'s status. A queued or running job not marked alive for `gen_stale_secs` reads as
/// `stalled`: its server stopped, or lost the store and may come back (then it reads as
/// before). Worked out here, never stored; past `gen_lost_secs` the sweep fails it.
async fn read_status(w: &Worlds, id: &str) -> Result<Value, (StatusCode, String)> {
    checked_id(id)?;
    let found = w.store.job_status(id).await.map_err(store_error)?;
    let (text, ago) = found.ok_or((StatusCode::NOT_FOUND, "no such world".to_owned()))?;
    let mut status: Value = serde_json::from_str(&text)
        .map_err(|e| internal(format!("world {id}: bad status: {e}")))?;
    let unfinished = matches!(status["status"].as_str(), Some("queued" | "running"));
    if unfinished && ago >= Duration::from_secs(w.limits.gen_stale_secs) {
        status["status"] = json!("stalled");
    }
    Ok(status)
}

/// [`read_status`] for a caller: the world's requester, or anyone with a role on its listed room
/// (`room_admins::role_on`), may see it; everyone else gets the same 404 as for no such world.
async fn read_status_for(
    w: &Worlds,
    who: &Identity,
    id: &str,
) -> Result<Value, (StatusCode, String)> {
    let status = read_status(w, id).await?;
    let mut allowed = status["owner"].as_str() == Some(who.subject.as_str());
    if let (false, Some((_, key, _))) = (allowed, hosted(w, &status)) {
        let role = crate::room_admins::role_on(w.db(), who, key.as_str()).await;
        allowed = role.map_err(|e| internal(format!("{e:?}")))?.is_some();
    }
    if allowed { Ok(status) } else { Err((StatusCode::NOT_FOUND, "no such world".to_owned())) }
}

/// The status, plus `room_key` and `address` once the host lists the world's room.
async fn world_status(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    UrlPath(id): UrlPath<String>,
) -> Reply {
    let mut status = read_status_for(&w, &who, &id).await?;
    // The requester's account id is theirs to see; a room admin does not get the owner's.
    if status["owner"].as_str() != Some(who.subject.as_str()) {
        status.as_object_mut().map(|s| s.remove("owner"));
    }
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
async fn world_bundle(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    UrlPath(id): UrlPath<String>,
) -> Reply {
    let status = read_status_for(&w, &who, &id).await?;
    let seed =
        status["seed_name"].as_str().ok_or((StatusCode::NOT_FOUND, "not done".to_owned()))?;
    let hash = status["bundle_hash"].as_str().unwrap_or_default().to_owned();
    let importer = w.importer.as_ref().ok_or_else(no_room)?;
    let v1 = importer.export(crate::room_id(seed), hash).await.map_err(unavailable)?;
    Ok(json_text(StatusCode::OK, v1))
}

async fn room_summary(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    UrlPath(id): UrlPath<String>,
) -> Reply {
    let status = read_status_for(&w, &who, &id).await?;
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
    who: Identity,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Reply {
    let status = read_status_for(&w, &who, &id).await?;
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
    let lost = access_lost(Arc::clone(&w), headers, id);
    crate::room_view::events(room, held, after, w.limits.view_history_events, lost)
        .await
        .map_err(|e| unavailable(format!("{e:?}")))
}

/// Whether the request's account can still read world `id`, through the same
/// [`read_status_for`] as the open. Fails closed: no session, no account, a store error or a
/// bad row all read as "no".
async fn still_allowed(w: &Worlds, headers: &HeaderMap, id: &str) -> bool {
    let Some(a) = &w.accounts else { return false };
    let Some(who) = a.identify(headers).await else { return false };
    read_status_for(w, &who, id).await.is_ok()
}

/// Completes the first time `check` says no, answers later than `every`, or does not answer in
/// `every`: a hung store ends the stream as a refusal does.
async fn until_lost<F, Fut>(every: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    loop {
        tokio::time::sleep(every).await;
        if !matches!(tokio::time::timeout(every, check()).await, Ok(true)) {
            return;
        }
    }
}

/// Completes when the request's account can no longer read world `id` (see [`still_allowed`]),
/// checked every `view_access_recheck_ms`. Any failure to check ends the stream.
async fn access_lost(w: Arc<Worlds>, headers: HeaderMap, id: String) {
    let every = Duration::from_millis(w.limits.view_access_recheck_ms.max(1));
    until_lost(every, || still_allowed(&w, &headers, &id)).await;
}

#[cfg(test)]
mod tests;
