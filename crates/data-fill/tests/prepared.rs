//! The seed a generation stores, byte for byte: seeds 1 to 20 of one world at 1 and at 10
//! players, through `seed::generate` on the manifest and the v1 import, pinned by one digest.
//! Every faster way to the same seed must give these bytes: here the prepared path, which
//! compiles the world once (`job::prepare`) and fills from the stored form (`job::fill`), whose
//! output is the seed file itself: no JSON bundle is written, parsed and encoded again.

use data_fill::job::{self, Fill};
use data_fill::prepared::{self, Prepared};
use data_fill::seed::{Player, encode_on, generate, generate_on};
use data_fill::synth::{Shape, players, uploaded};
use seed_bundle::package::key_of;
use serde_json::{Value, json};

/// The seed cap of the imports here: the product's.
const MAX: u64 = data_fill::prepared::SEED_MAX_BYTES;
/// SHA-256 of every seed file and package file of [`rooms`], in order.
const PINNED: &str = "063500c4eed6ef09194e1864adc22d39e4bfd82c21b94836f0161e01832424cf";

/// One small synthetic world as an uploaded manifest, and `n` players on it.
fn room(n: usize) -> (Value, Vec<Player>) {
    let (manifest, instance) = uploaded(&Shape { checks: 200, ..Shape::default() }, 1, 0);
    (manifest, players(&instance, n))
}

/// The seed file and package files the manifest path stores: `generate`, then the v1 import.
fn by_manifest(manifest: &Value, players: &[Player], rng: u64) -> (Vec<u8>, Vec<Vec<u8>>) {
    let bundle = generate(manifest, &format!("s{rng}"), players, rng).unwrap();
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
fn asked(players: &[Player], rng: u64) -> Fill<Player> {
    Fill {
        seed_name: format!("s{rng}"),
        players: players.to_vec(),
        rng_seed: rng,
        seed_max_bytes: MAX,
    }
}

fn request(file: &[u8], players: &[Player], rng: u64) -> Vec<u8> {
    asked(players, rng).framed(file).unwrap()
}

/// The seed file and package files the prepared path stores: the fill's output as it is (a
/// v2 seed file), and the game's package the world was prepared with.
fn by_prepared(file: &[u8], players: &[Player], rng: u64) -> (Vec<u8>, Vec<Vec<u8>>) {
    let seed = job::fill(&request(file, players, rng)).unwrap();
    (seed, vec![prepared::seed_game(file).unwrap().to_vec()])
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

/// A worker that stays up fills on a world it loaded before (`job::fill_on`): every seed of
/// [`rooms`] filled on one loaded world, again and again, is the seed a job that loads the file
/// itself makes, so the pin holds for it too. Fails if a fill left anything in the loaded world.
#[test]
fn a_fill_on_a_world_loaded_once_stores_the_bytes_of_a_fill_that_loads_it() {
    let mut all = Vec::new();
    for n in [1, 10] {
        let (m, p) = room(n);
        let (_, file) = prepared(&m);
        let loaded = Prepared::open(&file).unwrap();
        let on = |rng| job::fill_on(&loaded, &asked(&p, rng)).unwrap();
        for rng in 1..=20 {
            let (seed, report) = on(rng);
            let alone = job::fill_reported(&request(&file, &p, rng)).unwrap();
            assert_eq!((&seed, format!("{report:?}")), (&alone.0, format!("{:?}", alone.1)));
            all.extend(seed);
            all.extend(prepared::seed_game(&file).unwrap());
        }
        // An earlier seed again, after nineteen others on the same loaded world.
        assert_eq!(on(1).0, job::fill(&request(&file, &p, 1)).unwrap());
        assert!(job::request(b"{").unwrap_err().contains("bad fill request"));
    }
    assert_eq!(hex(&key_of(&all)), PINNED);
}

/// A fill leaves nothing in the loaded world it ran on: after fills for one and ten players, and after fills that are refused, the world prints exactly as it did when it
/// was loaded. A worker that keeps a world between jobs rests on this; a fill that staged
/// anything per seed in the prepared form would fail here.
#[test]
fn fills_leave_a_loaded_world_exactly_as_it_was_loaded() {
    let (m, ten) = room(10);
    let (_, file) = prepared(&m);
    let loaded = Prepared::open(&file).unwrap();
    let before = format!("{loaded:?}");
    let mut filled = 0;
    for players in [&ten[..1], &ten[..]] {
        for rng in 1..=9 {
            filled += usize::from(job::fill_on(&loaded, &asked(players, rng)).is_ok());
        }
    }
    assert!(filled >= 15, "only {filled} of 18 fills ran to the end");
    // Refused before the fill (a name twice) and inside it (an item the world lacks).
    let twice = [ten[0].clone(), ten[0].clone()];
    assert!(job::fill_on(&loaded, &asked(&twice, 1)).is_err());
    let mut lacking = ten[0].clone();
    lacking.instance.pool.push("no such item".to_owned());
    assert!(job::fill_on(&loaded, &asked(&[lacking], 1)).is_err());
    assert!(format!("{loaded:?}") == before, "a fill changed the loaded world");
    // And it still fills as a world loaded now does.
    let again = job::fill_on(&loaded, &asked(&ten, 2)).unwrap().0;
    assert_eq!(again, job::fill(&request(&file, &ten, 2)).unwrap());
}

#[test]
fn preparing_checks_a_manifest_and_the_file_opens_as_its_world() {
    let (m, p) = room(1);
    let (found, file) = prepared(&m);
    let w = data_fill::World::load(&m).unwrap();
    let record = json!({"game": "Synth", "version": "1", "items": w.items(), "locations": 200});
    assert_eq!(found, record);
    let world = Prepared::open(&file).unwrap();
    assert_eq!((world.game(), world.world().locations()), ("Synth", 200));
    // The same manifest prepares to the same bytes: the stored form is found by content.
    assert_eq!(prepared(&m).1, file);
    // What prepare refuses is what inspect refuses, and a cut file is refused, not filled.
    let mut bad = m.clone();
    bad.as_object_mut().unwrap().remove("version");
    assert!(job::prepare(bad.to_string().as_bytes()).unwrap_err().contains("version"));
    assert!(job::prepare(b"{").unwrap_err().contains("JSON"));
    assert!(job::fill(&request(&file[..file.len() / 2], &p, 1)).is_err());
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
    let bundle = generate(&m, "s1", &p, 1).unwrap();
    assert_eq!(world.data(), &bundle.games["Synth"]);
    let (_, packages) = by_manifest(&m, &p, 1);
    assert_eq!(packages, [prepared::seed_game(&file).unwrap().to_vec()]);
    assert_eq!(world.table().key(), &key_of(&packages[0]));
    assert_eq!(world.table().name(), "Synth");
}

/// Fails while the fill prints a JSON bundle: its output is a v2 seed file that opens on the
/// prepared world's package, with the hash of the bundle's canonical JSON, and a seed over the
/// request's cap is refused.
#[test]
fn the_fill_prints_a_seed_file_with_the_hash_of_its_bundle() {
    let (m, p) = room(2);
    let (_, file) = prepared(&m);
    let seed = job::fill(&request(&file, &p, 5)).unwrap();
    assert!(seed_bundle::v2::is_v2(&seed));
    let bundle = generate(&m, "s5", &p, 5).unwrap();
    let text = serde_json::to_string(&bundle).unwrap();
    let hash = seed_bundle::bundle_hash(&text).unwrap();
    assert_eq!(seed_bundle::v2::bundle_hash_of(&seed).unwrap().to_vec(), hash);
    let game = prepared::seed_game(&file).unwrap().to_vec();
    let back = seed_bundle::v2::decode(seed, |_| Ok(game.clone()), MAX).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&bundle).unwrap());
    let framed = request(&file, &p, 5);
    let (head, world) = job::unframe(&framed).unwrap();
    let mut head: Value = serde_json::from_slice(head).unwrap();
    head["seed_max_bytes"] = json!(64);
    let small = job::frame(head.to_string().as_bytes(), world).unwrap();
    assert!(job::fill(&small).unwrap_err().contains("cap"));
    // A field the reader does not know is refused, not dropped: a misspelt cap cannot pass.
    head["seed_max_byte"] = json!(64);
    let misspelt = job::frame(head.to_string().as_bytes(), world).unwrap();
    assert!(job::fill(&misspelt).unwrap_err().contains("bad fill request"));
    // A world that does not load says so in the words the server looks for.
    let junk = asked(&p, 5).framed(b"not a prepared world").unwrap();
    assert!(job::fill(&junk).unwrap_err().starts_with(job::UNLOADABLE));
}

/// A room of three players on one world, each instance changed as `what` says (the changes
/// the fill's own path test uses): start items, excluded locations, locked locations.
fn varied(shape: &Shape, what: &str) -> (Value, Vec<Player>) {
    varied_of(shape, what, 3)
}

/// [`varied`] with `n` players.
fn varied_of(shape: &Shape, what: &str, n: usize) -> (Value, Vec<Player>) {
    let (manifest, instance) = uploaded(shape, 1, 0);
    let players = (0..n)
        .map(|p| {
            let mut i = instance.clone();
            if what.contains("start") {
                i.start_inventory.push("p1".to_owned());
                i.logic_start.insert("p0".to_owned(), 1);
            }
            if what.contains("excluded") {
                // A sixth of the locations take no progression; other ones for each player.
                i.excluded = (0..i.locations.len())
                    .filter(|l| l % 6 == p % 6)
                    .map(|l| format!("l{l}"))
                    .collect();
            }
            if what.contains("locked") {
                // A progression item and a filler leave the pool for fixed locations.
                for (key, loc) in [("p1", "l3"), ("filler", "l10")] {
                    let at = i.pool.iter().position(|k| k == key).unwrap();
                    i.locked.insert(loc.to_owned(), i.pool.remove(at));
                }
            }
            Player { name: format!("P{p}"), instance: i }
        })
        .collect();
    (manifest, players)
}

/// The prepared path against the manifest path beyond plain rooms: start items (which the
/// seed file encodes through the prepared package), excluded and locked locations, the heavy
/// profile. Every seed, or its refusal, is the same on both.
#[test]
fn the_prepared_path_matches_on_start_items_excluded_and_locked_locations() {
    let (light, heavy) =
        (Shape { checks: 200, ..Shape::default() }, Shape { checks: 200, ..Shape::heavy() });
    let mut made = 0;
    for (shape, what) in [
        (&light, "start"),
        (&light, "excluded"),
        (&light, "locked"),
        (&light, "start+excluded+locked"),
        (&heavy, "start+excluded+locked"),
    ] {
        let (m, p) = varied(shape, what);
        let (_, file) = prepared(&m);
        let game = prepared::seed_game(&file).unwrap().to_vec();
        for rng in 1..=9 {
            let old = generate(&m, &format!("s{rng}"), &p, rng).map(|b| {
                let text = serde_json::to_vec(&b).unwrap();
                let (seed, packages) = seed_bundle::import_v1(&text, MAX).unwrap();
                (seed.bytes().to_vec(), packages)
            });
            let new = job::fill(&asked(&p, rng).framed(&file).unwrap())
                .map(|seed| (seed, vec![game.clone()]));
            assert_eq!(new, old, "{what}, seed {rng}");
            made += usize::from(old.is_ok());
            if let (Ok(seed), true) = (&old, what.contains("start")) {
                let back = seed_bundle::v2::decode(seed.0.clone(), |_| Ok(game.clone()), MAX);
                assert!(back.unwrap().slots.iter().all(|s| s.start_inventory.len() == 1));
            }
        }
    }
    assert!(made >= 40, "most of the 45 rooms fill: {made}");
}

/// The seed file `seed::encode_on` writes, against the bundle's JSON text read back as an
/// uploaded bundle is (`seed_bundle::import_v1`, which hashes the text as a JSON tree,
/// `seed_bundle::bundle_hash`): the same bytes, hash and package at 1, 10 and 100 players, on
/// plain rooms, with start items, excluded and locked locations, and on a world with shuffled
/// doors. Fails if the hash written without a tree, or a slot found by index, differs.
#[test]
fn the_encoded_seed_is_the_import_of_its_json_at_1_10_and_100_players() {
    let plain = Shape { checks: 60, layers: 3, ..Shape::default() };
    let doors = Shape { doors: true, timeless: true, ..plain };
    let mut made = 0;
    for (shape, what) in [(&plain, ""), (&plain, "start+excluded+locked"), (&doors, "shuffled")] {
        for n in [1, 10, 100] {
            let (m, p) = varied_of(shape, what, n);
            let (_, file) = prepared(&m);
            let on = Prepared::open(&file).unwrap();
            let (mut bundle, _) = generate_on(&on, "s", &p, 1).expect("the room fills");
            assert_eq!(m["shuffles"].is_object(), what == "shuffled");
            let text = serde_json::to_string(&bundle).unwrap();
            let (old, packages) = seed_bundle::import_v1(text.as_bytes(), MAX).unwrap();
            let new = encode_on(&on, &mut bundle, MAX).unwrap();
            assert!(new == old.bytes(), "{what}, {n} players: the seed files differ");
            assert_eq!(packages, [prepared::seed_game(&file).unwrap()], "{what}, {n} players");
            let hash = seed_bundle::bundle_hash(&text).unwrap();
            assert_eq!(seed_bundle::v2::bundle_hash_of(&new).unwrap()[..], hash[..]);
            assert_eq!(bundle.hash().unwrap()[..], hash[..], "{what}, {n} players");
            assert_eq!(
                serde_json::to_string(&bundle).unwrap(),
                text,
                "the bundle is left as it was"
            );
            made += 1;
        }
    }
    assert_eq!(made, 9);
}
