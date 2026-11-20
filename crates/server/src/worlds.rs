//! Uploads of `.apworld` and player `.yaml` files, and seed generation from a chosen set of
//! them, over HTTP. Admin only: every route needs `Authorization: Bearer <token>`.
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

use crate::host::{RoomsFolder, Waker};
use crate::manifests::{self, Manifests};
use crate::rooms::RoomKey;
use crate::sandbox::{AP, Failure, JOB, Mount, Sandbox, run_job};
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
use std::time::Duration;
use tokio::sync::Semaphore;

/// The API's shared state.
pub(crate) struct Worlds {
    dir: PathBuf,
    /// SHA-256 of the admin token, or None: open mode, no auth (compared in constant time, see [`authorized`]).
    token: Option<[u8; 32]>,
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
    /// Uploaded world and player files: in the database.
    uploads: manifests::Db,
    /// How a manifest fill or check runs.
    jobs: JobRunner,
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
    /// Wrong tokens per client address (`admin_auth_fail_burst`).
    auth_fails: edge_compat::RateTable<IpAddr>,
    /// Behind a reverse proxy: the header carrying the client address (`--trusted-proxy-header`).
    /// Off by default, because any client could send it.
    proxy_header: Option<HeaderName>,
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

/// What stores a kind of upload: its folder and file extension (in a job folder, and under the
/// data folder of an older server), and which game names its record holds. Every kind is stored
/// alike, so a new kind is one entry here plus its route and a [`UploadKind`].
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
    const fn store(self) -> UploadKind {
        match self {
            Self::Apworld => UploadKind::Apworld,
            Self::Yaml => UploadKind::Yaml,
        }
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

type Reply = Result<Response, (StatusCode, String)>;

fn internal(e: impl core::fmt::Display) -> (StatusCode, String) {
    tracing::error!(error = %e, "worlds api");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error (see the server log)".to_owned())
}

/// A failed store call as a 500 that hides its detail (it is in the log).
fn store_error(e: log::StoreError) -> (StatusCode, String) {
    internal(format!("{e:?}"))
}

fn json_reply(status: StatusCode, v: &Value) -> Response {
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
    for sub in ["worlds", "tmp"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(io)?;
    }
    let mem = Arc::new(log::MemLogStore::new());
    Ok(Worlds {
        dir: dir.to_owned(),
        token: token.map(|t| Sha256::digest(t.as_bytes()).into()),
        sandbox: Sandbox::new(paths, limits, dir)?,
        limits,
        one_gen: Arc::new(Semaphore::new(limits.gen_concurrency.max(1))),
        queued: Arc::new(AtomicUsize::new(0)),
        checks: Semaphore::new(limits.manifest_check_concurrency.max(1)),
        // Until [`Worlds::router`] gives it the host's store.
        manifests: Manifests::new(manifests::Db::Mem(Arc::clone(&mem)), &limits),
        uploads: manifests::Db::Mem(mem),
        jobs: JobRunner::Child(crate::sandbox::exe_path()?),
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
    /// Count a generation against `gen_queue_max`, or `None` when that many are already queued
    /// or running.
    fn admit(&self) -> Option<JobSlot> {
        let before = self.queued.fetch_add(1, Ordering::AcqRel);
        let slot = JobSlot(Arc::clone(&self.queued));
        (before < self.limits.gen_queue_max).then_some(slot)
    }

    /// Run a data-only job (`generate` or `inspect`) on `input`; see [`JobRunner`].
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
                tokio::task::spawn_blocking(move || {
                    match job.as_str() {
                        "generate" => data_fill::job::generate(&input),
                        _ => data_fill::job::inspect(&input),
                    }
                    .map_err(Failure::Refused)
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

    /// The routes, rooted at `/api` (every path under it needs the token), on the host's store
    /// `db`, after the files an older server kept are moved into it. With `rooms` (the host's
    /// rooms folder and the public `host:port`), finished worlds are hosted as rooms.
    /// `proxy_header` names the header a trusted reverse proxy puts the client address in.
    pub(crate) async fn router(
        mut self,
        importer: Waker,
        rooms: Option<(RoomsFolder, String)>,
        proxy_header: Option<HeaderName>,
        db: manifests::Db,
    ) -> Router {
        self.uploads = db.clone();
        self.manifests = Manifests::new(db, &self.limits);
        self.import_files(self.uploads.is_durable()).await;
        self.import_jobs(self.uploads.is_durable()).await;
        self.rooms = rooms;
        self.importer = Some(importer);
        self.proxy_header = proxy_header;
        let state = Arc::new(self);
        tokio::spawn(sweep(Arc::downgrade(&state)));
        routes(state)
    }
}

impl Worlds {
    /// Upload `id`'s record, if stored.
    ///
    /// # Errors
    /// The store failing, or a record that is not JSON.
    async fn upload_meta(&self, kind: Kind, id: &str) -> Result<Option<Value>, String> {
        let Some(key) = manifests::key_of(id) else { return Ok(None) };
        let meta = self.uploads.upload_meta(kind.store(), &key).await;
        let meta = meta.map_err(|e| format!("the store: {e:?}"))?;
        meta.map(|m| serde_json::from_str(&m).map_err(|e| format!("upload {id}: bad record: {e}")))
            .transpose()
    }

    /// Upload `id`'s file, checked against its id.
    ///
    /// # Errors
    /// No such upload, or the store failing.
    async fn upload_file(&self, kind: Kind, id: &str) -> Result<Vec<u8>, String> {
        let key = manifests::key_of(id).ok_or(format!("bad id {id}"))?;
        let file = self.uploads.upload_bytes(kind.store(), &key).await;
        file.map_err(|e| format!("upload {id}: {e:?}"))?.ok_or(format!("no upload {id}"))
    }

    /// Move the uploads an older server kept as files (`apworlds/` and `yamls/`, in game
    /// folders or flat) into the store, each checked against its id. With `remove` (a durable
    /// store) each file and its record go once stored, and then every folder left empty; an
    /// in-memory store keeps them, to read again on the next start. A file that does not move
    /// stays, with a warning, and is tried again on the next start.
    async fn import_files(&self, remove: bool) {
        let (mut moved, mut kept) = (0_usize, 0_usize);
        for kind in [Kind::Apworld, Kind::Yaml] {
            let root = self.dir.join(kind.folder());
            let Ok(games) = std::fs::read_dir(&root) else { continue };
            let mut dirs = vec![root];
            dirs.extend(games.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()));
            for dir in &dirs {
                let Ok(entries) = std::fs::read_dir(dir) else { continue };
                let records = entries.filter_map(Result::ok).map(|e| e.path());
                for record in records.filter(|p| p.extension().is_some_and(|x| x == "json")) {
                    match self.import_file(kind, &record, remove).await {
                        Ok(()) => moved += 1,
                        Err(why) => {
                            kept += 1;
                            let file = record.display();
                            tracing::warn!(%file, "old upload not moved into the store: {why}");
                        }
                    }
                }
            }
            if remove {
                // Game folders first, then the kind's; only an empty folder is removed.
                for dir in dirs.iter().rev() {
                    drop(std::fs::remove_dir(dir));
                }
            }
        }
        if moved + kept > 0 {
            tracing::info!(moved, kept, removed = remove, "old upload files moved into the store");
        }
    }

    /// Write a status to the store; its `id` and `status` fields say which job and where it is.
    ///
    /// # Errors
    /// A status without them, or the store failing.
    async fn save_status(&self, status: &Value) -> Result<(), String> {
        let id = status["id"].as_str().filter(|i| valid_id(i)).ok_or("a status with a bad id")?;
        let state = status["status"].as_str().and_then(JobState::parse);
        let state = state.ok_or("a status with a bad state")?;
        let text = status.to_string();
        self.uploads.job_put(id, state, &text).await.map_err(|e| format!("the store: {e:?}"))
    }

    /// Fail every queued or running job not marked alive for `gen_stale_secs`: the process
    /// running it stopped (this one before a restart, or another instance). A job its process
    /// marks alive meanwhile is left alone.
    async fn fail_stale(&self) {
        let older = Duration::from_secs(self.limits.gen_stale_secs);
        let stale = match self.uploads.job_stale(older).await {
            Ok(stale) => stale,
            Err(e) => return tracing::warn!(error = ?e, "could not look for stopped generations"),
        };
        for (id, text) in stale {
            let mut status: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
            status["id"] = json!(id);
            status["status"] = json!(JobState::Failed.as_str());
            status["error"] =
                json!("the server running this generation stopped before it finished");
            match self.uploads.job_fail_stale(&id, &status.to_string(), older).await {
                Ok(true) => tracing::warn!(world = %id, "generation failed: its server stopped"),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(world = %id, error = ?e, "could not fail a stopped generation")
                }
            }
        }
    }

    /// Move the statuses an older server kept as `worlds/<id>/status.json` into the store, and
    /// clear what a stopped process left in this host's job folders. A status that was queued or
    /// running is failed: the process running it was this host's, and it stopped. With
    /// `remove` (a durable store) each moved `status.json` goes, then every job folder left
    /// empty; an in-memory store keeps them, to read again on the next start.
    async fn import_jobs(&self, remove: bool) {
        let Ok(entries) = std::fs::read_dir(self.dir.join("worlds")) else { return };
        let mut moved = 0_usize;
        for world in entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()) {
            // A job's copies of its inputs, left by a process that stopped mid-job.
            for sub in [Kind::Apworld.folder(), Kind::Yaml.folder()] {
                drop(std::fs::remove_dir_all(world.join(sub)));
            }
            let file = world.join("status.json");
            if let Some(mut status) = read_json(&file) {
                if matches!(status["status"].as_str(), Some("queued" | "running")) {
                    status["status"] = json!(JobState::Failed.as_str());
                    status["error"] = json!("the server restarted before this generation finished");
                }
                match self.save_status(&status).await {
                    Ok(()) => {
                        moved += 1;
                        if remove && let Err(e) = std::fs::remove_file(&file) {
                            tracing::warn!(file = %file.display(), error = %e, "old status left");
                        }
                    }
                    Err(why) => tracing::warn!(
                        file = %file.display(),
                        "old status not moved into the store: {why}"
                    ),
                }
            }
            drop(std::fs::remove_dir(&world));
        }
        if moved > 0 {
            tracing::info!(moved, removed = remove, "old generation statuses moved into the store");
        }
    }

    /// Move one upload an older server kept: `record` is its `<id>.json`, beside its file.
    async fn import_file(&self, kind: Kind, record: &Path, remove: bool) -> Result<(), String> {
        let id = record.file_stem().and_then(|s| s.to_str()).filter(|s| valid_id(s));
        let id = id.ok_or("not an upload record")?;
        let file = record.with_extension(kind.ext());
        let meta = read_json(record).ok_or("its record is not readable JSON")?;
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        if content_id(&bytes) != id {
            return Err("its file does not hash to its id".to_owned());
        }
        let key = manifests::key_of(id).ok_or("bad id")?;
        let game = game_folder(kind, &meta);
        let row = UploadRecord { kind: kind.store(), id: key, game, meta: meta.to_string() };
        self.uploads.upload_put(&row, &bytes).await.map_err(|e| format!("the store: {e:?}"))?;
        if remove {
            for path in [&file, record] {
                std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
        Ok(())
    }
}

/// Fail stopped generations every `gen_heartbeat_secs` (the first time at once), until the API
/// is gone.
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

/// Mark job `id` alive every `gen_heartbeat_secs`, until aborted when the job ends.
async fn heartbeat(w: Arc<Worlds>, id: String) {
    let every = Duration::from_secs(w.limits.gen_heartbeat_secs.max(1));
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = w.uploads.job_touch(&id).await {
            tracing::warn!(world = %id, error = ?e, "could not mark a generation alive");
        }
    }
}

fn routes(state: Arc<Worlds>) -> Router {
    let l = state.limits;
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
        .route("/api/worlds", post(create_world))
        .route("/api/worlds/{id}", get(world_status))
        .route("/api/worlds/{id}/room", get(room_summary))
        .route("/api/worlds/{id}/bundle", get(world_bundle))
        .route("/api/worlds/{id}/room/events", get(room_events))
        // Anything else under /api is the token's too: 401 without it, never the web UI's 404.
        .route("/api", any(StatusCode::NOT_FOUND))
        .route("/api/", any(StatusCode::NOT_FOUND))
        .route("/api/{*rest}", any(StatusCode::NOT_FOUND))
        .route_layer(axum::middleware::from_fn_with_state(Arc::clone(&state), limit_writes))
        .route_layer(axum::middleware::from_fn_with_state(Arc::clone(&state), auth))
        // After the gate, so it is open: the web UI asks whether to prompt for a token.
        .route("/api/auth", get(auth_mode))
        // The world requests are small JSON.
        .layer(capped(l.yaml_max_bytes))
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

/// A client address may upload and request generations `admin_write_burst` times in a burst,
/// then `admin_write_refill_milli_per_sec` per second; past it, 429 until its allowance refills.
/// Runs behind the token check, so only a token holder is counted.
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
    let raw =
        raw_param(query.unwrap_or(""), "name").ok_or("the name query parameter is required")?;
    let name = percent_decode(raw).ok_or("the name is not valid percent-encoded UTF-8")?;
    if name.is_empty() || name.len() > 255 || name == "." || name == ".." {
        return Err("the name must be 1 to 255 bytes".to_owned());
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err("the name must be a plain file name".to_owned());
    }
    Ok(name)
}

/// The raw (still percent-encoded) value of query parameter `key`.
fn raw_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|p| p.strip_prefix(key)?.strip_prefix('='))
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
fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
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

/// Check a manifest in a capped child process (the same engine that will fill it), then store it
/// in the database. The same content again returns the stored record without a check.
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
    let timeout = Duration::from_millis(w.limits.inspect_timeout_ms);
    let found = w.run_job("inspect", body, timeout).await.map_err(|e| match e {
        Failure::Refused(why) => (StatusCode::UNPROCESSABLE_ENTITY, why),
        Failure::Broken(why) => internal(why),
    })?;
    let found: Value = serde_json::from_slice(&found)
        .map_err(|e| internal(format!("the manifest check printed bad JSON: {e}")))?;
    let stored = w.manifests.put(&name, body, &found).await.map_err(store_error)?;
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
    if let Some(meta) = w.upload_meta(kind, &id).await.map_err(internal)? {
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
    w.uploads.upload_put(&row, body).await.map_err(store_error)?;
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
fn valid_module(m: &str) -> bool {
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
    let records = w.uploads.upload_list(kind.store()).await.map_err(store_error)?;
    let mut all: Vec<Value> = records.iter().filter_map(|r| serde_json::from_str(r).ok()).collect();
    let key = |v: &Value| (v["name"].as_str().unwrap_or("").to_owned(), v["id"].to_string());
    all.sort_by_key(key);
    Ok(json_reply(StatusCode::OK, &Value::Array(all)))
}

/// The ids in `body[field]`: each valid, stored, and listed once. A bad choice is a 400 with
/// its reason; the store failing is a 500.
async fn chosen(
    w: &Worlds,
    body: &Value,
    field: &str,
    kind: Kind,
) -> Result<Vec<Value>, (StatusCode, String)> {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let ids = body.get(field).and_then(Value::as_array);
    let ids = ids.ok_or_else(|| bad(format!("{field} must be a list")))?;
    let mut metas = Vec::with_capacity(ids.len());
    for raw in ids {
        let id = raw.as_str().filter(|i| valid_id(i));
        let id = id.ok_or_else(|| bad(format!("{field}: bad id {raw}")))?;
        if metas.iter().any(|m: &Value| m["id"] == id) {
            return Err(bad(format!("{field}: {id} is listed twice")));
        }
        let meta = w.upload_meta(kind, id).await.map_err(internal)?;
        metas.push(meta.ok_or_else(|| bad(format!("{field}: no upload {id}")))?);
    }
    Ok(metas)
}

/// Lay out a world folder: the chosen files, written from the store (apworlds named by
/// module, as Archipelago loads them).
async fn prepare(
    w: &Worlds,
    world: &Path,
    apworlds: &[Value],
    yamls: &[Value],
) -> Result<(), String> {
    let e = |e: std::io::Error| e.to_string();
    for sub in ["apworlds", "yamls"] {
        std::fs::create_dir_all(world.join(sub)).map_err(e)?;
    }
    for (kind, metas) in [(Kind::Apworld, apworlds), (Kind::Yaml, yamls)] {
        for m in metas {
            let id = m["id"].as_str().unwrap_or("");
            let stem = if kind == Kind::Apworld { m["module"].as_str().unwrap_or("") } else { id };
            let file = w.upload_file(kind, id).await?;
            let to = world.join(kind.folder()).join(format!("{stem}.{}", kind.ext()));
            std::fs::write(&to, file).map_err(e)?;
        }
    }
    Ok(())
}

async fn create_world(State(w): State<Arc<Worlds>>, body: Bytes) -> Reply {
    let Some(slot) = w.admit() else { return Ok(busy(&w, "too many generations are queued")) };
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let req: Value = serde_json::from_slice(&body).map_err(|e| bad(format!("bad JSON: {e}")))?;
    if req.get("manifest").is_some() {
        return create_data_only_world(&w, &req, slot).await;
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
    let status = json!({"id": id, "status": "queued", "apworlds": apworld_ids, "yamls": yaml_ids});
    queue(&w, Some(world), status, slot).await
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

/// Write the queued status and start the job: the one place every kind of world is queued.
/// `world` is a legacy job's folder, removed when the job ends.
async fn queue(w: &Arc<Worlds>, world: Option<PathBuf>, status: Value, slot: JobSlot) -> Reply {
    if let Err(e) = w.save_status(&status).await {
        if let Some(world) = &world {
            drop(std::fs::remove_dir_all(world));
        }
        return Err(internal(e));
    }
    let id = status["id"].clone();
    tracing::info!(world = %id, "generation queued");
    tokio::spawn(generate(Arc::clone(w), world, status, slot));
    Ok(json_reply(StatusCode::ACCEPTED, &json!({"id": id, "status": "queued"})))
}

/// `POST /api/worlds` with `{"manifest": <id>, "yamls": [<id>...]}`: a data-only world, filled by
/// the engine in a capped child process (no world code runs). The player files are JSON
/// documents with a `name`, a `game` (the manifest's) and the world's options, pool and start
/// items (`data_fill::Instance`).
async fn create_data_only_world(w: &Arc<Worlds>, req: &Value, slot: JobSlot) -> Reply {
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
    let status = json!({
        "id": id, "status": "queued", "engine": ENGINE_DATA_ONLY,
        "manifest": manifest_id, "yamls": yaml_ids,
    });
    queue(w, None, status, slot).await
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
async fn generate(w: Arc<Worlds>, world: Option<PathBuf>, status: Value, slot: JobSlot) {
    let id = status["id"].as_str().unwrap_or("").to_owned();
    let beat = tokio::spawn(heartbeat(Arc::clone(&w), id));
    run_generation(&w, world.as_deref(), status, slot).await;
    beat.abort();
    if let Some(world) = world
        && let Err(e) = std::fs::remove_dir_all(&world)
    {
        tracing::warn!(world = %world.display(), error = %e, "job folder not cleaned up");
    }
}

async fn run_generation(w: &Worlds, world: Option<&Path>, mut status: Value, _slot: JobSlot) {
    let Ok(_permit) = w.one_gen.acquire().await else { return };
    let id = status["id"].as_str().unwrap_or("").to_owned();
    status["status"] = json!("running");
    if let Err(e) = w.save_status(&status).await {
        tracing::error!(error = %e, "status write failed");
    }
    tracing::info!(world = %id, "generation running");
    let result = match world {
        _ if status["engine"] == ENGINE_DATA_ONLY => generate_data_only(w, &status).await,
        Some(world) => generate_sandboxed(w, world).await,
        None => Err("a legacy generation without its job folder".to_owned()),
    };
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
    if let Err(e) = w.save_status(&status).await {
        tracing::error!(error = %e, "status write failed");
    }
    // Logged after the status is stored: tests and operators wait on these lines.
    if done {
        tracing::info!(world = %id, "generation done");
    } else {
        tracing::warn!(world = %id, "generation failed");
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

/// What a fill child reads on stdin (`data_fill::job`): the manifest as it was uploaded, not
/// parsed here.
#[derive(serde::Serialize)]
struct FillReq<'a> {
    manifest: &'a serde_json::value::RawValue,
    seed_name: &'a str,
    players: Vec<PlayerReq>,
    rng_seed: u64,
    workers: usize,
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
    let id = status["id"].as_str().unwrap_or("");
    let manifest = w.manifests.bytes(status["manifest"].as_str().unwrap_or("")).await;
    let manifest = manifest.map_err(|e| format!("{e:?}"))?.ok_or("the manifest is gone")?;
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
    let players = yamls.len();
    // Building the request is blocking work.
    let input = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let text = String::from_utf8(manifest.to_vec()).map_err(|e| e.to_string())?;
        let raw = serde_json::value::RawValue::from_string(text).map_err(|e| e.to_string())?;
        let req = FillReq {
            manifest: &raw,
            seed_name: &seed_name,
            players: player_requests(&files)?,
            rng_seed,
            workers: config::FILL_SETUP_WORKERS,
        };
        serde_json::to_vec(&req).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))?;
    let timeout = Duration::from_millis(w.limits.fill_timeout_ms);
    let bundle = w.run_job("generate", &input, timeout).await.map_err(|e| e.to_string())?;
    let limits = w.limits;
    let loaded = tokio::task::spawn_blocking(move || parse_bundle(&bundle, &limits))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))?;
    publish_loaded(w, loaded.clone()).await?;
    let report = json!({"engine": ENGINE_DATA_ONLY, "rng_seed": rng_seed, "players": players});
    Ok((loaded, report))
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

fn checked_id(id: &str) -> Result<(), (StatusCode, String)> {
    if valid_id(id) { Ok(()) } else { Err((StatusCode::BAD_REQUEST, "bad id".to_owned())) }
}

async fn read_status(w: &Worlds, id: &str) -> Result<Value, (StatusCode, String)> {
    checked_id(id)?;
    let text = w.uploads.job_status(id).await.map_err(store_error)?;
    let text = text.ok_or((StatusCode::NOT_FOUND, "no such world".to_owned()))?;
    serde_json::from_str(&text).map_err(|e| internal(format!("world {id}: bad status: {e}")))
}

/// The status on disk, plus `room_key` and `address` once the host lists the world's room.
async fn world_status(State(w): State<Arc<Worlds>>, UrlPath(id): UrlPath<String>) -> Reply {
    let mut status = read_status(&w, &id).await?;
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
    let status = read_status(&w, &id).await?;
    let seed =
        status["seed_name"].as_str().ok_or((StatusCode::NOT_FOUND, "not done".to_owned()))?;
    let hash = status["bundle_hash"].as_str().unwrap_or_default().to_owned();
    let importer = w.importer.as_ref().ok_or_else(no_room)?;
    let v1 = importer.export(crate::room_id(seed), hash).await.map_err(unavailable)?;
    Ok(json_text(StatusCode::OK, v1))
}

async fn room_summary(State(w): State<Arc<Worlds>>, UrlPath(id): UrlPath<String>) -> Reply {
    let status = read_status(&w, &id).await?;
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
    let status = read_status(&w, &id).await?;
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
        let mut w = open(&root.join("data"), Some("token"), paths, limits).unwrap();
        // The unit tests are not the server's executable: the jobs run in this process.
        w.jobs = JobRunner::InProcess;
        w
    }

    /// Store `body` in `w`'s store as an earlier upload did, with record `meta` plus its id;
    /// returns the record.
    async fn stored(w: &Worlds, kind: Kind, body: &[u8], mut meta: Value) -> Value {
        let id = content_id(body);
        meta["id"] = json!(id);
        let key = manifests::key_of(&id).unwrap();
        let game = game_folder(kind, &meta);
        let row = UploadRecord { kind: kind.store(), id: key, game, meta: meta.to_string() };
        w.uploads.upload_put(&row, body).await.unwrap();
        meta
    }

    /// Write `body` under `data/<folder>` as an older server kept an upload, with record
    /// `meta` plus its id; returns the record.
    fn old_file(root: &Path, folder: &str, ext: &str, body: &[u8], mut meta: Value) -> Value {
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
        let limits = Limits { apworld_max_bytes: 8, yaml_max_bytes: 4, ..Limits::DESIGN };
        let w = worlds(&root, limits);
        for (kind, folder, cap) in [(Kind::Apworld, "apworlds", 8), (Kind::Yaml, "yamls", 4)] {
            let body = vec![b'x'; cap];
            let meta = stored(&w, kind, &body, json!({"name": "a"})).await;
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
        let meta = stored(&w, Kind::Yaml, b"name: a", json!({"name": "a.yaml"})).await;
        let r = list(&w, Kind::Yaml).await.unwrap();
        assert_eq!(json_of(r).await, json!([meta]));
        assert_eq!(json_of(list(&w, Kind::Apworld).await.unwrap()).await, json!([]));
    }

    /// The reason a choice of yamls is refused (always a 400).
    async fn refused(w: &Worlds, ids: Value) -> String {
        let e = chosen(w, &json!({ "yamls": ids }), "yamls", Kind::Yaml).await.unwrap_err();
        assert_eq!(e.0, StatusCode::BAD_REQUEST, "{}", e.1);
        e.1
    }

    #[tokio::test]
    async fn chosen_ids_must_each_be_stored_and_listed_once() {
        let root = temp("chosen");
        let w = worlds(&root, Limits::DESIGN);
        let a = stored(&w, Kind::Yaml, b"a", json!({"name": "a"})).await;
        let b = stored(&w, Kind::Yaml, b"b", json!({"name": "b"})).await;
        let both = json!({ "yamls": [a["id"], b["id"]] });
        assert_eq!(chosen(&w, &both, "yamls", Kind::Yaml).await.unwrap(), [a.clone(), b]);
        assert!(refused(&w, json!([a["id"], a["id"]])).await.contains("listed twice"));
        assert!(refused(&w, json!([content_id(b"c")])).await.contains("no upload"));
        assert!(refused(&w, json!(["../x"])).await.contains("bad id"));
        assert!(refused(&w, json!("x")).await.contains("must be a list"));
        // The same bytes stored as the other kind are not a choice of this one.
        stored(&w, Kind::Apworld, b"zip", json!({})).await;
        assert!(refused(&w, json!([content_id(b"zip")])).await.contains("no upload"));
        let none = chosen(&w, &json!({}), "apworlds", Kind::Apworld).await.unwrap_err().1;
        assert!(none.contains("apworlds must be a list"), "{none}");
    }

    /// The job folder gets the chosen files from the store; nothing is kept beside it.
    #[tokio::test]
    async fn a_world_folder_holds_the_chosen_files_apworlds_named_by_module() {
        let root = temp("prepare");
        let w = worlds(&root, Limits::DESIGN);
        let apworld = stored(&w, Kind::Apworld, b"zip", json!({"module": "clique"})).await;
        let yaml = stored(&w, Kind::Yaml, b"name: a", json!({})).await;
        let world = root.join("data/worlds/w");
        prepare(&w, &world, &[apworld], std::slice::from_ref(&yaml)).await.unwrap();
        for old in ["apworlds", "yamls"] {
            assert!(!root.join("data").join(old).exists(), "no {old} folder in the data folder");
        }
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
        let one = stored(&w, Kind::Apworld, b"1", json!({"module": "clique"})).await;
        let two = stored(&w, Kind::Apworld, b"2", json!({"module": "clique"})).await;
        let yaml = stored(&w, Kind::Yaml, b"y", json!({})).await;
        let req = json!({"apworlds": [one["id"], two["id"]], "yamls": [yaml["id"]]});
        let e = create_world(State(w), Bytes::from(req.to_string())).await.unwrap_err();
        assert_eq!(e, (StatusCode::BAD_REQUEST, "two apworlds have the same module name".into()));
        let worlds_dir = std::fs::read_dir(root.join("data/worlds")).unwrap();
        assert_eq!(worlds_dir.count(), 0, "no world folder was made");
    }

    #[tokio::test]
    async fn a_world_status_needs_a_valid_id_and_an_existing_world() {
        let root = temp("status");
        let w = worlds(&root, Limits::DESIGN);
        let bad = read_status(&w, "../../token").await.unwrap_err();
        assert_eq!(bad.0, StatusCode::BAD_REQUEST);
        let none = read_status(&w, &content_id(b"none")).await.unwrap_err();
        assert_eq!(none.0, StatusCode::NOT_FOUND);
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

    /// The in-memory store under `w`, to age a job as if its process stopped.
    fn mem_store(w: &Worlds) -> Arc<log::MemLogStore> {
        match &w.uploads {
            manifests::Db::Mem(m) => Arc::clone(m),
            manifests::Db::Pg(_) => unreachable!("these tests run in memory"),
        }
    }

    /// Statuses an older server kept as files move into the store; the ones it left queued or
    /// running are failed, as its process stopped. The job folders a stopped process left go
    /// too: its copies of the inputs, then each folder left empty (#119).
    #[tokio::test]
    async fn old_statuses_move_into_the_store_and_unfinished_ones_fail() {
        let root = temp("recover");
        let worlds_dir = root.join("data/worlds");
        let id = |c: char| c.to_string().repeat(64);
        for (c, state) in [('a', "queued"), ('b', "running"), ('c', "done"), ('d', "failed")] {
            let dir = worlds_dir.join(id(c));
            std::fs::create_dir_all(dir.join("apworlds")).unwrap();
            let status = json!({"id": id(c), "status": state, "bundle": "/b"});
            std::fs::write(dir.join("status.json"), status.to_string()).unwrap();
        }
        let w = worlds(&root, Limits::DESIGN);
        w.import_jobs(true).await;
        for c in ['a', 'b'] {
            let status = read_status(&w, &id(c)).await.unwrap();
            assert_eq!(status["status"], "failed", "{c}");
            assert!(status["error"].as_str().unwrap().contains("restarted"));
        }
        let done = read_status(&w, &id('c')).await.unwrap();
        assert_eq!(done, json!({"id": id('c'), "status": "done", "bundle": "/b"}));
        assert_eq!(read_status(&w, &id('d')).await.unwrap()["status"], "failed");
        let left = std::fs::read_dir(&worlds_dir).unwrap().count();
        assert_eq!(left, 0, "every status moved and every job folder cleared");
    }

    /// A queued or running job whose process stopped marking it alive is failed by the sweep;
    /// one still marked alive, and a finished one, are left alone (#119).
    #[tokio::test]
    async fn the_sweep_fails_only_jobs_whose_server_stopped() {
        let root = temp("sweep");
        let w = worlds(&root, Limits::DESIGN);
        let id = |c: char| c.to_string().repeat(64);
        for (c, state) in [('a', "running"), ('b', "running"), ('c', "done")] {
            w.save_status(&json!({"id": id(c), "status": state})).await.unwrap();
        }
        let stale = Duration::from_secs(Limits::DESIGN.gen_stale_secs);
        let store = mem_store(&w);
        store.age_job_for_tests(&id('a'), stale);
        store.age_job_for_tests(&id('c'), stale);
        w.fail_stale().await;
        let a = read_status(&w, &id('a')).await.unwrap();
        assert_eq!(a["status"], "failed");
        assert!(a["error"].as_str().unwrap().contains("stopped"), "{a}");
        assert_eq!(read_status(&w, &id('b')).await.unwrap()["status"], "running", "still alive");
        assert_eq!(read_status(&w, &id('c')).await.unwrap()["status"], "done");
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

    /// Uploads an older server kept as files, by game and flat, move into the store: found by
    /// id, listed, and their files readable. In memory the files stay, to move again on the next
    /// start (#119).
    #[tokio::test]
    async fn old_upload_files_by_game_or_flat_move_into_the_store() {
        let root = temp("by-game");
        let flat = old_file(&root, "yamls", "yaml", b"flat", json!({"name": "flat"}));
        let grouped = old_file(&root, "yamls/clique", "yaml", b"g1", json!({"name": "g1"}));
        let same_game = old_file(&root, "yamls/clique", "yaml", b"g2", json!({"name": "g2"}));
        let world = old_file(&root, "apworlds/clique", "apworld", b"zip", json!({"module": "m"}));
        let w = worlds(&root, Limits::DESIGN);
        w.import_files(false).await;
        let ids = json!({ "yamls": [flat["id"], grouped["id"], same_game["id"]] });
        let got = chosen(&w, &ids, "yamls", Kind::Yaml).await.unwrap();
        assert_eq!(got, [flat.clone(), grouped.clone(), same_game.clone()]);
        let listed = json_of(list(&w, Kind::Yaml).await.unwrap()).await;
        let mut want = vec![flat, grouped.clone(), same_game];
        want.sort_by_key(|v| v["name"].as_str().unwrap().to_owned());
        assert_eq!(listed, Value::Array(want));
        assert_eq!(
            w.upload_file(Kind::Apworld, world["id"].as_str().unwrap()).await.unwrap(),
            b"zip"
        );
        assert!(
            root.join("data/yamls/clique")
                .join(format!("{}.yaml", grouped["id"].as_str().unwrap()))
                .is_file()
        );
    }

    /// With a durable store each moved file is removed, then the folders left empty; a file
    /// whose bytes are not its id's stays where it was, and nothing of it is stored.
    #[tokio::test]
    async fn a_moved_old_file_is_removed_and_a_damaged_one_stays() {
        let root = temp("move");
        let good = old_file(&root, "yamls/clique", "yaml", b"good", json!({"name": "good"}));
        let bad = old_file(&root, "apworlds", "apworld", b"bad", json!({"module": "m"}));
        let bad_id = bad["id"].as_str().unwrap();
        std::fs::write(root.join(format!("data/apworlds/{bad_id}.apworld")), b"torn").unwrap();
        let w = worlds(&root, Limits::DESIGN);
        w.import_files(true).await;
        assert!(w.upload_meta(Kind::Yaml, good["id"].as_str().unwrap()).await.unwrap().is_some());
        assert!(!root.join("data/yamls").exists(), "moved, and the emptied folders removed");
        assert!(w.upload_meta(Kind::Apworld, bad_id).await.unwrap().is_none());
        assert!(root.join(format!("data/apworlds/{bad_id}.json")).is_file(), "kept to look at");
    }

    /// The test world with a game name and version, as an uploaded manifest.
    fn manifest(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let doc: Value =
            serde_json::from_str(include_str!("../../data-fill/tests/fixtures/room_world.json"))
                .unwrap();
        let mut m = doc["manifest"].clone();
        m["game"] = json!("Fixture Game");
        m["version"] = json!("1.2");
        edit(&mut m);
        m.to_string().into_bytes()
    }

    /// A manifest is checked by the engine in a child (here in process) and stored in the
    /// database by content hash, with the listing columns beside it.
    #[tokio::test]
    async fn a_manifest_is_checked_stored_in_the_database_and_listed_in_pages() {
        let root = temp("manifest");
        let w = worlds(&root, Limits { list_page_max: 1, ..Limits::DESIGN });
        let body = manifest(|_| {});
        let r = upload_manifest(&w, Some("name=world.json"), &body).await.unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        let meta = json_of(r).await;
        assert_eq!(meta["id"], content_id(&body));
        assert_eq!((&meta["game"], &meta["version"]), (&json!("Fixture Game"), &json!("1.2")));
        assert_eq!(
            (&meta["name"], &meta["items"], &meta["locations"]),
            (&json!("world.json"), &json!(4), &json!(6))
        );
        assert!(!root.join("data/manifests").exists(), "no file: it is in the store");
        let again = upload_manifest(&w, Some("name=other.json"), &body).await.unwrap();
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(json_of(again).await, meta, "the stored record, not a second copy");
        let other = manifest(|m| m["version"] = json!("2"));
        upload_manifest(&w, Some("name=v2.json"), &other).await.unwrap();
        let w = Arc::new(w);
        let first = json_of(
            list_manifests(State(Arc::clone(&w)), RawQuery(Some("limit=50".into()))).await.unwrap(),
        )
        .await;
        assert_eq!(first["items"].as_array().map(Vec::len), Some(1), "capped at list_page_max");
        let cursor = first["next"].as_str().unwrap();
        let second = json_of(
            list_manifests(State(Arc::clone(&w)), RawQuery(Some(format!("after={cursor}"))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(second["items"].as_array().map(Vec::len), Some(1));
        assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
        let bad = list_manifests(State(w), RawQuery(Some("after=nope".into()))).await.unwrap_err();
        assert_eq!(bad.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_manifest_that_is_not_json_has_no_game_or_does_not_compile_is_refused() {
        let root = temp("manifest-bad");
        let w = worlds(&root, Limits::DESIGN);
        let bad: [(&str, Vec<u8>); 7] = [
            ("game with a NUL", manifest(|m| m["game"] = json!("a\u{0}b"))),
            ("game past the cap", manifest(|m| m["game"] = json!("x".repeat(129)))),
            ("not json", b"{".to_vec()),
            ("no game", manifest(|m| drop(m.as_object_mut().unwrap().remove("game")))),
            ("empty game", manifest(|m| m["game"] = json!(" "))),
            ("no version", manifest(|m| drop(m.as_object_mut().unwrap().remove("version")))),
            ("no regions", manifest(|m| drop(m.as_object_mut().unwrap().remove("regions")))),
        ];
        for (why, body) in bad {
            let e = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap_err();
            assert_eq!(e.0, StatusCode::UNPROCESSABLE_ENTITY, "{why}: {}", e.1);
        }
        let none = w.manifests.list(None, None, None).await.unwrap();
        assert!(none.0.is_empty(), "nothing kept");
    }

    /// A stored manifest whose bytes were torn is not answered as stored: the same upload again
    /// checks and stores it, which heals it.
    #[tokio::test]
    async fn uploading_a_torn_manifest_again_heals_it() {
        let root = temp("manifest-heal");
        let store = Arc::new(log::MemLogStore::new());
        let mut w = worlds(&root, Limits::DESIGN);
        w.manifests = Manifests::new(manifests::Db::Mem(Arc::clone(&store)), &Limits::DESIGN);
        let body = manifest(|_| {});
        let first = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        let id = content_id(&body);
        store.tear_manifest_for_tests(&hex_key(&id));
        assert!(w.manifests.bytes(&id).await.is_err(), "the torn row is refused on read");
        let again = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
        assert_eq!(again.status(), StatusCode::CREATED, "stored again, not answered as stored");
        assert_eq!(w.manifests.bytes(&id).await.unwrap().as_deref(), Some(&body));
        let third = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
        assert_eq!(third.status(), StatusCode::OK);
    }

    fn hex_key(id: &str) -> [u8; 32] {
        let mut key = [0_u8; 32];
        for (k, pair) in key.iter_mut().zip(id.as_bytes().chunks(2)) {
            *k = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
        }
        key
    }

    #[tokio::test]
    async fn a_manifest_over_its_cap_is_refused_and_the_cap_itself_is_taken() {
        let root = temp("manifest-cap");
        let w = worlds(&root, Limits { manifest_max_bytes: 8, ..Limits::DESIGN });
        let stored = w
            .manifests
            .put("m.json", b"12345678", &json!({"game": "G", "version": "1"}))
            .await
            .unwrap();
        let r = upload_manifest(&w, Some("name=m.json"), b"12345678").await.unwrap();
        assert_eq!((r.status(), json_of(r).await), (StatusCode::OK, stored));
        let e = upload_manifest(&w, Some("name=m.json"), b"123456789").await.unwrap_err();
        assert_eq!(e.0, StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Checks are child processes: past `manifest_check_concurrency` the answer is "busy,
    /// retry later", never an unbounded spawn.
    #[tokio::test]
    async fn manifest_checks_past_their_concurrency_are_refused_as_busy() {
        let root = temp("manifest-busy");
        let w = worlds(&root, Limits { manifest_check_concurrency: 1, ..Limits::DESIGN });
        let held = w.checks.try_acquire().unwrap();
        let r = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(r.headers().contains_key(header::RETRY_AFTER));
        drop(held);
        let r = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
    }

    /// One client address may write only `admin_write_burst` times in a burst; reads are free.
    #[tokio::test]
    async fn an_address_that_uploads_past_its_burst_gets_429_and_reads_still_work() {
        use tower::ServiceExt;
        let root = temp("write-rate");
        let limits =
            Limits { admin_write_burst: 2, admin_write_refill_milli_per_sec: 1, ..Limits::DESIGN };
        let app = routes(Arc::new(worlds(&root, limits)));
        let call = |method: &str, path: &str| {
            let req = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, "Bearer token")
                .body(axum::body::Body::from("x"))
                .unwrap();
            app.clone().oneshot(req)
        };
        for _ in 0..2 {
            let r = call("PUT", "/api/manifests?name=m.json").await.unwrap();
            assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY, "not JSON, but allowed in");
        }
        let r = call("PUT", "/api/manifests?name=m.json").await.unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(r.headers().contains_key(header::RETRY_AFTER));
        assert_eq!(call("GET", "/api/manifests").await.unwrap().status(), StatusCode::OK);
    }

    /// A player file as the sidecar's inspect records it: one player with a game.
    async fn player_yaml(w: &Worlds, name: &str, game: &str, doc: &Value) -> Value {
        let mut doc = doc.clone();
        doc["name"] = json!(name);
        doc["game"] = json!(game);
        let meta = json!({"name": name, "players": [{"name": name, "game": game}]});
        stored(w, Kind::Yaml, doc.to_string().as_bytes(), meta).await
    }

    fn fixture_instances() -> Vec<Value> {
        let doc: Value =
            serde_json::from_str(include_str!("../../data-fill/tests/fixtures/room_world.json"))
                .unwrap();
        doc["instances"].as_array().unwrap().clone()
    }

    /// The wiring: an uploaded manifest and two player files, a queued world, the engine's fill
    /// in a child (here in process), the seed stored in the host and its room listed and loaded.
    /// Fails without `generate_data_only` (the world fails: it would run the legacy path).
    #[tokio::test]
    async fn a_manifest_world_is_generated_by_the_engine_and_hosted_as_a_room() {
        let root = temp("data-only");
        let rooms = root.join("rooms");
        std::fs::create_dir_all(&rooms).unwrap();
        let (_, mut host) =
            crate::app(&crate::Source::Rooms(rooms), None, None, Limits::DESIGN).await.unwrap();
        let (waker, folder, db) = (host.waker(), host.folder().unwrap(), host.manifest_db());
        tokio::spawn(async move { host.supervise().await });
        let mut w = worlds(&root, Limits::DESIGN);
        w.importer = Some(waker);
        w.rooms = Some((folder, "host:1".to_owned()));
        w.uploads = db.clone();
        w.manifests = Manifests::new(db, &Limits::DESIGN);
        let w = Arc::new(w);

        let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
        let manifest_id = json_of(up).await["id"].clone();
        let insts = fixture_instances();
        let mut yamls = Vec::new();
        for (k, n) in ["Ann", "Bo"].iter().enumerate() {
            yamls.push(player_yaml(&w, n, "Fixture Game", &insts[k]).await["id"].clone());
        }
        let req = json!({"manifest": manifest_id, "yamls": yamls});
        let queued =
            create_world(State(Arc::clone(&w)), Bytes::from(req.to_string())).await.unwrap();
        assert_eq!(queued.status(), StatusCode::ACCEPTED);
        let id = json_of(queued).await["id"].as_str().unwrap().to_owned();
        let mut status = read_status(&w, &id).await.unwrap();
        assert_eq!(status["engine"], ENGINE_DATA_ONLY);
        for _ in 0..600 {
            status = read_status(&w, &id).await.unwrap();
            if status["status"] != "queued" && status["status"] != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status["status"], "done", "{status}");
        assert_eq!(status["slots"], 2);
        assert_eq!(status["generation"]["engine"], ENGINE_DATA_ONLY);
        let (folder, key, address) = hosted(&w, &status).expect("the room is listed");
        assert_eq!(address, key.address("host:1"));
        let (room, _visit) = folder.enter(key).await.unwrap();
        let summary = room.summary().await.unwrap();
        assert_eq!(summary["slots"].as_array().map(Vec::len), Some(2), "{summary}");
        assert_eq!(w.queued.load(Ordering::Acquire), 0, "the job's slot is released");
        // A manifest world takes no legacy world files.
        let both = json!({"manifest": manifest_id, "yamls": yamls, "apworlds": ["x"]});
        let e = create_world(State(w), Bytes::from(both.to_string())).await.unwrap_err();
        assert_eq!(e.0, StatusCode::BAD_REQUEST);
    }

    /// The API is the boundary: a player file for another game is refused before anything runs.
    #[tokio::test]
    async fn a_player_file_for_another_game_than_the_manifests_is_refused() {
        let root = temp("game-mismatch");
        let w = Arc::new(worlds(&root, Limits::DESIGN));
        let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
        let manifest_id = json_of(up).await["id"].clone();
        let insts = fixture_instances();
        let ok = player_yaml(&w, "Ann", "Fixture Game", &insts[0]).await;
        let other = player_yaml(&w, "Bo", "Other Game", &insts[1]).await;
        let req = json!({"manifest": manifest_id, "yamls": [ok["id"], other["id"]]});
        let e = create_world(State(w), Bytes::from(req.to_string())).await.unwrap_err();
        assert_eq!(e.0, StatusCode::BAD_REQUEST);
        assert!(e.1.contains("Bo") && e.1.contains("Fixture Game"), "{}", e.1);
        let made = std::fs::read_dir(root.join("data/worlds")).unwrap().count();
        assert_eq!(made, 0, "no world folder was made");
    }

    /// Past `gen_queue_max` queued or running generations a request is refused as busy; the
    /// slot comes back when a job ends.
    #[tokio::test]
    async fn generations_past_the_queue_limit_are_refused_as_busy() {
        let root = temp("queue-full");
        let w = Arc::new(worlds(&root, Limits { gen_queue_max: 1, ..Limits::DESIGN }));
        let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
        let manifest_id = json_of(up).await["id"].clone();
        let insts = fixture_instances();
        let yaml = player_yaml(&w, "Ann", "Fixture Game", &insts[0]).await;
        let req = json!({"manifest": manifest_id, "yamls": [yaml["id"]]}).to_string();
        // The one generation permit is taken, so the first job waits queued.
        let held = w.one_gen.acquire().await.unwrap();
        let first = create_world(State(Arc::clone(&w)), Bytes::from(req.clone())).await.unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let second = create_world(State(Arc::clone(&w)), Bytes::from(req.clone())).await.unwrap();
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(second.headers().contains_key(header::RETRY_AFTER));
        drop(held);
        for _ in 0..200 {
            if w.queued.load(Ordering::Acquire) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let third = create_world(State(w), Bytes::from(req)).await.unwrap();
        assert_eq!(third.status(), StatusCode::ACCEPTED, "the finished job gave its slot back");
    }

    #[cfg(feature = "pg-tests")]
    mod pg {
        use super::*;

        async fn store() -> manifests::Db {
            let url = config::load_secrets().unwrap().database_url.expect(
                "pg-tests needs CONSTELLATION_DATABASE_URL (docs/TESTING.md rule 5: no silent skip)",
            );
            let db = log::testing::fresh_schema(&url).await.unwrap();
            let pg = log::PgLogStore::connect(&db, &Limits::DESIGN).await.unwrap();
            manifests::Db::Pg(Arc::new(pg))
        }

        /// On Postgres an old upload file is moved in and removed, a new upload is stored, and
        /// both are there for the next process: nothing of either is left in the data folder
        /// (#119).
        #[tokio::test]
        async fn uploads_live_in_postgres_and_old_files_move_in_once() {
            let root = temp("pg-uploads");
            let old = old_file(&root, "yamls/clique", "yaml", b"old", json!({"name": "old"}));
            let db = store().await;
            let mut w = worlds(&root, Limits::DESIGN);
            w.uploads = db.clone();
            w.import_files(w.uploads.is_durable()).await;
            assert!(!root.join("data/yamls").exists(), "moved in and removed");
            let new = stored(&w, Kind::Apworld, b"zip", json!({"module": "m"})).await;
            drop(w);

            let mut w = worlds(&root, Limits::DESIGN);
            w.uploads = db;
            let ids = json!({ "yamls": [old["id"]] });
            assert_eq!(
                chosen(&w, &ids, "yamls", Kind::Yaml).await.unwrap(),
                std::slice::from_ref(&old)
            );
            assert_eq!(json_of(list(&w, Kind::Yaml).await.unwrap()).await, json!([old]));
            let file = w.upload_file(Kind::Apworld, new["id"].as_str().unwrap()).await.unwrap();
            assert_eq!(file, b"zip");
            // The same file uploaded again is answered from its record, without the sandbox.
            let again = upload(&w, Kind::Apworld, Some("name=x.apworld"), b"zip").await.unwrap();
            assert_eq!((again.status(), json_of(again).await), (StatusCode::OK, new));
        }

        /// A status written by one process is read by the next on the same database, and a
        /// job another process left running is failed by this one's sweep once it is stale,
        /// while a job still marked alive is not (#119).
        #[tokio::test]
        async fn statuses_live_in_postgres_and_a_stopped_servers_job_is_failed() {
            let root = temp("pg-jobs");
            let db = store().await;
            let id = |c: char| c.to_string().repeat(64);
            let mut first = worlds(&root, Limits::DESIGN);
            first.uploads = db.clone();
            first.save_status(&json!({"id": id('a'), "status": "done", "slots": 2})).await.unwrap();
            first.save_status(&json!({"id": id('b'), "status": "running"})).await.unwrap();
            drop(first);

            let mut w = worlds(&root, Limits::DESIGN);
            w.uploads = db.clone();
            let done = read_status(&w, &id('a')).await.unwrap();
            assert_eq!(done, json!({"id": id('a'), "status": "done", "slots": 2}));
            w.fail_stale().await;
            let alive = read_status(&w, &id('b')).await.unwrap();
            assert_eq!(alive["status"], "running", "marked alive just now");
            let mut later = worlds(&root, Limits { gen_stale_secs: 0, ..Limits::DESIGN });
            later.uploads = db;
            later.fail_stale().await;
            let failed = read_status(&later, &id('b')).await.unwrap();
            assert_eq!(failed["status"], "failed", "{failed}");
        }
    }
}
