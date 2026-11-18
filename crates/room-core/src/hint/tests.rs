// Expected values are hand calculations from the placement below and the reference rule
// (`MultiServer.py:816-819, 1929-1936`); cost vectors were computed with Python 3 using the
// reference formula `max(1, int(cost * 0.01 * size))`.
use crate::hint::{HintRefusal, HintRules, status};
use crate::{CoreError, Ctx, Reply, RoomState, apply, fold};
use config::Limits;
use proto::{Cid, Command, Event, EventKind, HintAsk, HintRequest, Seq, SlotNo, Timestamp};
use std::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

const LIM: Limits = Limits::DESIGN;

fn at() -> Ctx<'static> {
    Ctx { now: Timestamp(0), limits: &LIM, powf: f64::powf }
}

/// Slot 1's world: 10 -> slot 1 item 500, 11 -> slot 2 item 501 (a trap in the edge's eyes, not
/// here), 12 -> slot 2 item 502. Slot 2's world: 10 -> slot 1 item 503.
fn room(rules: HintRules) -> RoomState {
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
    RoomState::new([SlotNo(1), SlotNo(2)], p).unwrap().with_hints(rules, []).unwrap()
}

/// One point per check; a hint costs 50 percent of a world (slot 1: int(1.5) = 1).
const PAID: HintRules = HintRules { cost_pct: 50, check_points: 1 };
const FREE: HintRules = HintRules { cost_pct: 0, check_points: 1 };

fn ask(finding: u32, location: i64, status: u8) -> HintAsk {
    HintAsk { finding: SlotNo(finding), location, status }
}

fn hint(slot: u32, asks: Vec<HintAsk>, pay: bool, keep_found: bool) -> Command {
    Command::Hint(Box::new(HintRequest { slot: SlotNo(slot), asks, pay, keep_found }))
}

fn scout(slot: u32, locations: &[i64]) -> Command {
    let asks = locations.iter().map(|&l| ask(slot, l, status::UNSPECIFIED)).collect();
    hint(slot, asks, false, true)
}

fn check(s: &mut RoomState, slot: u32, location: i64) {
    let cid = Cid(u64::from(slot) << 32 | u64::try_from(location).unwrap());
    apply(s, &at(), Command::Check { slot: SlotNo(slot), cid, location }).unwrap();
}

fn hinted(r: &Reply) -> &crate::Hinted {
    match r {
        Reply::Hinted(h) => h,
        other => panic!("not a hint reply: {other:?}"),
    }
}

/// `(finding, location, status, found, new)` of each hint shown.
fn shown(r: &Reply) -> Vec<(u32, i64, u8, bool, bool)> {
    hinted(r)
        .shown
        .iter()
        .map(|s| (s.hint.finding.0, s.hint.location, s.hint.status, s.hint.found, s.new))
        .collect()
}

#[test]
fn the_cost_of_a_hint_follows_the_reference_float_rule() {
    // (percent, world size, cost), computed with Python 3 from the reference formula.
    for (pct, size, want) in [
        (0, 15, 0),
        (10, 15, 1),
        (10, 5, 1),
        (29, 100, 28),
        (58, 100, 57),
        (57, 100, 57),
        (100, 7, 7),
        (1, 1, 1),
        (10, 0, 1),
    ] {
        let r = HintRules { cost_pct: pct, check_points: 1 };
        assert_eq!(r.cost(size), want, "{pct} percent of {size}");
    }
}

#[test]
fn points_are_earned_per_own_check_and_spent_per_paid_hint() {
    let r = HintRules { cost_pct: 10, check_points: 3 };
    // World of 20: cost 2. Five checks earn 15; four paid hints spend 8.
    assert_eq!(r.points(20, 5, 4), 7);
    assert_eq!(r.points(20, 0, 0), 0);
    assert_eq!(HintRules { cost_pct: 100, check_points: 0 }.points(3, 0, 1), -3);
    assert_eq!(
        HintRules { cost_pct: 1, check_points: u32::MAX }.points(1, usize::MAX, 0),
        i64::MAX
    );
}

#[test]
fn a_scout_hint_is_free_and_kept_even_for_a_checked_location() {
    let mut s = room(PAID);
    check(&mut s, 1, 10);
    let a = apply(&mut s, &at(), scout(1, &[10, 11])).unwrap();
    assert_eq!(
        shown(&a.reply),
        [(1, 10, status::FOUND, true, true), (1, 11, status::UNSPECIFIED, false, true)]
    );
    assert_eq!(a.events.len(), 2);
    assert_eq!(
        a.events[0].kind,
        EventKind::HintAdded {
            finding: SlotNo(1),
            location: 10,
            status: status::FOUND,
            paid: false
        }
    );
    assert_eq!(a.depends_on, Some(a.events[1].seq));
    assert_eq!(s.hint_points(SlotNo(1)), 1, "scouts never cost points");
    assert_eq!(hinted(&a.reply).points, 1);
}

#[test]
fn an_identical_hint_is_one_hint_within_and_across_requests() {
    let mut s = room(FREE);
    let a = apply(&mut s, &at(), scout(1, &[11, 11, 11])).unwrap();
    assert_eq!(a.events.len(), 1);
    assert_eq!(shown(&a.reply), [(1, 11, status::UNSPECIFIED, false, true)]);
    let again = apply(&mut s, &at(), scout(1, &[11])).unwrap();
    assert_eq!(again.events.len(), 0);
    assert_eq!(shown(&again.reply), [(1, 11, status::UNSPECIFIED, false, false)]);
    // A paid ask for a known hint shows it again and costs nothing.
    let paid = apply(&mut s, &at(), hint(2, vec![ask(1, 11, status::PRIORITY)], true, false));
    let paid = paid.unwrap();
    assert_eq!(paid.events.len(), 0);
    assert_eq!(shown(&paid.reply), [(1, 11, status::UNSPECIFIED, false, false)]);
    assert_eq!(s.hints_of(SlotNo(2)).len(), 1);
}

#[test]
fn a_paid_request_creates_only_the_first_new_hint_when_the_slot_can_pay() {
    let mut s = room(PAID);
    check(&mut s, 2, 10);
    // Slot 2 has 1 point; its world of 1 makes a hint cost max(1, int(0.5)) = 1.
    assert_eq!((s.hint_points(SlotNo(2)), s.hint_cost(SlotNo(2))), (1, 1));
    let asks = vec![ask(1, 12, status::PRIORITY), ask(1, 11, status::AVOID)];
    let a = apply(&mut s, &at(), hint(2, asks.clone(), true, false)).unwrap();
    assert_eq!(shown(&a.reply), [(1, 12, status::PRIORITY, false, true)]);
    let h = hinted(&a.reply);
    assert_eq!((h.unpaid, h.points, h.cost), (1, 0, 1));
    assert_eq!(
        a.events[0].kind,
        EventKind::HintAdded {
            finding: SlotNo(1),
            location: 12,
            status: status::PRIORITY,
            paid: true
        }
    );
    // Out of points: the known one is shown again, the other is left unpaid.
    let b = apply(&mut s, &at(), hint(2, asks, true, false)).unwrap();
    assert_eq!(b.events.len(), 0);
    assert_eq!(shown(&b.reply), [(1, 12, status::PRIORITY, false, false)]);
    assert_eq!(hinted(&b.reply).unpaid, 1);
}

#[test]
fn a_paid_request_with_too_few_points_creates_nothing() {
    let mut s = room(PAID);
    let a = apply(&mut s, &at(), hint(1, vec![ask(1, 11, status::PRIORITY)], true, false)).unwrap();
    assert_eq!(a.events.len(), 0);
    assert_eq!(a.depends_on, None);
    let h = hinted(&a.reply);
    assert_eq!((h.shown.len(), h.unpaid, h.points, h.cost), (0, 1, 0, 1));
}

#[test]
fn free_hints_create_every_new_hint_and_still_count_as_used() {
    let mut s = room(FREE);
    let asks = vec![ask(1, 11, status::PRIORITY), ask(1, 12, status::PRIORITY)];
    let a = apply(&mut s, &at(), hint(2, asks, true, false)).unwrap();
    assert_eq!(a.events.len(), 2);
    assert_eq!(hinted(&a.reply).unpaid, 0);
    // The reference counts hints used even at no cost (`MultiServer.py:1874-1879`).
    assert_eq!(s.hints_used.get(&SlotNo(2)), Some(&2));
    assert_eq!(s.hint_points(SlotNo(2)), 0);
}

#[test]
fn a_paid_request_shows_a_checked_location_free_and_does_not_keep_it() {
    let mut s = room(PAID);
    check(&mut s, 1, 11);
    let a = apply(&mut s, &at(), hint(2, vec![ask(1, 11, status::PRIORITY)], true, false)).unwrap();
    assert_eq!(a.events.len(), 0);
    assert_eq!(shown(&a.reply), [(1, 11, status::FOUND, true, true)]);
    assert_eq!(hinted(&a.reply).unpaid, 0);
    assert_eq!(s.hints_of(SlotNo(2)).len(), 0);
}

#[test]
fn a_bad_request_is_refused_and_changes_nothing() {
    let mut s = room(FREE);
    let before = s.clone();
    let bad = [
        (hint(3, vec![], false, true), CoreError::UnknownSlot(SlotNo(3))),
        (hint(1, vec![ask(2, 11, 0)], false, true), CoreError::UnknownLocation(11)),
        (
            hint(1, vec![ask(1, 11, 0), ask(1, 12, 5)], false, true),
            CoreError::Hint(HintRefusal::BadStatus(5)),
        ),
        (
            hint(1, vec![ask(1, 11, status::FOUND)], false, true),
            CoreError::Hint(HintRefusal::BadStatus(40)),
        ),
    ];
    for (cmd, want) in bad {
        assert_eq!(apply(&mut s, &at(), cmd), Err(want));
        assert_eq!(s, before);
    }
}

fn update(slot: u32, finding: u32, location: i64, status: u8) -> Command {
    Command::HintStatus { slot: SlotNo(slot), finding: SlotNo(finding), location, status }
}

#[test]
fn only_the_receiver_may_change_a_status_and_never_to_found() {
    let mut s = room(FREE);
    apply(&mut s, &at(), scout(1, &[11])).unwrap();
    let before = s.clone();
    // Slot 1 finds it but slot 2 receives it.
    assert_eq!(
        apply(&mut s, &at(), update(1, 1, 11, 30)),
        Err(CoreError::Hint(HintRefusal::NoPermission))
    );
    assert_eq!(
        apply(&mut s, &at(), update(2, 1, 11, 40)),
        Err(CoreError::Hint(HintRefusal::BadStatus(40)))
    );
    assert_eq!(
        apply(&mut s, &at(), update(2, 1, 11, 7)),
        Err(CoreError::Hint(HintRefusal::BadStatus(7)))
    );
    assert_eq!(s, before);
    let a = apply(&mut s, &at(), update(2, 1, 11, status::AVOID)).unwrap();
    assert_eq!(a.reply, Reply::HintStatus { changed: true });
    assert_eq!(
        a.events[0].kind,
        EventKind::HintStatus { finding: SlotNo(1), location: 11, status: status::AVOID }
    );
    assert_eq!(s.hints_of(SlotNo(1))[0].status, status::AVOID);
    // The same status again writes nothing, and its answer waits for the change it rests on.
    let again = apply(&mut s, &at(), update(2, 1, 11, status::AVOID)).unwrap();
    assert_eq!((again.reply, again.events.len()), (Reply::HintStatus { changed: false }, 0));
    assert_eq!(again.depends_on, Some(a.events[0].seq));
}

#[test]
fn an_unknown_hint_is_ignored_and_a_found_hint_stays_found() {
    let mut s = room(FREE);
    let a = apply(&mut s, &at(), update(2, 1, 12, status::PRIORITY)).unwrap();
    assert_eq!((a.reply, a.events.len()), (Reply::HintStatus { changed: false }, 0));
    apply(&mut s, &at(), scout(1, &[12])).unwrap();
    check(&mut s, 1, 12);
    let a = apply(&mut s, &at(), update(2, 1, 12, status::PRIORITY)).unwrap();
    assert_eq!(a.events.len(), 0);
    assert_eq!(s.hints_of(SlotNo(2))[0].status, status::FOUND);
}

#[test]
fn a_check_marks_a_hint_found_for_both_sides() {
    let mut s = room(FREE);
    apply(&mut s, &at(), scout(1, &[11])).unwrap();
    assert!(!s.hints_of(SlotNo(2))[0].found);
    check(&mut s, 1, 11);
    for slot in [1, 2] {
        let h = s.hints_of(SlotNo(slot));
        assert_eq!((h.len(), h[0].found, h[0].status), (1, true, status::FOUND), "slot {slot}");
        assert_eq!((h[0].receiver, h[0].item), (SlotNo(2), 501));
    }
}

#[test]
fn start_hints_are_in_the_fresh_room_and_a_bad_one_is_refused() {
    let base = room(FREE);
    let s = base
        .clone()
        .with_hints(PAID, [(SlotNo(2), 10, status::PRIORITY), (SlotNo(2), 10, 0)])
        .unwrap();
    assert_eq!(s.head(), Seq(0));
    assert_eq!(s.hints_of(SlotNo(1)).len(), 1, "named twice, kept once");
    assert_eq!(s.hints_of(SlotNo(1))[0].status, status::PRIORITY);
    assert_eq!(s.hint_rules(), PAID);
    s.validate(Seq(0)).unwrap();
    assert_eq!(
        base.clone().with_hints(FREE, [(SlotNo(1), 99, 0)]),
        Err(CoreError::UnknownLocation(99))
    );
    assert_eq!(
        base.with_hints(FREE, [(SlotNo(1), 10, 9)]),
        Err(CoreError::Hint(HintRefusal::BadStatus(9)))
    );
}

#[test]
fn a_reply_about_a_known_hint_waits_until_that_hint_is_durable() {
    let mut s = room(FREE);
    let first = apply(&mut s, &at(), scout(1, &[11])).unwrap();
    let seq = first.events[0].seq;
    let again = apply(&mut s, &at(), scout(1, &[11])).unwrap();
    assert_eq!(again.depends_on, Some(seq));
    s.mark_durable(seq).unwrap();
    assert_eq!(apply(&mut s, &at(), scout(1, &[11])).unwrap().depends_on, None);
    // A start hint is durable from the start.
    let mut s = room(FREE).with_hints(FREE, [(SlotNo(1), 11, 0)]).unwrap();
    assert_eq!(apply(&mut s, &at(), scout(1, &[11])).unwrap().depends_on, None);
}

/// Every kind of hint event, then a fold of the log onto the fresh room.
#[test]
fn hints_fold_back_to_the_same_state_and_validate() {
    let fresh = room(PAID).with_hints(PAID, [(SlotNo(2), 10, status::PRIORITY)]).unwrap();
    let mut s = fresh.clone();
    let mut log: Vec<Event> = Vec::new();
    let cmds = [
        Command::Check { slot: SlotNo(2), cid: Cid(1), location: 10 },
        scout(1, &[11]),
        hint(2, vec![ask(1, 12, status::PRIORITY)], true, false),
        update(2, 1, 11, status::NO_PRIORITY),
    ];
    for c in cmds {
        log.extend(apply(&mut s, &at(), c).unwrap().events);
    }
    assert_eq!(log.len(), 5);
    let folded = fold(fresh, &LIM, log).unwrap();
    s.mark_durable(s.head()).unwrap();
    assert_eq!(folded, s);
    folded.validate(folded.head()).unwrap();
    let json = serde_json::to_vec(&folded).unwrap();
    let back = RoomState::from_snapshot(&folded, &json).unwrap();
    assert_eq!(back, folded);
    assert_eq!(back.hint_points(SlotNo(2)), 0, "1 check earned 1, one paid hint cost 1");
}

#[test]
fn fold_refuses_hint_events_that_break_the_rules() {
    let fresh = room(FREE);
    let added = |seq, location, status, paid| Event {
        seq: Seq(seq),
        slot: Some(SlotNo(1)),
        kind: EventKind::HintAdded { finding: SlotNo(1), location, status, paid },
    };
    let changed = |seq, location, status| Event {
        seq: Seq(seq),
        slot: Some(SlotNo(2)),
        kind: EventKind::HintStatus { finding: SlotNo(1), location, status },
    };
    let cases: [(Vec<Event>, CoreError); 5] = [
        (vec![added(1, 99, 0, false)], CoreError::UnknownLocation(99)),
        (vec![added(1, 11, 3, false)], CoreError::Hint(HintRefusal::BadStatus(3))),
        (vec![added(1, 11, 0, false), added(2, 11, 0, false)], CoreError::BadEvent),
        (vec![changed(1, 11, 30)], CoreError::BadEvent),
        (
            vec![added(1, 11, 0, false), changed(2, 11, 40)],
            CoreError::Hint(HintRefusal::BadStatus(40)),
        ),
    ];
    for (tail, want) in cases {
        assert_eq!(fold(fresh.clone(), &LIM, tail), Err(want));
    }
}

#[test]
fn a_snapshot_whose_hints_disagree_with_its_counters_is_refused() {
    let mut s = room(FREE);
    apply(&mut s, &at(), hint(2, vec![ask(1, 11, status::PRIORITY)], true, false)).unwrap();
    s.mark_durable(s.head()).unwrap();
    s.validate(s.head()).unwrap();
    let tamper = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = serde_json::to_value(&s).unwrap();
        edit(&mut v);
        let json = serde_json::to_vec(&v).unwrap();
        RoomState::from_snapshot(&s, &json).unwrap().validate(s.head())
    };
    let bad =
        |e: &dyn Fn(&mut serde_json::Value)| matches!(tamper(e), Err(CoreError::BadSnapshot(_)));
    assert!(bad(&|v| v["hint_writes"] = 2.into()), "head count");
    // Slot 1 is position 0; its location 11 is index 1 of 3.
    assert!(bad(&|v| v["hints"][0][1] = 3.into()), "an index outside the world");
    assert!(bad(&|v| v["hints"][0][2]["status"] = 5.into()));
    assert!(bad(&|v| v["hints"][0][2]["seq"] = 9.into()));
    assert!(
        bad(&|v| {
            let rec = serde_json::json!({"status": 0, "seq": 0});
            v["hints"].as_array_mut().unwrap().push(serde_json::json!([0, 0, rec]));
        }),
        "out of order"
    );
    assert!(bad(&|v| v["hints_used"]["9"] = 1.into()));
    assert!(bad(&|v| v["hints_used"]["2"] = 2.into()));
}

#[test]
fn a_hint_event_between_a_check_and_its_item_is_refused() {
    let fresh = room(FREE);
    let tail = vec![
        Event {
            seq: Seq(1),
            slot: Some(SlotNo(1)),
            kind: EventKind::Checked { cid: Cid(1), location: 10, at: Timestamp(0) },
        },
        Event {
            seq: Seq(2),
            slot: Some(SlotNo(1)),
            kind: EventKind::HintAdded { finding: SlotNo(1), location: 11, status: 0, paid: false },
        },
    ];
    assert_eq!(fold(fresh, &LIM, tail), Err(CoreError::PlacementMismatch));
}

#[allow(clippy::arithmetic_side_effects, reason = "a test model over small numbers")]
mod properties {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Check(u32, i64),
        Pay(u32, Vec<(u32, i64)>),
        Scout(u32, Vec<i64>),
    }

    fn op() -> impl Strategy<Value = Op> {
        let slot = 1..=2_u32;
        let loc = prop_oneof![Just(10_i64), Just(11), Just(12)];
        prop_oneof![
            (slot.clone(), loc.clone()).prop_map(|(s, l)| Op::Check(s, l)),
            (slot.clone(), proptest::collection::vec((1..=2_u32, loc.clone()), 0..4))
                .prop_map(|(s, a)| Op::Pay(s, a)),
            (slot, proptest::collection::vec(loc, 0..4)).prop_map(|(s, l)| Op::Scout(s, l)),
        ]
    }

    /// The independent model: points = check points x own checks - cost x paid hints, and a
    /// paid request buys at most one new unfound hint, only with points >= cost.
    #[derive(Default)]
    struct Model {
        checked: BTreeMap<u32, std::collections::BTreeSet<i64>>,
        hinted: std::collections::BTreeSet<(u32, i64)>,
        used: BTreeMap<u32, i64>,
    }

    fn size(slot: u32) -> i64 {
        if slot == 1 { 3 } else { 1 }
    }

    impl Model {
        fn points(&self, rules: HintRules, slot: u32) -> i64 {
            let cost = if rules.cost_pct == 0 {
                0
            } else {
                (i64::from(rules.cost_pct) * size(slot) / 100).max(1)
            };
            let checks =
                i64::try_from(self.checked.get(&slot).map_or(0, std::collections::BTreeSet::len))
                    .unwrap();
            i64::from(rules.check_points) * checks
                - cost * self.used.get(&slot).copied().unwrap_or(0)
        }
    }

    proptest! {
        #[test]
        fn hint_points_match_the_model_and_never_buy_more_than_one_paid_hint(
            pct in prop_oneof![Just(0_u8), Just(50), Just(100)],
            per in 0..3_u32,
            ops in proptest::collection::vec(op(), 0..40),
        ) {
            // Percentages whose float product is exact for worlds of 1 and 3, so the integer
            // model agrees with the reference's float rule.
            let rules = HintRules { cost_pct: pct, check_points: per };
            let mut s = room(rules);
            let mut m = Model::default();
            for op in ops {
                match op {
                    Op::Check(slot, l) => {
                        if slot == 2 && l != 10 { continue; }
                        check(&mut s, slot, l);
                        m.checked.entry(slot).or_default().insert(l);
                    }
                    Op::Pay(slot, asks) => {
                        let asks: Vec<_> = asks.into_iter().filter(|&(f, l)| f == 1 || l == 10).collect();
                        let before = m.points(rules, slot);
                        let cmd = hint(slot, asks.iter().map(|&(f, l)| ask(f, l, status::PRIORITY)).collect(), true, false);
                        let a = apply(&mut s, &at(), cmd).unwrap();
                        let created = a.events.len();
                        let new_unfound: std::collections::BTreeSet<_> = asks.iter().copied()
                            .filter(|k| !m.hinted.contains(k) && !m.checked.get(&k.0).is_some_and(|c| c.contains(&k.1)))
                            .collect();
                        if pct == 0 {
                            prop_assert_eq!(created, new_unfound.len());
                        } else {
                            let cost = (i64::from(pct) * size(slot) / 100).max(1);
                            prop_assert!(created <= 1);
                            prop_assert_eq!(created == 1, !new_unfound.is_empty() && before >= cost);
                        }
                        for e in &a.events {
                            if let EventKind::HintAdded { finding, location, paid, .. } = e.kind {
                                prop_assert!(paid);
                                m.hinted.insert((finding.0, location));
                                *m.used.entry(slot).or_default() += 1;
                            }
                        }
                        prop_assert_eq!(hinted(&a.reply).points, m.points(rules, slot));
                    }
                    Op::Scout(slot, locs) => {
                        let locs: Vec<_> = locs.into_iter().filter(|&l| slot == 1 || l == 10).collect();
                        let before = m.points(rules, slot);
                        let a = apply(&mut s, &at(), scout(slot, &locs)).unwrap();
                        let new: std::collections::BTreeSet<_> =
                            locs.iter().filter(|&&l| !m.hinted.contains(&(slot, l))).collect();
                        prop_assert_eq!(a.events.len(), new.len(), "a scout creates every new hint");
                        for e in &a.events {
                            if let EventKind::HintAdded { finding, location, paid, .. } = e.kind {
                                prop_assert!(!paid);
                                m.hinted.insert((finding.0, location));
                            }
                        }
                        prop_assert_eq!(m.points(rules, slot), before, "scouts are free");
                    }
                }
                for slot in [1, 2] {
                    prop_assert_eq!(s.hint_points(SlotNo(slot)), m.points(rules, slot));
                    // Paying never takes a slot below zero.
                    prop_assert!(s.hint_points(SlotNo(slot)) >= 0);
                }
            }
        }
    }
}

/// Review finding: the hint economy was read back from the snapshot, so a stale or edited
/// snapshot changed the room's points while the edge kept the seed's. The seed is the one
/// source: a snapshot takes the rules of the room it is read onto.
#[test]
fn a_snapshot_takes_its_hint_rules_from_the_room_not_the_file() {
    let s = room(PAID);
    let mut v = serde_json::to_value(&s).unwrap();
    v["hint_rules"] = serde_json::json!({"cost_pct": 0, "check_points": 9});
    let json = serde_json::to_vec(&v).unwrap();
    let back = RoomState::from_snapshot(&s, &json).unwrap();
    assert_eq!(back.hint_rules(), PAID);
}
