//! The host's read-only view of one running room over HTTP (issue #40): its summary as JSON and
//! its events as server-sent events. The data is the room's own ([`CompatRoom::summary`],
//! [`CompatRoom::watch`]); this module only frames it.

use crate::rooms::Visit;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use edge_compat::CompatRoom;
use proto::Seq;
use runtime::Published;
use serde_json::Value;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;

/// One SSE client's place in the room's stream.
struct Watch {
    room: Arc<CompatRoom>,
    live: mpsc::Receiver<Arc<Published>>,
    /// The last durable event sent (the SSE `id`), to resubscribe from after falling behind.
    cursor: Seq,
    queue: VecDeque<(Option<u64>, Value)>,
    /// Keeps the room in memory while the stream is open.
    _visit: Visit,
}

impl Watch {
    fn take(&mut self, p: &Published) {
        let durable = matches!(p, Published::Events { .. });
        for v in self.room.describe(p) {
            let id = v["seq"].as_u64().filter(|_| durable);
            self.queue.push_back((id, v));
        }
        if let Published::Events { events, .. } = p
            && let Some(last) = events.last()
        {
            self.cursor = last.seq;
        }
    }

    fn history(&mut self, history: Vec<proto::Event>) {
        if history.is_empty() {
            return;
        }
        let events: Arc<[proto::Event]> = history.into();
        self.take(&Published::Events { events, notes: Arc::from([]) });
    }
}

/// The room's events after `after` (the SSE `Last-Event-ID`, 0 for everything), then live ones.
/// Durable events carry their room `seq` as the SSE id; joins, leaves and chat carry none. A
/// client that falls behind is resubscribed from its last durable event, so it loses no durable
/// event (live joins, leaves and chat in between are skipped, as for every subscriber).
///
/// # Errors
/// When the room task has stopped.
pub(crate) async fn events(
    room: Arc<CompatRoom>,
    visit: Visit,
    after: Seq,
) -> Result<Response, String> {
    let sub = room.watch(after).await.map_err(|e| format!("room unavailable: {e:?}"))?;
    let mut w =
        Watch { room, live: sub.live, cursor: after, queue: VecDeque::new(), _visit: visit };
    w.history(sub.history);
    let stream = futures_util::stream::unfold(w, |mut w| async move {
        loop {
            if let Some((id, v)) = w.queue.pop_front() {
                let mut e = SseEvent::default().data(v.to_string());
                if let Some(id) = id {
                    e = e.id(id.to_string());
                }
                return Some((Ok::<_, Infallible>(e), w));
            }
            if let Some(p) = w.live.recv().await {
                w.take(&p);
            } else {
                // Fell behind, or the room stopped (then the stream ends).
                let sub = w.room.watch(w.cursor).await.ok()?;
                w.live = sub.live;
                w.history(sub.history);
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()).into_response())
}
