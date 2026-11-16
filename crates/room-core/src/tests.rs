// Hand-calculated expectations: the placement table below is the oracle for who receives what.
use super::{CoreError, Ctx, Reply, RoomState, SlotCursor, apply, fold};
use config::Limits;
use proto::{Cid, Command, Event, EventKind, ItemSeq, Seq, SlotNo, Timestamp};
use std::{collections::BTreeMap, vec, vec::Vec};

const NO_EVENTS: [Event; 0] = [];

const LIM: Limits =
    Limits { conn_mem_bytes: 0, dedupe_window_secs: 100, sim_seeds: 0, sim_replay_seed: None };

/// Slots 1 and 2. Location 10 -> slot 1 item 500, 11 -> slot 2 item 501, 12 -> slot 2 item 502.
fn room() -> RoomState {
    let p =
        BTreeMap::from([(10, (SlotNo(1), 500)), (11, (SlotNo(2), 501)), (12, (SlotNo(2), 502))]);
    RoomState::new([SlotNo(1), SlotNo(2)], p).unwrap()
}

fn at(t: u64) -> Ctx<'static> {
    Ctx { now: Timestamp(t), limits: &LIM }
}

fn check(slot: u32, cid: u64, location: u32) -> Command {
    Command::Check { slot: SlotNo(slot), cid: Cid(cid), location }
}

fn ack(slot: u32, applied: u64) -> Command {
    Command::Ack { slot: SlotNo(slot), applied: ItemSeq(applied) }
}

#[test]
fn an_empty_room_has_no_head_and_rejects_every_command() {
    let mut s = RoomState::new([], BTreeMap::new()).unwrap();
    assert_eq!(s.head(), Seq(0));
    assert_eq!(apply(&mut s, &at(0), check(1, 1, 10)), Err(CoreError::UnknownSlot(SlotNo(1))));
    assert_eq!(apply(&mut s, &at(0), ack(1, 0)), Err(CoreError::UnknownSlot(SlotNo(1))));
}

#[test]
fn a_placement_naming_a_missing_slot_is_refused() {
    let p = BTreeMap::from([(1, (SlotNo(9), 1))]);
    assert_eq!(RoomState::new([SlotNo(1)], p), Err(CoreError::UnknownSlot(SlotNo(9))));
}

#[test]
fn a_first_check_emits_the_check_and_the_item_for_the_receiving_slot() {
    let mut s = room();
    let a = apply(&mut s, &at(5), check(1, 1, 11)).unwrap();
    assert_eq!(a.reply, Reply::Checked { seq: Seq(1) });
    assert_eq!(
        a.events,
        vec![
            Event {
                seq: Seq(1),
                slot: Some(SlotNo(1)),
                kind: EventKind::Checked { cid: Cid(1), location: 11, at: Timestamp(5) }
            },
            Event {
                seq: Seq(2),
                slot: Some(SlotNo(2)),
                kind: EventKind::Item { location: 11, item: 501, item_seq: ItemSeq(1) }
            },
        ]
    );
    assert_eq!(s.head(), Seq(2));
}

#[test]
fn item_seq_is_contiguous_per_receiving_slot_not_per_checker() {
    let mut s = room();
    let mut seqs = Vec::new();
    for (cid, loc) in [(1, 11), (2, 10), (3, 12)] {
        for e in apply(&mut s, &at(0), check(1, cid, loc)).unwrap().events {
            if let EventKind::Item { item_seq, .. } = e.kind {
                seqs.push((e.slot.unwrap(), item_seq));
            }
        }
    }
    assert_eq!(seqs, [(SlotNo(2), ItemSeq(1)), (SlotNo(1), ItemSeq(1)), (SlotNo(2), ItemSeq(2))]);
}

#[test]
fn a_repeated_cid_inside_the_window_returns_the_original_and_emits_nothing() {
    let mut s = room();
    let first = apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    let before = s.clone();
    let again = apply(&mut s, &at(99), check(1, 1, 10)).unwrap();
    assert_eq!(again.reply, first.reply);
    assert_eq!(again.events, NO_EVENTS);
    assert_eq!(s, before);
}

#[test]
fn a_repeated_cid_with_a_different_location_still_returns_the_original() {
    let mut s = room();
    let first = apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    let again = apply(&mut s, &at(1), check(1, 1, 11)).unwrap();
    assert_eq!(again.reply, first.reply);
    assert_eq!(again.events, NO_EVENTS);
}

#[test]
fn the_same_cid_from_another_slot_is_a_different_key() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    let other = apply(&mut s, &at(0), check(2, 1, 11)).unwrap();
    assert_eq!(other.events.len(), 2);
}

#[test]
fn the_window_edge_is_exclusive() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    // At exactly now == at + window the entry has expired; the location is still checked.
    let r = apply(&mut s, &at(100), check(1, 1, 11)).unwrap();
    assert_eq!(r.events.len(), 2, "expired cid is a new command");
}

#[test]
fn an_expired_cid_on_a_checked_location_still_emits_nothing() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    let r = apply(&mut s, &at(10_000), check(1, 1, 10)).unwrap();
    assert_eq!(r.reply, Reply::AlreadyChecked);
    assert_eq!(r.events, NO_EVENTS);
}

#[test]
fn a_zero_window_disables_dedupe_but_never_double_checks() {
    let lim = Limits { dedupe_window_secs: 0, ..LIM };
    let ctx = Ctx { now: Timestamp(0), limits: &lim };
    let mut s = room();
    apply(&mut s, &ctx, check(1, 1, 10)).unwrap();
    let r = apply(&mut s, &ctx, check(1, 1, 10)).unwrap();
    assert_eq!(r.reply, Reply::AlreadyChecked);
    assert_eq!(r.events, NO_EVENTS);
}

#[test]
fn a_new_cid_on_a_checked_location_is_a_normal_reply_with_no_item_event() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    let r = apply(&mut s, &at(1), check(2, 9, 10)).unwrap();
    assert_eq!(r.reply, Reply::AlreadyChecked);
    assert_eq!(r.events, NO_EVENTS);
    assert_eq!(s.head(), Seq(2));
}

#[test]
fn an_unknown_location_is_an_error_and_changes_nothing() {
    let mut s = room();
    let before = s.clone();
    assert_eq!(apply(&mut s, &at(0), check(1, 1, 99)), Err(CoreError::UnknownLocation(99)));
    assert_eq!(s, before);
}

#[test]
fn an_ack_is_a_cursor_update_and_emits_no_event() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 11)).unwrap();
    let r = apply(&mut s, &at(0), ack(2, 1)).unwrap();
    assert_eq!(r.reply, Reply::Acked { applied: ItemSeq(1) });
    assert_eq!(r.events, NO_EVENTS);
    assert_eq!(s.head(), Seq(2));
    assert_eq!(
        s.cursor(SlotNo(2)),
        Some(SlotCursor { item_head: ItemSeq(1), applied: ItemSeq(1) })
    );
}

#[test]
fn an_ack_of_zero_on_a_fresh_slot_is_a_no_op() {
    let mut s = room();
    assert_eq!(
        apply(&mut s, &at(0), ack(1, 0)).unwrap().reply,
        Reply::Acked { applied: ItemSeq(0) }
    );
}

#[test]
fn an_ack_never_moves_backwards() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 11)).unwrap();
    apply(&mut s, &at(0), check(1, 2, 12)).unwrap();
    apply(&mut s, &at(0), ack(2, 2)).unwrap();
    let r = apply(&mut s, &at(0), ack(2, 1)).unwrap();
    assert_eq!(r.reply, Reply::Acked { applied: ItemSeq(2) });
    assert_eq!(s.cursor(SlotNo(2)).unwrap().applied, ItemSeq(2));
}

#[test]
fn an_ack_ahead_of_the_head_is_refused_and_changes_nothing() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 11)).unwrap();
    let before = s.clone();
    assert_eq!(apply(&mut s, &at(0), ack(2, 2)), Err(CoreError::AckAhead { head: ItemSeq(1) }));
    assert_eq!(
        apply(&mut s, &at(0), ack(2, u64::MAX)),
        Err(CoreError::AckAhead { head: ItemSeq(1) })
    );
    assert_eq!(s, before);
}

#[test]
fn seq_overflow_is_an_error_not_a_wrap() {
    let mut s = room();
    s.head = Seq(u64::MAX - 1); // one seq left, a check needs two
    let before = s.clone();
    assert_eq!(apply(&mut s, &at(0), check(1, 1, 10)), Err(CoreError::SeqOverflow));
    assert_eq!(s, before);
}

#[test]
fn item_seq_overflow_is_an_error_not_a_wrap() {
    let mut s = room();
    s.slots.get_mut(&SlotNo(1)).unwrap().item_head = ItemSeq(u64::MAX);
    let before = s.clone();
    assert_eq!(apply(&mut s, &at(0), check(1, 1, 10)), Err(CoreError::SeqOverflow));
    assert_eq!(s, before);
}

#[test]
fn fold_rebuilds_the_state_from_a_snapshot_and_a_tail() {
    let mut live = room();
    let mut events = Vec::new();
    for (cid, loc) in [(1, 10), (2, 11), (3, 12)] {
        events.extend(apply(&mut live, &at(cid), check(1, cid, loc)).unwrap().events);
    }
    let (head, tail) = events.split_at(2);
    let mut snap = room();
    snap = fold(snap, &LIM, head.iter().copied()).unwrap();
    let rebuilt = fold(snap, &LIM, tail.iter().copied()).unwrap();
    assert_eq!(rebuilt, live);
}

#[test]
fn fold_of_an_empty_tail_is_the_snapshot() {
    assert_eq!(fold(room(), &LIM, []).unwrap(), room());
}

#[test]
fn fold_refuses_a_gap_a_replay_and_a_slotless_event() {
    let mut live = room();
    let ev = apply(&mut live, &at(0), check(1, 1, 10)).unwrap().events;
    assert_eq!(
        fold(room(), &LIM, ev.iter().skip(1).copied()),
        Err(CoreError::LogGap { expected: Seq(1), got: Seq(2) })
    );
    assert_eq!(
        fold(live.clone(), &LIM, ev.iter().copied()),
        Err(CoreError::LogGap { expected: Seq(3), got: Seq(1) })
    );
    let mut bad = ev[0];
    bad.slot = None;
    assert_eq!(fold(room(), &LIM, [bad]), Err(CoreError::BadEvent));
}

#[test]
fn fold_refuses_an_item_that_skips_an_item_seq() {
    let e = Event {
        seq: Seq(1),
        slot: Some(SlotNo(1)),
        kind: EventKind::Item { location: 10, item: 500, item_seq: ItemSeq(2) },
    };
    assert_eq!(
        fold(room(), &LIM, [e]),
        Err(CoreError::ItemGap { expected: ItemSeq(1), got: ItemSeq(2) })
    );
}

#[test]
fn a_state_survives_a_json_snapshot_round_trip() {
    let mut s = room();
    apply(&mut s, &at(3), check(1, 1, 11)).unwrap();
    apply(&mut s, &at(3), ack(2, 1)).unwrap();
    let j = serde_json::to_string(&s).unwrap();
    assert_eq!(serde_json::from_str::<RoomState>(&j).unwrap(), s);
}

mod properties {
    use super::*;
    use proptest::prelude::{Strategy, any, prop_assert, prop_assert_eq, proptest};

    /// Includes unknown slots (0, 3), unknown locations (13, 99), tiny cid space for duplicates,
    /// and acks above any head.
    fn command() -> impl Strategy<Value = (u64, Command)> {
        let slot = 0_u32..4;
        let t = 0_u64..400;
        let check = (slot.clone(), 0_u64..6, location())
            .prop_map(|(slot, cid, location)| check(slot, cid, location));
        let ack = (slot, 0_u64..6).prop_map(|(s, a)| ack(s, a));
        (t, proptest::prop_oneof![3 => check, 1 => ack])
    }

    fn location() -> impl Strategy<Value = u32> {
        proptest::prop_oneof![9 => 10_u32..13, 1 => any::<u32>()]
    }

    /// Time never goes backwards in a run, but jumps freely.
    fn run(cmds: &[(u64, Command)]) -> (RoomState, Vec<Event>, Vec<Result<Reply, CoreError>>) {
        let mut s = room();
        let (mut now, mut events, mut replies) = (0_u64, Vec::new(), Vec::new());
        for (dt, c) in cmds {
            now = now.saturating_add(*dt);
            match apply(&mut s, &at(now), *c) {
                Ok(a) => {
                    events.extend(a.events);
                    replies.push(Ok(a.reply));
                }
                Err(e) => replies.push(Err(e)),
            }
        }
        (s, events, replies)
    }

    proptest! {
        #[test]
        fn seq_and_item_seq_are_contiguous(cmds in proptest::collection::vec(command(), 0..60)) {
            let (state, events, _) = run(&cmds);
            for (i, e) in events.iter().enumerate() {
                prop_assert_eq!(Some(e.seq.0), u64::try_from(i).ok().and_then(|v| v.checked_add(1)));
            }
            let mut per_slot: BTreeMap<SlotNo, u64> = BTreeMap::new();
            for e in &events {
                if let EventKind::Item { item_seq, .. } = e.kind {
                    let n = per_slot.entry(e.slot.unwrap()).or_default();
                    *n = n.saturating_add(1);
                    prop_assert_eq!(item_seq.0, *n);
                }
            }
            prop_assert_eq!(state.head().0, u64::try_from(events.len()).unwrap());
        }

        #[test]
        fn a_location_is_never_checked_or_delivered_twice(cmds in proptest::collection::vec(command(), 0..60)) {
            let (_, events, _) = run(&cmds);
            let mut seen = std::collections::BTreeSet::new();
            for e in &events {
                let (loc, kind) = match e.kind {
                    EventKind::Checked { location, .. } => (location, 0),
                    EventKind::Item { location, .. } => (location, 1),
                };
                prop_assert!(seen.insert((loc, kind)));
            }
        }

        #[test]
        fn replaying_a_command_at_the_same_time_is_idempotent(
            cmds in proptest::collection::vec(command(), 0..30),
            again in command(),
        ) {
            let (mut state, ..) = run(&cmds);
            let now = 400_u64.saturating_mul(30);
            let Ok(first) = apply(&mut state, &at(now), again.1) else { return Ok(()) };
            let after_first = state.clone();
            let second = apply(&mut state, &at(now), again.1).unwrap();
            prop_assert_eq!(second.events, NO_EVENTS);
            prop_assert_eq!(&state, &after_first);
            if matches!(again.1, Command::Check { .. }) && !first.events.is_empty() {
                prop_assert_eq!(second.reply, first.reply);
            }
        }

        #[test]
        fn apply_is_deterministic(cmds in proptest::collection::vec(command(), 0..60)) {
            prop_assert_eq!(run(&cmds), run(&cmds));
        }

        #[test]
        fn acks_never_decrease_and_never_pass_the_head(cmds in proptest::collection::vec(command(), 0..60)) {
            let mut s = room();
            let mut last: BTreeMap<SlotNo, ItemSeq> = BTreeMap::new();
            for (_, c) in &cmds {
                drop(apply(&mut s, &at(0), *c));
                for slot in [SlotNo(1), SlotNo(2)] {
                    let cur = s.cursor(slot).unwrap();
                    prop_assert!(cur.applied <= cur.item_head);
                    prop_assert!(cur.applied >= last.get(&slot).copied().unwrap_or(ItemSeq(0)));
                    last.insert(slot, cur.applied);
                }
            }
        }

        #[test]
        fn fold_of_snapshot_and_tail_equals_the_live_state(
            cmds in proptest::collection::vec(command(), 0..60),
            cut in 0_usize..60,
        ) {
            let (live, events, _) = run(&cmds);
            // Snapshot at an arbitrary event boundary, then fold the tail onto it.
            let cut = cut.min(events.len());
            let (head, tail) = events.split_at(cut);
            let snap = fold(room(), &LIM, head.iter().copied()).unwrap();
            let mut rebuilt = fold(snap, &LIM, tail.iter().copied()).unwrap();
            // Cursors are not in the log (D-02); rehydration restores them from the store.
            for slot in [SlotNo(1), SlotNo(2)] {
                rebuilt.restore_applied(slot, live.cursor(slot).unwrap().applied).unwrap();
            }
            prop_assert_eq!(rebuilt, live);
        }
    }
}
