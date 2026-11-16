//! End-to-end helpers: start the real `constellation-server` binary on an ephemeral loopback
//! port and talk to it over real WebSocket connections (`docs/TESTING.md`, end-to-end rules).
//!
//! No sleeps: a server is ready when it logs its bound address, and every wait is on a frame
//! or a process event, with a timeout only as a guard against a hang.

use futures_util::SinkExt;
use seed_bundle::SeedBundle;
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
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

/// The server binary, built once per test process with the cargo that built this crate, into
/// the same target directory and profile (a no-op when it is fresh).
fn binary() -> Res<PathBuf> {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            // <target>/<profile dir>/deps/<test binary>
            let profile_dir = exe.parent().and_then(Path::parent).ok_or("odd test path")?;
            let target = profile_dir.parent().ok_or("odd test path")?;
            let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
                Some("debug") | None => "dev",
                Some(other) => other,
            };
            let status = Command::new(env!("CARGO"))
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .args(["build", "--quiet", "--locked", "-p", "server"])
                .args(["--bin", "constellation-server"])
                .arg("--profile")
                .arg(profile)
                .arg("--target-dir")
                .arg(target)
                .status()
                .map_err(|e| format!("cargo build: {e}"))?;
            if !status.success() {
                return Err(format!("cargo build -p server failed: {status}"));
            }
            Ok(profile_dir.join("constellation-server"))
        })
        .clone()
}

/// A running server process. Killed on drop if not stopped.
#[derive(Debug)]
pub struct Server {
    child: Child,
    /// Where it listens.
    pub addr: SocketAddr,
    log: Arc<(Mutex<Vec<String>>, Condvar)>,
}

impl Server {
    /// Start the server on the fixture with extra arguments, and wait until it listens.
    ///
    /// # Errors
    /// When the binary cannot be built or started, or never logs its address.
    pub fn start(extra: &[&str]) -> Res<Self> {
        Self::start_env(extra, &[])
    }

    /// As [`Server::start`], with environment variables for the server process (how `config`
    /// reads limits and secrets).
    ///
    /// # Errors
    /// As [`Server::start`].
    pub fn start_env(extra: &[&str], env: &[(&str, &str)]) -> Res<Self> {
        let mut child = Command::new(binary()?)
            .arg("--bundle")
            .arg(fixture_path())
            .args(["--port", "0"])
            .args(extra)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn server: {e}"))?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let log = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        let sink = Arc::clone(&log);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(addr) = listening_addr(&line) {
                    let _ = tx.send(addr);
                }
                sink.0.lock().unwrap_or_else(PoisonError::into_inner).push(line);
                sink.1.notify_all();
            }
        });
        let addr = rx.recv_timeout(GUARD).map_err(|_| {
            let lines = log.0.lock().unwrap_or_else(PoisonError::into_inner).join("\n");
            format!("server never logged its address; log:\n{lines}")
        })?;
        Ok(Self { child, addr, log })
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
        let pid = self.child.id().to_string();
        let sent =
            Command::new("kill").args(["-TERM", &pid]).status().map_err(|e| e.to_string())?;
        if !sent.success() {
            return Err("kill -TERM failed".to_owned());
        }
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
