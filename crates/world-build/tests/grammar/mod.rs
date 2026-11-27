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
