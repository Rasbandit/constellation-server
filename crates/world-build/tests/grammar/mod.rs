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

/// Two exits between the same two rooms are two ways in, each with its rule; with a door each,
/// the key names the door (`region:to:door`), and two may not share one.
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
        &world(&doors.replace("  exit b: has(k) door back group g", "  exit b: has(k) door front")),
        &format!("world.rando:{}:23", line(8)),
        "two exits to `b` through door `front`",
    );
}

/// Checks have their own namespace: rules never read them, so a check may share a name with
/// an item, a region or an event, and keeps its own ledger id.
#[test]
fn a_check_may_share_a_name_with_an_item_a_region_or_an_event() {
    let b = ok(&world(
        "region a {\n  check k\n  check a\n  check opened: opened\n  event opened: true\n}",
    ));
    let ledger: serde_json::Value = serde_json::from_str(b.ledger.as_ref().unwrap()).unwrap();
    assert!(ledger["checks"]["k"]["id"].is_i64() && ledger["items"]["k"]["id"].is_i64());
    // A rule still cannot read a check, and says so.
    fails(
        &world("region a {\n  check c1\n  check c2: c1\n}"),
        &format!("world.rando:{}:13", line(3)),
        "`c1` is a check; a rule reads items, defs and events",
    );
}

/// Events and regions are visible world-wide: a rule reads an event another region file
/// fires without a `use` of that file. Defs, items, groups and options still need one.
#[test]
fn events_and_regions_need_no_use() {
    let files = super::lang::mem(&[
        (
            "world.rando",
            "world t\nuse rooms/a\nuse rooms/b\nstart a\nitem k progression\nitem junk filler\npool fill { junk 1 }\ngoal opened\n",
        ),
        ("rooms/a.rando", "region a {\n  check a1: opened\n  exit b\n}\n"),
        ("rooms/b.rando", "region b {\n  check b1\n  event opened: true\n  exit a\n}\n"),
    ]);
    ok(&files);
}

/// A number may be computed wherever one goes: copies from a table field, a family range's
/// ends, a `has` count from a def parameter or an option, item copies read from an option
/// (settled per seed), with `+`, `-`, `*`, `div` and `mod` folded at build time.
#[test]
fn numbers_are_computed_wherever_a_number_goes() {
    let b = ok(&world(
        "option extra: range 1..3 = 2
table dungeon(key: name, keys: int) {
  mire 2
  ember 3
}
for d in dungeon {
  item {d.key}_key progression x d.keys
  item {d.key}_map useful x d.keys - 1
}
item gem progression x opt.extra + 1
def need(n) = has(k, n - 1)
region a {
  for i in 1..2 * 6 {
    check c{i}
  }
  check x: need(3)
  check y: has(k, opt.extra + 1)
}",
    ));
    let pool = |extra: i64| {
        let inst = b.instance_json(&HashMap::from([("extra".to_owned(), extra)])).unwrap();
        let pool: Vec<String> = inst["pool"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        move |k: &str| pool.iter().filter(|p| *p == k).count()
    };
    let at2 = pool(2);
    assert_eq!(
        (at2("mire_key"), at2("ember_key"), at2("mire_map"), at2("ember_map")),
        (2, 3, 1, 2)
    );
    assert_eq!((at2("gem"), pool(3)("gem")), (3, 4));
    let checks = &region(&b, "a")["checks"];
    let rule = |loc: &str| {
        checks.as_array().unwrap().iter().find(|c| c["loc"] == loc).unwrap()["rule"].clone()
    };
    assert_eq!(rule("x"), serde_json::json!({"ref": "need__1_3"}));
    assert_eq!(b.manifest["defs"]["need__1_3"], serde_json::json!({"has": "k", "n": 2}));
    assert_eq!(
        rule("y"),
        serde_json::json!({"ge": [{"count": "k"}, {"int": ["+", {"opt": "extra"}, 1]}]})
    );
    assert_eq!(checks.as_array().unwrap().len(), 14);
    // What reads the player's state is still not a number of copies.
    fails(
        &world("item gem progression x count(k)\nregion a {\n  check c\n}"),
        &format!("world.rando:{}:24", line(1)),
        "a number of copies reads options and numbers only",
    );
}

/// A placement set may be written inside a family, and as a block around the regions it
/// covers, so a dungeon's keys and rooms are declared together once per dungeon.
#[test]
fn placement_sets_in_families_and_as_blocks() {
    let b = ok(&world(
        "table dungeon(key: name, keys: int) {
  mire 2
  ember 1
}
for d in dungeon {
  item {d.key}_key progression x d.keys
  group {d.key}_all = { {d.key}_key }
  placement {d.key}_keys { {d.key}_key } {
    region {d.key}_hall {
      check {d.key}_c1
      check {d.key}_c2: group_count({d.key}_all) >= 1
      exit a
    }
  }
}
placement vault { junk } {
  region vault_room {
    check v1
    exit a
  }
}
region a {
  check a1
  check a2
  exit mire_hall
  exit ember_hall
  exit vault_room
}",
    ));
    let inst = b.instance_json(&HashMap::new()).unwrap();
    assert_eq!(inst["item_groups"]["mire_key"], "mire_keys");
    assert_eq!(inst["item_groups"]["ember_key"], "ember_keys");
    assert_eq!(inst["location_groups"]["mire_c2"], "mire_keys");
    assert_eq!(inst["location_groups"]["v1"], "vault");
    assert!(inst["location_groups"].get("a1").is_none());
}

/// A mistake in a family's template is one error, written with the template and the first
/// expansion's binder values, and the count of the others.
#[test]
fn a_misspelling_in_a_template_is_one_error_with_its_count() {
    let all = errors(&world(
        "item dawn_pack progression\nitem dusk_pack progression\nitem noon_pack progression
table song(key: name, pack: name) {
  amber dawn
  basalt dusk
  cobalt noon
}
region a {
  for s in song {
    check {s.key}_clear: has({s.pack}_pak)
  }
}",
    ));
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(
        all[0].starts_with(&format!("world.rando:{}:30: ", line(11)))
            && all[0].contains(
                "unknown item `{s.pack}_pak` (as `dawn_pak` for `s = amber`, and 2 more)"
            ),
        "{all:#?}"
    );
}

/// An unknown name close to a declared one suggests it.
#[test]
fn unknown_names_suggest_the_nearest_declared_one() {
    let files = world(
        "option tricky: toggle = off\ndef can_be_goron = has(k)\nregion a {\n  check c: can_be_gorn\n  check d: has(kk) or opt.tricy\n  exit hal\n}\nregion hall {\n  exit a\n}",
    );
    let all = errors(&files);
    for want in [
        "did you mean `can_be_goron`?",
        "did you mean `k`?",
        "did you mean `opt.tricky`?",
        "did you mean `hall`?",
    ] {
        assert!(all.iter().any(|e| e.contains(want)), "{want} in {all:#?}");
    }
}

/// `fmt` breaks a rule too long for one line before each top-level `or` (or each `and` when it
/// has no `or`), indented under its line; the result is stable, and a generated file's hash
/// does not depend on the line breaks.
#[test]
fn fmt_breaks_long_rules_at_or_and_and() {
    let parts: Vec<String> = (0..8).map(|i| format!("has(item_number_{i})")).collect();
    let src = format!(
        "world t\nregion a {{\n  check c: {}\n  check d: {}\n  check e: has(x)\n}}\n",
        parts.join(" or "),
        parts.join(" and ")
    );
    let once = world_build::front::format_text(&src, "w.rando").unwrap();
    assert!(once.lines().all(|l| l.chars().count() <= 100), "{once}");
    assert!(once.contains("\n    or has(item_number_1)\n"), "{once}");
    assert!(once.contains("\n    and has(item_number_1)\n"), "{once}");
    assert!(once.contains("\n  check e: has(x)\n"), "{once}");
    // A part still too long breaks inside, one level deeper.
    let nested =
        format!("world t\nregion a {{\n  check f: has(x) and ({})\n}}\n", parts.join(" or "));
    let f = world_build::front::format_text(&nested, "w.rando").unwrap();
    assert!(
        f.contains("  check f: has(x)\n    and (has(item_number_0)\n      or has(item_number_1)\n"),
        "{f}"
    );
    assert_eq!(world_build::front::format_text(&f, "w.rando").unwrap(), f);
    assert_eq!(world_build::front::format_text(&once, "w.rando").unwrap(), once);
    let hash = |t: &str| {
        world_build::front::content_hash(
            &world_build::front::parse::parse(t, 0),
            &super::lang::mem(&[]),
        )
    };
    assert_eq!(hash(&src), hash(&once));
}

/// `stamp` writes a generated file's header for a body any tool wrote, and refreshes an old
/// header: a generator need not link the library.
#[test]
fn stamp_writes_and_refreshes_a_generated_header() {
    let body = "table song(key: name) {\n  a\n  b\n}\nregion a {\n  for s in song {\n    check {s.key}_c\n  }\n}\n";
    let entry =
        "world t\nuse songs\nstart a\nitem junk filler\npool fill { junk 1 }\ngoal reach(a)\n";
    let folder = super::lang::mem(&[("world.rando", entry), ("data/in.csv", "x")]);
    let one = world_build::front::stamp(
        body,
        "export 1",
        &[("data/in.csv", "x")],
        &[("song", 2)],
        &folder,
    )
    .unwrap();
    assert!(one.starts_with("generated {\n  tool \"export 1\"\n"), "{one}");
    let b = ok(&super::lang::mem(&[
        ("world.rando", entry),
        ("songs.rando", &one),
        ("data/in.csv", "x"),
    ]));
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
    let again = world_build::front::stamp(
        &one,
        "export 1",
        &[("data/in.csv", "x")],
        &[("song", 2)],
        &folder,
    )
    .unwrap();
    assert_eq!(again, one);
    let newer = world_build::front::stamp(&one, "export 2", &[], &[], &folder).unwrap();
    assert!(
        newer.contains("tool \"export 2\"") && !newer.contains("export 1") && newer.ends_with(body),
        "{newer}"
    );
}

/// Review findings on PR 228, each failing before its fix.
mod review {
    use super::*;

    /// A check in an always-present region and in an optional one: at the value that removes
    /// the optional region, its site there gives nothing (it carries the region's condition).
    #[test]
    fn a_check_site_in_a_removed_region_gives_nothing() {
        let b = ok(&world(
            "option vault: toggle = off\nregion a {\n  check drop: has(k)\n  check a1\n  exit b\n}\nregion b when opt.vault {\n  check drop\n  exit a\n}",
        ));
        let site = &region(&b, "b")["checks"][0];
        assert_eq!(site["rule"], serde_json::json!({"ne": [{"opt": "vault"}, 0]}), "{site}");
        // Before the fix this site read `true`, and the start's exit into the removed room
        // let the fill place `k` at `drop` behind its own lock.
        fills(&b, &HashMap::new(), 10);
    }

    /// `key` and `was` of a check in several regions are given once and keep one id.
    #[test]
    fn key_and_was_of_a_check_in_several_regions() {
        let two = "region a {\n  check drop KEY_A\n  exit b\n}\nregion b {\n  check drop KEY_B\n  exit a\n}";
        let ledger = |b: &world_build::Built| -> serde_json::Value {
            serde_json::from_str(b.ledger.as_ref().unwrap()).unwrap()
        };
        let b = ok(&world(&two.replace("KEY_A", "key \"dk\"").replace(" KEY_B", "")));
        let l = ledger(&b);
        assert_eq!(l["checks"].as_object().unwrap().keys().collect::<Vec<_>>(), ["dk"], "{l}");
        assert_eq!(b.manifest["locations"][0]["id"], l["checks"]["dk"]["id"]);
        fails(
            &world(&two.replace("KEY_A", "key \"dk\"").replace("KEY_B", "key \"dk\"")),
            &format!("world.rando:{}:9", line(6)),
            "`key` of check `drop` is also given in another declaration",
        );
        let old = super::super::lang::mem(&[(
            "world.rando",
            &world(&two.replace(" KEY_A", "").replace(" KEY_B", "")).0["world.rando"]
                .replace("drop", "old"),
        )]);
        let first = ok(&old);
        let mut files = world(&two.replace(" KEY_A", "").replace("KEY_B", "was old"));
        files.0.insert("ledger.json".into(), first.ledger.clone().unwrap());
        let l = ledger(&ok(&files));
        assert_eq!(l["checks"]["drop"]["id"], ledger(&first)["checks"]["old"]["id"], "{l}");
    }

    /// A read of a removed region's event in one alternative of an `or` is fine.
    #[test]
    fn an_optional_event_in_one_alternative_is_not_an_error() {
        ok(&world(
            &format!("{OPTIONAL}\nregion d {{\n  check d1: has(k) or lever\n  exit a\n}}")
                .replace("  exit b\n}\nregion b", "  exit b\n  exit d\n}\nregion b")
                .replace("  check c1\n", "  check c1\n  event lever: true\n"),
        ));
    }

    /// A shuffle may not move an exit into a removed region, whatever names it (a group here),
    /// though the exit itself may have a group.
    #[test]
    fn a_shuffle_cannot_move_an_exit_of_a_removed_region() {
        let files = world(
            "option vault: toggle = off\nshuffle s {\n  slots exits(g)\n}\nregion a {\n  check a1\n  exit b group g\n}\nregion b when opt.vault {\n  exit a\n}",
        );
        let all = errors(&files);
        assert!(
            all.iter().any(|e| e
                .contains("moves exit `a:b`, which leads into or out of a region with a `when`")),
            "{all:#?}"
        );
    }

    /// Parallel exits in a world with a shuffle need no door: unmoved, each is keyed by its
    /// place among them.
    #[test]
    fn parallel_exits_need_no_door_when_no_shuffle_moves_them() {
        let b = ok(&world(
            "shuffle s {\n  slots p, q\n  thing x\n  thing y\n}\nregion a {\n  check c\n  exit b: has(k)\n  exit b\n}\nregion b {\n  exit a\n}",
        ));
        let a = region(&b, "a");
        let keys: Vec<&str> =
            a["exits"].as_array().unwrap().iter().map(|e| e["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["a:b:1", "a:b:2"]);
    }

    /// `stamp` reads a comment inside an old header as part of the header.
    #[test]
    fn stamp_keeps_a_header_with_a_comment_inside() {
        let folder = super::super::lang::mem(&[("world.rando", "world t\n")]);
        let body = "region a {\n  check c\n}\n";
        let one = world_build::front::stamp(body, "x 1", &[], &[], &folder).unwrap();
        let noted = one.replacen("  tool", "  # a note\n  tool", 1);
        assert_eq!(world_build::front::stamp(&noted, "x 1", &[], &[], &folder).unwrap(), one);
    }
}
