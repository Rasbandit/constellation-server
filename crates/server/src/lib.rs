//! The composition root (D-99: one process). Loads a seed bundle, starts its room on
//! `room-core` and the log (Postgres when `CONSTELLATION_DATABASE_URL` is set, D-98, else in
//! memory), and serves the Archipelago-compatible edge over HTTP: a WebSocket at `/` (yawc,
//! D-97) and `GET /healthz` (axum).
//!
//! With Postgres the room survives a restart: the server claims the room (fencing any older
//! process), then rebuilds it from the stored snapshot and log tail through `room_core::fold`
//! and restores the stored applied cursors, before it accepts a connection.
//!
//! With `--data-dir` it also serves the admin API for uploads and seed generation
//! ([`worlds`], run in [`sandbox`]).

mod sandbox;
mod worlds;

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use config::{Generation, Limits, Secrets};
use edge_compat::CompatRoom;
use log::{LogStore, MemLogStore, OpenError, PgLogStore};
use proto::{Epoch, RoomId, Seq};
use room_core::RoomState;
use runtime::{Mailbox, RoomHandle, Stopped};
use seed_bundle::Seed;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use yawc::{DeflatePool, IncomingUpgrade, Options};

/// Command line settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Seed bundle file.
    pub bundle: PathBuf,
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
}

/// How to run the binary.
pub const USAGE: &str = "usage: constellation-server --bundle <path> --port <port> [--host <ip>] \
     [--data-dir <path> [--admin-host <ip>] [--admin-port <port>]] (room password: CONSTELLATION_ROOM_PASSWORD or \
     CONSTELLATION_ROOM_PASSWORD_FILE; durable log: CONSTELLATION_DATABASE_URL or \
     CONSTELLATION_DATABASE_URL_FILE; --data-dir needs CONSTELLATION_ADMIN_TOKEN or \
     CONSTELLATION_ADMIN_TOKEN_FILE, CONSTELLATION_AP_DIR, CONSTELLATION_AP_PYTHON and \
     CONSTELLATION_AP_GEN_SCRIPT)";

impl Args {
    /// Parse the arguments after the program name.
    ///
    /// # Errors
    /// A message naming the bad or missing argument.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let (mut bundle, mut port, mut host, mut data_dir) = (None, None, None, None);
        let (mut admin_host, mut admin_port) = (None, None);
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--bundle" => bundle = Some(PathBuf::from(value()?)),
                "--port" => port = Some(value()?.parse().map_err(|_| "--port needs a number")?),
                "--host" => host = Some(value()?.parse().map_err(|_| "--host needs an IP")?),
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
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
            bundle: bundle.ok_or("--bundle is required")?,
            port: port.ok_or("--port is required")?,
            host: host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            data_dir,
            admin_host: admin_host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            admin_port: admin_port.unwrap_or(0),
        })
    }
}

/// A validated bundle and the hash of its file's content ([`bundle_hash`]).
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The seed, in its v2 form whatever the file was; shared by the room state and the edge.
    pub bundle: Arc<Seed>,
    /// SHA-256 of the canonical v1 JSON (for a v2 file, of the JSON it was made from).
    pub hash: Vec<u8>,
}

/// Read and validate a seed file, v1 JSON or v2 (`docs/seed-bundle-v2.md`; its packages in
/// the `packages` folder beside it), refusing files over `limits.bundle_max_bytes`. The one
/// way a server loads a seed.
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
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let seed = seed_bundle::load(bytes, dir, cap)?;
    Ok(Loaded { hash: seed.hash().to_vec(), bundle: Arc::new(seed) })
}

/// A new room's state for a loaded bundle: the one place a room is built from a bundle.
///
/// # Errors
/// When the bundle does not make a valid room.
pub fn fresh_state(loaded: &Loaded) -> Result<RoomState, String> {
    let world: Arc<dyn room_core::World> = Arc::clone(&loaded.bundle) as _;
    RoomState::from_world(world).map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
struct AppState {
    room: Arc<CompatRoom>,
    /// For `/healthz`: the room task's liveness.
    live: RoomHandle,
    options: Options,
    /// One permit per open WebSocket (`Limits::max_connections`).
    slots: Arc<Semaphore>,
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
) -> Result<Option<Router>, String> {
    let Some(dir) = data_dir else { return Ok(None) };
    let token = token.ok_or(
        "--data-dir serves an admin API that runs uploaded code; set \
         CONSTELLATION_ADMIN_TOKEN or CONSTELLATION_ADMIN_TOKEN_FILE",
    )?;
    let generation = generation.ok_or(
        "--data-dir needs CONSTELLATION_AP_DIR, CONSTELLATION_AP_PYTHON and \
         CONSTELLATION_AP_GEN_SCRIPT",
    )?;
    worlds::router(dir, token, generation, limits).map(Some)
}

pub use seed_bundle::bundle_hash;

/// `/healthz`: 200 while the room task runs, 503 once it stopped.
const fn health(running: bool) -> (StatusCode, &'static str) {
    if running { (StatusCode::OK, "ok") } else { (StatusCode::SERVICE_UNAVAILABLE, "room stopped") }
}

async fn healthz(State(s): State<AppState>) -> (StatusCode, &'static str) {
    health(s.live.is_running())
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
        Some(snap) => {
            let state = RoomState::from_snapshot(fresh.world(), &snap.data)
                .map_err(|e| format!("stored snapshot at {}: {e}", snap.seq.0))?;
            state.validate(snap.seq).map_err(|e| e.to_string())?;
            (state, snap.seq)
        }
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

/// Start the room for `loaded` and build the router.
///
/// Also returns the room task, whose value says why it stopped (see [`run`]). With
/// `database_url` the log is in Postgres and the room is rebuilt from it; without, it is in
/// memory and a restart loses it. Everything that can fail on the bundle alone runs before the
/// room is claimed, so a bad bundle never fences a live room.
///
/// # Errors
/// A message when the bundle does not make a valid room, the database fails, or the stored
/// room was made from a different bundle.
pub async fn app(
    loaded: Loaded,
    password: Option<String>,
    database_url: Option<&str>,
    limits: Limits,
) -> Result<(Router, JoinHandle<Stopped>), String> {
    let fresh = fresh_state(&loaded)?;
    let Loaded { bundle, hash } = loaded;
    let room = room_id(bundle.seed_name());
    let (live, mailbox) = RoomHandle::mailbox(&limits);
    // The stored hash is of the v1 form, so the format recorded with it is v1's.
    let (seed_name, format) = (bundle.seed_name().to_owned(), seed_bundle::FORMAT);
    let compat = Arc::new(CompatRoom::new(bundle, live.clone(), password, limits)?);
    let task = if let Some(url) = database_url {
        let store = PgLogStore::connect(url, &limits)
            .await
            .map_err(|e| format!("postgres: {e:?} (see the log above)"))?;
        match store.open_room(room, &seed_name, format, &hash).await {
            Ok(()) => {}
            Err(OpenError::OtherBundle) => {
                return Err(format!(
                    "the database holds room {seed_name:?} made from a different bundle with \
                     this seed name; refusing to start"
                ));
            }
            Err(OpenError::Store(e)) => return Err(format!("postgres: {e:?}")),
        }
        let epoch = store.claim(room).await.map_err(|e| format!("postgres: {e:?}"))?;
        tracing::info!("log store: postgres");
        start_room(mailbox, room, epoch, fresh, Arc::new(store), &limits).await?
    } else {
        tracing::warn!(
            "log store: IN MEMORY, no CONSTELLATION_DATABASE_URL; a restart loses the room"
        );
        let store = Arc::new(MemLogStore::new());
        let epoch = store.claim(room);
        start_room(mailbox, room, epoch, fresh, store, &limits).await?
    };
    let options = edge_compat::ws_options(&limits, DeflatePool::new(limits.ws_deflate_pool));
    let state = AppState {
        room: compat,
        live,
        options,
        slots: Arc::new(Semaphore::new(limits.max_connections)),
    };
    let router =
        Router::new().route("/", get(upgrade)).route("/healthz", get(healthz)).with_state(state);
    Ok((router, task))
}

async fn upgrade(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: IncomingUpgrade,
) -> Response {
    let Ok(permit) = Arc::clone(&s.slots).try_acquire_owned() else {
        tracing::warn!("connection cap reached");
        return (StatusCode::SERVICE_UNAVAILABLE, "server full").into_response();
    };
    let (response, fut) = match ws.upgrade(s.options) {
        Ok(pair) => pair,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let deflate = response.headers().contains_key(header::SEC_WEBSOCKET_EXTENSIONS);
    tracing::info!(deflate, "websocket accepted");
    tokio::spawn(async move {
        match fut.await {
            Ok(socket) => edge_compat::serve(socket, s.room, peer.ip()).await,
            Err(e) => tracing::debug!(error = %e, "upgrade did not complete"),
        }
        drop(permit);
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
fn room_ended(r: Result<Stopped, tokio::task::JoinError>) -> RunError {
    match r {
        Ok(Stopped::Fenced { current }) => RunError::Fenced { current },
        Ok(Stopped::Failed) => RunError::Failed(
            "the room stopped (see the log above); exiting so it restarts from the log".to_owned(),
        ),
        Ok(Stopped::Closed) => RunError::Failed("the room closed unexpectedly".to_owned()),
        Err(e) => RunError::Failed(format!("the room task panicked or was cancelled: {e}")),
    }
}

/// Bind, start the room, log the address, and serve until SIGTERM or Ctrl-C, or until the
/// room stops.
///
/// The signal handlers are installed first, so a stop at any point is clean: before the
/// server listens it abandons startup and returns `Ok`. Nothing is left half-written: each
/// startup write (a migration, the room record, the claim) is its own transaction, and a claim
/// already sent may still commit, which only bumps the epoch as a crash after it would. The port is bound before the room is claimed, so a mistaken
/// second instance fails before it can fence the live one. A room that stops ends the process:
/// fenced by a newer owner with [`EXIT_FENCED`] (do not restart), otherwise with 1, so the
/// supervisor restarts it and the room reloads from the log instead of answering every client
/// with "unavailable".
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
        let loaded = load_bundle(&args.bundle, &limits)?;
        if args.data_dir.is_some() {
            sandbox::check_host(&limits)?;
        }
        let api = worlds_api(
            args.data_dir.as_deref(),
            secrets.admin_token.as_deref(),
            generation,
            limits,
        )?;
        let seed = loaded.bundle.seed_name().to_owned();
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(args.host, args.port))
            .await
            .map_err(|e| format!("bind: {e}"))?;
        let (router, room) =
            app(loaded, secrets.room_password, secrets.database_url.as_deref(), limits).await?;
        let admin = match api {
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
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        tracing::info!(%addr, seed, "listening");
        Ok::<_, RunError>((listener, router, room, admin))
    };
    let (listener, router, room, admin) = tokio::select! {
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
        r = room => return Err(room_ended(r)),
    }
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
        assert!(!tracing::dispatcher::has_been_set(), "no other unit test installs one");
        init_logging();
        assert!(tracing::dispatcher::has_been_set());
    }

    fn parse(s: &str) -> Result<Args, String> {
        Args::parse(s.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn the_documented_flags_parse_and_host_defaults_to_loopback() {
        let a = parse("--bundle b.json --port 0").unwrap();
        assert_eq!(a.bundle, PathBuf::from("b.json"));
        assert_eq!((a.port, a.host), (0, IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert_eq!(
            (a.data_dir, a.admin_host, a.admin_port),
            (None, IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
        );
        let a = parse("--port 38281 --host 0.0.0.0 --bundle x --data-dir /d").unwrap();
        assert_eq!((a.port, a.host), (38281, IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert_eq!(a.data_dir, Some(PathBuf::from("/d")));
        let a = parse("--bundle x --port 1 --data-dir /d --admin-host ::1 --admin-port 9").unwrap();
        assert_eq!((a.admin_host, a.admin_port), ("::1".parse().unwrap(), 9));
    }

    #[test]
    fn missing_bad_and_unknown_arguments_are_errors() {
        for (args, want) in [
            ("", "--bundle is required"),
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
        for d in ["ap/custom_worlds", "ap/worlds/generic", "tools/ap-gen"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("ap/worlds/__init__.py"), "").unwrap();
        let g = || {
            Some(Generation {
                ap_dir: root.join("ap"),
                python: "/usr/bin/python3".into(),
                sidecar: root.join("tools/ap-gen/ap_gen.py"),
                private: vec![],
            })
        };
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
}
