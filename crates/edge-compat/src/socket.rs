//! The socket loop: one task per connection, reading frames and the room's stream.
//!
//! Cancellation (doc 11 R5): both `select!` arms are cancel safe. `WebSocket::next_frame` keeps
//! partial frames inside the `WebSocket` value, not in the future (yawc's documented message
//! API; not verified under fault injection yet), and `broadcast::Receiver::recv` is cancel safe
//! per tokio's docs. Sends happen outside `select!`.

use crate::{CompatRoom, Session};
use config::Limits;
use futures_util::SinkExt;
use runtime::Published;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast::error::RecvError;
use yawc::{CompressionLevel, DeflatePool, Frame, OpCode, Options, WebSocket, close::CloseCode};

/// Socket options for this edge (doc 11 R1: every buffer and cap set here, from `config`;
/// D-97: permessage-deflate on, pooled compressors, `server_no_context_takeover`).
#[must_use]
pub fn ws_options(limits: &Limits, pool: DeflatePool) -> Options {
    Options::default()
        .with_limits(limits.compat_max_frame_bytes, limits.compat_max_message_bytes)
        .with_read_buffer_capacity(limits.ws_read_buffer_bytes)
        .with_write_buffer_capacity(limits.ws_write_buffer_bytes)
        .with_compression_level(CompressionLevel::fast())
        .server_no_context_takeover()
        .with_deflate_pool(pool)
        .with_utf8()
}

enum Input {
    Frame(yawc::Result<Frame>),
    Live(Result<Arc<Published>, RecvError>),
}

async fn next_live(session: &mut Session) -> Result<Arc<Published>, RecvError> {
    match session.live() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Serve one connection until either side ends it.
pub async fn serve<S>(mut ws: WebSocket<S>, room: Arc<CompatRoom>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut session = Session::new(room);
    if ws.send(Frame::text(session.hello())).await.is_err() {
        return;
    }
    loop {
        let input = tokio::select! {
            f = ws.next_frame() => Input::Frame(f),
            p = next_live(&mut session) => Input::Live(p),
        };
        let out = match input {
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
            Input::Live(Ok(p)) => session.on_published(&p),
            Input::Live(Err(RecvError::Lagged(n))) => {
                // Doc 11 R3: drop the slow session; the client resyncs from its item index.
                tracing::warn!(missed = n, "session fell behind the room, closing it");
                drop(ws.send(Frame::close(CloseCode::Again, "fell behind, reconnect")).await);
                break;
            }
            Input::Live(Err(RecvError::Closed)) => break,
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
