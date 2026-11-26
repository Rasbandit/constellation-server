//! The seed a generation stores, byte for byte: seeds 1 to 20 of one world at 1 and at 10
//! players, through `seed::generate` on the manifest and the v1 import, pinned by one digest.
//! Every faster way to the same seed must give these bytes: here the prepared path, which
//! compiles the world once (`job::prepare`) and fills from the stored form (`job::fill`).

use data_fill::job;
use data_fill::prepared::{self, Prepared};
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

/// The found record and the prepared world of `manifest`, as an upload stores them.
fn prepared(manifest: &Value) -> (Value, Vec<u8>) {
    let out = job::prepare(manifest.to_string().as_bytes()).unwrap();
    let (found, file) = job::unframe(&out).unwrap();
    (serde_json::from_slice(found).unwrap(), file.to_vec())
}

/// The fill request of `players` on a prepared world, as the server sends it.
fn request(file: &[u8], players: &[Player], rng: u64, workers: usize) -> Vec<u8> {
    let players: Vec<Value> = players
        .iter()
        .map(|p| {
            let i = &p.instance;
            json!({"name": p.name, "instance": {
                "options": i.options, "rolls": i.rolls, "locations": i.locations,
                "excluded": i.excluded, "locked": i.locked, "pool": i.pool,
                "pool_class": i.pool_class, "start_inventory": i.start_inventory,
                "logic_start": i.logic_start, "item_groups": i.item_groups,
                "location_groups": i.location_groups,
            }})
        })
        .collect();
    let head = json!({"seed_name": format!("s{rng}"), "players": players, "rng_seed": rng, "workers": workers});
    job::frame(head.to_string().as_bytes(), file)
}

/// The seed file and package files the prepared path stores.
fn by_prepared(file: &[u8], players: &[Player], rng: u64) -> (Vec<u8>, Vec<Vec<u8>>) {
    let out = job::fill(&request(file, players, rng, 1)).unwrap();
    let (seed, packages) = seed_bundle::import_v1(&out, MAX).unwrap();
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

/// Fails without `job::prepare` and `job::fill`: the world is compiled once, and every seed
/// filled from that stored form is the seed the manifest path makes.
#[test]
fn the_prepared_path_stores_the_bytes_the_manifest_path_does() {
    let mut all = Vec::new();
    for n in [1, 10] {
        let (m, p) = room(n);
        let (_, file) = prepared(&m);
        for rng in 1..=20 {
            let (seed, packages) = by_prepared(&file, &p, rng);
            assert_eq!(
                (&seed, &packages),
                (&by_manifest(&m, &p, rng).0, &by_manifest(&m, &p, rng).1)
            );
            all.extend(seed);
            all.extend(packages.concat());
        }
    }
    assert_eq!(hex(&key_of(&all)), PINNED);
}

#[test]
fn preparing_checks_a_manifest_as_inspect_does_and_the_file_opens_as_its_world() {
    let (m, p) = room(1);
    let (found, file) = prepared(&m);
    let inspected: Value =
        serde_json::from_slice(&job::inspect(m.to_string().as_bytes()).unwrap()).unwrap();
    assert_eq!(found, inspected);
    let world = Prepared::open(&file).unwrap();
    assert_eq!((world.game(), world.world().locations()), ("Synth", 200));
    // The same manifest prepares to the same bytes: the stored form is found by content.
    assert_eq!(prepared(&m).1, file);
    // More workers when the request asks: the same seed.
    let four = job::fill(&request(&file, &p, 3, 4)).unwrap();
    assert_eq!(four, job::fill(&request(&file, &p, 3, 1)).unwrap());
    // What prepare refuses is what inspect refuses, and a cut file is refused, not filled.
    let mut bad = m.clone();
    bad.as_object_mut().unwrap().remove("version");
    assert!(job::prepare(bad.to_string().as_bytes()).unwrap_err().contains("version"));
    assert!(job::prepare(b"{").unwrap_err().contains("JSON"));
    assert!(job::fill(&request(&file[..file.len() / 2], &p, 1, 1)).is_err());
    assert!(job::fill(b"\x02").is_err());
    assert!(job::run("nope", b"").unwrap_err().contains("unknown job"));
}

/// Fails without the tables kept in the prepared world: the names, ids and checksum every
/// seed of a world carries, and the package file every seed of it names, are made when the
/// world is prepared and are the ones the manifest path makes for each seed.
#[test]
fn the_prepared_world_holds_the_tables_every_seed_of_it_shares() {
    let (m, p) = room(1);
    let (_, file) = prepared(&m);
    let world = Prepared::open(&file).unwrap();
    let bundle = generate(&m, "s1", &p, 1, 1).unwrap();
    assert_eq!(world.data(), &bundle.games["Synth"]);
    let (_, packages) = by_manifest(&m, &p, 1);
    assert_eq!(packages, [prepared::seed_game(&file).unwrap().to_vec()]);
    assert_eq!(world.table().key(), &key_of(&packages[0]));
    assert_eq!(world.table().name(), "Synth");
}
