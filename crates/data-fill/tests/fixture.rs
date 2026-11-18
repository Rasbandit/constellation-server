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
        let filled = fill(&w, &plan, &mut a, config::FILL_MAX_ATTEMPTS).unwrap();
        assert_eq!(filled.placed.iter().flatten().count(), 6);
        assert_eq!(filled.placed[well], Some(charm), "seed {seed}: charm confined to the well");
        assert_eq!(validate(&w, &plan, &filled.placed), (true, 6), "seed {seed}");
        let mut b = SpecRng::from_seed([seed; 32]);
        assert_eq!(
            fill(&w, &plan, &mut b, config::FILL_MAX_ATTEMPTS).unwrap().placed,
            filled.placed
        );
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

/// A rules section whose one def body (about 60 KB) is rewritten in place as a chain of
/// `not` nodes ending in `true`: nesting far past the cap, which must be an error, not a
/// stack overflow.
#[test]
fn a_deeply_nested_rule_is_rejected_not_a_stack_overflow() {
    use data_fill::section::{op, sections};
    let mut m = manifest();
    let wide = json!({"has_any": vec!["lamp"; 255]});
    m["defs"]["wide"] = json!({"all": vec![wide; 100]});
    m["regions"][4]["checks"][0]["rule"] = json!({"ref": "wide"});
    let (mut rules, game) = sections(&m).unwrap();
    assert!(World::from_sections(rules.clone(), game.clone()).is_ok());
    let at = rules
        .windows(6)
        .position(|x| x[0] == op::ALL && x[1] == 100 && x[4] == op::HAS_ANY && x[5] == 255)
        .unwrap();
    let len = 4 + usize::from(u16::from_le_bytes([rules[at + 2], rules[at + 3]]));
    rules[at..at + len - 1].fill(op::NOT);
    rules[at + len - 1] = op::TRUE;
    let err = World::from_sections(rules, game).unwrap_err();
    assert!(err.contains("nested"), "{err}");
}

/// A chain of defs nested past the cap fails to compile with a message, before the
/// compiler's own recursion gets deep.
#[test]
fn a_def_chain_nested_too_deep_does_not_compile() {
    let chain = |n: usize| {
        let mut m = manifest();
        for k in 0..n {
            let next = json!({"ref": format!("d{}", k + 1)});
            m["defs"][format!("d{k}")] = json!({"all": [next, {"has": "lamp"}]});
        }
        m["defs"][format!("d{n}")] = json!({"has": "rope"});
        m["regions"][1]["checks"][0]["rule"] = json!({"ref": "d0"});
        m
    };
    // Each def is two levels (`all`, then `ref`), so 120 defs fit the cap and 300 do not.
    let w = World::load(&chain(120)).unwrap();
    // Evaluating the whole chain (lamp and rope held) stays inside a test thread's stack.
    let held = [w.item("lamp").unwrap(), w.item("rope").unwrap()];
    let plan = Plan::new(&w, &instance(ALL)).unwrap();
    let s = plan.solve(&w, &held);
    assert!(s.reached(w.loc("well").unwrap()));
    let err = data_fill::section::compile(&chain(300)).unwrap_err();
    assert!(err.contains("nested"), "{err}");
}

/// Start counts are instance data: negative ones and ones above the location count are
/// refused before any solve allocates or loops by them, and sums cannot overflow.
#[test]
fn start_counts_out_of_range_are_rejected() {
    let w = world(&manifest());
    for n in [-1, 1_000_000_000_000, i64::MAX] {
        let mut inst = instance(ALL);
        inst.logic_start = HashMap::from([("lamp".to_owned(), n)]);
        inst.start_inventory = vec!["lamp".to_owned()];
        let err = Plan::new(&w, &inst).unwrap_err();
        assert!(matches!(&err, data_fill::FillError::Instance(m) if m.contains("lamp")), "{err:?}");
    }
    let mut inst = instance(ALL);
    inst.logic_start = HashMap::from([("lamp".to_owned(), 6)]);
    assert!(Plan::new(&w, &inst).is_ok(), "as many as there are locations");
}

/// A rules section whose dependency index or `reach` placement disagrees with its rules
/// loads but solves wrong (a site never rechecked), so the loader refuses it.
#[test]
fn an_index_that_disagrees_with_the_rules_is_rejected() {
    use data_fill::section::{HEADER, op, sub};
    let (rules, game) = data_fill::section::sections(&manifest()).unwrap();
    let span = |b: &[u8], k: usize| {
        let at = HEADER + 8 * k;
        let n = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
        n(at)..n(at) + n(at + 4)
    };
    let load = |b: Vec<u8>| World::from_sections(b, game.clone());
    assert!(load(rules.clone()).is_ok());
    // No site reads time any more, by the index.
    let mut b = rules.clone();
    b[span(&rules, sub::TIME_SITES)].fill(0);
    assert!(load(b).unwrap_err().contains("dependency index"));
    // Every item's readers become site 0 (in range, so only a rebuild sees it).
    let mut b = rules.clone();
    let items = span(&rules, sub::ITEM_SITES);
    let starts = u32::from_le_bytes(b[items.start..items.start + 4].try_into().unwrap());
    let sw = usize::from(b[items.start + 4]);
    let list = items.start + 5 + (starts as usize * sw).div_ceil(8) + 5;
    b[list..items.end].fill(0);
    assert!(load(b).unwrap_err().contains("dependency index"));
    // `has lamp, n: 2` in the cave check becomes `reach region 0`.
    let code = span(&rules, sub::CODE);
    let at =
        code.start + rules[code.clone()].windows(3).position(|x| x == [op::HAS_N, 0, 2]).unwrap();
    let mut b = rules.clone();
    b[at..at + 3].copy_from_slice(&[op::REACH, 0, 0]);
    assert!(load(b).unwrap_err().contains("goal"));
}

/// Folding an option read into a 16-bit constant adds a byte, so a rule near the 64 KiB list
/// limit can grow past it; the fold keeps such a rule's original bytes, and the rule after it
/// in an `any` still reads right (`rope` alone opens the well).
#[test]
fn a_rule_that_folds_past_the_length_field_keeps_its_bytes() {
    let mut m = manifest();
    let cmp = json!({"ge": [{"count": "lamp"}, {"opt": "hard"}]});
    let part = json!({"all": vec![cmp; 255]});
    let wide = json!({"all": vec![part; 42]});
    m["regions"][1]["checks"][0]["rule"] = json!({"any": [wide, {"has": "rope"}]});
    let w = world(&m);
    let (reached, ..) = solve_opts(&w, 1, ALL, &["rope"]);
    assert!(reached.contains(&"well".to_owned()), "{reached:?}");
    let (reached, ..) = solve_opts(&w, 1, ALL, &["lamp"]);
    assert!(reached.contains(&"well".to_owned()), "{reached:?}");
    let (reached, ..) = solve_opts(&w, 2, ALL, &["lamp"]);
    assert!(!reached.contains(&"well".to_owned()), "{reached:?}");
}

/// With several unknown keys the error names the first in key order, whatever the order of
/// the instance's maps.
#[test]
fn unknown_key_errors_do_not_depend_on_map_order() {
    let w = world(&manifest());
    for _ in 0..32 {
        let mut inst = instance(ALL);
        let keys = ["zz", "aa", "mm", "qq", "bb", "yy"];
        inst.logic_start = keys.iter().map(|k| ((*k).to_owned(), 1)).collect();
        inst.locked = keys.iter().map(|k| ((*k).to_owned(), "lamp".to_owned())).collect();
        let err = Plan::new(&w, &inst).unwrap_err();
        assert!(format!("{err:?}").contains("\\\"aa\\\""), "{err:?}");
        inst.logic_start.clear();
        let err = Plan::new(&w, &inst).unwrap_err();
        assert!(format!("{err:?}").contains("\\\"aa\\\""), "{err:?}");
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
