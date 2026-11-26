//! The seed a generation stores, byte for byte: seeds 1 to 20 of one world at 1 and at 10
//! players, through `seed::generate` on the manifest and the v1 import, pinned by one digest.
//! Every faster way to the same seed must give these bytes.

use data_fill::seed::{Player, generate};
use data_fill::synth::{Shape, world};
use seed_bundle::package::key_of;
use serde_json::{Value, json};

/// The seed cap of the imports here.
const MAX: u64 = 1 << 24;
/// SHA-256 of every seed file and package file of [`rooms`], in order.
const PINNED: &str = "d004200e1b3c59171fe9ccf5f2cd797aa1e780ba4fe741ab8ea143c126a204e7";

/// One small synthetic world as an uploaded manifest, and `n` players on it.
fn room(n: usize) -> (Value, Vec<Player>) {
    let (mut manifest, instance) = world(&Shape { checks: 200, ..Shape::default() }, 1, 0);
    manifest["game"] = json!("Synth");
    manifest["version"] = json!("1");
    let players =
        (0..n).map(|p| Player { name: format!("P{p}"), instance: instance.clone() }).collect();
    (manifest, players)
}

/// The seed file and package files the manifest path stores: `generate`, then the v1 import.
fn by_manifest(manifest: &Value, players: &[Player], rng: u64) -> (Vec<u8>, Vec<Vec<u8>>) {
    let bundle = generate(manifest, &format!("s{rng}"), players, rng, 1).unwrap();
    let text = serde_json::to_vec(&bundle).unwrap();
    let (seed, packages) = seed_bundle::import_v1(&text, MAX).unwrap();
    (seed.bytes().to_vec(), packages)
}

/// Seeds 1 to 20 at 1 and at 10 players.
fn rooms() -> impl Iterator<Item = (Value, Vec<Player>, u64)> {
    [1, 10].into_iter().flat_map(|n| {
        let (m, p) = room(n);
        (1..=20).map(move |rng| (m.clone(), p.clone(), rng))
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_stored_seeds_of_seeds_1_to_20_at_1_and_10_players_are_pinned() {
    let mut all = Vec::new();
    for (m, p, rng) in rooms() {
        let (seed, packages) = by_manifest(&m, &p, rng);
        all.extend(seed);
        all.extend(packages.concat());
    }
    assert_eq!(hex(&key_of(&all)), PINNED);
}
