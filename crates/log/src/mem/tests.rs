use super::*;
use pollster::block_on;
use proto::{Cid, EventKind, Timestamp};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

const R: RoomId = RoomId(1);

fn ev(seq: u64) -> Event {
    Event {
        seq: Seq(seq),
        slot: Some(SlotNo(1)),
        kind: EventKind::Checked { cid: Cid(seq), location: 0, at: Timestamp(0) },
    }
}

fn append(s: &MemLogStore, epoch: Epoch, events: &[Event]) -> Result<AppendReceipt, AppendError> {
    block_on(s.append(AppendBatch { room: R, epoch, events, cursors: &[] }))
}

#[test]
fn an_empty_room_reads_as_empty() {
    let s = MemLogStore::new();
    assert_eq!(block_on(s.read_tail(R, Seq(0), 10)).unwrap(), []);
    assert_eq!(block_on(s.read_range(R, Seq(1), Seq(9))).unwrap(), []);
    assert_eq!(block_on(s.read_cursors(R)).unwrap(), []);
    assert_eq!(block_on(s.snapshot_get(R)).unwrap(), None);
}

#[test]
fn appended_events_read_back_as_tail_and_range() {
    let s = MemLogStore::new();
    let e = s.claim(R);
    assert_eq!(append(&s, e, &[ev(1), ev(2), ev(3)]).unwrap().head, Seq(3));
    assert_eq!(append(&s, e, &[ev(4)]).unwrap().head, Seq(4));
    assert_eq!(block_on(s.read_tail(R, Seq(1), 2)).unwrap(), [ev(2), ev(3)]);
    assert_eq!(block_on(s.read_tail(R, Seq(4), 2)).unwrap(), []);
    assert_eq!(block_on(s.read_tail(R, Seq(0), 0)).unwrap(), []);
    assert_eq!(block_on(s.read_range(R, Seq(2), Seq(3))).unwrap(), [ev(2), ev(3)]);
    assert_eq!(block_on(s.read_range(R, Seq(0), Seq(1))).unwrap(), [ev(1)]);
    assert_eq!(block_on(s.read_range(R, Seq(3), Seq(2))).unwrap(), []);
}

#[test]
fn an_old_epoch_is_fenced_and_writes_nothing() {
    let s = MemLogStore::new();
    let old = s.claim(R);
    append(&s, old, &[ev(1)]).unwrap();
    let new = s.claim(R);
    assert_eq!(append(&s, old, &[ev(2)]), Err(AppendError::Fenced { current: new }));
    assert_eq!(block_on(s.read_tail(R, Seq(0), 9)).unwrap(), [ev(1)]);
    assert_eq!(append(&s, new, &[ev(2)]).unwrap().head, Seq(2));
}

#[test]
fn a_future_or_unclaimed_epoch_is_fenced_too() {
    let s = MemLogStore::new();
    assert_eq!(append(&s, Epoch(5), &[ev(1)]), Err(AppendError::Fenced { current: Epoch(0) }));
}

#[test]
fn a_fenced_batch_does_not_move_cursors() {
    let s = MemLogStore::new();
    let old = s.claim(R);
    s.claim(R);
    let c = [CursorUpdate { slot: SlotNo(1), applied: ItemSeq(3) }];
    assert!(
        block_on(s.append(AppendBatch { room: R, epoch: old, events: &[], cursors: &c })).is_err()
    );
    assert_eq!(block_on(s.read_cursors(R)).unwrap(), []);
}

#[test]
fn a_gap_a_replay_or_a_bad_start_is_corrupt_and_writes_nothing() {
    let s = MemLogStore::new();
    let e = s.claim(R);
    append(&s, e, &[ev(1)]).unwrap();
    for bad in [&[ev(3)][..], &[ev(1)], &[ev(2), ev(4)], &[ev(0)]] {
        assert!(matches!(append(&s, e, bad), Err(AppendError::Corrupt(_))), "{bad:?}");
    }
    assert_eq!(block_on(s.read_tail(R, Seq(0), 9)).unwrap(), [ev(1)]);
}

#[test]
fn an_empty_batch_is_fine_and_cursors_only_ever_rise() {
    let s = MemLogStore::new();
    let e = s.claim(R);
    for applied in [5, 2, 7, 7] {
        let c = [CursorUpdate { slot: SlotNo(1), applied: ItemSeq(applied) }];
        block_on(s.append(AppendBatch { room: R, epoch: e, events: &[], cursors: &c })).unwrap();
    }
    assert_eq!(
        block_on(s.read_cursors(R)).unwrap(),
        [CursorUpdate { slot: SlotNo(1), applied: ItemSeq(7) }]
    );
}

#[test]
fn snapshots_are_fenced_and_cannot_be_ahead_of_the_log() {
    let s = MemLogStore::new();
    let old = s.claim(R);
    append(&s, old, &[ev(1)]).unwrap();
    let snap = |seq| Snapshot { seq: Seq(seq), data: vec![1, 2] };
    assert!(matches!(block_on(s.snapshot_put(R, snap(2), old)), Err(AppendError::Corrupt(_))));
    block_on(s.snapshot_put(R, snap(1), old)).unwrap();
    assert_eq!(block_on(s.snapshot_get(R)).unwrap(), Some(snap(1)));
    let new = s.claim(R);
    assert_eq!(
        block_on(s.snapshot_put(R, snap(0), old)),
        Err(AppendError::Fenced { current: new })
    );
    assert_eq!(block_on(s.snapshot_get(R)).unwrap(), Some(snap(1)));
}

#[test]
fn rooms_do_not_see_each_other() {
    let s = MemLogStore::new();
    let e = s.claim(R);
    append(&s, e, &[ev(1)]).unwrap();
    assert_eq!(block_on(s.read_tail(RoomId(2), Seq(0), 9)).unwrap(), []);
}

#[test]
fn no_append_that_starts_after_a_claim_returns_commits_under_the_old_epoch() {
    // Racing writer: it appends under the old epoch until fenced. `claimed` is set only after
    // `claim` returned, and read before each append starts, so an Ok observed with the flag
    // already set would be an old epoch committing after the fence. Not timing dependent.
    let s = MemLogStore::new();
    let old = s.claim(R);
    let claimed = AtomicBool::new(false);
    thread::scope(|sc| {
        let writer = sc.spawn(|| {
            let mut next = 1_u64;
            loop {
                let started_after_claim = claimed.load(Ordering::SeqCst);
                match append(&s, old, &[ev(next)]) {
                    Ok(_) => {
                        assert!(!started_after_claim, "old epoch committed after the claim");
                        next += 1;
                    }
                    Err(AppendError::Fenced { .. }) => break next - 1,
                    Err(e) => panic!("{e:?}"),
                }
            }
        });
        let new = s.claim(R);
        claimed.store(true, Ordering::SeqCst);
        let committed = writer.join().unwrap();
        let log = block_on(s.read_tail(R, Seq(0), usize::MAX)).unwrap();
        assert_eq!(u64::try_from(log.len()).unwrap(), committed);
        assert_eq!(append(&s, new, &[ev(committed + 1)]).unwrap().head, Seq(committed + 1));
    });
}

#[test]
fn an_older_snapshot_never_replaces_a_newer_one() {
    let s = MemLogStore::new();
    let e = s.claim(R);
    append(&s, e, &[ev(1), ev(2)]).unwrap();
    let snap = |seq| Snapshot { seq: Seq(seq), data: vec![u8::try_from(seq).unwrap()] };
    block_on(s.snapshot_put(R, snap(2), e)).unwrap();
    assert!(matches!(block_on(s.snapshot_put(R, snap(1), e)), Err(AppendError::Corrupt(_))));
    assert_eq!(block_on(s.snapshot_get(R)).unwrap(), Some(snap(2)));
    // The same position may be rewritten (its cursors can have moved).
    block_on(s.snapshot_put(R, Snapshot { seq: Seq(2), data: vec![9] }, e)).unwrap();
}
