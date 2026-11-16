//! The composition root (D-99: one process). Loads a seed bundle, starts its room on
//! `room-core` and the in-memory log, and serves the Archipelago-compatible edge over HTTP:
//! a WebSocket at `/` (yawc, D-97) and `GET /healthz` (axum).

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use config::Limits;
use edge_compat::CompatRoom;
use log::MemLogStore;
use proto::RoomId;
use room_core::RoomState;
use runtime::RoomHandle;
use seed_bundle::SeedBundle;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
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
    /// Room password, overriding the bundle's.
    pub password: Option<String>,
}

/// How to run the binary.
pub const USAGE: &str =
    "usage: constellation-server --bundle <path> --port <port> [--host <ip>] [--password <pw>]";

impl Args {
    /// Parse the arguments after the program name.
    ///
    /// # Errors
    /// A message naming the bad or missing argument.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let (mut bundle, mut port, mut host, mut password) = (None, None, None, None);
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--bundle" => bundle = Some(PathBuf::from(value()?)),
                "--port" => port = Some(value()?.parse().map_err(|_| "--port needs a number")?),
                "--host" => host = Some(value()?.parse().map_err(|_| "--host needs an IP")?),
                "--password" => password = Some(value()?),
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Self {
            bundle: bundle.ok_or("--bundle is required")?,
            port: port.ok_or("--port is required")?,
            host: host.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            password,
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
}

/// Start the room for `bundle` and build the router.
///
/// # Errors
/// A message when the bundle does not make a valid room.
pub fn app(bundle: SeedBundle, password: Option<String>, limits: Limits) -> Result<Router, String> {
    let store = Arc::new(MemLogStore::new());
    let room_id = RoomId(1);
    let epoch = store.claim(room_id);
    let state = RoomState::new(bundle.slot_nos(), bundle.placement()).map_err(|e| e.to_string())?;
    // The task ends with the process; a stopped room answers every request with Unavailable.
    let (handle, _task) = RoomHandle::spawn(room_id, epoch, state, store, limits);
    let room = Arc::new(CompatRoom::new(Arc::new(bundle), handle, password)?);
    let options = edge_compat::ws_options(&limits, DeflatePool::new(limits.ws_deflate_pool));
    Ok(Router::new()
        .route("/", get(upgrade))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(AppState { room, options }))
}

async fn upgrade(State(s): State<AppState>, ws: IncomingUpgrade) -> Response {
    let (response, fut) = match ws.upgrade(s.options) {
        Ok(pair) => pair,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let deflate = response.headers().contains_key(header::SEC_WEBSOCKET_EXTENSIONS);
    tracing::info!(deflate, "websocket accepted");
    tokio::spawn(async move {
        match fut.await {
            Ok(socket) => edge_compat::serve(socket, s.room).await,
            Err(e) => tracing::debug!(error = %e, "upgrade did not complete"),
        }
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
pub async fn run(args: Args, limits: Limits) -> Result<(), String> {
    let bundle = load_bundle(&args.bundle, &limits)?;
    let seed = bundle.seed_name.clone();
    let router = app(bundle, args.password, limits)?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(args.host, args.port))
        .await
        .map_err(|e| format!("bind: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    tracing::info!(%addr, seed, "listening");
    axum::serve(listener, router)
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
        assert_eq!((a.port, a.host, a.password), (0, IpAddr::V4(Ipv4Addr::LOCALHOST), None));
        let a = parse("--port 38281 --host 0.0.0.0 --password pw --bundle x").unwrap();
        assert_eq!((a.port, a.password.as_deref()), (38281, Some("pw")));
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
}
