//! The socket loop: one task per connection, reading frames and the room's stream.
//!
//! Cancellation (doc 11 R5): both `select!` arms are cancel safe. `WebSocket::next_frame` keeps
//! partial frames inside the `WebSocket` value, not in the future (yawc's documented message
//! API; not verified under fault injection yet), and `mpsc::Receiver::recv` is cancel safe
//! per tokio's docs. Sends happen outside `select!`.

use crate::{CompatRoom, Session};
use config::Limits;
use futures_util::SinkExt;
use runtime::Published;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{Instant, MissedTickBehavior};
use yawc::{CompressionLevel, DeflatePool, Frame, OpCode, Options, WebSocket, close::CloseCode};

/// Socket options for this edge (doc 11 R1: every buffer and cap set here, from `config`;
/// D-97: permessage-deflate on, pooled compressors, `server_no_context_takeover`). Also
/// `client_no_context_takeover` (RFC 7692 7.1.1.1: every client must honour it, offered or
/// not): each client message inflates alone, so a connection holds no inflate state between
/// messages; it borrows one from the pool. Measured: about 40 KB less per connection.
#[must_use]
pub fn ws_options(limits: &Limits, pool: DeflatePool) -> Options {
    Options::default()
        .with_limits(limits.compat_max_frame_bytes, limits.compat_max_message_bytes)
        .with_read_buffer_capacity(limits.ws_read_buffer_bytes)
        .with_write_buffer_capacity(limits.ws_write_buffer_bytes)
        .with_compression_level(CompressionLevel::fast())
        .server_no_context_takeover()
        .client_no_context_takeover()
        .with_deflate_pool(pool)
        .with_utf8()
}

/// Stands in for "no deadline" when the configured one is too far to represent.
const A_YEAR: Duration = Duration::from_hours(8760);

enum Input {
    Frame(yawc::Result<Frame>),
    ConnectTimeout,
    Tick,
    /// `None`: the room dropped our queue (we fell behind) or stopped.
    Live(Option<Arc<Published>>),
}

async fn next_live(session: &mut Session) -> Option<Arc<Published>> {
    match session.live() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Serve one connection from `peer` until either side ends it.
pub async fn serve<S>(mut ws: WebSocket<S>, room: Arc<CompatRoom>, peer: IpAddr)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let limits = room.limits;
    let mut session = Session::with_peer(room, peer);
    if ws.send(Frame::text(session.hello())).await.is_err() {
        return;
    }
    let start = Instant::now();
    // A deadline too far to represent means: no deadline (a year is far enough).
    let connect_by = start
        .checked_add(Duration::from_millis(limits.connect_timeout_ms))
        .unwrap_or_else(|| start.checked_add(A_YEAR).unwrap_or(start));
    let idle = Duration::from_millis(limits.idle_timeout_ms);
    let mut ping = tokio::time::interval(Duration::from_millis(limits.ping_interval_ms.max(1)));
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut heard = Instant::now();
    loop {
        let input = tokio::select! {
            f = ws.next_frame() => Input::Frame(f),
            p = next_live(&mut session) => Input::Live(p),
            () = tokio::time::sleep_until(connect_by), if !session.is_joined() => Input::ConnectTimeout,
            _ = ping.tick() => Input::Tick,
        };
        if matches!(input, Input::Frame(Ok(_))) {
            heard = Instant::now();
        }
        let out = match input {
            Input::ConnectTimeout => {
                tracing::info!("connect timeout: no slot connected in time, closing");
                drop(ws.send(Frame::close(CloseCode::Policy, "connect timeout")).await);
                break;
            }
            Input::Tick => {
                if heard.elapsed() >= idle {
                    tracing::info!("idle timeout: nothing from the client, closing");
                    break;
                }
                if ws.send(Frame::ping(Vec::new())).await.is_err() {
                    break;
                }
                continue;
            }
            Input::Frame(Ok(frame)) => {
                // yawc answers pings and closes itself; only data frames reach the session.
                if !matches!(frame.opcode(), OpCode::Text | OpCode::Binary) {
                    continue;
                }
                // Not `Frame::as_str`, which panics on bad UTF-8 in a binary frame.
                let Ok(text) = std::str::from_utf8(frame.payload()) else { break };
                session.on_text(text).await
            }
            Input::Frame(Err(e)) => {
                tracing::debug!(error = %e, "connection ended");
                break;
            }
            Input::Live(Some(p)) => session.on_published(&p).await,
            // Doc 11 R3: resync from our cursor; fails only when the room is gone.
            Input::Live(None) => session.resync().await,
        };
        let Ok(frames) = out else {
            drop(ws.send(Frame::close(CloseCode::Away, "room unavailable")).await);
            break;
        };
        for text in frames {
            if ws.send(Frame::text(text)).await.is_err() {
                session.leave().await;
                return;
            }
        }
    }
    session.leave().await;
}
