//! Fixtures the integration tests share. Each test file is a crate of its own and takes them
//! with `mod support;`.

use serde_json::Value;

/// A fixture world of `fixtures/`, parsed: `room_world` (one manifest, three players'
/// instances) or `fairness_world` (one player, on a world where uniform placements are often
/// beatable).
pub fn fixture(name: &str) -> Value {
    let text = match name {
        "room_world" => include_str!("../fixtures/room_world.json"),
        "fairness_world" => include_str!("../fixtures/fairness_world.json"),
        other => panic!("no fixture {other:?}"),
    };
    serde_json::from_str(text).expect("fixture parses")
}
