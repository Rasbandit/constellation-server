//! The fill's output as a seed: `data_fill::seed::generate` assembles a bundle that the v2
//! format carries back byte for byte (canonical JSON), with spheres and every item class.

use data_fill::Instance;
use data_fill::seed::{Player, generate};
use seed_bundle::{SeedBundle, bundle_hash, package, v2};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("fixtures/room_world.json");

fn fixture() -> (Value, Vec<Instance>) {
    let doc: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut manifest = doc["manifest"].clone();
    manifest["game"] = json!("Fixture");
    manifest["version"] = json!("1");
    let insts = doc["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| Instance::deserialize(v).unwrap())
        .collect();
    (manifest, insts)
}

/// Two players: the first's coins are traps, the second's are useful; the first starts with a lamp.
fn players() -> (Value, Vec<Player>) {
    let (m, insts) = fixture();
    let mut a = insts[0].clone();
    a.pool_class.insert("coin".into(), "trap".into());
    a.start_inventory = vec!["lamp".into()];
    a.pool = ["key_a", "key_b", "coin", "coin", "coin", "coin"].map(String::from).into();
    let mut b = insts[1].clone();
    b.pool_class.insert("coin".into(), "useful".into());
    (m, vec![Player { name: "Ann".into(), instance: a }, Player { name: "Bo".into(), instance: b }])
}

fn make(rng: u64, workers: usize) -> SeedBundle {
    let (m, p) = players();
    generate(&m, "seed-one", &p, rng, workers).unwrap()
}

#[test]
fn the_bundle_names_slots_games_spheres_and_every_item_class() {
    let b = make(7, 1);
    assert_eq!((b.seed_name.as_str(), b.slots.len()), ("seed-one", 2));
    assert_eq!(b.slots[1].name, "Bo");
    assert_eq!(b.connect_names["Ann"], (0, 1));
    assert_eq!(b.slots[0].start_inventory, [3], "the lamp's id");
    let g = &b.games["Fixture"];
    assert_eq!((g.item_name_to_id["lamp"], g.location_name_to_id["l6"]), (3, 6));
    let all: Vec<_> = b.slots.iter().flat_map(|s| &s.locations).collect();
    assert_eq!(all.len(), 12);
    assert!(all.iter().all(|p| p.sphere.is_some()), "every placed location is reachable");
    assert!(all.iter().any(|p| p.sphere == Some(0)));
    let flags: Vec<u8> = all.iter().map(|p| p.flags).collect();
    for want in [1_u8, 2, 4] {
        assert!(flags.contains(&want), "flag {want} missing in {flags:?}");
    }
    assert!(flags.iter().all(|f| *f <= 0b111));
}

#[test]
fn the_v2_round_trip_is_byte_equal_on_canonical_json() {
    let b = make(7, 1);
    let text = serde_json::to_string(&b).unwrap();
    let hash: [u8; 32] = bundle_hash(&text).unwrap().try_into().unwrap();
    let enc = v2::encode(&b, &hash, 1 << 24).unwrap();
    assert!(v2::is_v2(&enc.seed));
    let files: BTreeMap<_, _> =
        enc.packages.iter().map(|f| (package::key_of(f), f.clone())).collect();
    let back = v2::decode(enc.seed.clone(), |k| Ok(files[k].clone()), 1 << 24).unwrap();
    let canon = |b: &SeedBundle| package::canonical(serde_json::to_value(b).unwrap());
    assert_eq!(canon(&back), canon(&b));
    assert_eq!(bundle_hash(&serde_json::to_string(&back).unwrap()).unwrap(), hash);
}

#[test]
fn the_same_seed_gives_the_same_bundle_with_any_worker_count_and_others_differ() {
    let canon = |b: &SeedBundle| package::canonical(serde_json::to_value(b).unwrap());
    assert_eq!(canon(&make(7, 1)), canon(&make(7, 8)));
    assert!((0..8).any(|r| canon(&make(r, 1)) != canon(&make(7, 1))));
}

#[test]
fn a_manifest_without_a_game_or_players_without_names_are_refused() {
    let (mut m, p) = players();
    let e = generate(&m, "s", &p, 1, 1).map(|_| ());
    assert!(e.is_ok());
    m.as_object_mut().unwrap().remove("game");
    assert!(generate(&m, "s", &p, 1, 1).unwrap_err().contains("game"));
    let (m, mut p) = players();
    p[1].name = "Ann".into();
    assert!(generate(&m, "s", &p, 1, 1).unwrap_err().contains("repeats"));
    p[1].name = String::new();
    assert!(generate(&m, "s", &p, 1, 1).unwrap_err().contains("name"));
    assert!(generate(&m, "s", &[], 1, 1).is_err());
}
