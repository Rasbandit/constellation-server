//! End-to-end helpers (a dev-dependency of `server`, whose integration tests in
//! `crates/server/tests/` hold the scenarios): start the real `constellation-server` binary on an ephemeral loopback
//! port and talk to it over real WebSocket connections (`docs/TESTING.md`, end-to-end rules).
//!
//! No sleeps: a server is ready when it logs its bound address, and every wait is on a frame
//! or a process event, with a timeout only as a guard against a hang.

pub mod synth;

use futures_util::SinkExt;
use seed_bundle::SeedBundle;
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;
use tokio::net::TcpStream;
use yawc::{Frame, MaybeTlsStream, OpCode, Options, WebSocket};

/// Helper failures, as text for the test's panic message.
pub type Res<T> = Result<T, String>;

/// How long any single wait may take before the test fails instead of hanging.
pub const GUARD: Duration = Duration::from_secs(20);

/// The committed two-slot fixture bundle.
#[must_use]
pub fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/two-slot.json")
}

/// The fixture, parsed with the schema crate (the expected values in tests come from it).
///
/// # Errors
/// When the fixture is missing or invalid.
pub fn fixture() -> Res<SeedBundle> {
    let text = std::fs::read_to_string(fixture_path()).map_err(|e| e.to_string())?;
    SeedBundle::from_json(&text).map_err(|e| e.to_string())
}

/// A running server process. Killed on drop if not stopped.
#[derive(Debug)]
pub struct Server {
    child: Child,
    /// Where it listens.
    pub addr: SocketAddr,
    log: Arc<(Mutex<Vec<String>>, Condvar)>,
    /// Set when the server's stderr closes (it exited), notified on the log's condvar.
    closed: Arc<AtomicBool>,
}

impl Server {
    /// Start the server binary `bin` on the fixture with extra arguments and environment
    /// variables (how `config` reads limits and secrets), and wait until it listens. Cargo
    /// builds and names the binary: integration tests of the `server` package pass
    /// `env!("CARGO_BIN_EXE_constellation-server")`.
    ///
    /// # Errors
    /// When the binary cannot be started or never logs its address.
    pub fn start(bin: &Path, extra: &[&str], env: &[(&str, &str)]) -> Res<Self> {
        Self::spawn(command(bin, extra, env))
    }

    /// Like [`Server::start`], but with every seed bundle in the folder `rooms` (`--rooms`)
    /// instead of the fixture, then the `extra` arguments.
    ///
    /// # Errors
    /// When the binary cannot be started or never logs its address.
    pub fn start_rooms(
        bin: &Path,
        rooms: &Path,
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> Res<Self> {
        let mut c = base_command(bin, env);
        c.arg("--rooms").arg(rooms).args(["--port", "0"]).args(extra);
        Self::spawn(c)
    }

    fn spawn(mut command: Command) -> Res<Self> {
        let mut child = command.spawn().map_err(|e| format!("spawn server: {e}"))?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let log = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        let sink = Arc::clone(&log);
        let closed = Arc::new(AtomicBool::new(false));
        let eof = Arc::clone(&closed);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let addr = listening_addr(&line);
                sink.0.lock().unwrap_or_else(PoisonError::into_inner).push(line);
                sink.1.notify_all();
                // After the push: a started server's log always holds its listening line.
                if let Some(addr) = addr {
                    let _ = tx.send(addr);
                }
            }
            let _guard = sink.0.lock().unwrap_or_else(PoisonError::into_inner);
            eof.store(true, Ordering::SeqCst);
            sink.1.notify_all();
        });
        let addr = rx.recv_timeout(GUARD).map_err(|_| {
            let lines = log.0.lock().unwrap_or_else(PoisonError::into_inner).join("\n");
            format!("server never logged its address; log:\n{lines}")
        })?;
        Ok(Self { child, addr, log, closed })
    }

    /// `ws://` URL of the edge.
    #[must_use]
    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Every log line so far.
    #[must_use]
    pub fn log(&self) -> Vec<String> {
        self.log.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Wait until a log line contains `needle` (the signal for server-side events), and return
    /// it.
    ///
    /// # Errors
    /// When no such line appears within the guard timeout.
    pub fn wait_for_log(&self, needle: &str) -> Res<String> {
        let (lines, cv) = &*self.log;
        let (guard, _) = cv
            .wait_timeout_while(lines.lock().unwrap_or_else(PoisonError::into_inner), GUARD, |l| {
                !l.iter().any(|x| x.contains(needle))
            })
            .unwrap_or_else(PoisonError::into_inner);
        let found = guard.iter().find(|x| x.contains(needle)).cloned();
        let all = guard.join("\n");
        drop(guard);
        found.ok_or_else(|| format!("no log line with {needle:?}; log:\n{all}"))
    }

    /// Send SIGTERM and wait for the exit.
    ///
    /// # Errors
    /// When the signal cannot be sent or the process does not exit.
    pub fn stop(mut self) -> Res<ExitStatus> {
        sigterm(self.child.id())?;
        self.child.wait().map_err(|e| e.to_string())
    }

    /// Wait for the server to exit on its own (its stderr closing is the signal) and return
    /// the status.
    ///
    /// # Errors
    /// When it is still running after the guard timeout, with its log.
    pub fn wait_exit(mut self) -> Res<ExitStatus> {
        let (lines, cv) = &*self.log;
        let (guard, _) = cv
            .wait_timeout_while(lines.lock().unwrap_or_else(PoisonError::into_inner), GUARD, |_| {
                !self.closed.load(Ordering::SeqCst)
            })
            .unwrap_or_else(PoisonError::into_inner);
        let all = guard.join("\n");
        drop(guard);
        if !self.closed.load(Ordering::SeqCst) {
            return Err(format!("server still running after {GUARD:?}; log:\n{all}"));
        }
        self.child.wait().map_err(|e| e.to_string())
    }

    /// `kill -9`: SIGKILL, no shutdown path runs. Returns once the process is gone.
    ///
    /// # Errors
    /// When the signal cannot be sent or the wait fails.
    pub fn kill(mut self) -> Res<ExitStatus> {
        // std's `Child::kill` sends SIGKILL on Unix.
        self.child.kill().map_err(|e| e.to_string())?;
        self.child.wait().map_err(|e| e.to_string())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Already exited after `stop`, or a failed test: either way make sure it is gone.
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

/// The server command `Server::start` runs: the fixture on an ephemeral port, `extra`
/// arguments, only the database variables in `env`, stderr piped. For a scenario that must
/// not wait for the server to listen.
#[must_use]
pub fn command(bin: &Path, extra: &[&str], env: &[(&str, &str)]) -> Command {
    let mut c = base_command(bin, env);
    c.arg("--bundle").arg(fixture_path()).args(["--port", "0"]).args(extra);
    c
}

/// The server binary with only the database variables in `env` and stderr piped, no
/// arguments yet.
fn base_command(bin: &Path, env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(bin);
    // A scenario says which database it uses; it never inherits one (CI sets the URL
    // for the Postgres tests, and parallel servers must not share a room).
    c.env_remove("CONSTELLATION_DATABASE_URL")
        .env_remove("CONSTELLATION_DATABASE_URL_FILE")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    c
}

/// Send SIGTERM to process `pid` (what a supervisor's stop does).
///
/// # Errors
/// When `kill` cannot run or fails.
pub fn sigterm(pid: u32) -> Res<()> {
    let sent = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map_err(|e| e.to_string())?;
    if sent.success() { Ok(()) } else { Err("kill -TERM failed".to_owned()) }
}

fn listening_addr(line: &str) -> Option<SocketAddr> {
    let (_, rest) = line.split_once("listening addr=")?;
    rest.split_whitespace().next()?.parse().ok()
}

/// A protocol client on yawc's WebSocket client.
pub struct Client {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    queue: VecDeque<Value>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("queued", &self.queue.len()).finish_non_exhaustive()
    }
}

impl Client {
    /// Connect with permessage-deflate offered.
    ///
    /// # Errors
    /// When the handshake fails.
    pub async fn connect(url: &str) -> Res<Self> {
        let url = url.parse().map_err(|e| format!("{url}: {e}"))?;
        let opts = Options::default().with_low_latency_compression();
        let ws = tokio::time::timeout(GUARD, WebSocket::connect(url).with_options(opts))
            .await
            .map_err(|_| "connect timed out".to_owned())?
            .map_err(|e| format!("connect: {e}"))?;
        Ok(Self { ws, queue: VecDeque::new() })
    }

    /// Open TCP to `addr` and do the WebSocket handshake for `url`, so the request's `Host`
    /// header is `url`'s host without that name resolving anywhere.
    ///
    /// # Errors
    /// When the connection or the handshake fails.
    pub async fn connect_via(addr: SocketAddr, url: &str) -> Res<Self> {
        let url = url.parse().map_err(|e| format!("{url}: {e}"))?;
        let opts = Options::default().with_low_latency_compression();
        let handshake = async {
            let tcp = TcpStream::connect(addr).await.map_err(|e| format!("tcp: {e}"))?;
            WebSocket::handshake(url, MaybeTlsStream::Plain(tcp), opts)
                .await
                .map_err(|e| format!("handshake: {e}"))
        };
        let ws = tokio::time::timeout(GUARD, handshake)
            .await
            .map_err(|_| "connect timed out".to_owned())??;
        Ok(Self { ws, queue: VecDeque::new() })
    }

    /// Send one frame holding these packets.
    ///
    /// # Errors
    /// When the socket fails.
    pub async fn send(&mut self, packets: &Value) -> Res<()> {
        self.send_text(&packets.to_string()).await
    }

    /// Send raw text.
    ///
    /// # Errors
    /// When the socket fails.
    pub async fn send_text(&mut self, text: &str) -> Res<()> {
        self.ws.send(Frame::text(text.to_owned())).await.map_err(|e| e.to_string())
    }

    /// The next packet, in arrival order.
    ///
    /// # Errors
    /// On a closed socket, a non-JSON frame or the guard timeout.
    pub async fn next(&mut self) -> Res<Value> {
        while self.queue.is_empty() {
            let frame = tokio::time::timeout(GUARD, self.ws.next_frame())
                .await
                .map_err(|_| "no frame within the guard timeout".to_owned())?
                .map_err(|e| format!("socket: {e}"))?;
            match frame.opcode() {
                OpCode::Text => match serde_json::from_slice(frame.payload()) {
                    Ok(Value::Array(v)) => self.queue.extend(v),
                    Ok(other) => return Err(format!("frame is not a list: {other}")),
                    Err(e) => return Err(format!("frame is not JSON: {e}")),
                },
                OpCode::Close => return Err("server closed the connection".to_owned()),
                _ => {}
            }
        }
        self.queue.pop_front().ok_or_else(|| "empty queue".to_owned())
    }

    /// Read until `n` pings from the server have arrived (data frames are queued, not lost).
    ///
    /// # Errors
    /// As [`Client::next`].
    pub async fn wait_pings(&mut self, n: usize) -> Res<()> {
        let mut seen = 0;
        while seen < n {
            let frame = tokio::time::timeout(GUARD, self.ws.next_frame())
                .await
                .map_err(|_| "no frame within the guard timeout".to_owned())?
                .map_err(|e| format!("socket: {e}"))?;
            match frame.opcode() {
                OpCode::Ping => seen = seen.saturating_add(1),
                OpCode::Text => match serde_json::from_slice(frame.payload()) {
                    Ok(Value::Array(v)) => self.queue.extend(v),
                    other => return Err(format!("unexpected frame: {other:?}")),
                },
                OpCode::Close => return Err("server closed the connection".to_owned()),
                _ => {}
            }
        }
        Ok(())
    }

    /// The next packet with this `cmd`, skipping (and returning) others before it.
    ///
    /// # Errors
    /// As [`Client::next`].
    pub async fn next_cmd(&mut self, cmd: &str) -> Res<(Value, Vec<Value>)> {
        let mut skipped = Vec::new();
        loop {
            let p = self.next().await?;
            if p.get("cmd").and_then(Value::as_str) == Some(cmd) {
                return Ok((p, skipped));
            }
            skipped.push(p);
        }
    }

    /// Send one `LocationChecks` for these locations.
    ///
    /// # Errors
    /// When the socket fails.
    pub async fn check(&mut self, locations: &[i64]) -> Res<()> {
        self.send(&serde_json::json!([{"cmd": "LocationChecks", "locations": locations}])).await
    }

    /// Close politely.
    ///
    /// # Errors
    /// When the socket fails.
    pub async fn close(mut self) -> Res<()> {
        self.ws
            .send(Frame::close(yawc::close::CloseCode::Normal, "bye"))
            .await
            .map_err(|e| e.to_string())
    }
}

/// The fixture's locations in `from`'s world whose item goes to `to`, as (location, item,
/// flags), in fixture order.
///
/// # Errors
/// When the fixture is missing or has no slot `from`.
pub fn locations_for(from: u32, to: u32) -> Res<Vec<(i64, i64, u8)>> {
    let b = fixture()?;
    let slot = b.slot(from).ok_or_else(|| format!("no slot {from}"))?;
    Ok(slot
        .locations
        .iter()
        .filter(|p| p.receiver == to)
        .map(|p| (p.location, p.item, p.flags))
        .collect())
}

/// The fixture's starting inventory for `slot` as the `NetworkItem`s a client that asks for it
/// gets first (protocol doc, "Data Package Contents": location -2, player 0).
///
/// # Errors
/// When the fixture is missing or has no slot `slot`.
pub fn start_items(slot: u32) -> Res<Vec<Value>> {
    let b = fixture()?;
    let s = b.slot(slot).ok_or_else(|| format!("no slot {slot}"))?;
    Ok(s.start_inventory
        .iter()
        .map(|i| serde_json::json!({"class": "NetworkItem", "item": i, "location": -2, "player": 0, "flags": 0}))
        .collect())
}

/// Connect to `url` as slot `name` (no password) and return the client, the `Connected`
/// packet and anything that came before it.
///
/// # Errors
/// When the socket fails or the first packet is not `RoomInfo`.
pub async fn join(url: &str, name: &str) -> Res<(Client, Value, Vec<Value>)> {
    join_client(Client::connect(url).await?, name).await
}

/// `GET path` with this `Host` header on a fresh connection (as a WebSocket upgrade when
/// `upgrade`); the whole response, read up to its `content-length` (the server may keep the
/// connection open).
///
/// # Panics
/// When the connection fails or the response takes longer than [`GUARD`].
pub async fn http_get(addr: SocketAddr, host: &str, path: &str, upgrade: bool) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    let ws = if upgrade {
        "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
    } else {
        "Connection: close\r\n"
    };
    tcp.write_all(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{ws}\r\n").as_bytes())
        .await
        .unwrap();
    let mut got = Vec::new();
    let mut buf = [0_u8; 1024];
    loop {
        let text = String::from_utf8_lossy(&got).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let len = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)
                })
                .and_then(|v| v.trim().parse::<usize>().ok());
            if len.is_some_and(|n| body.len() >= n) {
                return text;
            }
        }
        let n = tokio::time::timeout(GUARD, tcp.read(&mut buf)).await.unwrap().unwrap();
        if n == 0 {
            return text;
        }
        got.extend_from_slice(buf.get(..n).unwrap());
    }
}

/// [`join`] through [`Client::connect_via`]: TCP to `addr`, handshake for `url`.
///
/// # Errors
/// As [`join`].
pub async fn join_via(addr: SocketAddr, url: &str, name: &str) -> Res<(Client, Value, Vec<Value>)> {
    join_client(Client::connect_via(addr, url).await?, name).await
}

async fn join_client(mut c: Client, name: &str) -> Res<(Client, Value, Vec<Value>)> {
    let info = c.next().await?;
    if info.get("cmd").and_then(Value::as_str) != Some("RoomInfo") {
        return Err(format!("first packet is not RoomInfo: {info}"));
    }
    c.send(&connect_packet(name, None)).await?;
    let (connected, skipped) = c.next_cmd("Connected").await?;
    Ok((c, connected, skipped))
}

/// A `Connect` packet for the fixture's game.
#[must_use]
pub fn connect_packet(name: &str, password: Option<&str>) -> Value {
    serde_json::json!([{
        "cmd": "Connect", "password": password, "game": "Archipela-Go 2: Electric Boogaloo",
        "name": name, "uuid": "e2e", "items_handling": 7, "tags": ["AP"], "slot_data": true,
        "version": {"major": 0, "minor": 6, "build": 8, "class": "Version"}
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listening_line_yields_the_address_and_others_do_not() {
        let line = "2026-11-16T21:43:09Z  INFO server: listening addr=127.0.0.1:38441 seed=\"1\"";
        assert_eq!(listening_addr(line), "127.0.0.1:38441".parse().ok());
        assert_eq!(listening_addr("INFO server: websocket accepted deflate=true"), None);
        assert_eq!(listening_addr("listening addr=nonsense"), None);
    }
}

#[cfg(test)]
mod lint_copy {
    /// `crates/e2e-ap` is its own workspace (it needs the sibling checkout) and carries a copy
    /// of the workspace lints; this keeps the copy honest.
    #[test]
    fn the_e2e_ap_lints_match_the_workspace() {
        let root = include_str!("../../../Cargo.toml");
        let ap = include_str!("../../e2e-ap/Cargo.toml");
        let section = |text: &str, start: &str| {
            text.split_once(start).map(|(_, rest)| rest.trim().to_owned())
        };
        let want = section(root, "[workspace.lints.rust]")
            .unwrap()
            .replace("[workspace.lints.", "[lints.");
        assert_eq!(section(ap, "[lints.rust]").unwrap(), want);
    }
}
