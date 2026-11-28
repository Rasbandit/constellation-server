//! The build tool end to end: the example world builds to a manifest the engine prepares and
//! fills beatable, the canonical bytes are pinned, and every check names its place.

mod grammar;
mod lang;
mod pack;
mod seeds;
mod support;
mod trials;

use std::collections::HashMap;

use data_fill::seed::{Player, generate};
use data_fill::{World as EngineWorld, job};
use support::{F, tidewell, two_rooms};
use world_build::model::{At, Def, Grant, Members, Pos, Rule, Verb, World};
use world_build::{Built, build_model};

fn built(w: &World) -> Built {
    build_model(w, "w").unwrap_or_else(|e| {
        panic!("{}", e.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
    })
}

/// Every error of building `w`, as printed.
fn errors(w: &World) -> Vec<String> {
    build_model(w, "w").map(|_| ()).unwrap_err().iter().map(ToString::to_string).collect()
}

/// The one error of `w` that contains `needle`, at `line:col`.
fn fails(w: &World, needle: &str, pos: Pos) {
    let all = errors(w);
    let at = format!("w:{}:{}: ", pos.line, pos.col);
    assert!(
        all.iter().any(|e| e.starts_with(&at) && e.contains(needle)),
        "want {at}..{needle}.. in {all:#?}"
    );
}

fn text(r: &str, pos: Pos) -> At<Rule> {
    At::new(Rule::Text(r.to_owned()), pos)
}

/// Tidewell with the rule of check `key` replaced by `rule` at `pos`.
fn with_check_rule(key: &str, rule: &str, pos: Pos) -> World {
    let mut w = tidewell();
    let c = w.regions.iter_mut().flat_map(|r| &mut r.checks).find(|c| c.key.v == key).unwrap();
    c.rule = text(rule, pos);
    w
}

const P: Pos = Pos { file: 0, line: 900, col: 5 };

#[test]
fn the_smallest_world_builds_to_the_pinned_canonical_bytes() {
    let b = built(&two_rooms());
    let want = concat!(
        r#"{"defs":{},"game":"two_rooms","goal":{"reach":"region:vault"},"#,
        r#""items":[{"id":1,"key":"key"},{"id":2,"key":"nothing"}],"#,
        r#""locations":[{"id":2,"key":"chest"},{"id":1,"key":"shelf"}],"options":[],"#,
        r#""regions":[{"can_stay":false,"checks":[{"loc":"shelf","rule":{"true":{}}}],"events":[],"#,
        r#""exits":[{"rule":{"has":"key"},"to":"vault"}],"key":"hall","stays":[]},"#,
        r#"{"can_stay":false,"checks":[{"loc":"chest","rule":{"true":{}}}],"events":[],"exits":[],"#,
        r#""key":"vault","stays":[]}],"start":"hall","version":1,"x.time":{"slices":1,"start_mask":1}}"#,
        "\n"
    );
    assert_eq!(b.json(), want);
    let inst = b.default_instance().unwrap();
    assert_eq!(inst.pool, ["key", "nothing"]);
}

#[test]
fn building_the_same_world_twice_gives_the_same_bytes() {
    let (a, b) = (built(&tidewell()), built(&tidewell()));
    assert_eq!(a.json(), b.json());
    assert!(a.json().len() > 1000);
}

#[test]
fn the_example_world_round_trips_through_the_engine_with_no_warnings() {
    let b = built(&tidewell());
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
    // The upload check the server runs: a game, a version, and it compiles and prepares.
    job::prepare(b.json().as_bytes()).unwrap();
    let w = EngineWorld::load(&serde_json::from_str(b.json()).unwrap()).unwrap();
    assert!(w.has_shuffles());
    assert_eq!((w.items(), w.locations()), (7, 17));
    let inst = b.default_instance().unwrap();
    assert_eq!(inst.locations.len(), 17);
    assert_eq!(inst.locked.get("old_chest").map(String::as_str), Some("chart"));
    assert_eq!(inst.excluded, ["tide_pool"]);
    assert_eq!(inst.pool.len(), 16);
    assert_eq!(inst.pool.iter().filter(|i| *i == "shell").count(), 9);
}

/// Build the example, load it in the engine, fill seeds 1 to 50 at 1 and 4 players: every seed
/// generates, and `generate` refuses a world a player cannot finish, so every seed is beatable.
#[test]
fn the_example_world_fills_beatable_for_seeds_1_to_50_at_1_and_4_players() {
    let b = built(&tidewell());
    let m: serde_json::Value = serde_json::from_str(b.json()).unwrap();
    let inst = b.default_instance().unwrap();
    for players in [1usize, 4] {
        let p: Vec<Player> = (0..players)
            .map(|k| Player { name: format!("P{k}"), instance: inst.clone() })
            .collect();
        for seed in 1..=50 {
            let bundle = generate(&m, "s", &p, seed)
                .unwrap_or_else(|e| panic!("{players} players, seed {seed}: {e}"));
            assert_eq!(bundle.slots.len(), players);
        }
    }
}

#[test]
fn other_option_values_build_instances_that_fill_beatable() {
    let b = built(&tidewell());
    let m: serde_json::Value = serde_json::from_str(b.json()).unwrap();
    let opts: HashMap<String, i64> =
        [("market_stalls", 0), ("start_gift", 1), ("pearls_needed", 3), ("tricky_climbs", 1)]
            .map(|(k, v)| (k.to_owned(), v))
            .into();
    let inst = b.instance(&opts).unwrap();
    assert_eq!(inst.locations.len(), 15);
    assert_eq!(inst.start_inventory, ["lamp"]);
    assert!(!inst.pool.contains(&"lamp".to_owned()));
    for seed in 1..=10 {
        let p = vec![Player { name: "P".into(), instance: inst.clone() }];
        generate(&m, "s", &p, seed).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
    }
    assert!(b.instance(&[("pearls_needed".to_owned(), 9)].into()).unwrap_err().contains("outside"));
    assert!(b.instance(&[("nope".to_owned(), 1)].into()).unwrap_err().contains("unknown option"));
}

#[test]
fn every_error_is_collected_not_only_the_first() {
    let mut w = with_check_rule("bollard", "has(nope)", P);
    w.regions[1].checks[2].rule = text("lamp", Pos::new(901, 1));
    let all = errors(&w);
    assert!(all.len() >= 2, "{all:?}");
    assert!(all[0].starts_with("w:900:") && all[1].starts_with("w:901:"), "in file order: {all:?}");
}

#[test]
fn a_rule_error_names_the_line_and_the_column_of_its_character() {
    // `has(` is at column 5, so the bad name 4 characters in is at column 9.
    fails(&with_check_rule("bollard", "has(nope)", P), "unknown item `nope`", P.right(4));
    fails(
        &with_check_rule("bollard", "has(oar) and", P),
        "ends where a value was expected",
        P.right(12),
    );
    fails(&with_check_rule("bollard", "a < b < c", P), "do not chain", P.right(6));
}

#[test]
fn unknown_names_say_what_the_name_is() {
    fails(&with_check_rule("bollard", "lamp", P), "is an item", P);
    fails(&with_check_rule("bollard", "lamp", P), "has(lamp)", P);
    fails(&with_check_rule("bollard", "dock", P), "is a region", P);
    fails(&with_check_rule("bollard", "mystery", P), "unknown name `mystery`", P);
    fails(&with_check_rule("bollard", "opt.nope", P), "unknown option `nope`", P);
    fails(
        &with_check_rule("bollard", "opt.start_gift == sword", P),
        "not a value of option",
        P.right(18),
    );
    fails(&with_check_rule("bollard", "opt.pearls_needed", P), "not a toggle", P);
    fails(&with_check_rule("bollard", "at.wardens.cellar", P), "has no slot `cellar`", P);
    fails(
        &with_check_rule("bollard", "at.wardens.tower_door == shark", P),
        "has no thing `shark`",
        P.right(25),
    );
    fails(
        &with_check_rule("bollard", "has(tools)", P),
        "is an item group, not an item",
        P.right(4),
    );
    fails(&with_check_rule("bollard", "zap(oar)", P), "unknown function", P);
}

#[test]
fn a_name_declared_twice_is_an_error_at_the_second() {
    // A check may share a name with an item now (checks have their own namespace); a def
    // still may not.
    let mut w = tidewell();
    w.defs[0].name = At::new("oar".into(), P);
    fails(&w, "`oar` is already an item", P);
    let mut w = tidewell();
    w.regions[1].key = At::new("dock".into(), P);
    fails(&w, "`dock` is already a region", P);
    let mut w = tidewell();
    w.items[0].key = At::new("nothing".into(), P);
    fails(&w, "reserved", P);
    let mut w = tidewell();
    w.items[0].key = At::new("Oar-1".into(), P);
    fails(&w, "not a valid item name", P);
}

#[test]
fn a_region_no_exit_leads_to_is_an_error() {
    let mut w = tidewell();
    let f = F::default();
    let mut island = f.region("far_island", vec![f.check("driftwood", "true")], vec![], &[]);
    island.key.pos = P;
    w.regions.push(island);
    fails(&w, "region `far_island` cannot be reached", P);
    let mut w = tidewell();
    w.regions[0].exits[3].to = At::new("moon".into(), P);
    fails(&w, "not a region", P);
    let mut w = tidewell();
    w.start = Some(At::new("moon".into(), P));
    fails(&w, "start `moon` is not a region", P);
}

#[test]
fn rules_that_could_turn_false_are_refused_d132() {
    fails(&with_check_rule("bollard", "not has(oar)", P), "`not` over what the player holds", P);
    fails(&with_check_rule("bollard", "not beacon_lit", P), "`not` over what the player holds", P);
    fails(
        &with_check_rule("bollard", "not at.wardens.tower_door", P),
        "`not` over what the player holds",
        P,
    );
    fails(&with_check_rule("bollard", "count(pearl) < 3", P), "can turn false", P);
    fails(&with_check_rule("bollard", "count(pearl) == 2", P), "can turn false", P);
    fails(&with_check_rule("bollard", "3 >= count(pearl)", P), "can turn false", P);
    fails(&with_check_rule("bollard", "count(pearl) >= count(oar)", P), "can turn false", P);
    fails(
        &with_check_rule("bollard", "count(pearl) * opt.pearls_needed >= 2", P),
        "a product that reads items",
        P,
    );
    fails(
        &with_check_rule("bollard", "count(pearl) div 0 >= 1", P),
        "positive number",
        P.right(17),
    );
    // Options, and name tests of a shuffle, may be negated and compared freely.
    built(&with_check_rule("bollard", "not opt.tricky_climbs or opt.pearls_needed < 3", P));
    built(&with_check_rule("bollard", "not at.wardens.tower_door == crab", P));
    built(&with_check_rule("bollard", "2 <= count(pearl) and count(pearl) * 2 + 1 > 2", P));
}

#[test]
fn a_rule_reads_only_its_own_world_d133() {
    fails(
        &with_check_rule("bollard", "has(other.bell)", P),
        "reads only its own world",
        P.right(4),
    );
    fails(&with_check_rule("bollard", "player2.beacon_lit", P), "D-133", P);
}

#[test]
fn settled_fields_may_read_options_only_d134() {
    let mut w = tidewell();
    w.regions[0].checks[0].when = Some(text("has(oar)", P));
    fails(&w, "`when` may read options only", P);
    let mut w = tidewell();
    w.regions[0].checks[0].no_progression = Some(text("opt.tricky_climbs and beacon_lit", P));
    fails(&w, "`no_progression` may read options only", P.right(22));
    let mut w = tidewell();
    w.start_items[0].when = Some(text("has(oar)", P));
    fails(&w, "`start_items` may read options only", P);
    let mut w = tidewell();
    w.regions[0].when = Some(text("at.wardens.tower_door == crab", P));
    // A shuffle's result is not known when the instance is built: refused, not misread.
    fails(&w, "cannot read a shuffle result yet", P);
}

#[test]
fn reach_is_for_the_goal_only_and_the_engine_limits_are_build_errors() {
    fails(&with_check_rule("bollard", "reach(dock)", P), "goal only", P);
    fails(&with_check_rule("bollard", "has(pearl, 300)", P), "0 to 255", P.right(11));
    fails(&with_check_rule("bollard", "count(pearl) >= 40000", P), "16-bit", P.right(16));
    fails(
        &with_check_rule("bollard", "opt.pearls_needed - 1 >= 0", P),
        "`-` is not supported yet",
        P,
    );
    fails(
        &with_check_rule("bollard", "opt.pearls_needed mod 2 >= 0", P),
        "`mod` is not supported yet",
        P,
    );
    fails(&with_check_rule("bollard", "during(night)", P), "time context", P);
    fails(&with_check_rule("bollard", "roll(price) >= 2", P), "`roll` is not supported", P);
}

#[test]
fn every_short_form_lowers_to_the_engine_shape() {
    let w = with_check_rule(
        "bollard",
        "has(pearl, 2) and has_all(oar, lamp) and has_any(lamp, oar) and level(oar) >= 1 and counter(beacon_lit) >= 1 and opt.start_gift in [none, lamp] and min(count(oar), 1) + max(1, 2) >= 3 and count(oar) * 2 div 2 >= 1",
        P,
    );
    let b = built(&w);
    let dock = &b.manifest["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "dock")
        .unwrap()["checks"];
    let rule = &dock.as_array().unwrap().iter().find(|c| c["loc"] == "bollard").unwrap()["rule"];
    let want = serde_json::json!({"all": [
        {"has": "pearl", "n": 2},
        {"has_all": ["lamp", "oar"]},
        {"has_any": ["lamp", "oar"]},
        {"ge": [{"count": "oar"}, 1]},
        {"ge": [{"counter": "beacon_lit"}, 1]},
        {"any": [{"eq": [{"opt": "start_gift"}, 0]}, {"eq": [{"opt": "start_gift"}, 1]}]},
        // `max(1, 2)` reads numbers only, so it folds to 2 (`>= 3`: at least 2 always holds).
        {"ge": [{"int": ["+", {"int": ["min", {"count": "oar"}, 1]}, 2]}, 3]},
        {"ge": [{"int": ["div", {"int": ["*", {"count": "oar"}, 2]}, 2]}, 1]},
    ]});
    assert_eq!(*rule, want);
    // A group count is one sum of counts, which the engine keeps as one counter.
    let grotto =
        b.manifest["regions"].as_array().unwrap().iter().find(|r| r["key"] == "grotto").unwrap();
    assert_eq!(
        grotto["checks"][0]["rule"],
        serde_json::json!({"ge": [{"int": ["+", {"count": "crowbar"}, {"count": "lamp"}, {"count": "oar"}]}, 2]})
    );
}

#[test]
fn and_inside_or_without_parentheses_is_a_warning() {
    let b = built(&with_check_rule("bollard", "has(oar) or has(lamp) and has(crowbar)", P));
    assert_eq!(b.warnings.len(), 1);
    assert_eq!(b.warnings[0].pos, Some(P.right(12)));
}

#[test]
fn defs_that_refer_to_themselves_are_an_error() {
    let mut w = tidewell();
    let f = F::default();
    w.defs.push(Def::plain(f.s("loop_a"), text("loop_b", P)));
    w.defs.push(Def::plain(f.s("loop_b"), f.r("loop_a")));
    let all = errors(&w).join("\n");
    assert!(all.contains("refers to itself"), "{all}");
}

#[test]
fn too_many_items_for_the_checks_is_a_build_error_naming_the_options() {
    let mut w = tidewell();
    w.items[3].count = Some(At::new(13, P));
    let all = errors(&w).join("\n");
    // 16 checks with the stalls on, 14 with them off: only the off setting fails.
    assert!(all.contains("market_stalls = 0") && all.contains("items to place"), "{all}");
    let mut w = tidewell();
    w.items[4].count = Some(At::new(0, P));
    fails(&w, "no copy left", w.regions[3].checks[0].holds.as_ref().unwrap().pos);
    let mut w = tidewell();
    w.start_items.push(Grant { item: At::new("ghost".into(), P), n: None, when: None });
    fails(&w, "`ghost` in `start_items` is not an item", P);
}

#[test]
fn shuffles_outside_the_engines_first_slice_are_build_errors() {
    let mut w = tidewell();
    w.shuffles[1].later.push(At::new("pin".into(), P));
    fails(&w, "shuffle field `pin` is not supported yet", P);
    let mut w = tidewell();
    w.shuffles[0].check = Some(At::new("reach".into(), P));
    fails(&w, "check `reach`", P);
    let mut w = tidewell();
    w.shuffles[1].coupled = Some(At::new(Rule::Const(true), P));
    fails(&w, "only a shuffle that moves exits", P);
    let mut w = tidewell();
    w.shuffles[0].coupled = Some(text("opt.tricky_climbs", P));
    fails(&w, "reads options is not supported", P);
    let mut w = tidewell();
    w.shuffles[0].slots = At::new(Members::Exits(vec![At::new("sideways".into(), P)]), P);
    fails(&w, "neither an exit group nor an exit", P);
    let mut w = tidewell();
    w.shuffles[0].name = At::new("layout".into(), P);
    fails(&w, "reserved for the engine", P);
    let mut w = tidewell();
    let Some(At { v: Members::Names(things), .. }) = &mut w.shuffles[1].things else { panic!() };
    things[0].later.push(At::new("count".into(), P));
    fails(&w, "thing field `count`", P);
    // A rule cannot read where an exit shuffle put things.
    fails(&with_check_rule("bollard", "at.doors.x", P), "moves exits", P);
    // A permission row names real slots and things.
    let mut w = tidewell();
    let f = F::default();
    let mut r = f.row(Verb::Only, &["tower_door"], &["shark"]);
    r.object[0].pos = P;
    w.shuffles[1].allow.push(r);
    fails(&w, "no thing or group `shark`", P);
    // A row the engine accepts builds: the tower door takes the eel only.
    let mut w = tidewell();
    w.shuffles[1].allow.push(f.row(Verb::Only, &["tower_door"], &["eel"]));
    let b = built(&w);
    assert_eq!(
        b.manifest["shuffles"]["wardens"]["allow"][0],
        serde_json::json!({"only": {"groups": [], "names": ["eel"]}, "slots": {"groups": [], "names": ["tower_door"]}})
    );
}

#[test]
fn fields_this_version_does_not_build_are_named() {
    let mut w = tidewell();
    w.later.push(At::new("rolls".into(), P));
    fails(&w, "field `rolls` is not supported yet", P);
    let mut w = tidewell();
    w.format = Some(At::new("cgm-data/2".into(), P));
    fails(&w, "not one this tool reads", P);
    let mut w = tidewell();
    w.goal = None;
    assert!(errors(&w).iter().any(|e| e == "w: the world has no `goal`"));
}

#[test]
fn long_arithmetic_and_deep_def_chains_are_errors_not_stack_overflows() {
    let long = "count(oar)".to_owned() + &" * 1".repeat(100_000) + " >= 1";
    assert!(errors(&with_check_rule("bollard", &long, P)).iter().any(|e| e.contains("deep")));
    let wide = "count(oar)".to_owned() + &" + count(oar)".repeat(10_000) + " >= 1";
    fails(&with_check_rule("bollard", &wide, P), "terms in one sum", P);
    // 64 defs, each 60 `not`s over the one before: every rule within its own cap, the chain not.
    let mut w = tidewell();
    let f = F::default();
    w.defs.push(Def::plain(f.s("d0"), f.r("opt.tricky_climbs")));
    for k in 1..64 {
        w.defs.push(Def::plain(
            f.s(&format!("d{k}")),
            f.r(&("not ".repeat(60) + &format!("d{}", k - 1))),
        ));
    }
    let all = errors(&w).join("\n");
    assert!(all.contains("nest more than 256 deep, counting the defs"), "{all}");
}

#[test]
fn a_def_in_a_settled_field_is_evaluated_and_checked_as_that_field() {
    let mut w = tidewell();
    let f = F::default();
    w.defs.push(Def::plain(f.s("stalls_on"), f.r("opt.market_stalls")));
    for c in w.regions[1].checks.iter_mut().filter(|c| c.when.is_some()) {
        c.when = Some(f.r("stalls_on"));
    }
    let b = built(&w);
    assert_eq!(b.default_instance().unwrap().locations.len(), 17);
    assert_eq!(b.instance(&[("market_stalls".to_owned(), 0)].into()).unwrap().locations.len(), 15);
    // A def cannot carry a shuffle read, or a state read, into a settled field.
    let mut w = tidewell();
    w.defs.push(Def::plain(f.s("crab_up"), f.r("at.wardens.tower_door == crab")));
    w.regions[0].checks[0].when = Some(text("crab_up", P));
    fails(&w, "cannot read a shuffle result yet, and def `crab_up` reads one", P);
    let mut w = tidewell();
    w.regions[0].checks[0].when = Some(f.r("can_climb"));
    assert!(errors(&w).iter().any(|e| e.contains("`when` may read options only")));
}

#[test]
fn a_def_with_an_error_is_reported_once_where_it_is() {
    let mut w = with_check_rule("bollard", "bad and bad", Pos::new(901, 1));
    w.defs.push(Def::plain(At::new("bad".into(), Pos::new(899, 1)), text("has(nope)", P)));
    let all = errors(&w);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(all[0].starts_with("w:900:9: unknown item `nope`"), "{all:#?}");
}

#[test]
fn the_pool_test_tries_middle_values_of_a_range() {
    let mut w = tidewell();
    let f = F::default();
    w.items[3].count = Some(f.at(13));
    for c in w.regions[1].checks.iter_mut().filter(|c| c.when.is_some()) {
        c.when = Some(f.r("opt.pearls_needed != 2"));
    }
    let all = errors(&w).join("\n");
    assert!(all.contains("pearls_needed = 2") && all.contains("items to place"), "{all}");
}

#[test]
fn start_items_are_capped_before_any_copy_is_written() {
    let mut w = tidewell();
    w.items[5].count = Some(At::new(5000, P));
    w.start_items.push(Grant {
        item: At::new("shell".into(), P),
        n: Some(At::new(5000, P)),
        when: None,
    });
    let all = errors(&w).join("\n");
    assert!(all.contains("start item copies, at most 1024"), "{all}");
}

#[test]
fn a_shuffle_that_reads_its_own_result_is_an_error() {
    let mut w = tidewell();
    let Some(At { v: Members::Names(things), .. }) = &mut w.shuffles[1].things else { panic!() };
    things[0].rule = Some(text("at.wardens.grotto_gate", Pos::new(5, 1)));
    let pos = w.shuffles[1].name.pos;
    fails(&w, "reads its own result", pos);
}

#[test]
fn an_item_group_lists_each_item_once() {
    let mut w = tidewell();
    w.item_groups[0].1.push(At::new("oar".into(), P));
    fails(&w, "is in item group `tools` twice", P);
}

#[test]
fn past_the_error_cap_the_rest_are_counted() {
    let mut w = tidewell();
    let f = F::default();
    for c in w.regions.iter_mut().flat_map(|r| &mut r.checks) {
        c.rule = f.r("has(nope) and ".repeat(20).trim_end_matches(" and "));
    }
    let all = errors(&w);
    assert_eq!(all.len(), config::WORLD_BUILD_MAX_ERRORS + 1);
    assert!(all.last().unwrap().contains("140 more errors"), "{:?}", all.last());
}

#[test]
fn a_def_used_twice_by_each_next_def_builds_in_linear_time() {
    // `eK: e(K-1) and e(K-1)` doubles a copied tree each level; referenced, it stays small.
    let mut w = tidewell();
    let f = F::default();
    w.defs.push(Def::plain(f.s("e0"), f.r("opt.tricky_climbs")));
    for k in 1..40 {
        w.defs.push(Def::plain(f.s(&format!("e{k}")), f.r(&format!("e{0} and e{0}", k - 1))));
    }
    w.regions[0].checks[0].when = Some(f.r("e39"));
    let start = std::time::Instant::now();
    let b = built(&w);
    assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
    assert_eq!(b.default_instance().unwrap().locations.len(), 16);
    assert_eq!(b.instance(&[("tricky_climbs".to_owned(), 1)].into()).unwrap().locations.len(), 17);
}

#[test]
fn a_def_chain_the_engine_counts_deeper_gets_a_depth_error_not_a_report_request() {
    // Each def adds one `not` over the one before; an event at the bottom is one node to the
    // build and two to the engine. Somewhere past 240 deep one of the two depth checks refuses
    // it, and both say what to do.
    for n in 240..=256 {
        let mut w = tidewell();
        let f = F::default();
        w.defs.push(Def::plain(f.s("x0"), f.r("opt.tricky_climbs")));
        for k in 1..n {
            w.defs.push(Def::plain(f.s(&format!("x{k}")), f.r(&format!("not x{}", k - 1))));
        }
        w.regions[0].checks[0].rule = f.r(&format!("x{}", n - 1));
        if let Err(e) = build_model(&w, "w") {
            let all = e.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
            assert!(all.contains("deep") && !all.contains("please report"), "{n}: {all}");
        }
    }
}

#[test]
fn a_def_that_reads_a_shuffle_cannot_settle_a_field() {
    let mut w = tidewell();
    let f = F::default();
    w.defs.push(Def::plain(
        f.s("crab_deep"),
        f.r("opt.tricky_climbs or at.wardens.tower_door == crab"),
    ));
    w.defs.push(Def::plain(f.s("via"), f.r("crab_deep")));
    w.regions[0].checks[0].when = Some(text("via", P));
    fails(&w, "def `via` reads one", P);
}

#[test]
fn the_pool_test_sees_options_read_through_a_def() {
    let mut w = tidewell();
    let f = F::default();
    w.items[3].count = Some(f.at(13));
    w.defs.push(Def::plain(f.s("not_one"), f.r("opt.pearls_needed != 1")));
    for c in w.regions[1].checks.iter_mut().filter(|c| c.when.is_some()) {
        c.when = Some(f.r("not_one"));
    }
    let all = errors(&w).join("\n");
    assert!(all.contains("pearls_needed = 1") && all.contains("items to place"), "{all}");
}

#[test]
fn past_the_warning_cap_the_rest_are_counted() {
    let mut w = tidewell();
    let f = F::default();
    let mixed = "has(oar) or has(lamp) and has(oar) or ".repeat(12) + "true";
    for c in w.regions.iter_mut().flat_map(|r| &mut r.checks) {
        c.rule = f.r(&mixed);
    }
    let b = built(&w);
    assert_eq!(b.warnings.len(), config::WORLD_BUILD_MAX_ERRORS + 1);
    assert!(b.warnings.last().unwrap().msg.contains("4 more warnings"), "{:?}", b.warnings.last());
}

#[test]
fn ledger_ids_are_kept_apart_from_names_and_the_rest_are_numbered_on() {
    let mut w = tidewell();
    w.items[1].id = Some(At::new(40, P)); // lamp
    w.regions[0].checks[1].id = Some(At::new(7, P)); // net_pile
    let b = built(&w);
    let id = |table: &str, key: &str| {
        b.manifest[table].as_array().unwrap().iter().find(|r| r["key"] == key).unwrap()["id"]
            .as_i64()
    };
    assert_eq!(
        (id("items", "lamp"), id("items", "oar"), id("items", "crowbar")),
        (Some(40), Some(41), Some(42))
    );
    assert_eq!(
        (id("locations", "net_pile"), id("locations", "bollard"), id("locations", "tide_pool")),
        (Some(7), Some(8), Some(9))
    );
    // Renaming an item keeps its id.
    let mut renamed = w.clone();
    renamed.items[1].key = At::new("lantern".into(), P);
    for r in &mut renamed.regions {
        for c in &mut r.checks {
            if let Rule::Text(t) = &mut c.rule.v {
                *t = t.replace("lamp", "lantern");
            }
        }
        for (_, rule) in &mut r.events {
            if let Rule::Text(t) = &mut rule.v {
                *t = t.replace("lamp", "lantern");
            }
        }
    }
    renamed.start_items[0].item = At::new("lantern".into(), P);
    renamed.item_groups[0].1[1] = At::new("lantern".into(), P);
    let f = F::default();
    let Some(At { v: Members::Names(things), .. }) = &mut renamed.shuffles[1].things else {
        panic!()
    };
    things[1].rule = Some(f.r("has(lantern)"));
    renamed.options[3].values[1] = At::new("lantern".into(), P);
    renamed.start_items[0].when = Some(f.r("opt.start_gift == lantern"));
    let b2 = built(&renamed);
    let lantern = b2.manifest["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "lantern")
        .unwrap()["id"]
        .as_i64();
    assert_eq!(lantern, Some(40));
    let mut w = tidewell();
    w.items[0].id = Some(At::new(3, P));
    w.items[2].id = Some(At::new(3, Pos::new(901, 2)));
    fails(&w, "item id 3 is used twice", Pos::new(901, 2));
    let mut w = tidewell();
    w.regions[0].checks[0].id = Some(At::new(0, P));
    fails(&w, "check id 0 is outside", P);
    let mut w = tidewell();
    w.items[0].id = Some(At::new(1 << 53, P));
    assert!(errors(&w).iter().any(|e| e.contains("item ids would pass 2^53")));
}

#[test]
fn many_generated_checks_build_quickly_and_keep_their_source_place() {
    // What a front end makes of a family or a table: many checks, each at its declaration.
    let mut w = tidewell();
    let family = Pos::new(500, 3);
    let n = 20_000;
    w.regions[0].checks.extend((0..n).map(|k| world_build::model::Check {
        also: false,
        key: At::new(format!("shelf_{k}"), family),
        id: None,
        rule: At::new(
            Rule::Text(if k % 2 == 0 { "true".into() } else { "has(oar)".into() }),
            family,
        ),
        when: None,
        vanilla: None,
        holds: None,
        no_progression: None,
        placement: None,
        later: vec![],
    }));
    let start = std::time::Instant::now();
    let b = built(&w);
    assert!(start.elapsed().as_secs() < 20, "{:?}", start.elapsed());
    assert_eq!(b.default_instance().unwrap().locations.len(), 17 + n);
    // One bad generated check points at the family that made it.
    w.regions[0].checks[9_000].rule = At::new(Rule::Text("has(nope)".into()), family);
    fails(&w, "unknown item `nope`", family.right(4));
}

#[test]
fn match_on_an_option_covers_every_value_and_lowers_to_an_or_of_arms() {
    let b = built(&with_check_rule(
        "bollard",
        "match opt.start_gift { none => has(oar)  lamp => true }",
        P,
    ));
    let dock =
        b.manifest["regions"].as_array().unwrap().iter().find(|r| r["key"] == "dock").unwrap();
    assert_eq!(
        dock["checks"][0]["rule"],
        serde_json::json!({"any": [
            {"all": [{"any": [{"eq": [{"opt": "start_gift"}, 0]}]}, {"has": "oar"}]},
            {"all": [{"any": [{"eq": [{"opt": "start_gift"}, 1]}]}, {"true": {}}]},
        ]})
    );
    built(&with_check_rule("bollard", "match opt.tricky_climbs { on => true  _ => has(oar) }", P));
    fails(
        &with_check_rule("bollard", "match opt.start_gift { none => true }", P),
        "does not cover lamp",
        P,
    );
    fails(
        &with_check_rule("bollard", "match opt.start_gift { none, sword => true }", P),
        "`sword` is not a value",
        P.right(29),
    );
    fails(
        &with_check_rule("bollard", "match opt.start_gift { _ => true  none => true }", P),
        "`_` is the last arm",
        P.right(28),
    );
    fails(&with_check_rule("bollard", "match opt.pearls_needed { _ => true }", P), "is a range", P);
}

#[test]
fn a_def_with_parameters_is_written_once_per_argument_list() {
    let mut w = tidewell();
    let f = F::default();
    w.defs.push(Def {
        name: f.s("enough"),
        params: vec![f.s("n")],
        rule: f.r("count(pearl) >= n"),
    });
    w.regions[0].checks[0].rule = f.r("enough(2) or enough(opt.pearls_needed)");
    w.regions[0].checks[1].rule = f.r("enough(2)");
    let b = built(&w);
    assert_eq!(
        b.manifest["defs"]["enough__1_2"],
        serde_json::json!({"ge": [{"count": "pearl"}, 2]})
    );
    assert_eq!(
        b.manifest["defs"]["enough__18_0opt_pearls_needed"],
        serde_json::json!({"ge": [{"count": "pearl"}, {"opt": "pearls_needed"}]})
    );
    assert!(b.manifest["defs"].get("enough").is_none());
    fails(&with_check_rule("bollard", "can_climb(1)", P), "unknown function", P);
    let mut w2 = w.clone();
    w2.regions[0].checks[2].rule = text("enough(1, 2)", P);
    fails(&w2, "takes 1 arguments, given 2", P);
    let mut w3 = w.clone();
    w3.regions[0].checks[2].rule = text("enough", P);
    fails(&w3, "takes 1 arguments", P);
    let mut w4 = w;
    w4.regions[0].checks[2].rule = text("enough(count(oar))", P);
    fails(&w4, "an argument is a number", P.right(7));
    // A def that calls itself with a changed argument runs into the depth cap, not forever.
    let mut w5 = tidewell();
    w5.defs.push(Def { name: f.s("down"), params: vec![f.s("n")], rule: f.r("down(n + 1)") });
    w5.regions[0].checks[0].rule = f.r("down(0)");
    assert!(errors(&w5).iter().any(|e| e.contains("deep")));
}

#[test]
fn count_of_several_items_is_one_sum_and_number_only_arithmetic_folds() {
    let b = built(&with_check_rule(
        "bollard",
        "count(oar, lamp) >= 10 - 9 and count(pearl) >= 7 mod 4",
        P,
    ));
    let dock =
        b.manifest["regions"].as_array().unwrap().iter().find(|r| r["key"] == "dock").unwrap();
    assert_eq!(
        dock["checks"][0]["rule"],
        serde_json::json!({"all": [
            {"ge": [{"int": ["+", {"count": "lamp"}, {"count": "oar"}]}, 1]},
            {"ge": [{"count": "pearl"}, 3]},
        ]})
    );
}
