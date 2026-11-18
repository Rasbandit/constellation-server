// Hand-calculated expectations: the placement table below is the oracle for who receives what.
use super::{CoreError, Ctx, Reply, RoomState, SlotCursor, apply, fold};
use config::Limits;
use proto::{Cid, Command, Event, EventKind, ItemSeq, Seq, SlotNo, Timestamp};
use std::{collections::BTreeMap, vec, vec::Vec};

const NO_EVENTS: [Event; 0] = [];

const LIM: Limits =
    Limits { conn_mem_bytes: 0, dedupe_window_secs: 100, sim_seeds: 0, ..Limits::DESIGN };

/// Slots 1 and 2. Slot 1's location 10 -> slot 1 item 500, 11 -> slot 2 item 501, 12 -> slot 2
/// item 502. Slot 2's location 10 (the same id in another world) -> slot 1 item 503.
fn room() -> RoomState {
    let p = BTreeMap::from([
        (
            SlotNo(1),
            BTreeMap::from([
                (10, (SlotNo(1), 500)),
                (11, (SlotNo(2), 501)),
                (12, (SlotNo(2), 502)),
            ]),
        ),
        (SlotNo(2), BTreeMap::from([(10, (SlotNo(1), 503))])),
    ]);
    RoomState::new([SlotNo(1), SlotNo(2)], p).unwrap()
}

fn at(t: u64) -> Ctx<'static> {
    Ctx { now: Timestamp(t), limits: &LIM, powf: f64::powf }
}

fn check(slot: u32, cid: u64, location: i64) -> Command {
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
    let p = BTreeMap::from([(SlotNo(1), BTreeMap::from([(1, (SlotNo(9), 1))]))]);
    assert_eq!(RoomState::new([SlotNo(1)], p), Err(CoreError::UnknownSlot(SlotNo(9))));
    let p = BTreeMap::from([(SlotNo(8), BTreeMap::from([(1, (SlotNo(1), 1))]))]);
    assert_eq!(RoomState::new([SlotNo(1)], p), Err(CoreError::UnknownSlot(SlotNo(8))));
}

#[test]
fn the_same_location_id_in_two_slots_is_two_locations() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    let other = apply(&mut s, &at(0), check(2, 2, 10)).unwrap();
    assert_eq!(other.reply, Reply::Checked { seq: Seq(3) });
    assert_eq!(
        other.events[1].kind,
        EventKind::Item { from: SlotNo(2), location: 10, item: 503, item_seq: ItemSeq(2) }
    );
}

#[test]
fn a_location_only_another_slot_has_is_unknown_to_this_slot() {
    let mut s = room();
    assert_eq!(apply(&mut s, &at(0), check(2, 1, 11)), Err(CoreError::UnknownLocation(11)));
    assert_eq!(s, room());
}

#[test]
fn ids_at_the_edges_of_i64_are_plain_locations() {
    let p = BTreeMap::from([(
        SlotNo(1),
        BTreeMap::from([(i64::MIN, (SlotNo(1), i64::MAX)), (-1, (SlotNo(1), 0))]),
    )]);
    let mut s = RoomState::new([SlotNo(1)], p).unwrap();
    for (cid, loc) in [(1, i64::MIN), (2, -1)] {
        assert_eq!(apply(&mut s, &at(0), check(1, cid, loc)).unwrap().events.len(), 2);
    }
}

#[test]
fn a_goal_is_one_event_and_a_repeat_answers_from_it() {
    let mut s = room();
    let first = apply(&mut s, &at(0), Command::Goal { slot: SlotNo(2) }).unwrap();
    assert_eq!(
        first.events,
        vec![Event { seq: Seq(1), slot: Some(SlotNo(2)), kind: EventKind::Goal }]
    );
    assert_eq!((first.reply, first.depends_on), (Reply::Goal { seq: Seq(1) }, Some(Seq(1))));
    assert!(s.has_goal(SlotNo(2)) && !s.has_goal(SlotNo(1)));
    let again = apply(&mut s, &at(0), Command::Goal { slot: SlotNo(2) }).unwrap();
    assert_eq!((again.reply, again.events), (Reply::Goal { seq: Seq(1) }, vec![]));
    assert_eq!(again.depends_on, Some(Seq(1)), "the original is not durable yet");
    s.mark_durable(Seq(1)).unwrap();
    let later = apply(&mut s, &at(0), Command::Goal { slot: SlotNo(2) }).unwrap();
    assert_eq!(later.depends_on, None);
}

#[test]
fn a_goal_for_an_unknown_slot_is_refused() {
    let mut s = room();
    assert_eq!(
        apply(&mut s, &at(0), Command::Goal { slot: SlotNo(0) }),
        Err(CoreError::UnknownSlot(SlotNo(0)))
    );
    assert_eq!(s, room());
}

#[test]
fn checks_and_goals_interleave_and_fold_back_to_the_same_state() {
    let mut live = room();
    let mut events = Vec::new();
    events.extend(apply(&mut live, &at(0), check(1, 1, 11)).unwrap().events);
    events.extend(apply(&mut live, &at(0), Command::Goal { slot: SlotNo(1) }).unwrap().events);
    events.extend(apply(&mut live, &at(0), check(2, 2, 10)).unwrap().events);
    live.mark_durable(live.head()).unwrap();
    let folded = fold(room(), &LIM, events).unwrap();
    assert_eq!(folded, live);
    assert_eq!(folded.validate(Seq(5)), Ok(()));
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
                kind: EventKind::Item {
                    from: SlotNo(1),
                    location: 11,
                    item: 501,
                    item_seq: ItemSeq(1)
                }
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
    let other = apply(&mut s, &at(0), check(2, 1, 10)).unwrap();
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
    let ctx = Ctx { now: Timestamp(0), limits: &lim, powf: f64::powf };
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
    let r = apply(&mut s, &at(1), check(1, 9, 10)).unwrap();
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
    snap = fold(snap, &LIM, head.iter().cloned()).unwrap();
    let rebuilt = fold(snap, &LIM, tail.iter().cloned()).unwrap();
    // The live state learns its events are durable from the caller; fold knows by definition.
    live.mark_durable(live.head()).unwrap();
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
        fold(room(), &LIM, ev.iter().skip(1).cloned()),
        Err(CoreError::LogGap { expected: Seq(1), got: Seq(2) })
    );
    assert_eq!(
        fold(live.clone(), &LIM, ev.iter().cloned()),
        Err(CoreError::LogGap { expected: Seq(3), got: Seq(1) })
    );
    let mut bad = ev[0].clone();
    bad.slot = None;
    assert_eq!(fold(room(), &LIM, [bad]), Err(CoreError::BadEvent));
}

#[test]
fn fold_refuses_an_item_that_skips_an_item_seq() {
    // The Checked event is valid, so only the item_seq is wrong (this test used to send a bare
    // Item; fold now rejects that as a placement error, see below).
    let checked = Event {
        seq: Seq(1),
        slot: Some(SlotNo(1)),
        kind: EventKind::Checked { cid: Cid(1), location: 10, at: Timestamp(0) },
    };
    let e = Event {
        seq: Seq(2),
        slot: Some(SlotNo(1)),
        kind: EventKind::Item { from: SlotNo(1), location: 10, item: 500, item_seq: ItemSeq(2) },
    };
    assert_eq!(
        fold(room(), &LIM, [checked, e]),
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
        let ack = (slot.clone(), 0_u64..6).prop_map(|(s, a)| ack(s, a));
        let goal = slot.prop_map(|s| Command::Goal { slot: SlotNo(s) });
        (t, proptest::prop_oneof![6 => check, 2 => ack, 1 => goal])
    }

    fn location() -> impl Strategy<Value = i64> {
        proptest::prop_oneof![9 => 10_i64..13, 1 => any::<i64>()]
    }

    /// Time never goes backwards in a run, but jumps freely.
    fn run(cmds: &[(u64, Command)]) -> (RoomState, Vec<Event>, Vec<Result<Reply, CoreError>>) {
        let mut s = room();
        let (mut now, mut events, mut replies) = (0_u64, Vec::new(), Vec::new());
        for (dt, c) in cmds {
            now = now.saturating_add(*dt);
            match apply(&mut s, &at(now), c.clone()) {
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
                let key = match e.kind {
                    EventKind::Checked { location, .. } => (e.slot, location, 0),
                    EventKind::Item { from, location, .. } => (Some(from), location, 1),
                    EventKind::Goal => (e.slot, 0, 2),
                    EventKind::KvSet { .. }
                    | EventKind::HintAdded { .. }
                    | EventKind::HintStatus { .. } => continue,
                };
                prop_assert!(seen.insert(key));
            }
        }

        #[test]
        fn replaying_a_command_at_the_same_time_is_idempotent(
            cmds in proptest::collection::vec(command(), 0..30),
            again in command(),
        ) {
            let (mut state, ..) = run(&cmds);
            let now = 400_u64.saturating_mul(30);
            let Ok(first) = apply(&mut state, &at(now), again.1.clone()) else { return Ok(()) };
            let after_first = state.clone();
            let second = apply(&mut state, &at(now), again.1.clone()).unwrap();
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
                drop(apply(&mut s, &at(0), c.clone()));
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
            let (mut live, events, _) = run(&cmds);
            // Snapshot at an arbitrary event boundary, then fold the tail onto it.
            let cut = cut.min(events.len());
            let (head, tail) = events.split_at(cut);
            let snap = fold(room(), &LIM, head.iter().cloned()).unwrap();
            let mut rebuilt = fold(snap, &LIM, tail.iter().cloned()).unwrap();
            // Cursors are not in the log (D-02); rehydration restores them from the store.
            for slot in [SlotNo(1), SlotNo(2)] {
                rebuilt.restore_applied(slot, live.cursor(slot).unwrap().applied).unwrap();
            }
            live.mark_durable(live.head()).unwrap();
            prop_assert_eq!(rebuilt, live);
        }
    }
}

mod hold_rule {
    use super::*;

    #[test]
    fn a_new_check_depends_on_its_last_event_until_that_is_durable() {
        let mut s = room();
        let a = apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
        assert_eq!(a.depends_on, Some(Seq(2)));
    }

    #[test]
    fn a_repeat_of_an_uncommitted_check_must_wait_for_the_original() {
        let mut s = room();
        apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
        let again = apply(&mut s, &at(1), check(1, 1, 10)).unwrap();
        assert_eq!(again.reply, Reply::Checked { seq: Seq(1) });
        assert_eq!(again.events, NO_EVENTS);
        assert_eq!(again.depends_on, Some(Seq(2)));
        s.mark_durable(Seq(2)).unwrap();
        assert_eq!(apply(&mut s, &at(2), check(1, 1, 10)).unwrap().depends_on, None);
    }

    #[test]
    fn already_checked_by_an_uncommitted_event_must_wait_for_it() {
        let mut s = room();
        apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
        let r = apply(&mut s, &at(0), check(1, 9, 10)).unwrap();
        assert_eq!((r.reply, r.depends_on), (Reply::AlreadyChecked, Some(Seq(2))));
        s.mark_durable(Seq(1)).unwrap();
        // Half durable is still not durable for the whole command.
        assert_eq!(apply(&mut s, &at(0), check(1, 9, 10)).unwrap().depends_on, Some(Seq(2)));
        s.mark_durable(Seq(2)).unwrap();
        assert_eq!(apply(&mut s, &at(0), check(1, 9, 10)).unwrap().depends_on, None);
    }

    #[test]
    fn an_ack_never_waits() {
        let mut s = room();
        apply(&mut s, &at(0), check(1, 1, 11)).unwrap();
        assert_eq!(apply(&mut s, &at(0), ack(2, 1)).unwrap().depends_on, None);
    }

    #[test]
    fn durable_only_rises_and_never_passes_the_head() {
        let mut s = room();
        assert_eq!(s.mark_durable(Seq(1)), Err(CoreError::DurableAhead { head: Seq(0) }));
        apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
        s.mark_durable(Seq(2)).unwrap();
        s.mark_durable(Seq(1)).unwrap();
        assert_eq!(s.durable(), Seq(2));
    }

    #[test]
    fn a_folded_state_is_durable_by_definition() {
        let mut live = room();
        let ev = apply(&mut live, &at(0), check(1, 1, 10)).unwrap().events;
        let mut s = fold(room(), &LIM, ev).unwrap();
        assert_eq!(s.durable(), Seq(2));
        assert_eq!(apply(&mut s, &at(1), check(1, 1, 10)).unwrap().depends_on, None);
    }
}

#[test]
fn expired_dedupe_entries_are_pruned() {
    let mut s = room();
    apply(&mut s, &at(0), check(1, 1, 10)).unwrap();
    apply(&mut s, &at(50), check(1, 2, 11)).unwrap();
    assert_eq!(s.dedupe_len(), 2, "both inside the 100 s window");
    apply(&mut s, &at(120), check(1, 3, 12)).unwrap();
    assert_eq!(s.dedupe_len(), 2, "the entry from t=0 expired, the one from t=50 did not");
    let lim = Limits { dedupe_window_secs: 0, ..LIM };
    let ctx = Ctx { now: Timestamp(0), limits: &lim, powf: f64::powf };
    let mut z = room();
    apply(&mut z, &ctx, check(1, 1, 10)).unwrap();
    apply(&mut z, &ctx, check(1, 2, 11)).unwrap();
    assert_eq!(z.dedupe_len(), 1, "a zero window keeps only the newest entry");
}

mod hostile_logs_and_snapshots {
    use super::*;
    use serde_json::{Value, json};

    fn checked(seq: u64, slot: u32, location: i64) -> Event {
        Event {
            seq: Seq(seq),
            slot: Some(SlotNo(slot)),
            kind: EventKind::Checked { cid: Cid(seq), location, at: Timestamp(0) },
        }
    }

    /// An item checked by slot 1.
    fn item(seq: u64, slot: u32, location: i64, item: i64, item_seq: u64) -> Event {
        Event {
            seq: Seq(seq),
            slot: Some(SlotNo(slot)),
            kind: EventKind::Item { from: SlotNo(1), location, item, item_seq: ItemSeq(item_seq) },
        }
    }

    #[test]
    fn fold_refuses_a_check_for_a_location_outside_the_placement() {
        assert_eq!(fold(room(), &LIM, [checked(1, 1, 99)]), Err(CoreError::UnknownLocation(99)));
    }

    #[test]
    fn fold_refuses_a_check_that_is_never_followed_by_its_item() {
        assert_eq!(
            fold(room(), &LIM, [checked(1, 1, 10), checked(2, 1, 11)]),
            Err(CoreError::PlacementMismatch)
        );
    }

    #[test]
    fn fold_refuses_an_item_with_no_check_before_it() {
        assert_eq!(fold(room(), &LIM, [item(1, 1, 10, 500, 1)]), Err(CoreError::PlacementMismatch));
    }

    #[test]
    fn fold_refuses_an_item_that_disagrees_with_the_placement() {
        for bad in [item(2, 2, 10, 500, 1), item(2, 1, 10, 777, 1), item(2, 1, 11, 500, 1)] {
            assert_eq!(
                fold(room(), &LIM, [checked(1, 1, 10), bad.clone()]),
                Err(CoreError::PlacementMismatch),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn fold_refuses_a_location_checked_twice() {
        let ev = [checked(1, 1, 10), item(2, 1, 10, 500, 1), checked(3, 1, 10)];
        assert_eq!(fold(room(), &LIM, ev), Err(CoreError::PlacementMismatch));
    }

    #[test]
    fn fold_refuses_a_second_goal_for_a_slot_and_a_goal_between_check_and_item() {
        let goal = |seq| Event { seq: Seq(seq), slot: Some(SlotNo(1)), kind: EventKind::Goal };
        assert_eq!(fold(room(), &LIM, [goal(1), goal(2)]), Err(CoreError::RepeatedGoal(SlotNo(1))));
        assert_eq!(
            fold(room(), &LIM, [checked(1, 1, 10), goal(2)]),
            Err(CoreError::PlacementMismatch)
        );
    }

    #[test]
    fn fold_refuses_an_item_from_a_slot_that_did_not_check_it() {
        let mut stray = item(2, 1, 10, 500, 1);
        stray.kind =
            EventKind::Item { from: SlotNo(2), location: 10, item: 500, item_seq: ItemSeq(1) };
        assert_eq!(
            fold(room(), &LIM, [checked(1, 1, 10), stray]),
            Err(CoreError::PlacementMismatch)
        );
    }

    #[test]
    fn fold_refuses_an_event_for_an_unknown_slot() {
        assert_eq!(fold(room(), &LIM, [checked(1, 9, 10)]), Err(CoreError::UnknownSlot(SlotNo(9))));
    }

    fn live() -> RoomState {
        let mut s = room();
        apply(&mut s, &at(1), check(1, 1, 11)).unwrap();
        apply(&mut s, &at(1), ack(2, 1)).unwrap();
        s.mark_durable(s.head()).unwrap();
        s
    }

    fn tampered(edit: impl FnOnce(&mut Value)) -> RoomState {
        let mut v = serde_json::to_value(live()).unwrap();
        edit(&mut v);
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_good_snapshot_validates() {
        assert_eq!(live().validate(Seq(2)), Ok(()));
        assert_eq!(room().validate(Seq(0)), Ok(()));
    }

    #[test]
    fn a_snapshot_whose_seq_is_not_its_head_is_refused() {
        assert!(matches!(live().validate(Seq(1)), Err(CoreError::BadSnapshot(_))));
        assert!(matches!(live().validate(Seq(3)), Err(CoreError::BadSnapshot(_))));
    }

    #[test]
    fn a_snapshot_that_was_not_durable_when_taken_is_refused() {
        let mut s = room();
        apply(&mut s, &at(1), check(1, 1, 11)).unwrap();
        assert!(matches!(s.validate(Seq(2)), Err(CoreError::BadSnapshot(_))));
    }

    /// Each case names the rule that must refuse it, so a weakened rule cannot hide behind a
    /// later one. A check's item event always follows it, so a check (and its dedupe entry) can
    /// never sit at the head of a valid snapshot; a goal can.
    #[test]
    fn inconsistent_snapshots_are_errors_not_panics() {
        const BEYOND: &str = "checked location outside the placement or at or beyond the head";
        let cases: [(&str, Value, &str); 13] = [
            (
                "applied above item_head",
                json!({"slots": {"2": {"item_head": 1, "applied": 5}}}),
                "applied is above item_head",
            ),
            (
                "dedupe for a slot that does not exist",
                json!({"dedupe": {"9": {}}}),
                "dedupe entry for a missing slot",
            ),
            (
                "dedupe entry from the future",
                json!({"dedupe": {"1": {"7": {"seq": 99, "at": 0}}}}),
                "dedupe entry at or beyond the head",
            ),
            (
                "dedupe entry at the head, where only an item event can be",
                json!({"dedupe": {"1": {"1": {"seq": 2, "at": 1}}}}),
                "dedupe entry at or beyond the head",
            ),
            (
                "checked location outside the placement",
                json!({"checked": {"1": {"99": 1}}}),
                BEYOND,
            ),
            ("check at the head, its item missing", json!({"checked": {"1": {"11": 2}}}), BEYOND),
            (
                "placement from a slot that does not exist",
                json!({"placement": {"9": {"13": [2, 503]}}}),
                "placement names a missing slot",
            ),
            (
                "placement to a slot that does not exist",
                json!({"placement": {"1": {"11": [9, 501]}}}),
                "placement names a missing slot",
            ),
            (
                "goal for a slot that does not exist",
                json!({"goals": {"9": 1}}),
                "goal for a missing slot or beyond the head",
            ),
            (
                "goal beyond the head",
                json!({"goals": {"1": 3}}),
                "goal for a missing slot or beyond the head",
            ),
            (
                "item_head that matches no checks",
                json!({"slots": {"1": {"item_head": 4, "applied": 0}}}),
                "item_head does not match the checks placed at the slot",
            ),
            (
                "stopped between a check and its item",
                json!({"awaiting": [1, 11]}),
                "snapshot stops between a check and its item",
            ),
            (
                "head that matches no checks",
                json!({"checked": {"1": {}}}),
                "head is not two events per check, one per goal, key write and hint event",
            ),
        ];
        for (name, patch, why) in cases {
            let s = tampered(|v| {
                for (k, new) in patch.as_object().unwrap() {
                    let slot = v.get_mut(k).unwrap();
                    match (slot.as_object_mut(), new.as_object()) {
                        (Some(old), Some(new)) => old.extend(new.clone()),
                        _ => *slot = new.clone(),
                    }
                }
            });
            assert_eq!(s.validate(Seq(2)), Err(CoreError::BadSnapshot(why)), "{name}");
        }
    }

    #[test]
    fn a_snapshot_whose_last_event_is_a_goal_validates() {
        let mut s = live();
        apply(&mut s, &at(1), Command::Goal { slot: SlotNo(1) }).unwrap();
        s.mark_durable(s.head()).unwrap();
        assert_eq!(s.head(), Seq(3));
        assert_eq!(s.validate(Seq(3)), Ok(()));
    }

    #[test]
    fn errors_display_their_variant() {
        assert_eq!(std::format!("{}", CoreError::SeqOverflow), "SeqOverflow");
    }

    #[test]
    fn a_head_that_does_not_match_the_checks_is_refused() {
        let s = tampered(|v| v["head"] = json!(7));
        assert!(matches!(s.validate(Seq(7)), Err(CoreError::BadSnapshot(_))));
    }
}

mod key_value {
    use super::*;
    use crate::KvRefusal;
    use proto::{KvOp, KvWrite};
    use serde_json::{Value, json};
    use std::boxed::Box;

    fn set(slot: u32, key: &str, default: Option<Value>, ops: Vec<(KvOp, Value)>) -> Command {
        Command::KvSet(Box::new(KvWrite { slot: SlotNo(slot), key: key.into(), default, ops }))
    }

    fn add(key: &str, n: i64) -> Command {
        set(1, key, None, vec![(KvOp::Add, json!(n))])
    }

    #[test]
    fn a_write_is_one_event_carrying_the_result_and_replies_with_before_and_after() {
        let mut s = room();
        let a = apply(&mut s, &at(0), add("k", 5)).unwrap();
        let five = Box::new(json!(5));
        assert_eq!(
            a.events,
            vec![Event {
                seq: Seq(1),
                slot: Some(SlotNo(1)),
                kind: EventKind::KvSet { key: "k".into(), value: five },
            }]
        );
        assert_eq!(
            (a.reply, a.depends_on),
            // "k" and `5`: 2 bytes logged.
            (
                Reply::KvSet(Box::new(crate::KvDone {
                    seq: Seq(1),
                    original: json!(0),
                    value: json!(5),
                    bytes: 2
                })),
                Some(Seq(1))
            )
        );
        let b = apply(&mut s, &at(0), add("k", 2)).unwrap();
        assert!(matches!(b.reply, Reply::KvSet(ref d) if d.original == json!(5) && d.bytes == 2));
        assert_eq!(s.kv_get("k"), Some(&json!(7)));
        assert_eq!(s.kv_get("other"), None);
    }

    /// Reference `MultiServer.py:2292`: the start value is the stored one, else the packet's
    /// default (even an explicit `null`), else 0; the key is stored even with no operations.
    #[test]
    fn a_missing_key_starts_from_the_default_and_without_one_from_zero() {
        let mut s = room();
        apply(&mut s, &at(0), set(1, "d", Some(json!([1])), vec![])).unwrap();
        assert_eq!(s.kv_get("d"), Some(&json!([1])));
        apply(&mut s, &at(0), set(1, "d", Some(json!([9])), vec![(KvOp::Add, json!([2]))]))
            .unwrap();
        assert_eq!(s.kv_get("d"), Some(&json!([1, 2])), "a stored value beats the default");
        apply(&mut s, &at(0), set(1, "n", Some(Value::Null), vec![])).unwrap();
        assert_eq!(s.kv_get("n"), Some(&Value::Null));
        apply(&mut s, &at(0), set(1, "z", None, vec![])).unwrap();
        assert_eq!(s.kv_get("z"), Some(&json!(0)));
    }

    #[test]
    fn every_limit_refuses_the_write_and_changes_nothing() {
        let lim = Limits {
            kv_max_key_bytes: 4,
            kv_max_ops: 2,
            kv_max_depth: 2,
            kv_max_value_bytes: 10,
            kv_max_room_bytes: 20,
            ..LIM
        };
        let ctx = Ctx { now: Timestamp(0), limits: &lim, powf: f64::powf };
        let mut s = room();
        // "abcd" + "12345678" = 12 bytes stored; the room may hold 20.
        apply(&mut s, &ctx, set(1, "abcd", Some(json!("123456")), vec![])).unwrap();
        let before = s.clone();
        let replace = |key: &str, v: Value| set(1, key, None, vec![(KvOp::Replace, v)]);
        let cases = [
            (replace("_read_x", json!(1)), KvRefusal::ReservedKey),
            (replace("abcde", json!(1)), KvRefusal::KeyTooLong),
            (set(1, "k", None, vec![(KvOp::Add, json!(1)); 3]), KvRefusal::TooManyOps),
            (replace("k", json!([[[1]]])), KvRefusal::TooDeep),
            (replace("k", json!({"a": {"b": {"c": 1}}})), KvRefusal::TooDeep),
            (set(1, "k", Some(json!([[[1]]])), vec![]), KvRefusal::TooDeep),
            (replace("k", json!("123456789")), KvRefusal::ValueTooLarge),
            // 12 stored + "k" and "12345" (7 bytes) + ... = 20 fits; one more byte does not.
            (replace("kk", json!("123456")), KvRefusal::RoomFull),
            (
                set(1, "k", None, vec![(KvOp::Add, json!(1)), (KvOp::Mod, json!(0))]),
                KvRefusal::ZeroDivision(KvOp::Mod),
            ),
        ];
        for (cmd, want) in cases {
            assert_eq!(apply(&mut s, &ctx, cmd.clone()), Err(CoreError::Kv(want)), "{cmd:?}");
            assert_eq!(s, before, "{cmd:?} changed the state");
        }
        // Exactly at the room cap is fine: "k" and "12345" with its quotes are 8 bytes.
        apply(&mut s, &ctx, replace("k", json!("12345"))).unwrap();
        // Growing a stored key counts only the growth: 13 instead of 12 passes the cap.
        let grow = apply(&mut s, &ctx, replace("abcd", json!("1234567")));
        assert_eq!(grow, Err(CoreError::Kv(KvRefusal::RoomFull)));
        apply(&mut s, &ctx, replace("abcd", json!("12345"))).unwrap();
        // A key at the length cap, a value at the depth and size caps: all accepted.
        apply(&mut room(), &ctx, replace("_rea", json!([[1]]))).unwrap();
        apply(&mut room(), &ctx, replace("abcd", json!("12345678"))).unwrap();
        // The reserved prefix is exactly `_read_`; `_readme` is an ordinary key (reference
        // `MultiServer.py:2286`).
        let lim = Limits { kv_max_key_bytes: 16, ..lim };
        let ctx = Ctx { now: Timestamp(0), limits: &lim, powf: f64::powf };
        apply(&mut room(), &ctx, replace("_readme", json!(1))).unwrap();
    }

    /// Every write is logged in full, so the room caps the bytes it has ever logged for keys,
    /// not only what is stored now; the count is rebuilt by a fold, so a restart keeps it.
    #[test]
    fn the_logged_bytes_of_a_room_are_capped_and_the_count_survives_a_fold() {
        let lim = Limits { kv_max_room_log_bytes: 20, ..LIM };
        let ctx = Ctx { now: Timestamp(0), limits: &lim, powf: f64::powf };
        let write = || set(1, "k", None, vec![(KvOp::Replace, json!("12345"))]); // 8 bytes
        let mut s = room();
        let mut events = Vec::new();
        for _ in 0..2 {
            events.extend(apply(&mut s, &ctx, write()).unwrap().events);
        }
        assert_eq!(apply(&mut s, &ctx, write()), Err(CoreError::Kv(KvRefusal::LogFull)));
        let mut folded = fold(room(), &lim, events).unwrap();
        assert_eq!(apply(&mut folded, &ctx, write()), Err(CoreError::Kv(KvRefusal::LogFull)));
        // "k" and `123` are 4 bytes: 16 + 4 is exactly the cap, which is allowed.
        let at_cap = set(1, "k", None, vec![(KvOp::Replace, json!(123))]);
        assert!(apply(&mut folded, &ctx, at_cap).is_ok(), "exactly at the cap is allowed");
    }

    #[test]
    fn a_key_write_for_an_unknown_slot_is_refused() {
        let mut s = room();
        assert_eq!(
            apply(&mut s, &at(0), set(9, "k", None, vec![])),
            Err(CoreError::UnknownSlot(SlotNo(9)))
        );
        assert_eq!(s, room());
    }

    #[test]
    fn writes_fold_back_to_the_same_store_and_validate() {
        let mut live = room();
        let mut events = Vec::new();
        events.extend(apply(&mut live, &at(0), add("a", 1)).unwrap().events);
        events.extend(apply(&mut live, &at(0), check(1, 1, 11)).unwrap().events);
        events.extend(apply(&mut live, &at(0), add("a", 1)).unwrap().events);
        events.extend(
            apply(&mut live, &at(0), set(2, "b", None, vec![(KvOp::Replace, json!({"x": [1.5]}))]))
                .unwrap()
                .events,
        );
        live.mark_durable(live.head()).unwrap();
        let folded = fold(room(), &LIM, events.clone()).unwrap();
        assert_eq!(folded, live);
        assert_eq!(folded.kv_get("a"), Some(&json!(2)));
        assert_eq!(folded.validate(Seq(5)), Ok(()));
        // Through a JSON snapshot too, as a store would keep it.
        let snap: RoomState =
            serde_json::from_str(&serde_json::to_string(&folded).unwrap()).unwrap();
        assert_eq!(snap, folded);
        // A fold re-uses the recorded value; it never evaluates operations again.
        let tampered: Vec<Event> = events
            .into_iter()
            .map(|mut e| {
                if let EventKind::KvSet { value, .. } = &mut e.kind {
                    **value = json!("recorded");
                }
                e
            })
            .collect();
        assert_eq!(fold(room(), &LIM, tampered).unwrap().kv_get("a"), Some(&json!("recorded")));
    }

    #[test]
    fn fold_refuses_a_key_write_between_a_check_and_its_item() {
        let ev = [
            hostile_logs_and_snapshots_events::checked(1, 1, 10),
            Event {
                seq: Seq(2),
                slot: Some(SlotNo(1)),
                kind: EventKind::KvSet { key: "k".into(), value: Box::new(json!(1)) },
            },
        ];
        assert_eq!(fold(room(), &LIM, ev), Err(CoreError::PlacementMismatch));
    }

    type SnapshotEdit<'a> = &'a dyn Fn(&mut Value);

    #[test]
    fn a_snapshot_whose_store_disagrees_with_its_counters_is_refused() {
        let mut s = room();
        apply(&mut s, &at(0), add("a", 1)).unwrap();
        s.mark_durable(s.head()).unwrap();
        assert_eq!(s.validate(Seq(1)), Ok(()));
        let edit = |f: &dyn Fn(&mut Value)| {
            let mut v = serde_json::to_value(&s).unwrap();
            f(&mut v);
            serde_json::from_value::<RoomState>(v).unwrap().validate(Seq(1)).unwrap_err()
        };
        let cases: [(SnapshotEdit<'_>, &str); 4] = [
            (&|v| v["kv_bytes"] = json!(99), "kv_bytes"),
            (&|v| v["kv_writes"] = json!(0), "head"),
            (
                &|v| {
                    v["kv"] = json!({"_read_x": 1});
                    v["kv_bytes"] = json!(8);
                },
                "reserved",
            ),
            (
                &|v| {
                    v["kv"]["b"] = json!(1);
                    v["kv_bytes"] = json!(4);
                },
                "more keys",
            ),
        ];
        for (f, want) in cases {
            let CoreError::BadSnapshot(why) = edit(f) else { panic!("not a snapshot error") };
            assert!(why.contains(want), "{why} lacks {want}");
        }
    }
}

mod hostile_logs_and_snapshots_events {
    use super::*;

    pub(super) fn checked(seq: u64, slot: u32, location: i64) -> Event {
        Event {
            seq: Seq(seq),
            slot: Some(SlotNo(slot)),
            kind: EventKind::Checked { cid: Cid(seq), location, at: Timestamp(0) },
        }
    }
}

mod float_round_trip {
    use super::*;
    use proptest::prelude::{any, prop_assert_eq, proptest};
    use serde_json::json;
    use std::boxed::Box;

    /// A stored float must come back bit for bit from the log's JSON bytes, or a replayed room
    /// disagrees with the live one. Found with a search over random floats: `serde_json`'s
    /// default parser reads this one back 1 ULP off (unit in the last place); the
    /// `float_roundtrip` feature fixes it.
    #[test]
    fn a_float_that_the_default_parser_rounds_survives_the_log() {
        for x in [1.071_566_039_146_582_6e-75, -1.819_967_304_027_17e-179, 0.1 + 0.2] {
            let e = Event {
                seq: Seq(1),
                slot: Some(SlotNo(1)),
                kind: EventKind::KvSet { key: "k".into(), value: Box::new(json!(x)) },
            };
            let back: Event = serde_json::from_slice(&serde_json::to_vec(&e).unwrap()).unwrap();
            assert_eq!(back, e, "{x:e}");
        }
    }

    proptest! {
        #[test]
        fn every_finite_float_survives_the_log(bits in any::<u64>()) {
            let x = f64::from_bits(bits);
            if x.is_finite() {
                let v = json!(x);
                let back: serde_json::Value =
                    serde_json::from_slice(&serde_json::to_vec(&v).unwrap()).unwrap();
                prop_assert_eq!(back.as_f64().map(f64::to_bits), Some(bits));
            }
        }
    }
}
