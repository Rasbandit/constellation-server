// Expected events are read off the placement table below by hand.
use super::*;
use log::MemLogStore;
use log::{AppendReceipt, CursorUpdate, Snapshot, StoreError};
use proto::{Cid, EventKind, ItemSeq};
use room_core::Placement;
use std::collections::BTreeMap;
use tokio::sync::{Notify, mpsc::error::TryRecvError};

const ROOM: RoomId = RoomId(7);

/// Slot 1's location 10 -> slot 2 item 500; slot 2's location 10 -> slot 2 item 501.
fn state() -> RoomState {
    let p: Placement = BTreeMap::from([
        (SlotNo(1), BTreeMap::from([(10, (SlotNo(2), 500))])),
        (SlotNo(2), BTreeMap::from([(10, (SlotNo(2), 501))])),
    ]);
    RoomState::new([SlotNo(1), SlotNo(2)], p).unwrap()
}

fn start() -> (RoomHandle, Arc<MemLogStore>) {
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(ROOM);
    let (h, _task) = RoomHandle::spawn(ROOM, epoch, state(), Arc::clone(&store), Limits::DESIGN);
    (h, store)
}

/// The cid is the location, as the compatible edge makes it.
fn check(slot: u32, location: i64) -> Command {
    let cid = Cid(u64::from_le_bytes(location.to_le_bytes()));
    Command::Check { slot: SlotNo(slot), cid, location }
}

async fn log_len(store: &MemLogStore) -> usize {
    store.read_tail(ROOM, Seq(0), usize::MAX).await.unwrap().len()
}

fn events(p: &Published) -> Vec<Event> {
    match p {
        Published::Events(e) => e.to_vec(),
        Published::Ext { .. } => panic!("expected events, got {p:?}"),
    }
}

fn seqs(evs: &[Event]) -> Vec<u64> {
    evs.iter().map(|e| e.seq.0).collect()
}

#[tokio::test]
async fn a_reply_arrives_only_after_its_events_are_in_the_log_and_published() {
    let (h, store) = start();
    let mut sub = h.subscribe(Seq(0)).await.unwrap();
    assert_eq!(sub.history, []);
    assert_eq!(h.command(check(1, 10)).await.unwrap(), Ok(Reply::Checked { seq: Seq(1) }));
    // The reply was released, so the append already happened and the publish too.
    assert_eq!(log_len(&store).await, 2);
    let batch = events(&sub.live.try_recv().unwrap());
    assert!(matches!(batch[0].kind, EventKind::Checked { cid: Cid(10), location: 10, .. }));
    assert_eq!(
        batch[1].kind,
        EventKind::Item { from: SlotNo(1), location: 10, item: 500, item_seq: ItemSeq(1) }
    );
    assert_eq!(sub.live.try_recv().unwrap_err(), TryRecvError::Empty);
}

#[tokio::test]
async fn a_subscriber_gets_the_history_after_its_cursor_then_only_new_events() {
    let (h, _) = start();
    h.command(check(1, 10)).await.unwrap().unwrap();
    let all = h.subscribe(Seq(0)).await.unwrap();
    assert_eq!(seqs(&all.history), [1, 2]);
    let mut tail = h.subscribe(Seq(1)).await.unwrap();
    assert_eq!(seqs(&tail.history), [2]);
    assert_eq!(h.subscribe(Seq(2)).await.unwrap().history, []);
    assert_eq!(h.subscribe(Seq(3)).await.err(), Some(RoomError::Unavailable), "beyond the head");
    h.command(check(2, 10)).await.unwrap().unwrap();
    assert_eq!(seqs(&events(&tail.live.try_recv().unwrap())), [3, 4]);
}

#[tokio::test]
async fn a_batch_is_one_append_one_publish_and_one_outcome_per_command() {
    let (h, store) = start();
    let mut sub = h.subscribe(Seq(0)).await.unwrap();
    let out = h
        .commands(vec![check(1, 10), check(1, 99), check(1, 10), Command::Goal { slot: SlotNo(1) }])
        .await
        .unwrap();
    assert_eq!(
        out,
        [
            Ok(Reply::Checked { seq: Seq(1) }),
            Err(CoreError::UnknownLocation(99)),
            Ok(Reply::Checked { seq: Seq(1) }),
            Ok(Reply::Goal { seq: Seq(3) }),
        ]
    );
    assert_eq!(log_len(&store).await, 3);
    assert_eq!(seqs(&events(&sub.live.try_recv().unwrap())), [1, 2, 3]);
    assert_eq!(sub.live.try_recv().unwrap_err(), TryRecvError::Empty);
}

#[tokio::test]
async fn an_empty_batch_and_a_refused_command_leave_the_room_serving() {
    let (h, store) = start();
    assert_eq!(h.commands(vec![]).await.unwrap(), []);
    assert_eq!(h.command(check(9, 10)).await.unwrap(), Err(CoreError::UnknownSlot(SlotNo(9))));
    assert_eq!(log_len(&store).await, 0);
    assert!(h.command(check(1, 10)).await.unwrap().is_ok());
}

#[tokio::test]
async fn a_fenced_room_stops_and_every_handle_then_reports_unavailable() {
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(ROOM);
    let (h, task) = RoomHandle::spawn(ROOM, epoch, state(), Arc::clone(&store), Limits::DESIGN);
    assert!(h.is_running());
    let current = store.claim(ROOM); // another owner took the room
    assert_eq!(h.command(check(1, 10)).await, Err(RoomError::Unavailable));
    assert_eq!(task.await.unwrap(), Stopped::Fenced { current }, "the reason is reported");
    h.stopped().await;
    assert!(!h.is_running(), "a stopped room must not report itself running");
    assert_eq!(log_len(&store).await, 0, "a fenced writer must not land anything");
    assert_eq!(h.clone().subscribe(Seq(0)).await.err(), Some(RoomError::Unavailable));
    assert_eq!(
        h.ext(SlotNo(1), Arc::new(serde_json::json!({}))).await,
        Err(RoomError::Unavailable)
    );
}

#[tokio::test]
async fn a_room_whose_handles_are_all_dropped_reports_closed() {
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(ROOM);
    let (h, mailbox) = RoomHandle::mailbox(&Limits::DESIGN);
    let task = mailbox.spawn(ROOM, epoch, state(), store, Limits::DESIGN);
    assert!(h.command(check(1, 10)).await.unwrap().is_ok(), "a handle made before spawn works");
    drop(h);
    assert_eq!(task.await.unwrap(), Stopped::Closed);
}

#[tokio::test]
async fn ext_messages_are_ordered_with_events() {
    let (h, _) = start();
    let mut sub = h.subscribe(Seq(0)).await.unwrap();
    let body = Arc::new(serde_json::json!({"say": "hi"}));
    h.ext(SlotNo(2), Arc::clone(&body)).await.unwrap();
    h.command(check(1, 10)).await.unwrap().unwrap();
    assert_eq!(*sub.live.try_recv().unwrap(), Published::Ext { from: SlotNo(2), body });
    assert_eq!(seqs(&events(&sub.live.try_recv().unwrap())), [1, 2]);
}

fn small_queue() -> RoomHandle {
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(ROOM);
    let limits = Limits { session_queue: 1, ..Limits::DESIGN };
    RoomHandle::spawn(ROOM, epoch, state(), store, limits).0
}

#[tokio::test]
async fn a_full_queue_drops_only_that_subscriber_and_its_cursor_resync_loses_nothing() {
    let h = small_queue();
    let mut slow = h.subscribe(Seq(0)).await.unwrap();
    h.command(check(1, 10)).await.unwrap().unwrap();
    let mut fast = h.subscribe(Seq(2)).await.unwrap();
    h.command(check(2, 10)).await.unwrap().unwrap(); // slow's queue is full: it is dropped
    assert_eq!(seqs(&events(&slow.live.recv().await.unwrap())), [1, 2]);
    assert_eq!(slow.live.recv().await, None, "dropped, not stalled");
    assert_eq!(seqs(&events(&fast.live.recv().await.unwrap())), [3, 4], "others unaffected");
    let again = h.subscribe(Seq(2)).await.unwrap();
    assert_eq!(seqs(&again.history), [3, 4], "the resync from the cursor has the missed events");
}

#[tokio::test]
async fn a_full_queue_skips_chat_but_keeps_the_subscriber() {
    let h = small_queue();
    let mut sub = h.subscribe(Seq(0)).await.unwrap();
    for _ in 0..3 {
        h.ext(SlotNo(1), Arc::new(serde_json::json!("chat"))).await.unwrap();
    }
    assert!(matches!(*sub.live.recv().await.unwrap(), Published::Ext { .. }));
    h.command(check(1, 10)).await.unwrap().unwrap();
    assert_eq!(seqs(&events(&sub.live.recv().await.unwrap())), [1, 2]);
}

/// A store whose appends wait for `release` (or fail with `Unavailable` when `fail`), and
/// that says when an append has started.
#[derive(Debug)]
struct GatedStore {
    inner: MemLogStore,
    fail: bool,
    started: Notify,
    release: Notify,
}

impl LogStore for GatedStore {
    async fn append(&self, batch: AppendBatch<'_>) -> Result<AppendReceipt, AppendError> {
        self.started.notify_one();
        if self.fail {
            return Err(AppendError::Unavailable);
        }
        self.release.notified().await;
        self.inner.append(batch).await
    }
    async fn read_tail(
        &self,
        room: RoomId,
        after: Seq,
        limit: usize,
    ) -> Result<Vec<Event>, StoreError> {
        self.inner.read_tail(room, after, limit).await
    }
    async fn read_range(&self, room: RoomId, from: Seq, to: Seq) -> Result<Vec<Event>, StoreError> {
        self.inner.read_range(room, from, to).await
    }
    async fn read_cursors(&self, room: RoomId) -> Result<Vec<CursorUpdate>, StoreError> {
        self.inner.read_cursors(room).await
    }
    async fn snapshot_get(&self, room: RoomId) -> Result<Option<Snapshot>, StoreError> {
        self.inner.snapshot_get(room).await
    }
    async fn snapshot_put(
        &self,
        room: RoomId,
        snap: Snapshot,
        epoch: Epoch,
    ) -> Result<(), AppendError> {
        self.inner.snapshot_put(room, snap, epoch).await
    }
}

fn gated(fail: bool) -> (RoomHandle, Arc<GatedStore>, JoinHandle<Stopped>) {
    let inner = MemLogStore::new();
    let epoch = inner.claim(ROOM);
    let store =
        Arc::new(GatedStore { inner, fail, started: Notify::new(), release: Notify::new() });
    let (h, task) = RoomHandle::spawn(ROOM, epoch, state(), Arc::clone(&store), Limits::DESIGN);
    (h, store, task)
}

#[tokio::test]
async fn nothing_is_published_or_replied_while_the_append_is_in_flight() {
    let (h, store, _task) = gated(false);
    let mut sub = h.subscribe(Seq(0)).await.unwrap();
    let pending = tokio::spawn({
        let h = h.clone();
        async move { h.command(check(1, 10)).await }
    });
    store.started.notified().await;
    assert_eq!(sub.live.try_recv().unwrap_err(), TryRecvError::Empty, "published before durable");
    assert!(!pending.is_finished(), "replied before durable");
    store.release.notify_one();
    assert_eq!(pending.await.unwrap(), Ok(Ok(Reply::Checked { seq: Seq(1) })));
    assert_eq!(seqs(&events(&sub.live.try_recv().unwrap())), [1, 2]);
}

#[tokio::test]
async fn a_failed_append_publishes_nothing_to_any_subscriber() {
    let (h, store, task) = gated(true);
    let mut a = h.subscribe(Seq(0)).await.unwrap();
    let mut b = h.subscribe(Seq(0)).await.unwrap();
    assert_eq!(h.command(check(1, 10)).await, Err(RoomError::Unavailable));
    store.started.notified().await;
    h.stopped().await;
    assert!(!h.is_running());
    assert_eq!(task.await.unwrap(), Stopped::Failed, "a store failure is not a fence");
    // The room stopped, so the queues end, and they end empty.
    assert_eq!(a.live.recv().await, None);
    assert_eq!(b.live.recv().await, None);
}

#[tokio::test]
async fn subscribers_that_left_are_pruned_on_subscribe_even_in_a_quiet_room() {
    let (h, _) = start();
    for _ in 0..50 {
        drop(h.subscribe(Seq(0)).await.unwrap());
    }
    let kept = h.subscribe(Seq(0)).await.unwrap();
    assert_eq!(h.subscribers().await.unwrap(), 1, "only the live one is kept");
    drop(kept);
}

#[test]
fn a_room_error_displays_what_happened() {
    assert_eq!(RoomError::Unavailable.to_string(), "room unavailable");
}

#[tokio::test]
async fn a_later_batch_that_writes_nothing_is_still_answered() {
    let (h, _) = start();
    h.command(check(1, 10)).await.unwrap().unwrap();
    // Both replies rest on writes that are already durable, so nothing new is appended.
    let ack = Command::Ack { slot: SlotNo(2), applied: ItemSeq(1) };
    assert_eq!(h.command(ack).await.unwrap(), Ok(Reply::Acked { applied: ItemSeq(1) }));
    assert_eq!(h.command(check(1, 10)).await.unwrap(), Ok(Reply::Checked { seq: Seq(1) }));
    assert!(h.is_running());
}

#[tokio::test]
async fn the_subscriber_count_is_the_number_of_live_subscriptions() {
    let (h, _) = start();
    assert_eq!(h.subscribers().await.unwrap(), 0);
    let (a, b) = (h.subscribe(Seq(0)).await.unwrap(), h.subscribe(Seq(0)).await.unwrap());
    assert_eq!(h.subscribers().await.unwrap(), 2);
    drop((a, b));
}

#[tokio::test]
async fn stopped_stays_pending_while_the_room_runs() {
    let (h, _) = start();
    tokio::select! {
        biased;
        () = h.stopped() => panic!("stopped resolved for a running room"),
        () = std::future::ready(()) => {}
    }
}
