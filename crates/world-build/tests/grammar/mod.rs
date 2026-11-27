//! The second round of grammar changes, from the trial worlds of doc 18 (a module of
//! `build.rs`): optional regions, error recovery, checks in several regions, parallel exits,
//! the check namespace, computed numbers, placement blocks, family errors and the tool's
//! polish. Each test failed before its change.

use std::collections::HashMap;

use super::lang::{errors, fails, line, ok, region, world};
use super::trials::fills;

/// An optional area in a world with a shuffle: two rooms behind a toggle.
const OPTIONAL: &str = "option vault: toggle = off
shuffle s {
  slots p, q
  thing x
  thing y
}
region a {
  check a1
  exit b
}
region b when opt.vault {
  check b1: has(k)
  exit a
  exit c
}
region c when opt.vault {
  check c1
  exit b
}";

/// A region `when` removes the region for the seeds where it is false: its checks, events and
/// exits are gone, and the engine's whole-world check (every region reached, with a shuffle in
/// the world) still passes. Before, every seed at `off` failed in the engine.
#[test]
fn a_region_when_removes_the_region_and_both_values_fill() {
    let b = ok(&world(OPTIONAL));
    for vault in [0, 1] {
        fills(&b, &HashMap::from([("vault".to_owned(), vault)]), 10);
    }
    // The exits into and out of an optional room carry its condition.
    let into = &region(&b, "a")["exits"];
    assert!(into.to_string().contains(r#"{"ne":[{"opt":"vault"},0]}"#), "{into}");
}

/// A rule that reads an event only an optional region fires would be false in every seed
/// where that region is gone: `check` names it instead of passing the world.
#[test]
fn a_read_of_an_event_only_an_optional_region_fires_is_an_error() {
    let files = world(
        &format!("{OPTIONAL}\nregion d {{\n  check d1: lever\n  exit a\n}}")
            .replace("  exit b\n}\nregion b", "  exit b\n  exit d\n}\nregion b")
            .replace("  check c1\n", "  check c1\n  event lever: true\n"),
    );
    let all = errors(&files);
    assert!(
        all.iter().any(|e| e.contains("`lever` is fired only in regions that do not exist")
            && e.contains("vault = off")),
        "{all:#?}"
    );
}

/// A region with no `when` that is reached only through optional ones would be cut off: an
/// error naming the setting, not a seed failure.
#[test]
fn a_region_reached_only_through_an_optional_one_is_an_error() {
    let files = world(&format!("{OPTIONAL}\nregion e {{\n  check e1\n}}").replace(
        "region c when opt.vault {\n  check c1\n",
        "region c when opt.vault {\n  check c1\n  exit e\n",
    ));
    fails(
        &files,
        &format!("world.rando:{}:8", line(21)),
        "region `e` cannot be reached with vault = off",
    );
}

/// `reach` of an optional region in the goal dangles at the value that removes it, and the
/// start region always exists.
#[test]
fn the_goal_and_the_start_cannot_depend_on_an_optional_region() {
    let goal = world(OPTIONAL)
        .0
        .into_iter()
        .map(|(k, v)| (k, v.replace("goal reach(a)", "goal reach(c)")))
        .collect();
    let all = errors(&world_build::front::Mem(goal));
    assert!(
        all.iter().any(|e| e.contains("region `c`, which does not exist with vault = off")),
        "{all:#?}"
    );
    let start = world(&OPTIONAL.replace("region a {", "region a when opt.vault {"));
    let all = errors(&start);
    assert!(all.iter().any(|e| e.contains("the start region always exists")), "{all:#?}");
}

/// One missing `}` hides nothing: the region left open is named with the line it opened on,
/// a misspelt name in another file is still reported, and names the broken file may declare
/// in the part that did not parse are not reported as unknown (no cascade).
#[test]
fn a_syntax_error_does_not_hide_name_errors_in_other_files() {
    let files = super::lang::mem(&[
        (
            "world.rando",
            "world t\nuse rooms/a\nuse rooms/b\nstart a\nitem k progression\nitem junk filler\npool fill { junk 1 }\ngoal reach(a)\n",
        ),
        (
            "rooms/a.rando",
            "region a {\n  check a1\n  exit b\nregion lost {\n  check l1\n}\ndef opened = has(\n",
        ),
        ("rooms/b.rando", "region b {\n  check b1: has(kk)\n  check b2: opened\n  exit a\n}\n"),
    ]);
    let all = errors(&files);
    assert!(
        all.iter().any(|e| e.starts_with("rooms/a.rando:4:1: ")
            && e.contains("region `a`, opened at line 1, is not closed")),
        "{all:#?}"
    );
    assert!(
        all.iter()
            .any(|e| e.starts_with("rooms/b.rando:2:17: ") && e.contains("unknown item `kk`")),
        "{all:#?}"
    );
    assert!(
        !all.iter().any(|e| e.contains("`opened`") || e.contains("cannot be reached")),
        "{all:#?}"
    );
}

/// An overlay whose target is in a file that did not parse is not reported as missing.
#[test]
fn an_overlay_on_a_base_that_did_not_parse_is_not_a_missing_target() {
    let files = super::lang::mem(&[
        (
            "world.rando",
            "world t\nuse base\nuse fixes\nstart a\nitem k progression\nitem junk filler\npool fill { junk 1 }\ngoal reach(a)\n",
        ),
        ("base.rando", "region a {\n  check a1\n  check gone\nregion z {\n  exit a\n}\n"),
        ("fixes.rando", "use base\nreplace check a.gone: has(k)\n"),
    ]);
    let all = errors(&files);
    assert!(all.iter().any(|e| e.starts_with("base.rando:4:1: ")), "{all:#?}");
    assert!(!all.iter().any(|e| e.contains("target_missing")), "{all:#?}");
}

/// A check declared in several regions is one location reachable from each, with each
/// region's own rule: one id, one place in the pool, a site per region in the engine's form,
/// and no event per region.
#[test]
fn a_check_declared_in_several_regions_is_one_location() {
    let b = ok(&world(
        "item lamp progression\nregion a {\n  check drop: has(lamp)\n  check torch\n  exit b\n}\nregion b {\n  check drop vanilla k\n  exit a\n}",
    ));
    let locs: Vec<&str> = b.manifest["locations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["key"].as_str().unwrap())
        .collect();
    assert_eq!(locs, ["drop", "torch"]);
    assert_eq!(
        region(&b, "a")["checks"][0],
        serde_json::json!({"loc": "drop", "rule": {"has": "lamp"}})
    );
    assert_eq!(
        region(&b, "b")["checks"][0],
        serde_json::json!({"loc": "drop", "rule": {"true": {}}})
    );
    assert_eq!(b.manifest["locations"][0]["vanilla"], "k");
    let inst = b.instance_json(&HashMap::new()).unwrap();
    assert_eq!(inst["locations"], serde_json::json!(["drop", "torch"]));
    let ledger: serde_json::Value = serde_json::from_str(b.ledger.as_ref().unwrap()).unwrap();
    assert_eq!(ledger["checks"].as_object().unwrap().len(), 2);
    fills(&b, &HashMap::new(), 10);
}

/// The declarations of one check may each give a rule, and every other modifier once; the
/// same region may not declare it twice.
#[test]
fn a_check_in_several_regions_gives_each_modifier_once() {
    fails(
        &world(
            "region a {\n  check drop vanilla k\n  exit b\n}\nregion b {\n  check drop vanilla junk\n  exit a\n}",
        ),
        &format!("world.rando:{}:22", line(6)),
        "`vanilla` of check `drop` is also given in region `a`",
    );
    fails(
        &world("region a {\n  check drop\n  check drop: has(k)\n}"),
        &format!("world.rando:{}:9", line(3)),
        "`drop` is already a check in region `a`",
    );
}

/// Two exits between the same two rooms are two ways in, each with its rule; in a world with
/// a shuffle each has its own door, and its key names the door (`region:to:door`).
#[test]
fn several_exits_to_one_region_each_keep_their_rule_and_door() {
    let b = ok(&world(
        "region a {\n  check c\n  exit b: has(k)\n  exit b: has(junk)\n}\nregion b {\n  exit a\n}",
    ));
    assert_eq!(region(&b, "a")["exits"].as_array().unwrap().len(), 2);
    let doors = "shuffle s {\n  slots exits(g)\n  coupled\n}\nregion a {\n  check c\n  exit b door front group g\n  exit b: has(k) door back group g\n}\nregion b {\n  exit a door front\n  exit a door back\n}";
    let b = ok(&world(doors));
    let a = region(&b, "a");
    let keys: Vec<&str> =
        a["exits"].as_array().unwrap().iter().map(|e| e["key"].as_str().unwrap()).collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"a:b:front") && keys.contains(&"a:b:back"), "{keys:?}");
    fails(
        &world(&doors.replace("  exit b: has(k) door back group g", "  exit b: has(k) group g")),
        &format!("world.rando:{}:8", line(8)),
        "each needs its own `door`",
    );
}
