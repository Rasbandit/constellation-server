//! The composition root (D-99: one process). Loads one seed bundle (`--bundle`) or every
//! bundle in a folder (`--rooms`, watched for new ones while the server runs: [`host`]),
//! starts a room for each on `room-core` and the log (Postgres when
//! `CONSTELLATION_DATABASE_URL` is set, D-98, else in memory), and serves the
//! Archipelago-compatible edge over HTTP: a WebSocket at `/{room_key}` (yawc, D-97) and `GET /healthz`
//! (axum). Each room has a room key ([`rooms::RoomKey`]).
//!
//! With Postgres every room survives a restart: the server claims each room (fencing any older
//! process), then rebuilds it from the stored snapshot and log tail through `room_core::fold`
//! and restores the stored applied cursors, before it accepts a connection.
//!
//! With `--data-dir` it also serves the admin API for uploads and seed generation
//! ([`worlds`], run in [`sandbox`]).

mod room_view;
mod sandbox;
mod web;
mod worlds;

pub mod host;
pub mod rooms;

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use config::{Generation, Limits, Secrets};
use edge_compat::CompatRoom;
use host::{Host, Skip};
use log::{LogStore, OpenError, PgLogStore};
use proto::{Epoch, RoomId, Seq};
use room_core::RoomState;
use rooms::{Room, RoomKey, Rooms, read};
use runtime::{Mailbox, RoomHandle, Stopped};
use seed_bundle::Seed;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::Semaphore;
use tokio::task::{JoinError, JoinHandle};
use yawc::{DeflatePool, IncomingUpgrade, Options};

/// Where the rooms come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// One seed bundle file: one room.
    Bundle(PathBuf),
    /// A folder: one room per `*.json` seed bundle in it, including bundles added while the
    /// server runs.
    Rooms(PathBuf),
}

/// Command line settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The seed bundle file or the rooms folder.
    pub source: Source,
    /// Port; 0 picks a free one (the bound address is logged).
    pub port: u16,
    /// Address to bind.
    pub host: IpAddr,
    /// Folder for uploads and generated seeds; turns on the admin API (needs the admin token).
    pub data_dir: Option<PathBuf>,
    /// Address of the admin API's own listener (default loopback: it runs uploaded code, so it
    /// is never on the room's port; reach it through SSH or a TLS proxy).
    pub admin_host: IpAddr,
    /// Port of the admin API; 0 picks a free one (logged as `admin api addr=`).
    pub admin_port: u16,
    /// The host name clients reach this server by; a room's join address is
    /// `{public_host}:{port}/{room_key}` (default `localhost`).
    pub public_host: String,
    /// The built web UI, served on the room port (`--web-dir`); none: no UI.
    pub web_dir: Option<PathBuf>,
}

/// How to run the binary.
pub const USAGE: &str = "usage: constellation-server (--bundle <file> | --rooms <folder>) \
     --port <port> [--host <ip>] [--public-host <name>] \
     [--web-dir <path>] [--data-dir <path> [--admin-host <ip>] [--admin-port <port>]] \
     (room password: CONSTELLATION_ROOM_PASSWORD or CONSTELLATION_ROOM_PASSWORD_FILE; \
     durable log: CONSTELLATION_DATABASE_URL or CONSTELLATION_DATABASE_URL_FILE; --data-dir \
     needs CONSTELLATION_ADMIN_TOKEN or CONSTELLATION_ADMIN_TOKEN_FILE, CONSTELLATION_AP_DIR, \
     CONSTELLATION_AP_PYTHON and CONSTELLATION_AP_GEN_SCRIPT)";

impl Args {
    /// Parse the arguments after the program name.
    ///
    /// # Errors
    /// A message naming the bad or missing argument.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let (mut source, mut port, mut host, mut data_dir) = (None, None, None, None);
        let (mut admin_host, mut admin_port, mut public_host) = (None, None, None);
        let mut web_dir = None;
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                // As with every flag, the last one given wins (a test appends `--bundle`).
                "--bundle" | "--rooms" => {
                    let path = PathBuf::from(value()?);
                    source = Some(if flag == "--bundle" {
                        Source::Bundle(path)
                    } else {
                        Source::Rooms(path)
                    });
                }
                "--port" => port = Some(value()?.parse().map_err(|_| "--port needs a number")?),
                "--host" => host = Some(value()?.parse().map_err(|_| "--host needs an IP")?),
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
                "--public-host" => public_host = Some(value()?),
                "--web-dir" => web_dir = Some(PathBuf::from(value()?)),
                "--admin-host" => {
                    admin_host = Some(value()?.parse().map_err(|_| "--admin-host needs an IP")?);
                }
                "--admin-port" => {
                    admin_port = Some(value()?.parse().map_err(|_| "--admin-port needs a number")?);
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Self {
            source: source.ok_or("--bundle or --rooms is required")?,
            port: port.ok_or("--port is required")?,
            host: host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            data_dir,
            admin_host: admin_host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            admin_port: admin_port.unwrap_or(0),
            public_host: public_host.unwrap_or_else(|| "localhost".to_owned()),
            web_dir,
        })
    }
}

/// A validated seed (v2, `docs/seed-bundle-v2.md`) and the hash of the v1 file it came from
/// ([`bundle_hash`]).
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The seed; shared by the room state and the edge.
    pub bundle: Arc<Seed>,
    /// SHA-256 of the canonical v1 JSON it was made from: the room's identity in Postgres.
    pub hash: Vec<u8>,
    /// Package files an import made, for the host to put in its store; empty for a seed read
    /// from the store.
    pub packages: Vec<Vec<u8>>,
}

/// Import a v1 bundle file (`--bundle`, the `--rooms` folder), refusing files over
/// `limits.bundle_max_bytes`. Seeds and packages then live in the store, never in files.
///
/// # Errors
/// A message for an unreadable, oversized or invalid file.
pub fn load_bundle(path: &std::path::Path, limits: &Limits) -> Result<Loaded, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    let cap = limits.bundle_max_bytes;
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
        return Err(format!("{}: larger than {cap} bytes", path.display()));
    }
    parse_bundle(&bytes, limits).map_err(|e| format!("{}: {e}", path.display()))
}

/// Import a v1 bundle's bytes: validate and encode them to v2 (packages shared in-process).
///
/// # Errors
/// When it is not a valid v1 bundle (a v2 seed file is refused: seeds come from the store).
pub fn parse_bundle(file: &[u8], limits: &Limits) -> Result<Loaded, String> {
    if seed_bundle::v2::is_v2(file) {
        return Err("a v2 seed file is not imported from disk; seeds live in the store".into());
    }
    let (seed, packages) = seed_bundle::import_v1(file, limits.bundle_max_bytes)?;
    Ok(Loaded { hash: seed.hash().to_vec(), bundle: Arc::new(seed), packages })
}

/// A new room's state for a loaded bundle: the one place a room is built from a bundle.
///
/// # Errors
/// When the bundle does not make a valid room.
pub fn fresh_state(loaded: &Loaded) -> Result<RoomState, String> {
    seed_bundle::room_state(&loaded.bundle).map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
struct AppState {
    /// Changed only by the [`Host`]; a request holds the lock for one lookup.
    rooms: Arc<RwLock<Rooms>>,
    /// Asks the host to load a cold room.
    wake: host::Waker,
    /// `--bundle`: the one room takes every path (see [`route`]).
    any_path: bool,
    options: Options,
    /// One permit per open WebSocket (`Limits::max_connections`).
    slots: Arc<Semaphore>,
    /// The web UI for every request that is not a WebSocket upgrade; none: only rooms.
    web: Option<Arc<web::WebDir>>,
}

/// The room's id: the first 8 bytes of SHA-256 of the seed name, so a restart on the same
/// bundle finds the same room.
#[must_use]
pub fn room_id(seed_name: &str) -> RoomId {
    let h = Sha256::digest(seed_name.as_bytes());
    let mut first = [0_u8; 8];
    first.copy_from_slice(h.get(..8).unwrap_or(&[0; 8]));
    RoomId(u64::from_be_bytes(first))
}

/// Lowercase hex of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut h, x| {
        // Writing to a String cannot fail.
        let _ = write!(h, "{x:02x}");
        h
    })
}

/// The admin API for `--data-dir`, or why it cannot start: it needs the admin token and the
/// generation paths. Checked before the port is bound or the room claimed.
fn worlds_api(
    data_dir: Option<&std::path::Path>,
    token: Option<&str>,
    generation: Option<Generation>,
    limits: Limits,
) -> Result<Option<worlds::Worlds>, String> {
    let Some(dir) = data_dir else { return Ok(None) };
    let token = token.ok_or(
        "--data-dir serves an admin API that runs uploaded code; set \
         CONSTELLATION_ADMIN_TOKEN or CONSTELLATION_ADMIN_TOKEN_FILE",
    )?;
    let generation = generation.ok_or(
        "--data-dir needs CONSTELLATION_AP_DIR, CONSTELLATION_AP_PYTHON and \
         CONSTELLATION_AP_GEN_SCRIPT",
    )?;
    worlds::open(dir, token, generation, limits).map(Some)
}

pub use seed_bundle::bundle_hash;

/// `/healthz`: 200 while every room task runs, 503 once one stopped.
const fn health(running: bool) -> (StatusCode, &'static str) {
    if running { (StatusCode::OK, "ok") } else { (StatusCode::SERVICE_UNAVAILABLE, "room stopped") }
}

async fn healthz(State(s): State<AppState>) -> (StatusCode, &'static str) {
    health(read(&s.rooms).all_running())
}

/// The build's version: the workspace version, plus `+<short sha>` when `CONSTELLATION_BUILD_SHA`
/// was set at compile time. A release build leaves it unset and shows the bare version.
fn version_string(version: &str, sha: Option<&str>) -> String {
    match sha.filter(|s| !s.is_empty()) {
        Some(sha) => format!("{version}+{}", sha.get(..7).unwrap_or(sha)),
        None => version.to_owned(),
    }
}

/// `GET /version`: `{"version": "<version>[+<short sha>]"}`. The web UI compares it with its own.
async fn version() -> impl IntoResponse {
    let v = version_string(env!("CARGO_PKG_VERSION"), option_env!("CONSTELLATION_BUILD_SHA"));
    ([(header::CONTENT_TYPE, "application/json")], format!("{{\"version\":\"{v}\"}}"))
}

/// Rebuild the room from `store` (snapshot, then the tail through `fold`, then the stored
/// cursors) and start its task on `mailbox`. `epoch` must be the claim just made.
async fn start_room<S: LogStore>(
    mailbox: Mailbox,
    room: RoomId,
    epoch: Epoch,
    fresh: RoomState,
    store: Arc<S>,
    limits: &Limits,
) -> Result<JoinHandle<Stopped>, String> {
    let (state, from) = match store.snapshot_get(room).await.map_err(|e| format!("{e:?}"))? {
        Some(snap) => match RoomState::from_snapshot(&fresh, &snap.data) {
            // A snapshot of an older shape is not corrupt: the log alone rebuilds the room.
            Err(room_core::CoreError::BadSnapshot(room_core::UNKNOWN_SNAPSHOT)) => {
                tracing::warn!(
                    room = room.0,
                    at = snap.seq.0,
                    "old snapshot shape; replaying the log"
                );
                (fresh, Seq(0))
            }
            Err(e) => return Err(format!("stored snapshot at {}: {e}", snap.seq.0)),
            Ok(state) => {
                state.validate(snap.seq).map_err(|e| e.to_string())?;
                (state, snap.seq)
            }
        },
        None => (fresh, Seq(0)),
    };
    let tail = store.read_tail(room, from, usize::MAX).await.map_err(|e| format!("{e:?}"))?;
    let events = tail.len();
    let mut state = room_core::fold(state, limits, tail).map_err(|e| e.to_string())?;
    for c in store.read_cursors(room).await.map_err(|e| format!("{e:?}"))? {
        state.restore_applied(c.slot, c.applied).map_err(|e| e.to_string())?;
    }
    tracing::info!(room = room.0, epoch = epoch.0, events, head = state.durable().0, "room loaded");
    Ok(mailbox.spawn(room, epoch, state, store, *limits))
}

/// A room built from its bundle, not yet claimed or started, and without its key.
struct Prepared {
    id: RoomId,
    fresh: RoomState,
    room: Room,
    mailbox: Mailbox,
}

fn prepare(loaded: Loaded, password: Option<&String>, limits: Limits) -> Result<Prepared, String> {
    let fresh =
        fresh_state(&loaded).map_err(|e| format!("seed {:?}: {e}", loaded.bundle.seed_name()))?;
    let Loaded { bundle, .. } = loaded;
    let id = room_id(bundle.seed_name());
    let (live, mailbox) = RoomHandle::mailbox(&limits);
    let seed = bundle.seed_name().to_owned();
    let compat = Arc::new(CompatRoom::new(bundle, live.clone(), password.cloned(), limits)?);
    Ok(Prepared { id, fresh, room: Room { seed, compat, live }, mailbox })
}

/// List the rooms of `source` and build the router.
///
/// Also returns the [`Host`], which loads rooms on their first connect, unloads idle ones,
/// ends abandoned ones, watches the rooms folder, and says when the process must end
/// ([`Host::supervise`]). With `database_url` the rooms and their logs are in Postgres: every
/// room stored there and not finished is listed, and keeps its stored room key. Without, they
/// are in memory, keys are drawn fresh, and a restart loses the rooms. `password` overrides
/// every room's. A bundle is validated before its room is claimed, so a bad bundle never
/// fences a live room.
///
/// With `--bundle` the room is loaded now and every failure is an error. With `--rooms` the
/// folder must exist, but it may be empty, and a bundle that does not make a room (invalid, a
/// stored room made from another bundle or finished, past `max_rooms`) is skipped with a
/// warning; rooms stay cold until their first connect.
///
/// # Errors
/// A message when the bundle does not make a valid room, the rooms folder cannot be read, the
/// database fails, or a stored room was made from a different bundle or has finished.
pub async fn app(
    source: &Source,
    password: Option<String>,
    database_url: Option<&str>,
    limits: Limits,
) -> Result<(Router, Host), String> {
    app_with_web(source, password, database_url, limits, None).await
}

/// [`app`], also serving the web UI built in `web_dir` on every path that is not a room's
/// WebSocket upgrade.
///
/// # Errors
/// As [`app`], and when `web_dir` is not a folder holding `index.html`.
pub async fn app_with_web(
    source: &Source,
    password: Option<String>,
    database_url: Option<&str>,
    limits: Limits,
    web_dir: Option<&std::path::Path>,
) -> Result<(Router, Host), String> {
    let web = web_dir.map(web::WebDir::open).transpose()?.map(Arc::new);
    let mut host = match source {
        Source::Bundle(path) => {
            let loaded = load_bundle(path, &limits)?;
            let mut host = Host::new(database_url, password, limits, None).await?;
            let key = host.import(loaded).await?;
            host.wake(&key).await?;
            host
        }
        Source::Rooms(dir) => {
            host::check_folder(dir)?;
            let mut host = Host::new(database_url, password, limits, Some(dir.clone())).await?;
            host.list_stored().await?;
            host
        }
    };
    host.scan().await;
    let options = edge_compat::ws_options(&limits, DeflatePool::new(limits.ws_deflate_pool));
    let state = AppState {
        rooms: host.rooms(),
        wake: host.waker(),
        any_path: matches!(source, Source::Bundle(_)),
        options,
        slots: Arc::new(Semaphore::new(limits.max_connections)),
        web,
    };
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
        // A WebSocket upgrade on any other path is a room's (`/{room_key}`; `route` decides);
        // a plain GET is the web UI's.
        .fallback(get(upgrade_or_web))
        .with_state(state);
    Ok((router, host))
}

/// Record the room in Postgres and return its key and how long it has been idle: the stored
/// key, or a new one from `draw` for a new room (drawn again, up to `room_key_draws` times,
/// when it collides with another room's).
async fn open_with_key(
    store: &PgLogStore,
    id: RoomId,
    loaded: &Loaded,
    limits: Limits,
    mut draw: impl FnMut() -> Result<RoomKey, String>,
) -> Result<(RoomKey, u64), Skip> {
    let mut last = Skip::Later("no room key drawn".to_owned());
    // The stored hash is of the v1 form, so the format recorded with it is v1's.
    let (seed, format) = (loaded.bundle.seed_name(), seed_bundle::FORMAT);
    for _ in 0..limits.room_key_draws {
        let new_key = draw().map_err(Skip::Later)?;
        let world = log::World::Seed(loaded.bundle.bytes());
        match store.open_room(id, seed, format, &loaded.hash, new_key.as_str(), world).await {
            Ok(o) => {
                let key = RoomKey::parse(&o.key).map_err(|e| Skip::Final(format!("stored {e}")))?;
                return Ok((key, o.idle_secs));
            }
            Err(OpenError::Finished { reason }) => {
                return Err(Skip::Final(format!(
                    "room {seed:?} has finished ({reason}); it is not served again"
                )));
            }
            Err(OpenError::OtherBundle) => {
                return Err(Skip::Final(format!(
                    "the database holds room {seed:?} made from a different bundle with this seed \
                     name; refusing to start"
                )));
            }
            Err(OpenError::Store(e)) => last = Skip::Later(format!("postgres: {e:?}")),
        }
    }
    Err(last)
}

/// Rebuild and start one prepared room, and log its key.
async fn start<S: LogStore>(
    p: Prepared,
    key: RoomKey,
    epoch: Epoch,
    store: Arc<S>,
    limits: &Limits,
) -> Result<JoinHandle<Stopped>, String> {
    let handle = start_room(p.mailbox, p.id, epoch, p.fresh, store, limits).await?;
    tracing::info!(%key, seed = p.room.seed, "room in memory");
    Ok(handle)
}

/// The room a connection to this path joins (`/{room_key}`, doc 01 section 3.8.1).
///
/// With `--bundle` (`any_path`) the one room takes every connection, whatever the path, so old
/// addresses keep working. With `--rooms` the path must be the key of a listed room, however
/// many there are: the count changes at runtime, so an old link to a room that ended must never
/// land in another one.
fn route(rooms: &Rooms, path: &str, any_path: bool) -> Result<Route, String> {
    let only = if any_path { rooms.only_with_key() } else { None };
    let key = match only {
        Some((key, _)) => key.clone(),
        None => match RoomKey::from_path(path) {
            Some(key) => key,
            None => {
                return Err(format!(
                    "this server runs {} rooms; connect through <this host>/{{room_key}}",
                    rooms.len()
                ));
            }
        },
    };
    listed(rooms, key)
}

/// The listed room `key`: ready (counted as a visit) when in memory, else cold.
fn listed(rooms: &Rooms, key: RoomKey) -> Result<Route, String> {
    let Some(listing) = rooms.get(&key) else {
        return Err(format!("no room with key {key} on this server"));
    };
    Ok(match &listing.room {
        // Counted while the lookup holds the lock, so the host never unloads a room a
        // connection is entering.
        Some(room) => Route::Ready(key, Arc::clone(&room.compat), listing.activity.enter()),
        None => Route::Cold(key),
    })
}

/// Where a connection goes.
enum Route {
    /// A room in memory; the visit counts the connection.
    Ready(RoomKey, Arc<CompatRoom>, rooms::Visit),
    /// A listed room that is not in memory: wake it, then route again.
    Cold(RoomKey),
}

/// Why a connection or a room view got no room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoRoom {
    /// No listed room for this address: the text is safe to send to the client.
    Unknown(String),
    /// The listed room did not load: the text is for the log only, as it can hold database
    /// or store errors.
    Failed(String),
}

impl core::fmt::Display for NoRoom {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unknown(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

/// What a client is told when its room did not load; the detail goes to the log.
const NOT_LOADED: &str = "the room could not be loaded; try again";

/// The answer to a connection that got no room. A room that did not load is the server's
/// fault and may work on a retry, so it is a 503 with a generic body, never the error text.
fn refused(path: &str, no: NoRoom) -> Response {
    match no {
        NoRoom::Unknown(why) => {
            tracing::info!(path, "{why}");
            (StatusCode::NOT_FOUND, why).into_response()
        }
        NoRoom::Failed(why) => {
            tracing::warn!(path, %why, "room did not load");
            (StatusCode::SERVICE_UNAVAILABLE, NOT_LOADED).into_response()
        }
    }
}

/// The room a connection to this path joins, woken first when it is cold.
async fn find(
    s: &AppState,
    path: &str,
) -> Result<(RoomKey, Arc<CompatRoom>, rooms::Visit), NoRoom> {
    let found = route(&read(&s.rooms), path, s.any_path).map_err(NoRoom::Unknown)?;
    let key = match found {
        Route::Ready(key, room, visit) => return Ok((key, room, visit)),
        Route::Cold(key) => key,
    };
    let (room, visit) = enter(&s.rooms, &s.wake, key.clone()).await?;
    Ok((key, room, visit))
}

/// The listed room `key`, woken first when it is cold, with a visit that keeps it in memory
/// while held (connections, and the admin API's room view).
pub(crate) async fn enter(
    rooms: &RwLock<Rooms>,
    wake: &host::Waker,
    key: RoomKey,
) -> Result<(Arc<CompatRoom>, rooms::Visit), NoRoom> {
    // Twice at most: a room can go cold again between its wake and the second lookup only if
    // it idled for the whole idle time in between.
    for _ in 0..2 {
        let found = listed(&read(rooms), key.clone()).map_err(NoRoom::Unknown)?;
        match found {
            Route::Ready(_, room, visit) => return Ok((room, visit)),
            Route::Cold(key) => wake.wake(key).await.map_err(NoRoom::Failed)?,
        }
    }
    Err(NoRoom::Failed(format!("room {key} went cold again right after it loaded")))
}

async fn upgrade_or_web(
    State(s): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    ws: Result<IncomingUpgrade, StatusCode>,
) -> Response {
    let wants_ws = headers
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if let (false, Some(web)) = (wants_ws, &s.web) {
        return web.serve(&method, uri.path(), &headers).await;
    }
    match ws {
        Ok(ws) => upgrade(State(s), peer, uri, ws).await,
        Err(status) => status.into_response(),
    }
}

async fn upgrade(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    uri: axum::http::Uri,
    ws: IncomingUpgrade,
) -> Response {
    let Ok(permit) = Arc::clone(&s.slots).try_acquire_owned() else {
        tracing::warn!("connection cap reached");
        return (StatusCode::SERVICE_UNAVAILABLE, "server full").into_response();
    };
    let path = uri.path();
    let (key, room, visit) = match find(&s, path).await {
        Ok(found) => found,
        Err(no) => return refused(path, no),
    };
    let (response, fut) = match ws.upgrade(s.options) {
        Ok(pair) => pair,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let deflate = response.headers().contains_key(header::SEC_WEBSOCKET_EXTENSIONS);
    tracing::info!(%key, deflate, "websocket accepted");
    tokio::spawn(async move {
        match fut.await {
            Ok(socket) => edge_compat::serve(socket, room, peer.ip()).await,
            Err(e) => tracing::debug!(error = %e, "upgrade did not complete"),
        }
        drop(permit);
        drop(visit);
        tracing::info!("connection closed");
    });
    response.into_response()
}

/// Structured logs to stderr at info (colour only on a terminal). The level is fixed until
/// `config` owns a log setting (doc 05 6.2 rule 5: no other crate reads the environment).
pub fn init_logging() {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_max_level(tracing::Level::INFO)
        .init();
}

/// Exit status when a newer owner fenced this process.
///
/// A supervisor must not restart it
/// (systemd: `RestartPreventExitStatus=3`), or two instances on one database would fence each
/// other in a loop. Every other failure exits 1 and should be restarted.
pub const EXIT_FENCED: u8 = 3;

/// Why [`run`] ended with an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// A newer owner holds the room ([`EXIT_FENCED`]).
    Fenced {
        /// The store's current epoch.
        current: Epoch,
    },
    /// Anything else (exit 1): bad input, the socket, the database, a room that stopped.
    Failed(String),
}

impl RunError {
    /// The process exit status for this error.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Fenced { .. } => EXIT_FENCED,
            Self::Failed(_) => 1,
        }
    }
}

impl From<String> for RunError {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

impl core::fmt::Display for RunError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fenced { current } => write!(
                f,
                "room fenced by a newer owner current={}; exiting with {EXIT_FENCED}, do not restart",
                current.0
            ),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// What the room task's end means for the process.
fn room_ended(r: Result<Stopped, JoinError>) -> RunError {
    match r {
        Ok(Stopped::Fenced { current }) => RunError::Fenced { current },
        Ok(Stopped::Failed) => RunError::Failed(
            "the room stopped (see the log above); exiting so it restarts from the log".to_owned(),
        ),
        Ok(Stopped::Closed) => RunError::Failed("the room closed unexpectedly".to_owned()),
        Err(e) => RunError::Failed(format!("the room task panicked or was cancelled: {e}")),
    }
}

/// Bind, start the rooms, log the address, and serve until SIGTERM or Ctrl-C, or until a
/// room's end must end the process ([`Host::supervise`]).
///
/// The signal handlers are installed first, so a stop at any point is clean: before the
/// server listens it abandons startup and returns `Ok`. Nothing is left half-written: each
/// startup write (a migration, the room record, the claim) is its own transaction, and a claim
/// already sent may still commit, which only bumps the epoch as a crash after it would. The port is bound before the room is claimed, so a mistaken
/// second instance fails before it can fence the live one. A fenced room ends the process with
/// [`EXIT_FENCED`] (do not restart). With `--bundle` any other end of the room exits with 1, so
/// the supervisor restarts it and the room reloads from the log instead of answering every
/// client with "unavailable"; with `--rooms` the host restarts that room alone ([`host`]).
///
/// # Errors
/// [`RunError::Fenced`], or [`RunError::Failed`] when the bundle, the socket or the database
/// fails, the room stopped, or `--data-dir` lacks its settings.
pub async fn run(
    args: Args,
    limits: Limits,
    secrets: Secrets,
    generation: Option<Generation>,
) -> Result<(), RunError> {
    let mut stop = shutdown();
    let startup = async {
        if args.data_dir.is_some() {
            sandbox::check_host(generation.as_ref().is_some_and(|g| g.container_mode), &limits)?;
        }
        let api = worlds_api(
            args.data_dir.as_deref(),
            secrets.admin_token.as_deref(),
            generation,
            limits,
        )?;
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(args.host, args.port))
            .await
            .map_err(|e| format!("bind: {e}"))?;
        let (router, host) = app_with_web(
            &args.source,
            secrets.room_password,
            secrets.database_url.as_deref(),
            limits,
            args.web_dir.as_deref(),
        )
        .await?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let published = |f| (f, format!("{}:{}", args.public_host, addr.port()));
        let admin = match api.map(|w| w.router(host.waker(), host.folder().map(published))) {
            Some(api) => {
                let at = SocketAddr::new(args.admin_host, args.admin_port);
                let l = tokio::net::TcpListener::bind(at)
                    .await
                    .map_err(|e| format!("bind the admin API: {e}"))?;
                let addr = l.local_addr().map_err(|e| e.to_string())?;
                tracing::info!(%addr, "admin api");
                Some((l, api))
            }
            None => None,
        };
        tracing::info!(%addr, rooms = host.len(), "listening");
        Ok::<_, RunError>((listener, router, host, admin))
    };
    let (listener, router, mut host, admin) = tokio::select! {
        r = startup => r?,
        () = &mut stop => {
            tracing::info!("stopped before listening");
            return Ok(());
        }
    };
    let serve = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(stop);
    // The admin API ends with the room's listener (dropped by the select below).
    let admin = async {
        match admin {
            Some((l, api)) => axum::serve(l, api).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        r = serve => r.map_err(|e| e.to_string())?,
        r = admin => return Err(RunError::Failed(format!("the admin API stopped: {r:?}"))),
        fatal = host.supervise() => return Err(fatal),
    }
    host.stop().await;
    tracing::info!("stopped");
    Ok(())
}

/// Resolves on the first SIGTERM or Ctrl-C. The handlers are registered here, not when the
/// future is first polled: until then the default action kills the process.
fn shutdown() -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
    use tokio::signal::unix::{SignalKind, signal};
    let (term, int) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt()));
    Box::pin(async move {
        tokio::select! {
            () = recv(term) => tracing::info!("SIGTERM, shutting down"),
            () = recv(int) => tracing::info!("Ctrl-C, shutting down"),
        }
    })
}

async fn recv(s: std::io::Result<tokio::signal::unix::Signal>) {
    match s {
        Ok(mut s) => drop(s.recv().await),
        Err(e) => {
            tracing::error!(error = %e, "no signal handler");
            std::future::pending::<()>().await;
        }
    }
}

// The integration tests in `tests/` use it; unit tests do not.
#[cfg(test)]
use {e2e as _, sqlx as _};

#[cfg(test)]
mod tests {
    use super::*;

    // Without a subscriber the server never logs its listening line, and every end-to-end
    // test would wait out its guard timeout instead of failing here in milliseconds.
    #[test]
    fn init_logging_installs_the_global_subscriber() {
        // `has_been_set` also counts a test's thread-scoped subscriber (the host's log
        // capture), so the check is that no second global one can be set.
        init_logging();
        let again =
            tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::new());
        assert!(again.is_err(), "the global subscriber is in place");
    }

    fn parse(s: &str) -> Result<Args, String> {
        Args::parse(s.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn the_documented_flags_parse_and_host_defaults_to_loopback() {
        let a = parse("--bundle b.json --port 0").unwrap();
        assert_eq!(a.source, Source::Bundle(PathBuf::from("b.json")));
        assert_eq!((a.port, a.host), (0, IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert_eq!(
            (a.data_dir, a.admin_host, a.admin_port),
            (None, IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
        );
        let a = parse("--port 38281 --host 0.0.0.0 --bundle x --data-dir /d").unwrap();
        assert_eq!((a.port, a.host), (38281, IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert_eq!(a.data_dir, Some(PathBuf::from("/d")));
        assert_eq!(a.public_host, "localhost");
        assert_eq!(
            parse("--bundle x --port 1 --public-host play.example").unwrap().public_host,
            "play.example"
        );
        let a = parse("--bundle x --port 1 --data-dir /d --admin-host ::1 --admin-port 9").unwrap();
        assert_eq!((a.admin_host, a.admin_port), ("::1".parse().unwrap(), 9));
        let a = parse("--rooms seeds --port 1").unwrap();
        assert_eq!(a.source, Source::Rooms(PathBuf::from("seeds")));
        let a = parse("--bundle b --port 1 --rooms r --bundle c").unwrap();
        assert_eq!(a.source, Source::Bundle(PathBuf::from("c")), "the last source wins");
    }

    #[test]
    fn missing_bad_and_unknown_arguments_are_errors() {
        for (args, want) in [
            ("", "--bundle or --rooms is required"),
            ("--port 1", "--bundle or --rooms is required"),
            ("--bundle b", "--port is required"),
            ("--bundle b --port", "--port needs a value"),
            ("--bundle b --port 70000", "--port needs a number"),
            ("--bundle b --port 1 --host nope", "--host needs an IP"),
            ("--bundle b --port 1 --verbose", "unknown argument"),
            ("--bundle b --port 1 --password pw", "unknown argument"),
            ("--bundle b --port 1 --data-dir", "--data-dir needs a value"),
            ("--bundle b --port 1 --admin-port x", "--admin-port needs a number"),
            ("--bundle b --port 1 --admin-host x", "--admin-host needs an IP"),
        ] {
            assert!(parse(args).unwrap_err().contains(want), "{args:?}");
        }
    }

    #[test]
    fn a_bundle_over_the_cap_or_missing_is_refused() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        assert!(load_bundle(&fixture, &Limits::DESIGN).is_ok());
        let small = Limits { bundle_max_bytes: 10, ..Limits::DESIGN };
        assert!(load_bundle(&fixture, &small).unwrap_err().contains("larger than 10 bytes"));
        assert!(load_bundle(std::path::Path::new("/nonexistent/b.json"), &small).is_err());
    }

    #[test]
    fn a_bundle_exactly_at_the_cap_loads_and_one_byte_over_does_not() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let len = std::fs::metadata(&fixture).unwrap().len();
        let at = Limits { bundle_max_bytes: len, ..Limits::DESIGN };
        assert!(load_bundle(&fixture, &at).is_ok(), "a file of exactly the cap is allowed");
        let under = Limits { bundle_max_bytes: len - 1, ..Limits::DESIGN };
        let e = load_bundle(&fixture, &under).unwrap_err();
        assert!(e.contains(&format!("larger than {} bytes", len - 1)), "{e}");
    }

    /// Seeds are never read from disk: a v2 seed file is refused at import (seeds and
    /// packages live in the store).
    #[test]
    fn a_v2_seed_file_is_not_imported_from_disk() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let v1 = load_bundle(&fixture, &Limits::DESIGN).unwrap();
        assert_eq!(v1.packages.len(), 2, "the import hands its two package files to the host");
        let e = parse_bundle(v1.bundle.bytes(), &Limits::DESIGN).unwrap_err();
        assert!(e.contains("seeds live in the store"), "{e}");
    }

    #[test]
    fn the_room_id_is_the_seed_names_sha256_prefix() {
        // The first 8 bytes of the published SHA-256 digests of "" and of "abc"
        // (FIPS 180-2 appendix B.1 and the well-known empty-string digest).
        assert_eq!(room_id(""), RoomId(0xe3b0_c442_98fc_1c14));
        assert_eq!(room_id("abc"), RoomId(0xba78_16bf_8f01_cfea));
    }

    /// Golden value from an independent implementation: Python's `json.dumps(json.load(f),
    /// sort_keys=True, separators=(",", ":"), ensure_ascii=False)` hashed with `hashlib.sha256`
    /// (rerun 2026-11-17 on the regenerated fixture). The hash depends on the bundle file's content
    /// only, never on the Rust type, so adding a defaulted field to `SeedBundle` cannot lock an
    /// existing room out.
    #[test]
    fn the_bundle_hash_is_sha256_of_the_canonical_input_json() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let hex = hex(&load_bundle(&fixture, &Limits::DESIGN).unwrap().hash);
        assert_eq!(hex, "97cee53e7307b8ec357b4419cc1cfd89bfc52d72b57767297d22905ededb9275");
    }

    #[test]
    fn the_bundle_hash_ignores_formatting_and_key_order_and_sees_content() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let text = std::fs::read_to_string(&fixture).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let pretty = serde_json::to_string_pretty(&v).unwrap();
        assert_eq!(bundle_hash(&text).unwrap(), bundle_hash(&pretty).unwrap());
        // Same object, keys written in another order.
        assert_eq!(
            bundle_hash(r#"{"a":1,"b":[{"y":2,"x":3}]}"#),
            bundle_hash(r#"{"b":[{"x":3,"y":2}],"a":1}"#)
        );
        v["race_mode"] = serde_json::json!(1 - v["race_mode"].as_i64().unwrap_or(0));
        assert_ne!(bundle_hash(&text).unwrap(), bundle_hash(&v.to_string()).unwrap());
        assert_eq!(bundle_hash(&text).unwrap().len(), 32);
        assert!(bundle_hash("{not json").is_err());
    }

    #[test]
    fn the_key_is_the_one_path_segment() {
        let key = |p: &str| RoomKey::from_path(p).map(|k| k.as_str().to_owned());
        assert_eq!(key("/abcdefgh23"), Some("abcdefgh23".to_owned()));
        assert_eq!(key("/ABCDEFGH23"), Some("abcdefgh23".to_owned()));
        assert_eq!(key("/abcdefgh23/"), Some("abcdefgh23".to_owned()));
        for none in [
            "/",
            "//",
            "/abcdefgh23//",
            "/abcdefgh23/x",
            "",
            "/healthz",
            "/abcdefgh1x",
            "/abcdefgh23/more",
            "abcdefgh23.x",
        ] {
            assert_eq!(key(none), None, "{none:?}");
        }
    }

    #[test]
    fn a_fence_exits_3_and_every_other_room_end_exits_1() {
        let fenced = room_ended(Ok(Stopped::Fenced { current: Epoch(7) }));
        assert_eq!(fenced.exit_code(), 3);
        assert!(fenced.to_string().contains("fenced by a newer owner current=7"), "{fenced}");
        assert_eq!(room_ended(Ok(Stopped::Failed)).exit_code(), 1);
        assert_eq!(room_ended(Ok(Stopped::Closed)).exit_code(), 1);
        assert_eq!(RunError::from("bind: in use".to_owned()).exit_code(), 1);
    }

    #[test]
    fn the_admin_api_is_off_without_a_data_dir_and_refused_without_its_token() {
        let root = std::env::temp_dir().join(format!("server-api-{}", std::process::id()));
        let g = || Some(fake_generation(&root));
        assert!(worlds_api(None, None, None, Limits::DESIGN).unwrap().is_none());
        assert!(worlds_api(None, Some("t"), g(), Limits::DESIGN).unwrap().is_none());
        let dir = root.join("data");
        // A temporary upload a previous process left behind is cleared at start.
        std::fs::create_dir_all(dir.join("tmp/left-behind")).unwrap();
        let e = worlds_api(Some(&dir), None, g(), Limits::DESIGN).unwrap_err();
        assert!(e.contains("CONSTELLATION_ADMIN_TOKEN"), "{e}");
        let e = worlds_api(Some(&dir), Some("t"), None, Limits::DESIGN).unwrap_err();
        assert!(e.contains("CONSTELLATION_AP_DIR"), "{e}");
        assert!(worlds_api(Some(&dir), Some("t"), g(), Limits::DESIGN).unwrap().is_some());
        assert!(dir.join("apworlds").is_dir() && dir.join("worlds").is_dir());
        assert!(dir.join("tmp").is_dir() && !dir.join("tmp/left-behind").exists());
    }

    /// A fake Archipelago checkout and sidecar path under `root`: enough for the sandbox to
    /// accept the settings (nothing here is run).
    fn fake_generation(root: &std::path::Path) -> Generation {
        for d in ["ap/custom_worlds", "ap/worlds/generic", "tools/ap-gen"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("ap/worlds/__init__.py"), "").unwrap();
        Generation {
            ap_dir: root.join("ap"),
            python: "/usr/bin/python3".into(),
            sidecar: root.join("tools/ap-gen/ap_gen.py"),
            private: vec![],
            container_mode: false,
        }
    }

    /// The status code of one HTTP/1.1 GET to `addr`.
    async fn status_of(addr: SocketAddr, path: &str, token: Option<&str>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let head =
            format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n");
        s.write_all(head.as_bytes()).await.unwrap();
        let mut first = [0_u8; 12];
        s.read_exact(&mut first).await.unwrap();
        String::from_utf8_lossy(&first[9..12]).parse().unwrap()
    }

    #[tokio::test]
    async fn the_room_view_routes_need_the_admin_token() {
        let root = std::env::temp_dir().join(format!("server-view-auth-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&root));
        std::fs::create_dir_all(root.join("rooms")).unwrap();
        let (_, host) =
            app(&Source::Rooms(root.join("rooms")), None, None, Limits::DESIGN).await.unwrap();
        let worlds = worlds_api(
            Some(&root.join("data")),
            Some("s3cret"),
            Some(fake_generation(&root)),
            Limits::DESIGN,
        )
        .unwrap()
        .unwrap();
        let api = worlds.router(host.waker(), host.folder().map(|f| (f, "localhost:1".to_owned())));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, api).await });
        let id = "a".repeat(64);
        for path in [format!("/api/worlds/{id}/room"), format!("/api/worlds/{id}/room/events")] {
            assert_eq!(status_of(addr, &path, None).await, 401, "{path} without the token");
            assert_eq!(status_of(addr, &path, Some("wrong")).await, 401, "{path}, wrong token");
            assert_eq!(status_of(addr, &path, Some("s3cret")).await, 404, "{path}: no such world");
        }
        drop(std::fs::remove_dir_all(&root));
    }

    async fn reply(r: Response) -> (StatusCode, String) {
        let status = r.status();
        let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// A room that did not load answers 503 with a generic body: its error text (database,
    /// store) goes to the log only. An unknown room keeps its 404 and its reason (#63).
    #[tokio::test]
    async fn a_failed_load_hides_its_error_text_and_an_unknown_room_keeps_its_reason() {
        let failed = refused("/k", NoRoom::Failed("postgres: secret detail".to_owned()));
        assert_eq!(
            reply(failed).await,
            (StatusCode::SERVICE_UNAVAILABLE, "the room could not be loaded; try again".to_owned())
        );
        let unknown = refused("/", NoRoom::Unknown("no room with key k".to_owned()));
        assert_eq!(reply(unknown).await, (StatusCode::NOT_FOUND, "no room with key k".to_owned()));
    }

    /// A build folder (`index.html`, one hashed asset) next to a secret it must never serve,
    /// with a symlink to the secret inside the folder.
    fn web_fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("server-web-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&root));
        let dist = root.join("dist");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), "<!doctype html><title>UI</title>").unwrap();
        std::fs::write(dist.join("assets/app-abc123.js"), "console.log(1)").unwrap();
        std::fs::write(root.join("secret.txt"), "TOP SECRET").unwrap();
        std::os::unix::fs::symlink(root.join("secret.txt"), dist.join("leak.txt")).unwrap();
        std::os::unix::fs::symlink(&root, dist.join("outside")).unwrap();
        (root, dist)
    }

    /// Serve `source` and the UI in `dist` on a free port; the test sends raw requests to it.
    async fn serve_ui(source: &Source, dist: &std::path::Path) -> SocketAddr {
        let (router, host) =
            app_with_web(source, None, None, Limits::DESIGN, Some(dist)).await.unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let _host = host;
            axum::serve(l, router.into_make_service_with_connect_info::<SocketAddr>()).await
        });
        addr
    }

    /// One raw HTTP/1.1 request (no client rewrites the path); the head and the body as text.
    async fn raw(addr: SocketAddr, path: &str, extra: &str) -> String {
        raw_method(addr, "GET", path, extra).await
    }

    async fn raw_method(addr: SocketAddr, method: &str, path: &str, extra: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head =
            format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{extra}Connection: close\r\n\r\n");
        s.write_all(head.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        // An upgraded socket stays open: read until the head is in, then stop.
        let mut buf = [0_u8; 4096];
        while !out.windows(4).any(|w| w == b"\r\n\r\n") || !extra.contains("Upgrade") {
            let n = s.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(buf.get(..n).unwrap());
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    const UPGRADE: &str = "Upgrade: websocket\r\nConnection: Upgrade\r\n\
        Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n";

    #[tokio::test]
    async fn the_ui_is_served_with_cache_headers_and_the_spa_fallback() {
        let (root, dist) = web_fixture("spa");
        std::fs::create_dir_all(root.join("rooms")).unwrap();
        let addr = serve_ui(&Source::Rooms(root.join("rooms")), &dist).await;
        let index = raw(addr, "/", "").await;
        assert!(index.starts_with("HTTP/1.1 200"), "{index}");
        assert!(index.contains("<title>UI</title>") && index.contains("text/html"), "{index}");
        assert!(index.to_lowercase().contains("cache-control: no-cache"), "{index}");
        for h in [
            "x-content-type-options: nosniff",
            "referrer-policy: no-referrer",
            "x-frame-options: deny",
            "frame-ancestors 'none'",
            "etag: w/",
            "last-modified:",
        ] {
            assert!(index.to_lowercase().contains(h), "{h}: {index}");
        }
        let asset = raw(addr, "/assets/app-abc123.js", "").await;
        assert!(asset.contains("console.log(1)") && asset.contains("text/javascript"), "{asset}");
        assert!(asset.to_lowercase().contains("max-age=31536000, immutable"), "{asset}");
        // A UI route, and a plain GET of a would-be room key, load the app.
        for path in ["/create", "/rooms/abc/admin", "/abcdefgh23"] {
            let r = raw(addr, path, "").await;
            assert!(
                r.starts_with("HTTP/1.1 200") && r.contains("<title>UI</title>"),
                "{path}: {r}"
            );
        }
        // A missing file with an extension, or anything under /assets, is a 404, not the app.
        for path in
            ["/assets/gone.js", "/assets/gone", "/favicon.ico", "/x.map", "/api/yamls", "/api"]
        {
            assert!(raw(addr, path, "").await.starts_with("HTTP/1.1 404"), "{path}");
        }
        // The app only for a request that accepts HTML.
        let json = raw(addr, "/create", "Accept: application/json\r\n").await;
        assert!(json.starts_with("HTTP/1.1 404"), "{json}");
        let page = raw(addr, "/create", "Accept: text/html,application/xhtml+xml;q=0.9\r\n").await;
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        // A directory is not a file.
        assert!(raw(addr, "/assets", "").await.starts_with("HTTP/1.1 404"));
        // The probes keep working.
        assert!(raw(addr, "/healthz", "").await.ends_with("ok"));
        assert!(raw(addr, "/version", "").await.contains("{\"version\":\""));
        drop(std::fs::remove_dir_all(&root));
    }

    #[tokio::test]
    async fn the_ui_sends_the_precompressed_file_validators_and_head() {
        let (root, dist) = web_fixture("encoding");
        std::fs::create_dir_all(root.join("rooms")).unwrap();
        std::fs::write(dist.join("assets/app-abc123.js.br"), "BROTLI-BYTES").unwrap();
        std::fs::write(dist.join("assets/app-abc123.js.gz"), "GZIP-BYTES").unwrap();
        let addr = serve_ui(&Source::Rooms(root.join("rooms")), &dist).await;
        let js = "/assets/app-abc123.js";
        let br = raw(addr, js, "Accept-Encoding: gzip, br\r\n").await;
        assert!(
            br.contains("BROTLI-BYTES") && br.to_lowercase().contains("content-encoding: br"),
            "{br}"
        );
        assert!(br.contains("text/javascript"), "the type is the original file's: {br}");
        let gz = raw(addr, js, "Accept-Encoding: gzip\r\n").await;
        assert!(gz.contains("GZIP-BYTES"), "{gz}");
        let plain = raw(addr, js, "").await;
        assert!(
            plain.contains("console.log(1)") && !plain.to_lowercase().contains("content-encoding"),
            "{plain}"
        );
        // A matching ETag is a 304 with no body.
        let etag = br.lines().find_map(|l| l.strip_prefix("etag: ")).unwrap().to_owned();
        let again =
            raw(addr, js, &format!("Accept-Encoding: gzip, br\r\nIf-None-Match: {etag}\r\n")).await;
        assert!(again.starts_with("HTTP/1.1 304") && !again.contains("BROTLI"), "{again}");
        // HEAD: the headers of the GET, no body.
        let head = raw_method(addr, "HEAD", "/create", "").await;
        assert!(head.starts_with("HTTP/1.1 200") && !head.contains("<title>"), "{head}");
        // POST to a UI path is not a page load (the fallback route only takes GET, so 405).
        assert!(
            raw_method(addr, "POST", "/create", "Content-Length: 0\r\n")
                .await
                .starts_with("HTTP/1.1 405")
        );
        drop(std::fs::remove_dir_all(&root));
    }

    #[tokio::test]
    async fn an_upgrade_at_a_key_reaches_the_room_not_the_ui() {
        let (root, dist) = web_fixture("upgrade");
        std::fs::create_dir_all(root.join("rooms")).unwrap();
        let addr = serve_ui(&Source::Rooms(root.join("rooms")), &dist).await;
        let r = raw(addr, "/abcdefgh23", UPGRADE).await;
        assert!(r.starts_with("HTTP/1.1 404"), "{r}");
        assert!(r.contains("no room with key abcdefgh23") && !r.contains("<title>"), "{r}");
        drop(std::fs::remove_dir_all(&root));
    }

    #[tokio::test]
    async fn with_a_bundle_an_upgrade_on_any_path_joins_the_room_and_a_get_is_the_ui() {
        let (root, dist) = web_fixture("bundle");
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let addr = serve_ui(&Source::Bundle(fixture), &dist).await;
        for path in ["/", "/anything/at/all"] {
            let r = raw(addr, path, UPGRADE).await;
            assert!(r.starts_with("HTTP/1.1 101"), "{path}: {r}");
        }
        assert!(raw(addr, "/anything", "").await.contains("<title>UI</title>"));
        drop(std::fs::remove_dir_all(&root));
    }

    #[tokio::test]
    async fn the_ui_never_serves_a_file_outside_its_folder() {
        let (root, dist) = web_fixture("traversal");
        std::fs::create_dir_all(root.join("rooms")).unwrap();
        let addr = serve_ui(&Source::Rooms(root.join("rooms")), &dist).await;
        for path in [
            "/../secret.txt",
            "/assets/../../secret.txt",
            "/%2e%2e/secret.txt",
            "/%2E%2E%2Fsecret.txt",
            "/..%2fsecret.txt",
            "/assets/%2e%2e/%2e%2e/secret.txt",
            "/leak.txt",
            "/outside/secret.txt",
            "/outside/dist/../secret.txt",
        ] {
            let r = raw(addr, path, "").await;
            assert!(!r.contains("TOP SECRET"), "{path} leaked: {r}");
        }
        drop(std::fs::remove_dir_all(&root));
    }

    #[tokio::test]
    async fn a_web_dir_without_index_html_is_refused() {
        let (root, dist) = web_fixture("noindex");
        std::fs::remove_file(dist.join("index.html")).unwrap();
        let e = app_with_web(&Source::Rooms(root.clone()), None, None, Limits::DESIGN, Some(&dist))
            .await
            .unwrap_err();
        assert!(e.contains("no index.html"), "{e}");
        drop(std::fs::remove_dir_all(&root));
    }

    #[test]
    fn web_dir_is_a_flag_and_off_by_default() {
        assert_eq!(parse("--bundle b --port 1").unwrap().web_dir, None);
        let a = parse("--bundle b --port 1 --web-dir /w").unwrap();
        assert_eq!(a.web_dir, Some(PathBuf::from("/w")));
        assert_eq!(parse("--bundle b --port 1 --web-dir").unwrap_err(), "--web-dir needs a value");
    }

    #[test]
    fn hex_is_lowercase_two_digits_per_byte() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0, 0x0f, 0xab, 0xff]), "000fabff");
    }

    #[test]
    fn healthz_is_ok_only_while_the_room_runs() {
        assert_eq!(health(true), (StatusCode::OK, "ok"));
        assert_eq!(health(false), (StatusCode::SERVICE_UNAVAILABLE, "room stopped"));
    }

    #[test]
    fn version_has_a_short_sha_only_when_one_was_built_in() {
        assert_eq!(version_string("0.1.0", None), "0.1.0");
        assert_eq!(version_string("0.1.0", Some("")), "0.1.0");
        assert_eq!(version_string("0.1.0", Some("abcdef1234567")), "0.1.0+abcdef1");
        assert_eq!(version_string("0.1.0", Some("abc")), "0.1.0+abc");
    }
}
