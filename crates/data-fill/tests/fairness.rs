//! Fairness statistics, and the engine-fairness run on the small fixture world.

use data_fill::fairness::{
    Freq, chi2, chi2_upper, coverage_gaps, independence, max_deviation, report, sample,
    uniform_distinct,
};
use data_fill::{Instance, Plan, World};
use serde_json::Value;

fn freq(rows: &[(u16, u32, u64)], samples: u64) -> Freq {
    let mut f = Freq { samples, ..Freq::default() };
    for (i, l, n) in rows {
        f.counts.insert((*i, *l), *n);
    }
    f
}

#[test]
fn entropy_of_a_uniform_item_is_log2_of_its_locations() {
    let f = freq(&[(0, 0, 25), (0, 1, 25), (0, 2, 25), (0, 3, 25)], 100);
    let (d, h) = f.spread(0);
    assert_eq!(d, 4);
    assert!((h - 2.0).abs() < 1e-12);
    let one = freq(&[(1, 7, 100)], 100);
    assert_eq!(one.spread(1), (1, 0.0));
}

#[test]
fn chi_square_tail_matches_tables() {
    // Table values: P(X >= 18.307 | k = 10) = 0.05, P(X >= 124.342 | k = 100) = 0.05.
    assert!((chi2_upper(18.307, 10.0) - 0.05).abs() < 0.003);
    assert!((chi2_upper(124.342, 100.0) - 0.05).abs() < 0.002);
    assert!(chi2_upper(0.0, 5.0) > 0.99);
    assert!(chi2_upper(500.0, 5.0) < 1e-6);
}

#[test]
fn chi_square_separates_same_and_different_distributions() {
    let a = freq(&[(0, 0, 500), (0, 1, 500)], 1000);
    let b = freq(&[(0, 0, 1010), (0, 1, 990)], 2000);
    let (_, dof, p) = chi2(&a, &b);
    assert!((dof - 1.0).abs() < 1e-12);
    assert!(p > 0.5, "same distribution, p {p}");
    let c = freq(&[(0, 0, 900), (0, 1, 100)], 1000);
    assert!(chi2(&c, &b).2 < 1e-6);
    let (dev, i, l) = max_deviation(&c, &b);
    assert_eq!((i, l), (0, 0));
    assert!((dev - (0.9 - 0.505)).abs() < 1e-12);
}

#[test]
fn coverage_gaps_flag_pairs_the_baseline_reaches_and_the_fill_misses() {
    let fill = freq(&[(0, 0, 1000), (0, 1, 0)], 1000);
    let base = freq(&[(0, 0, 500), (0, 1, 500), (0, 2, 1)], 1000);
    let gaps = coverage_gaps(&fill, &base, 10.0);
    assert_eq!(gaps.len(), 1, "location 2 is too rare in the baseline to judge");
    assert_eq!((gaps[0].0, gaps[0].1), (0, 1));
}

#[test]
fn independence_flags_seeds_that_copy_their_neighbour() {
    let mut rng = data_fill::fairness::seeded(7, b"testtest");
    let random: Vec<Vec<Option<u16>>> = (0..200)
        .map(|_| {
            (0..30).map(|_| Some(u16::try_from(rng.below(5).unwrap_or(0)).unwrap_or(0))).collect()
        })
        .collect();
    assert_eq!(independence(&random)["flag"], Value::Bool(false));
    // Every odd seed repeats most of the seed before it (not a chain, so distant seeds differ).
    let mut sticky = random.clone();
    for s in (1..sticky.len()).step_by(2) {
        sticky[s][..25].copy_from_slice(&random[s - 1][..25]);
    }
    assert_eq!(independence(&sticky)["flag"], Value::Bool(true));
}

#[test]
fn uniform_distinct_saturates_at_the_location_count() {
    assert!((uniform_distinct(10, 1000) - 10.0).abs() < 1e-6);
    assert!((uniform_distinct(1000, 1) - 1.0).abs() < 1e-9);
    assert!(uniform_distinct(0, 5).abs() < 1e-12);
}

fn fixture() -> (World, Instance, Vec<u16>, Vec<bool>) {
    let v: Value =
        serde_json::from_str(include_str!("fixtures/fairness_world.json")).expect("fixture parses");
    let inst: Instance = serde_json::from_value(v["instance"].clone()).expect("instance");
    let w = World::load(&v["manifest"]).expect("world");
    let pool = inst.pool.iter().map(|k| w.item(k).expect("item")).collect();
    let prog = inst.progression(&w);
    (w, inst, pool, prog)
}

#[test]
fn baseline_keeps_only_beatable_placements_and_report_measures_bias() {
    let (w, inst, pool, prog) = fixture();
    let plan = Plan::new(&w, &inst).expect("plan");
    let s = sample(&w, &plan, &pool, &prog, 300, 300, 100_000);
    assert_eq!(s.fill.samples, 300);
    assert_eq!(s.failed, 0);
    assert_eq!(s.baseline.samples, 300);
    assert!(s.accepted < s.attempts, "some uniform placements must be rejected");
    // key_a gates everything past the start, so it can never sit behind itself (l3, l4, l5).
    let key_a = w.item("key_a").expect("key_a");
    for l in ["l3", "l4", "l5"] {
        let l = w.loc(l).expect("loc");
        assert_eq!(s.baseline.p(key_a, l), 0.0);
        assert_eq!(s.fill.p(key_a, l), 0.0);
    }
    let r = report(&w, &s);
    assert!(r["baseline"]["p_value"].as_f64().is_some());
    assert!(
        r["baseline"]["max_deviation"]["value"].as_f64().is_some_and(|d| (0.0..=1.0).contains(&d))
    );
    assert_eq!(r["independence"]["flag"], Value::Bool(false));
}

#[test]
fn a_room_is_measured_against_the_same_baseline() {
    use data_fill::fairness::{Flat, report_named, sample_room};
    use data_fill::room;
    let v: Value =
        serde_json::from_str(include_str!("fixtures/room_world.json")).expect("fixture parses");
    let insts = room::instances(v["instances"].as_array().expect("list")).expect("instances");
    let insts: Vec<&Instance> = insts.iter().collect();
    let c = room::Compiled::new(&vec![&v["manifest"]; insts.len()], &insts, 1).expect("room");
    let (worlds, seats) = (&c.worlds, c.seats());
    let flat = Flat::new(&seats).expect("flat");
    let s = sample_room(&seats, &flat, 400, 400, 100_000).expect("room");
    assert_eq!((s.fill.samples, s.failed, s.baseline.samples), (400, 0, 400));
    assert!(s.accepted < s.attempts, "some uniform rooms must be rejected");
    // A player's first key opens the door of their own hall: it can never lie behind it, in
    // the fill or in any beatable room. (Flat numbering: player 1's items, then player 2's.)
    let key_a = flat
        .placed(&[vec![Some(data_fill::Given { to: 0, item: worlds[0].item("key_a").unwrap() })]])
        [0]
    .expect("flat item");
    for l in ["l3", "l4", "l5"] {
        let l = worlds[0].loc(l).expect("loc");
        assert_eq!(s.baseline.p(key_a, l), 0.0);
        assert_eq!(s.fill.p(key_a, l), 0.0);
    }
    let r = report_named(&|i| flat.item(i), &|l| flat.loc(l), &s);
    // Rounds favour early locations on purpose (docs/port-check.md); this catches gross breakage.
    assert!(r["baseline"]["max_deviation"]["value"].as_f64().is_some_and(|d| d < 0.25), "{r}");
    assert_eq!(r["independence"]["flag"], Value::Bool(false));
}

#[test]
fn a_room_with_a_mismatched_seat_is_refused_before_anything_is_indexed() {
    use data_fill::fairness::{Flat, sample_room};
    use data_fill::room::{self, Seat};
    let v: Value =
        serde_json::from_str(include_str!("fixtures/room_world.json")).expect("fixture parses");
    let w = World::load(&v["manifest"]).expect("world");
    let mut inst: Instance = serde_json::from_value(v["instances"][0].clone()).expect("instance");
    inst.locked.insert("l6".to_owned(), "coin".to_owned());
    inst.pool.retain(|k| k != "coin");
    let plan = Plan::new(&w, &inst).expect("plan");
    // The plan locks the sixth location of a world that has one.
    let tiny = World::load(&serde_json::json!({
        "options": [], "items": [{"key": "coin", "id": 1}],
        "locations": [{"key": "a", "id": 1}], "start": "r",
        "x.time": {"slices": 1, "start_mask": 1}, "defs": {},
        "regions": [{"key": "r", "can_stay": false, "checks": [{"loc": "a", "rule": {"true": {}}}],
                     "exits": [], "events": [], "stays": []}],
        "goal": {"reach": "region:r"}
    }))
    .expect("tiny world");
    let prog = vec![false];
    let seats = [Seat { w: &tiny, plan: &plan, progression: &prog }];
    let flat = Flat::new(&seats).expect("flat");
    assert!(room::check_placed(&seats, &[vec![None]]).is_err());
    assert!(sample_room(&seats, &flat, 1, 0, 0).is_err());
}

/// The mode comparison on the small room: every mode is measured on the same seeds, the exact
/// fill is at distance 0 from itself, and a deeper depth dial moves progression later.
#[test]
fn modes_are_compared_on_the_same_room_and_the_depth_dial_moves_progression() {
    use data_fill::fairness::{FILLS, OTHER_FILLS, shape, shape_report};
    use data_fill::room::{self, Algorithm};
    let v: Value =
        serde_json::from_str(include_str!("fixtures/room_world.json")).expect("fixture parses");
    let insts = room::instances(v["instances"].as_array().expect("list")).expect("instances");
    let insts: Vec<&Instance> = insts.iter().collect();
    let c = room::Compiled::new(&vec![&v["manifest"]; insts.len()], &insts, 1).expect("room");
    let seats = c.seats();
    let exact = shape(&seats, (300, FILLS), Algorithm::Exact, 20).expect("exact");
    let own = shape_report(&exact, Some(&exact));
    assert_eq!(own["against_exact"]["sphere_distribution_distance"], 0.0);
    assert_eq!(own["against_exact"]["location_chi2"], 0.0);
    assert_eq!((exact.failed, exact.own, exact.required.len()), (0, 300, 20));
    assert!(own["required_spheres_mean"].as_f64().is_some_and(|x| x >= 1.0), "{own}");
    let depth = |mode: &str| {
        let s = shape(&seats, (300, OTHER_FILLS), mode.parse().expect("mode"), 0).expect("shape");
        assert_eq!(s.own + s.fell_back, 300, "{mode}: a fallback seed is counted apart");
        assert_eq!(s.failed, 0, "{mode}");
        let r = shape_report(&s, Some(&exact));
        assert!(r["against_exact"]["sphere_distribution_distance"].as_f64().is_some(), "{r}");
        (r["progression_relative_depth_mean"].as_f64().expect("depth"), s.own)
    };
    // The exact fill on other seeds is the noise floor: independent samples are not at 0.
    let other = shape(&seats, (300, OTHER_FILLS), Algorithm::Exact, 0).expect("exact again");
    let floor = shape_report(&other, Some(&exact));
    assert!(floor["against_exact"]["location_chi2"].as_f64().is_some_and(|x| x > 0.0), "{floor}");
    for mode in ["level", "shuffle"] {
        let (shallow, _) = depth(&format!("{mode},depth=-300"));
        let (flat, own) = depth(mode);
        let (deep, _) = depth(&format!("{mode},depth=300"));
        assert!(own > 150, "{mode} placed {own} of 300 rooms itself");
        println!("{mode}: relative depth {shallow} {flat} {deep}");
        // Level fill places every item by the dial. Shuffle fill steers only the few items it
        // moves (about two a room here), so its dial is held to the two ends, not the middle.
        assert!(shallow < deep, "{mode}: {shallow} {flat} {deep}");
        assert!(mode == "shuffle" || (shallow < flat && flat < deep), "{mode}: {flat}");
    }
}
