// Expected events are read off the placement table below by hand.
use super::*;
use log::MemLogStore;
use proto::{Cid, EventKind, ItemSeq};
use room_core::Placement;
use std::collections::BTreeMap;
use tokio::sync::broadcast::error::TryRecvError;

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

fn event(p: &Published) -> &Event {
    match p {
        Published::Event(e) => e,
        Published::Ext { .. } => panic!("expected an event, got {p:?}"),
    }
}

#[tokio::test]
async fn a_reply_arrives_only_after_its_events_are_in_the_log_and_published() {
    let (h, store) = start();
    let mut sub = h.subscribe().await.unwrap();
    assert_eq!(sub.history, []);
    assert_eq!(h.command(check(1, 10)).await.unwrap(), Ok(Reply::Checked { seq: Seq(1) }));
    // The reply was released, so the append already happened and the publish too.
    assert_eq!(log_len(&store).await, 2);
    let item = sub.live.try_recv().unwrap();
    assert!(matches!(event(&item).kind, EventKind::Checked { cid: Cid(10), location: 10, .. }));
    let item = sub.live.try_recv().unwrap();
    assert_eq!(
        event(&item).kind,
        EventKind::Item { from: SlotNo(1), location: 10, item: 500, item_seq: ItemSeq(1) }
    );
    assert_eq!(sub.live.try_recv().unwrap_err(), TryRecvError::Empty);
}

#[tokio::test]
async fn a_late_subscriber_gets_the_history_then_only_new_events() {
    let (h, _) = start();
    h.command(check(1, 10)).await.unwrap().unwrap();
    let mut sub = h.subscribe().await.unwrap();
    assert_eq!(sub.history.iter().map(|e| e.seq.0).collect::<Vec<_>>(), [1, 2]);
    h.command(check(2, 10)).await.unwrap().unwrap();
    assert_eq!(event(&sub.live.try_recv().unwrap()).seq, Seq(3));
}

#[tokio::test]
async fn a_batch_is_one_append_with_one_outcome_per_command() {
    let (h, store) = start();
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
    let (h, store) = start();
    store.claim(ROOM); // another owner took the room
    assert_eq!(h.command(check(1, 10)).await, Err(RoomError::Unavailable));
    assert_eq!(log_len(&store).await, 0, "a fenced writer must not land anything");
    assert_eq!(h.clone().subscribe().await.err(), Some(RoomError::Unavailable));
    assert_eq!(
        h.ext(SlotNo(1), Arc::new(serde_json::json!({}))).await,
        Err(RoomError::Unavailable)
    );
}

#[tokio::test]
async fn ext_messages_are_ordered_with_events() {
    let (h, _) = start();
    let mut sub = h.subscribe().await.unwrap();
    let body = Arc::new(serde_json::json!({"say": "hi"}));
    h.ext(SlotNo(2), Arc::clone(&body)).await.unwrap();
    h.command(check(1, 10)).await.unwrap().unwrap();
    assert_eq!(*sub.live.try_recv().unwrap(), Published::Ext { from: SlotNo(2), body });
    assert_eq!(event(&sub.live.try_recv().unwrap()).seq, Seq(1));
}

#[tokio::test]
async fn a_subscriber_that_falls_too_far_behind_is_told_it_lagged() {
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(ROOM);
    let limits = Limits { session_queue: 1, ..Limits::DESIGN };
    let (h, _task) = RoomHandle::spawn(ROOM, epoch, state(), store, limits);
    let mut sub = h.subscribe().await.unwrap();
    h.command(check(1, 10)).await.unwrap().unwrap();
    assert!(matches!(sub.live.try_recv(), Err(TryRecvError::Lagged(1))));
}
