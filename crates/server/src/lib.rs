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
use runtime::RoomHandle;
use seed_bundle::SeedBundle;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;
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

/// Read and validate a bundle, refusing files over `limits.bundle_max_bytes`.
///
/// # Errors
/// A message for an unreadable, oversized or invalid file.
pub fn load_bundle(path: &std::path::Path, limits: &Limits) -> Result<SeedBundle, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut text = String::new();
    let cap = limits.bundle_max_bytes;
    file.take(cap.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > cap {
        return Err(format!("{}: larger than {cap} bytes", path.display()));
    }
    SeedBundle::from_json(&text).map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
struct AppState {
    room: Arc<CompatRoom>,
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

/// SHA-256 of the bundle's canonical JSON (whitespace in the file does not change it).
///
/// # Errors
/// When the bundle cannot be encoded.
pub fn bundle_hash(bundle: &SeedBundle) -> Result<Vec<u8>, String> {
    let canonical = serde_json::to_vec(bundle).map_err(|e| e.to_string())?;
    Ok(Sha256::digest(&canonical).to_vec())
}

/// Rebuild the room from `store` (snapshot, then the tail through `fold`, then the stored
/// cursors) and start its task. `epoch` must be the claim just made.
async fn start_room<S: LogStore>(
    room: RoomId,
    epoch: Epoch,
    fresh: RoomState,
    store: Arc<S>,
    limits: Limits,
) -> Result<RoomHandle, String> {
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
    // The task ends with the process; a stopped room answers every request with Unavailable.
    let (handle, _task) = RoomHandle::spawn(room, epoch, state, store, limits);
    Ok(handle)
}

/// Start the room for `bundle` and build the router. With `database_url` the log is in
/// Postgres and the room is rebuilt from it; without, it is in memory and a restart loses it.
///
/// # Errors
/// A message when the bundle does not make a valid room, the database fails, or the stored
/// room was made from a different bundle.
pub async fn app(
    bundle: SeedBundle,
    password: Option<String>,
    database_url: Option<&str>,
    limits: Limits,
) -> Result<Router, String> {
    let room = room_id(&bundle.seed_name);
    let fresh = RoomState::new(bundle.slot_nos(), bundle.placement()).map_err(|e| e.to_string())?;
    let handle = if let Some(url) = database_url {
        let store = PgLogStore::connect(url, limits.db_pool_connections)
            .await
            .map_err(|e| format!("postgres: {e:?} (see the log above)"))?;
        match store.open_room(room, &bundle.seed_name, &bundle_hash(&bundle)?).await {
            Ok(()) => {}
            Err(OpenError::OtherBundle) => {
                return Err(format!(
                    "the database holds room {:?} made from a different bundle with this seed                      name; refusing to start",
                    bundle.seed_name
                ));
            }
            Err(OpenError::Store(e)) => return Err(format!("postgres: {e:?}")),
        }
        let epoch = store.claim(room).await.map_err(|e| format!("postgres: {e:?}"))?;
        tracing::info!("log store: postgres");
        start_room(room, epoch, fresh, Arc::new(store), limits).await?
    } else {
        tracing::warn!(
            "log store: IN MEMORY, no CONSTELLATION_DATABASE_URL; a restart loses the room"
        );
        let store = Arc::new(MemLogStore::new());
        let epoch = store.claim(room);
        start_room(room, epoch, fresh, store, limits).await?
    };
    let room = Arc::new(CompatRoom::new(Arc::new(bundle), handle, password, limits)?);
    let options = edge_compat::ws_options(&limits, DeflatePool::new(limits.ws_deflate_pool));
    Ok(Router::new().route("/", get(upgrade)).route("/healthz", get(|| async { "ok" })).with_state(
        AppState { room, options, slots: Arc::new(Semaphore::new(limits.max_connections)) },
    ))
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

/// Bind, log the address, and serve until SIGTERM or Ctrl-C.
///
/// # Errors
/// A message when the bundle or the socket fails.
pub async fn run(args: Args, limits: Limits, secrets: Secrets) -> Result<(), String> {
    let bundle = load_bundle(&args.bundle, &limits)?;
    let seed = bundle.seed_name.clone();
    let router =
        app(bundle, secrets.room_password, secrets.database_url.as_deref(), limits).await?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(args.host, args.port))
        .await
        .map_err(|e| format!("bind: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    tracing::info!(%addr, seed, "listening");
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "no SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = term => tracing::info!("SIGTERM, shutting down"),
        r = tokio::signal::ctrl_c() => tracing::info!(ok = r.is_ok(), "Ctrl-C, shutting down"),
    }
}

// The integration tests in `tests/` use it; unit tests do not.
#[cfg(test)]
use e2e as _;

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_room_id_is_the_seed_names_sha256_prefix() {
        // The first 8 bytes of the published SHA-256 digests of "" and of "abc"
        // (FIPS 180-2 appendix B.1 and the well-known empty-string digest).
        assert_eq!(room_id(""), RoomId(0xe3b0_c442_98fc_1c14));
        assert_eq!(room_id("abc"), RoomId(0xba78_16bf_8f01_cfea));
    }

    #[test]
    fn the_bundle_hash_ignores_formatting_and_sees_content() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let b = load_bundle(&fixture, &Limits::DESIGN).unwrap();
        let text = std::fs::read_to_string(&fixture).unwrap();
        let reformatted: serde_json::Value = serde_json::from_str(&text).unwrap();
        let again =
            SeedBundle::from_json(&serde_json::to_string_pretty(&reformatted).unwrap()).unwrap();
        assert_eq!(bundle_hash(&b).unwrap(), bundle_hash(&again).unwrap());
        let mut changed = b.clone();
        changed.race_mode ^= 1;
        assert_ne!(bundle_hash(&b).unwrap(), bundle_hash(&changed).unwrap());
        assert_eq!(bundle_hash(&b).unwrap().len(), 32);
    }
}
