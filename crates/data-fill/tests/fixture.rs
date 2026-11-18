//! A tiny hand-made world that exercises every construct the engine reads: time masks with
//! forward expansion, a stay rule, event counters, a self-granting disabled check, option
//! folding, a confined item, and the fill.

use std::collections::HashMap;

use data_fill::{Instance, Plan, World, fill, validate};
use serde_json::{Value, json};
use spec_rng::SpecRng;

const NIGHT: u64 = 0b1100;

fn manifest() -> Value {
    json!({
        "options": [{"key": "hard", "default": 0}],
        "items": [
            {"key": "lamp", "id": 1}, {"key": "rope", "id": 2},
            {"key": "charm", "id": 3}, {"key": "coin", "id": 4}
        ],
        "locations": [
            {"key": "gift", "id": 1}, {"key": "well", "id": 2}, {"key": "stall", "id": 3},
            {"key": "deep", "id": 4}, {"key": "flower", "id": 5},
            {"key": "shrine", "id": 6, "vanilla": "charm"}
        ],
        "start": "menu",
        "x.time": {"slices": 4, "start_mask": 1},
        "defs": {"can_climb": {"has": "rope"}},
        "regions": [
            {"key": "menu", "can_stay": true,
             "checks": [{"loc": "gift", "rule": {"true": {}}}],
             "exits": [{"to": "town", "rule": {"true": {}}}, {"to": "cave", "rule": {"has": "lamp"}}],
             "events": [], "stays": []},
            {"key": "town", "can_stay": true,
             "checks": [{"loc": "well", "rule": {"ref": "can_climb"}}],
             "exits": [{"to": "night_market", "rule": {"x.time_any": NIGHT}}],
             "events": [{"event": "bell", "rule": {"true": {}}}], "stays": []},
            {"key": "night_market", "can_stay": false,
             "checks": [{"loc": "stall", "rule": {"ge": [{"counter": "bell"}, 2]}},
                        {"loc": "shrine", "rule": {"true": {}}}],
             "exits": [], "events": [], "stays": []},
            {"key": "cave", "can_stay": true,
             "checks": [{"loc": "deep", "rule": {"any": [
                 {"all": [{"eq": [{"opt": "hard"}, 0]}, {"has": "rope"}]},
                 {"has": "lamp", "n": 2}]}}],
             "exits": [{"to": "garden", "rule": {"x.time_any": 0b1000}}],
             "events": [{"event": "bell", "rule": {"true": {}}}],
             "stays": [{"slice": 2, "rule": {"has": "charm"}}]},
            {"key": "garden", "can_stay": false,
             "checks": [{"loc": "flower", "rule": {"true": {}}}],
             "exits": [], "events": [], "stays": []}
        ],
        "goal": {"reach": "region:garden"}
    })
}

fn instance(included: &[&str]) -> Instance {
    Instance {
        locations: included.iter().map(|s| (*s).to_owned()).collect(),
        ..Instance::default()
    }
}

fn world(m: &Value) -> World {
    World::load(m).unwrap()
}

type Solved = (Vec<String>, HashMap<String, u64>, Vec<i64>);

fn solve(w: &World, included: &[&str], items: &[&str]) -> Solved {
    solve_opts(w, 0, included, items)
}

fn solve_opts(w: &World, hard: i64, included: &[&str], items: &[&str]) -> Solved {
    let mut inst = instance(included);
    inst.options = HashMap::from([("hard".to_owned(), hard)]);
    let plan = Plan::new(w, &inst).unwrap();
    let extra: Vec<u16> = items.iter().map(|k| w.item(k).unwrap()).collect();
    let s = plan.solve(w, &extra);
    let reached = (0..u32::try_from(w.locations()).unwrap())
        .filter(|l| s.reached(*l))
        .map(|l| w.loc_key(l).to_owned())
        .collect();
    let masks = s.reached_regions().map(|(r, m)| (w.region_key(r).to_owned(), m)).collect();
    (reached, masks, s.counters.clone())
}

const ALL: &[&str] = &["gift", "well", "stall", "deep", "flower", "shrine"];

#[test]
fn no_items_reaches_by_time_only() {
    let w = world(&manifest());
    let (reached, masks, _) = solve(&w, ALL, &[]);
    assert_eq!(reached, ["gift", "shrine"]);
    // The start region never waits; town expands to every slice and passes night on.
    assert_eq!(masks["menu"], 1);
    assert_eq!(masks["town"], 0b1111);
    assert_eq!(masks["night_market"], 0b1111);
    assert!(!masks.contains_key("cave"));
}

#[test]
fn a_region_that_cannot_wait_does_not_reach_night() {
    let mut m = manifest();
    m["regions"][1]["can_stay"] = json!(false);
    let w = world(&m);
    let (reached, masks, _) = solve(&w, ALL, &[]);
    assert_eq!(reached, ["gift"]);
    assert_eq!(masks["town"], 1);
    assert!(!masks.contains_key("night_market"));
}

#[test]
fn a_failing_stay_rule_stops_expansion() {
    let w = world(&manifest());
    let (reached, masks, _) = solve(&w, ALL, &["lamp"]);
    assert_eq!(masks["cave"], 0b0011, "waiting into slice 2 needs the charm");
    assert!(!reached.contains(&"flower".to_owned()));
    let (reached, masks, _) = solve(&w, ALL, &["lamp", "charm"]);
    assert_eq!(masks["cave"], 0b1111);
    assert!(reached.contains(&"flower".to_owned()));
}

#[test]
fn event_instances_add_to_their_counter() {
    let w = world(&manifest());
    let (reached, _, counters) = solve(&w, ALL, &[]);
    assert_eq!(counters, [1]);
    assert!(!reached.contains(&"stall".to_owned()));
    let (reached, _, counters) = solve(&w, ALL, &["lamp"]);
    assert_eq!(counters, [2], "bell fires once in town and once in the cave");
    assert!(reached.contains(&"stall".to_owned()));
}

#[test]
fn a_disabled_check_grants_its_vanilla_item() {
    let w = world(&manifest());
    // Shrine is not a location this seed, so reaching it grants its vanilla charm.
    let (reached, masks, _) = solve(&w, &["gift", "well", "stall", "deep", "flower"], &["lamp"]);
    assert_eq!(masks["cave"], 0b1111);
    assert!(reached.contains(&"flower".to_owned()));
}

#[test]
fn options_bind_per_seed() {
    let w = world(&manifest());
    let has_deep =
        |hard| solve_opts(&w, hard, ALL, &["lamp", "rope"]).0.contains(&"deep".to_owned());
    assert!(has_deep(0));
    assert!(!has_deep(1));
    let (reached, ..) = solve_opts(&w, 1, ALL, &["lamp", "lamp"]);
    assert!(reached.contains(&"deep".to_owned()));
}

#[test]
fn adding_items_one_by_one_matches_a_fresh_solve() {
    let w = world(&manifest());
    let plan = Plan::new(&w, &instance(ALL)).unwrap();
    let mut inc = plan.solve(&w, &[]);
    for k in ["rope", "lamp", "charm"] {
        inc.add(w.item(k).unwrap(), 1);
        inc.run();
    }
    let ids: Vec<u16> = ["rope", "lamp", "charm"].iter().map(|k| w.item(k).unwrap()).collect();
    let fresh = plan.solve(&w, &ids);
    assert!((0..6).all(|l| inc.reached(l) == fresh.reached(l)));
    assert_eq!(inc.masks, fresh.masks);
    assert_eq!(inc.counters, fresh.counters);
    assert!(fresh.goal());
}

#[test]
fn non_monotone_not_and_def_cycles_are_rejected() {
    let mut m = manifest();
    m["regions"][0]["checks"][0]["rule"] = json!({"not": {"has": "lamp"}});
    assert!(World::load(&m).unwrap_err().contains("non-monotone"));
    let mut m = manifest();
    m["defs"] = json!({"a": {"ref": "b"}, "b": {"ref": "a"}});
    m["regions"][0]["checks"][0]["rule"] = json!({"ref": "a"});
    assert!(World::load(&m).unwrap_err().contains("cycle"));
    // `not` over an option comparison folds away.
    let mut m = manifest();
    m["regions"][0]["checks"][0]["rule"] = json!({"not": {"eq": [{"opt": "hard"}, 1]}});
    assert!(World::load(&m).is_ok());
}

fn fill_instance() -> Instance {
    let mut inst = instance(ALL);
    inst.pool = ["lamp", "rope", "charm", "coin", "coin", "coin"].map(str::to_owned).to_vec();
    inst.pool_class = HashMap::from([
        ("lamp".to_owned(), "progression".to_owned()),
        ("rope".to_owned(), "progression".to_owned()),
        ("charm".to_owned(), "progression".to_owned()),
        ("coin".to_owned(), "filler".to_owned()),
    ]);
    // The charm may only go to the town group (the well).
    inst.item_groups = HashMap::from([("charm".to_owned(), "town".to_owned())]);
    inst.location_groups = HashMap::from([("well".to_owned(), "town".to_owned())]);
    inst
}

#[test]
fn fill_is_beatable_confined_and_deterministic() {
    let w = world(&manifest());
    let inst = fill_instance();
    let plan = Plan::new(&w, &inst).unwrap();
    let charm = w.item("charm").unwrap();
    let well = w.loc("well").unwrap() as usize;
    for seed in 0u8..32 {
        let mut a = SpecRng::from_seed([seed; 32]);
        let filled = fill(&w, &plan, &mut a, 20).unwrap();
        assert_eq!(filled.placed.iter().flatten().count(), 6);
        assert_eq!(filled.placed[well], Some(charm), "seed {seed}: charm confined to the well");
        assert_eq!(validate(&w, &plan, &filled.placed), (true, 6), "seed {seed}");
        let mut b = SpecRng::from_seed([seed; 32]);
        assert_eq!(fill(&w, &plan, &mut b, 20).unwrap().placed, filled.placed);
    }
}

#[test]
fn fill_reports_count_mismatch_and_impossible_seeds() {
    let w = world(&manifest());
    let mut inst = fill_instance();
    inst.pool.pop();
    let plan = Plan::new(&w, &inst).unwrap();
    let err = fill(&w, &plan, &mut SpecRng::from_seed([0; 32]), 3).unwrap_err();
    assert!(matches!(err, data_fill::FillError::Count { items: 5, locations: 6 }));
    // The charm confined to a location that needs the charm cannot be placed.
    let mut inst = fill_instance();
    inst.location_groups = HashMap::from([("flower".to_owned(), "town".to_owned())]);
    let plan = Plan::new(&w, &inst).unwrap();
    let err = fill(&w, &plan, &mut SpecRng::from_seed([0; 32]), 3).unwrap_err();
    assert_eq!(err, data_fill::FillError::Stuck { item: "charm".to_owned(), attempts: 3 });
}

#[test]
fn spheres_collect_round_by_round_and_skip_non_progression() {
    let m = manifest();
    let w = world(&m);
    let plan = Plan::new(&w, &instance(&["gift", "well", "stall", "deep", "flower"])).unwrap();
    let progression: Vec<bool> = (0..4).map(|i| w.item_key(i) != "coin").collect();
    let place = |pairs: &[(&str, &str)]| {
        let mut placed = vec![None; w.locations()];
        for (l, i) in pairs {
            placed[w.loc(l).unwrap() as usize] = Some(w.item(i).unwrap());
        }
        placed
    };
    let keys = |ls: &[u32]| ls.iter().map(|l| w.loc_key(*l).to_owned()).collect::<Vec<_>>();
    let placed = place(&[
        ("gift", "lamp"),
        ("stall", "rope"),
        ("flower", "coin"),
        ("well", "coin"),
        ("deep", "coin"),
    ]);
    let s = data_fill::spheres::spheres(&w, &plan, &placed, &progression);
    let got: Vec<Vec<String>> = s.spheres.iter().map(|x| keys(x)).collect();
    assert_eq!(got, vec![vec!["gift"], vec!["stall", "flower"], vec!["well", "deep"]]);
    assert!(s.beatable && s.unreachable.is_empty());

    // Rope locked behind itself: well and deep are never reached. A coin counts for nothing.
    let placed = place(&[
        ("gift", "lamp"),
        ("deep", "rope"),
        ("well", "coin"),
        ("stall", "coin"),
        ("flower", "coin"),
    ]);
    let s = data_fill::spheres::spheres(&w, &plan, &placed, &progression);
    assert_eq!(s.spheres.len(), 2);
    assert_eq!(keys(&s.unreachable), vec!["well", "deep"]);
    assert!(s.beatable);
    let all_prog = vec![true; w.items()];
    let placed = place(&[("gift", "coin")]);
    let none = data_fill::spheres::spheres(&w, &plan, &placed, &all_prog);
    assert_eq!(none.spheres, vec![vec![w.loc("gift").unwrap()]]);
    assert!(!none.beatable);
}

#[test]
fn the_section_decodes_to_a_manifest_that_compiles_to_the_same_bytes() {
    let mut m = manifest();
    // Shapes the compiler normalizes, and every integer operator.
    m["regions"][0]["checks"][0]["rule"] = json!({"has_all": ["lamp", "rope"]});
    m["rolls"] = json!({"price": {"note": "seeded"}});
    m["regions"][1]["checks"][0]["rule"] = json!({"le": [
        {"x.roll": "price", "at": "well"},
        {"int": ["+", {"int": ["*", {"count": "coin"}, 2]}, {"int": ["div", {"count": "coin"}, 4]},
                 {"int": ["min", 3, {"opt": "hard"}]}, {"int": ["max", {"counter": "bell"}]}]}]});
    let bytes = data_fill::section::compile(&m).unwrap();
    let w = World::from_package(&bytes).unwrap();
    let d = data_fill::decode::manifest(&w);
    assert_eq!(data_fill::section::compile(&d).unwrap(), bytes);
    assert_eq!(
        d["regions"][0]["checks"][0]["rule"],
        json!({"all": [{"has": "lamp"}, {"has": "rope"}]})
    );
    assert_eq!(d["regions"][3]["checks"][0]["rule"]["any"][1], json!({"has": "lamp", "n": 2}));
    assert_eq!(d["regions"][1]["checks"][0]["rule"], m["regions"][1]["checks"][0]["rule"]);
    assert_eq!(d["locations"][5], json!({"key": "shrine", "id": 6, "vanilla": "charm"}));
    assert_eq!(d["defs"], m["defs"]);
}

#[test]
fn a_sum_of_copies_over_items_is_one_group_counter() {
    let mut m = manifest();
    // Distinct lamps (at most one counts) plus every coin: needs a lamp and two coins.
    let sum = json!({"int": ["+", {"int": ["min", {"count": "lamp"}, 1]}, {"count": "coin"}]});
    m["regions"][0]["checks"][0]["rule"] = json!({"ge": [sum, 3]});
    let bytes = data_fill::section::compile(&m).unwrap();
    let w = World::from_package(&bytes).unwrap();
    let d = data_fill::decode::manifest(&w);
    assert_eq!(d["regions"][0]["checks"][0]["rule"]["ge"][0], sum);
    assert_eq!(data_fill::section::compile(&d).unwrap(), bytes);
    let gift = |items: &[&str]| solve(&w, ALL, items).0.contains(&"gift".to_owned());
    assert!(!gift(&["lamp", "lamp", "coin"]), "the second lamp is capped");
    assert!(gift(&["lamp", "coin", "coin"]));
    assert!(!gift(&["coin", "coin"]));
    assert!(gift(&["coin", "coin", "coin"]));
}

#[test]
fn compile_errors_name_the_manifest_path() {
    let cases = [
        (
            "/regions/0/checks/0/rule",
            json!({"has": "nope"}),
            r#"regions["menu"].checks["gift"].rule.has: unknown item "nope""#,
        ),
        (
            "/regions/1/exits/0/rule",
            json!({"any": [{"true": {}}, {"ref": "missing"}]}),
            r#"regions["town"].exits["night_market"].rule.any[1].ref: unknown def "missing""#,
        ),
        (
            "/regions/3/stays/0/rule",
            json!({"ge": [{"opt": "easy"}, 1]}),
            r#"regions["cave"].stays[2].rule.ge[0].opt: undeclared option "easy""#,
        ),
        ("/goal", json!({"all": [{"x.time_any": -1}]}), "goal.all[0].x.time_any: expected a mask"),
        (
            "/regions/0/checks/0/rule",
            json!({"reach": "region:garden"}),
            r#"regions["menu"].checks["gift"].rule.reach: `reach` is allowed in the goal only"#,
        ),
        (
            "/regions/0/checks/0/rule",
            json!({"lt": [{"count": "coin"}, 2]}),
            "regions[\"menu\"].checks[\"gift\"].rule.lt: this comparison can turn false as the \
          player collects (it tests for fewer items or counters); compare with `ge` or `gt`, \
          items on the left",
        ),
        (
            "/regions/0/checks/0/rule",
            json!({"ge": [3, {"counter": "bell"}]}),
            "regions[\"menu\"].checks[\"gift\"].rule.ge: this comparison can turn false as the \
          player collects (it tests for fewer items or counters); compare with `ge` or `gt`, \
          items on the left",
        ),
        (
            "/defs/can_climb",
            json!({"ge": [{"int": ["*", {"count": "rope"}, -1]}, 0]}),
            r#"defs["can_climb"].ge[0].int: a product that reads items needs a non-negative constant factor"#,
        ),
    ];
    for (at, rule, want) in cases {
        let mut m = manifest();
        *m.pointer_mut(at).unwrap() = rule;
        assert_eq!(World::load(&m).unwrap_err(), want);
    }
}

#[test]
fn a_damaged_section_is_rejected_before_any_solve() {
    let bytes = data_fill::section::compile(&manifest()).unwrap();
    assert!(World::from_package(&bytes).is_ok());
    assert!(World::from_package(&bytes[..bytes.len() - 3]).is_err(), "truncated");
    // Every single-byte change either still loads or is rejected; none may panic.
    for k in 0..bytes.len() {
        let mut b = bytes.clone();
        b[k] ^= 0xff;
        let _ = World::from_package(&b);
    }
}

#[test]
fn a_reset_state_solves_like_a_fresh_one() {
    let w = world(&manifest());
    let plan = Plan::new(&w, &instance(ALL)).unwrap();
    let lamp = w.item("lamp").unwrap();
    let charm = w.item("charm").unwrap();
    let mut r = plan.solve(&w, &[lamp, charm]);
    plan.solve_into(&mut r, &[], &[]);
    let fresh = plan.solve(&w, &[]);
    assert!((0..6).all(|l| r.reached(l) == fresh.reached(l)));
    assert_eq!((&r.masks, &r.counters, &r.counts), (&fresh.masks, &fresh.counters, &fresh.counts));
}

#[test]
fn start_items_own_their_half_days_before_the_first_visit() {
    let mut m = manifest();
    m["x.time"]["owned"] = json!({
        "when": {"eq": [{"opt": "hard"}, 0]}, "none_owned_mask": 1,
        "half_days": [{"first": 0, "last": 1, "rule": {"true": {}}},
                      {"first": 2, "last": 3, "rule": {"has": "charm"}}]});
    let w = world(&m);
    let mut inst = instance(ALL);
    assert_eq!(solve(&w, ALL, &[]).1["menu"], 0b0011);
    // A start item, not a found one: ownership must still see it.
    inst.logic_start = HashMap::from([("charm".to_owned(), 1)]);
    let plan = Plan::new(&w, &inst).unwrap();
    assert_eq!(plan.solve(&w, &[]).masks[w.region("menu").unwrap() as usize], 0b1111);
    // The condition is an option: clocks off means every slice is owned.
    assert_eq!(solve_opts(&w, 1, ALL, &[]).1["menu"], 1);
}
