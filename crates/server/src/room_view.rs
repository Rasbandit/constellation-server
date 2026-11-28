//! The host's read-only view of one running room over HTTP (issue #40): its events as
//! server-sent events. The data is the room's own ([`CompatRoom::watch`],
//! [`CompatRoom::describe`]); this module only frames it.

use crate::rooms::Visit;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use edge_compat::CompatRoom;
use futures_util::Stream;
use proto::Seq;
use runtime::{Published, RoomError};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, mpsc};

/// What a view stream holds while it is open: it keeps the room in memory and counts against
/// `view_streams`.
pub(crate) struct Held {
    pub(crate) _visit: Visit,
    pub(crate) _permit: OwnedSemaphorePermit,
}

/// One viewer's place in the room's stream.
struct Watch {
    room: Arc<CompatRoom>,
    live: mpsc::Receiver<Arc<Published>>,
    /// The last durable event taken (the SSE `id`), to resubscribe from after falling behind.
    cursor: Seq,
    /// Most durable events replayed on a (re)subscribe (`view_history_events`).
    cap: u64,
    /// Entries to send, with the SSE id of the durable ones.
    queue: VecDeque<(Option<u64>, Value)>,
    _held: Option<Held>,
}

impl Watch {
    /// Subscribe after `after`. A reset (the room restarted, or `after` is too far behind) is
    /// sent first as `{"kind": "reset"}`, so the viewer drops what it showed and starts over.
    async fn open(
        room: Arc<CompatRoom>,
        after: Seq,
        cap: u64,
        held: Option<Held>,
    ) -> Result<Self, RoomError> {
        let mut w = Self {
            room,
            live: mpsc::channel(1).1,
            cursor: after,
            cap,
            queue: VecDeque::new(),
            _held: held,
        };
        w.subscribe().await?;
        Ok(w)
    }

    async fn subscribe(&mut self) -> Result<(), RoomError> {
        let (sub, start, reset) = self.room.watch(self.cursor, self.cap).await?;
        if reset {
            self.queue.push_back((None, json!({"kind": "reset"})));
        }
        self.cursor = start;
        self.live = sub.live;
        if !sub.history.is_empty() {
            let events: Arc<[proto::Event]> = sub.history.into();
            self.take(&Published::Events { events, notes: Arc::from([]) });
        }
        Ok(())
    }

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

    /// The next entry; `None` once the room has stopped.
    async fn next(&mut self) -> Option<(Option<u64>, Value)> {
        loop {
            if let Some(e) = self.queue.pop_front() {
                return Some(e);
            }
            if let Some(p) = self.live.recv().await {
                self.take(&p);
            } else {
                // Fell behind (resume from the cursor), or the room stopped (end).
                self.subscribe().await.ok()?;
            }
        }
    }
}

/// The room's entries after `after` (0 for a new view), then live ones, as a stream.
async fn entries(
    room: Arc<CompatRoom>,
    after: Seq,
    cap: u64,
    held: Option<Held>,
) -> Result<impl Stream<Item = (Option<u64>, Value)>, RoomError> {
    let w = Watch::open(room, after, cap, held).await?;
    Ok(futures_util::stream::unfold(w, |mut w| async move { w.next().await.map(|e| (e, w)) }))
}

/// The room's events as server-sent events: durable ones carry their room `seq` as the SSE id,
/// joins, leaves, chat and resets carry none. A viewer that falls behind is resubscribed from
/// its last durable event; one that resumes past the head (the room restarted) or more than
/// `cap` events back gets a reset and the newest `cap` events. The stream ends when `until`
/// completes (the viewer lost access).
///
/// # Errors
/// When the room task has stopped.
pub(crate) async fn events(
    room: Arc<CompatRoom>,
    held: Held,
    after: Seq,
    cap: u64,
    until: impl Future<Output = ()> + Send + 'static,
) -> Result<Response, RoomError> {
    use futures_util::StreamExt;
    let stream = entries(room, after, cap, Some(held)).await?.take_until(until).map(|(id, v)| {
        let e = SseEvent::default().data(v.to_string());
        Ok::<_, Infallible>(match id {
            Some(id) => e.id(id.to_string()),
            None => e,
        })
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;
    use crate::{Source, app};
    use config::Limits;
    use futures_util::StreamExt;

    /// The e2e fixture's room, in memory, with its handle to make events in it.
    async fn fixture_room() -> (Host, Arc<CompatRoom>, runtime::RoomHandle) {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../seed-bundle/fixtures/two-slot.json");
        let (_, host) = app(&Source::Bundle(fixture), None, None, Limits::DESIGN).await.unwrap();
        let rooms = host.rooms();
        let room = crate::rooms::read(&rooms).iter().next().unwrap().1.room.clone().unwrap();
        (host, Arc::clone(&room.compat), room.live)
    }

    /// Check `n` of Alice's (slot 1) locations straight through the room handle.
    async fn checks(room: &runtime::RoomHandle, n: usize) {
        let locations = e2e::locations_for(1, 2).unwrap();
        assert!(locations.len() >= n, "the fixture has {} such locations", locations.len());
        for (location, _, _) in locations.into_iter().take(n) {
            let cid = proto::Cid(location.unsigned_abs());
            room.command(proto::Command::Check { slot: proto::SlotNo(1), cid, location })
                .await
                .unwrap()
                .unwrap();
        }
    }

    async fn take(
        s: &mut (impl Stream<Item = (Option<u64>, Value)> + Unpin),
        n: usize,
    ) -> Vec<(Option<u64>, String)> {
        let mut out = Vec::new();
        for _ in 0..n {
            let (id, v) = tokio::time::timeout(std::time::Duration::from_secs(5), s.next())
                .await
                .expect("an entry in time")
                .expect("the stream is open");
            out.push((id, v["kind"].as_str().unwrap().to_owned()));
        }
        out
    }

    #[tokio::test]
    async fn a_new_view_replays_history_with_ids_then_follows_live() {
        let (_host, room, live) = fixture_room().await;
        checks(&live, 1).await;
        let mut s = Box::pin(entries(Arc::clone(&room), Seq(0), 400, None).await.unwrap());
        assert_eq!(
            take(&mut s, 2).await,
            [(Some(1), "checked".to_owned()), (Some(2), "item".to_owned())]
        );
        checks(&live, 2).await;
        assert_eq!(
            take(&mut s, 2).await,
            [(Some(3), "checked".to_owned()), (Some(4), "item".to_owned())],
            "only the new check, live"
        );
    }

    #[tokio::test]
    async fn a_resume_gets_only_what_came_after_its_id() {
        let (_host, room, live) = fixture_room().await;
        checks(&live, 2).await;
        let mut s = Box::pin(entries(Arc::clone(&room), Seq(2), 400, None).await.unwrap());
        assert_eq!(
            take(&mut s, 2).await,
            [(Some(3), "checked".to_owned()), (Some(4), "item".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_resume_past_the_head_after_a_restart_resets_and_starts_over() {
        let (_host, room, live) = fixture_room().await;
        checks(&live, 1).await;
        let mut s = Box::pin(entries(Arc::clone(&room), Seq(57), 400, None).await.unwrap());
        assert_eq!(
            take(&mut s, 3).await,
            [
                (None, "reset".to_owned()),
                (Some(1), "checked".to_owned()),
                (Some(2), "item".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn history_is_capped_to_the_newest_events() {
        let (_host, room, live) = fixture_room().await;
        checks(&live, 3).await;
        let mut s = Box::pin(entries(Arc::clone(&room), Seq(0), 2, None).await.unwrap());
        assert_eq!(
            take(&mut s, 2).await,
            [(Some(5), "checked".to_owned()), (Some(6), "item".to_owned())],
            "a new view gets the last 2 of 6 events and no reset"
        );
        let mut far = Box::pin(entries(Arc::clone(&room), Seq(1), 2, None).await.unwrap());
        assert_eq!(take(&mut far, 1).await, [(None, "reset".to_owned())], "a gap past the cap");
    }
}
