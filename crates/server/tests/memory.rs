//! Memory per loaded bundle, per room and per connection, on synthetic worlds sized like real
//! ones (`e2e::synth`). Run with `--nocapture` to see the numbers:
//! `cargo test -p server --features memory-harness --test memory -- --nocapture`.
//!
//! The budget: 10,000 active rooms in 4 GB, so about 400 KB per one-slot room with its
//! connection (issue 68).
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the server and the e2e helpers"
)]

#[path = "support/memory.rs"]
mod harness;

use e2e::synth::{Shape, bundle};
use seed_bundle::SeedBundle;

/// 4 GB over 10,000 rooms.
const ROOM_BUDGET: i64 = 400 * 1024;

fn run(name: &str, shape: &Shape) -> harness::Numbers {
    let text = bundle(shape);
    let b = SeedBundle::from_json(&text).unwrap();
    let path = temp_file(&format!("{name}.json"), &text);
    let n = harness::measure_bundle(&path, &b);
    println!("{} seed={}", n.row(name), shape.seed);
    println!("{name}: per active room = {} bytes", n.per_room(i64::from(shape.slots)));
    n
}

#[test]
fn a_one_slot_room_with_its_connection_fits_the_budget() {
    let n = run("synthetic-1-slot", &Shape::ONE_SLOT);
    assert!(n.per_room(1) <= ROOM_BUDGET, "{}", n.row("synthetic-1-slot"));
}

#[test]
fn an_eight_slot_room_with_its_connections_fits_eight_budgets() {
    let n = run("synthetic-8-slot", &Shape::EIGHT_SLOT);
    assert!(n.per_room(8) <= 8 * ROOM_BUDGET, "{}", n.row("synthetic-8-slot"));
}

/// Write `text` to a fresh temporary file named `name` and return its path.
#[must_use]
pub fn temp_file(name: &str, text: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("memory-harness-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}
