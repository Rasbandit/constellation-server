//! The composition root (D-99: one process). Loads a seed bundle, starts its room on
//! `room-core` and the log (Postgres when `CONSTELLATION_DATABASE_URL` is set, D-98, else in
//! memory), and serves the Archipelago-compatible edge over HTTP: a WebSocket at `/` (yawc,
//! D-97) and `GET /healthz` (axum).
//!
//! With Postgres the room survives a restart: the server claims the room (fencing any older
//! process), then rebuilds it from the stored snapshot and log tail through `room_core::fold`
//! and restores the stored applied cursors, before it accepts a connection.

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use config::{Limits, Secrets};
use edge_compat::CompatRoom;
use log::{LogStore, MemLogStore, OpenError, PgLogStore};
use proto::{Epoch, RoomId, Seq};
use room_core::RoomState;
use runtime::{Mailbox, RoomHandle, Stopped};
use seed_bundle::SeedBundle;
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
}

/// How to run the binary.
pub const USAGE: &str = "usage: constellation-server --bundle <path> --port <port> [--host <ip>] \
     (room password: CONSTELLATION_ROOM_PASSWORD or CONSTELLATION_ROOM_PASSWORD_FILE; \
     durable log: CONSTELLATION_DATABASE_URL or CONSTELLATION_DATABASE_URL_FILE)";

impl Args {
    /// Parse the arguments after the program name.
    ///
    /// # Errors
    /// A message naming the bad or missing argument.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let (mut bundle, mut port, mut host) = (None, None, None);
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--bundle" => bundle = Some(PathBuf::from(value()?)),
                "--port" => port = Some(value()?.parse().map_err(|_| "--port needs a number")?),
                "--host" => host = Some(value()?.parse().map_err(|_| "--host needs an IP")?),
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Self {
            bundle: bundle.ok_or("--bundle is required")?,
            port: port.ok_or("--port is required")?,
            host: host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        })
    }
}

/// A validated bundle and the hash of its file's content ([`bundle_hash`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// The bundle.
    pub bundle: SeedBundle,
    /// SHA-256 of the file's canonical JSON.
    pub hash: Vec<u8>,
}

/// Read and validate a bundle, refusing files over `limits.bundle_max_bytes`.
///
/// # Errors
/// A message for an unreadable, oversized or invalid file.
pub fn load_bundle(path: &std::path::Path, limits: &Limits) -> Result<Loaded, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut text = String::new();
    let cap = limits.bundle_max_bytes;
    file.take(cap.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > cap {
        return Err(format!("{}: larger than {cap} bytes", path.display()));
    }
    let bundle = SeedBundle::from_json(&text).map_err(|e| e.to_string())?;
    Ok(Loaded { hash: bundle_hash(&text)?, bundle })
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

/// SHA-256 of the bundle file's canonical JSON.
///
/// Canonical: parsed as plain JSON, every object's keys sorted, no whitespace. It depends on the file's content only, never on the `SeedBundle`
/// type, so a new defaulted field in the type does not change a stored room's hash.
///
/// # Errors
/// When the text is not JSON.
pub fn bundle_hash(text: &str) -> Result<Vec<u8>, String> {
    let mut v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    v.sort_all_objects();
    let canonical = serde_json::to_vec(&v).map_err(|e| e.to_string())?;
    Ok(Sha256::digest(&canonical).to_vec())
}

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
    limits: Limits,
) -> Result<JoinHandle<Stopped>, String> {
    let (state, from) = match store.snapshot_get(room).await.map_err(|e| format!("{e:?}"))? {
        Some(snap) => {
            let state: RoomState = serde_json::from_slice(&snap.data)
                .map_err(|e| format!("stored snapshot at {}: {e}", snap.seq.0))?;
            state.validate(snap.seq).map_err(|e| e.to_string())?;
            (state, snap.seq)
        }
        None => (fresh, Seq(0)),
    };
    let tail = store.read_tail(room, from, usize::MAX).await.map_err(|e| format!("{e:?}"))?;
    let events = tail.len();
    let mut state = room_core::fold(state, &limits, tail).map_err(|e| e.to_string())?;
    for c in store.read_cursors(room).await.map_err(|e| format!("{e:?}"))? {
        state.restore_applied(c.slot, c.applied).map_err(|e| e.to_string())?;
    }
    tracing::info!(room = room.0, epoch = epoch.0, events, head = state.durable().0, "room loaded");
    Ok(mailbox.spawn(room, epoch, state, store, limits))
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
    let Loaded { bundle, hash } = loaded;
    let room = room_id(&bundle.seed_name);
    let fresh = RoomState::new(bundle.slot_nos(), bundle.placement()).map_err(|e| e.to_string())?;
    let (live, mailbox) = RoomHandle::mailbox(&limits);
    let (seed_name, format) = (bundle.seed_name.clone(), bundle.format);
    let compat = Arc::new(CompatRoom::new(Arc::new(bundle), live.clone(), password, limits)?);
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
        start_room(mailbox, room, epoch, fresh, Arc::new(store), limits).await?
    } else {
        tracing::warn!(
            "log store: IN MEMORY, no CONSTELLATION_DATABASE_URL; a restart loses the room"
        );
        let store = Arc::new(MemLogStore::new());
        let epoch = store.claim(room);
        start_room(mailbox, room, epoch, fresh, store, limits).await?
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
/// fails or the room stopped.
pub async fn run(args: Args, limits: Limits, secrets: Secrets) -> Result<(), RunError> {
    let mut stop = shutdown();
    let startup = async {
        let loaded = load_bundle(&args.bundle, &limits)?;
        let seed = loaded.bundle.seed_name.clone();
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(args.host, args.port))
            .await
            .map_err(|e| format!("bind: {e}"))?;
        let (router, room) =
            app(loaded, secrets.room_password, secrets.database_url.as_deref(), limits).await?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        tracing::info!(%addr, seed, "listening");
        Ok::<_, RunError>((listener, router, room))
    };
    let (listener, router, room) = tokio::select! {
        r = startup => r?,
        () = &mut stop => {
            tracing::info!("stopped before listening");
            return Ok(());
        }
    };
    let serve = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(stop);
    tokio::select! {
        r = serve => r.map_err(|e| e.to_string())?,
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
        let a = parse("--port 38281 --host 0.0.0.0 --bundle x").unwrap();
        assert_eq!((a.port, a.host), (38281, IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
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
    /// (run 2026-11-16 on the committed fixture). The hash depends on the bundle file's content
    /// only, never on the Rust type, so adding a defaulted field to `SeedBundle` cannot lock an
    /// existing room out.
    #[test]
    fn the_bundle_hash_is_sha256_of_the_canonical_input_json() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let hex = load_bundle(&fixture, &Limits::DESIGN).unwrap().hash.iter().fold(
            String::new(),
            |mut h, x| {
                use std::fmt::Write;
                write!(h, "{x:02x}").unwrap();
                h
            },
        );
        assert_eq!(hex, "a78ee159f63ad862df2636415f1ce02922dd714d6ffa9290f67ded38b76eab7a");
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
    fn healthz_is_ok_only_while_the_room_runs() {
        assert_eq!(health(true), (StatusCode::OK, "ok"));
        assert_eq!(health(false), (StatusCode::SERVICE_UNAVAILABLE, "room stopped"));
    }
}
