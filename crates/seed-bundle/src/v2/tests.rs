// Expected values: the v1 JSON itself (round trips), and bit widths counted by hand.
use super::*;
use crate::bundle_hash;
use serde_json::{Value, json};

fn fixture_text() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../e2e/fixtures/two-slot.json");
    std::fs::read_to_string(path).unwrap()
}

/// Two slots of one game G: items 7 and 8 (indexes 0 and 1), locations 100 and 101.
fn small() -> Value {
    json!({
        "format": 1, "seed_name": "s1", "race_mode": 1,
        "generator_version": {"major": 0, "minor": 6, "build": 8},
        "server_options": {
            "password": "pw", "hint_cost": 10, "location_check_points": 1,
            "release_mode": "auto", "collect_mode": "auto-enabled", "remaining_mode": "goal"
        },
        "slots": [
            {"id": 2, "team": 0, "name": "B", "game": "G", "kind": "player",
             "minimum_client_version": {"major": 0, "minor": 1, "build": 6}, "group_members": [],
             "locations": [{"location": 101, "item": 7, "receiver": 1, "flags": 4},
                           {"location": 100, "item": 8, "receiver": 2, "flags": 0}],
             "slot_data": {}, "start_inventory": [8, 8, 7]},
            {"id": 1, "team": 0, "name": "A", "game": "G", "kind": "spectator",
             "minimum_client_version": {"major": 0, "minor": 1, "build": 6}, "group_members": [],
             "locations": [{"location": 100, "item": 7, "receiver": 2, "flags": 1}],
             "slot_data": {"k": [1, 2]}, "start_inventory": []}
        ],
        "connect_names": {"A": [0, 1], "B": [0, 2], "Bee": [0, 2]},
        "games": {"G": {
            "item_name_to_id": {"Seven": 7, "Eight": 8},
            "location_name_to_id": {"L100": 100, "L101": 101, "L102": 102},
            "checksum": "abc", "item_name_groups": {}, "location_name_groups": {}
        }}
    })
}

fn hash_of(text: &str) -> [u8; 32] {
    bundle_hash(text).unwrap().try_into().unwrap()
}

fn encoded(text: &str) -> (SeedBundle, Encoded) {
    let b = SeedBundle::from_json(text).unwrap();
    let e = encode(&b, &hash_of(text), 1 << 26).unwrap();
    (b, e)
}

fn opened(text: &str) -> (Seed, BTreeMap<Key, GameData>) {
    let (b, e) = encoded(text);
    let mut data = BTreeMap::new();
    let games = e
        .packages
        .iter()
        .map(|f| {
            let key = package::key_of(f);
            let (name, g) = package::game_of(f, 1 << 26).unwrap();
            assert!(b.games.contains_key(&name));
            let p = Arc::new(GamePackage::new(key, &name, &g));
            data.insert(key, g);
            p
        })
        .collect();
    (Seed::open(e.seed, games, 1 << 26).unwrap(), data)
}

fn canonical_of(v: &impl serde::Serialize) -> Vec<u8> {
    package::canonical(serde_json::to_value(v).unwrap())
}

#[test]
fn the_fixture_round_trips_to_the_same_canonical_json_and_hash() {
    let text = fixture_text();
    let (seed, data) = opened(&text);
    let back = seed.to_v1(&data).unwrap();
    let input: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(canonical_of(&back), package::canonical(input));
    assert_eq!(bundle_hash(&serde_json::to_string(&back).unwrap()).unwrap(), seed.hash());
    assert_eq!(seed.hash()[..], bundle_hash(&text).unwrap()[..]);
}

#[test]
fn a_small_bundle_round_trips_with_slots_and_locations_in_id_order() {
    let text = small().to_string();
    let (seed, data) = opened(&text);
    let mut want = SeedBundle::from_json(&text).unwrap();
    want.slots.sort_by_key(|s| s.id);
    for s in &mut want.slots {
        s.locations.sort_by_key(|p| p.location);
    }
    assert_eq!(seed.to_v1(&data).unwrap(), want);
    assert_eq!(seed.password(), Some("pw"));
    assert_eq!(seed.race_mode(), 1);
    assert_eq!(seed.connect("Bee"), Some((0, 2)));
    assert_eq!(seed.connect("Nobody"), None);
}

#[test]
fn bit_widths_are_the_fewest_this_seed_needs() {
    // Item indexes 0..=1: 1 bit. Receiver positions 0..=1: 1 bit. Flags up to 4: 3 bits.
    let (seed, _) = opened(&small().to_string());
    assert_eq!(seed.bit_widths(), (1, 1, 3));
    // The fixture: 32 items in its game, so indexes up to 31 at most 5 bits; two slots.
    let (seed, _) = opened(&fixture_text());
    let (wi, wr, wf) = seed.bit_widths();
    assert!(wi <= 5 && wr == 1 && wf <= 3, "{wi} {wr} {wf}");
}

#[test]
fn one_slot_receives_into_zero_bits() {
    let mut v = small();
    v["slots"] = json!([v["slots"][0].clone()]);
    v["slots"][0]["locations"] = json!([{"location": 101, "item": 8, "receiver": 2, "flags": 0}]);
    v["connect_names"] = json!({"B": [0, 2]});
    let (seed, data) = opened(&v.to_string());
    // Item index 1 needs 1 bit; one receiver and flags 0 need none.
    assert_eq!(seed.bit_widths(), (1, 0, 0));
    assert_eq!(seed.to_v1(&data).unwrap().slots[0].locations[0].item, 8);
}

#[test]
fn every_location_is_found_by_id_and_by_index() {
    let text = fixture_text();
    let b = SeedBundle::from_json(&text).unwrap();
    let (seed, _) = opened(&text);
    for s in &b.slots {
        let v = seed.slot(s.id).unwrap();
        assert_eq!(v.placed(), s.locations.len());
        for (i, p) in s.locations.iter().enumerate() {
            assert_eq!(v.find(p.location), Some(i));
            let got = v.placement(i).unwrap();
            assert_eq!(
                (got.location, got.item, got.receiver, got.flags),
                (p.location, p.item, p.receiver, p.flags)
            );
            let o = seed.ordinal(v.pos(), i).unwrap();
            assert_eq!(seed.from_ordinal(o), Some((v.pos(), i)));
        }
        assert_eq!(v.find(-12345), None);
        assert_eq!(v.placement(s.locations.len()), None);
        assert_eq!(v.target(s.locations.len()), None);
    }
    // A location of the game that is not placed in this world.
    let (seed, _) = opened(&small().to_string());
    assert_eq!(seed.slot(1).unwrap().find(101), None);
    assert_eq!(seed.slot(1).unwrap().find(102), None);
}

#[test]
fn small_slot_data_is_kept_raw_and_large_slot_data_is_compressed() {
    let mut v = small();
    v["slots"][1]["slot_data"] = json!({"text": "x".repeat(4000)});
    let (seed, _) = opened(&v.to_string());
    let a = seed.slot(1).unwrap();
    assert_eq!(a.slot_data().unwrap(), json!({"text": "x".repeat(4000)}));
    assert!(seed.bytes().len() < 2000, "4000 repeated bytes compress");
    assert_eq!(seed.slot(2).unwrap().slot_data().unwrap(), json!({}));
}

#[test]
fn the_fixture_is_a_fraction_of_its_json() {
    let text = fixture_text();
    let (_, e) = encoded(&text);
    // Everything per game sits in the packages; the seed keeps ids as bits.
    assert!(e.seed.len() * 20 < text.len(), "{} vs {}", e.seed.len(), text.len());
}

/// Rewrite the body hash after an edit, so the edit reaches the rule under test.
fn rehash(bytes: &mut [u8]) {
    let h = body_hash(bytes);
    bytes[64..96].copy_from_slice(&h);
}

fn reopen(bytes: Vec<u8>, text: &str) -> Result<Seed, BundleError> {
    let (_, e) = encoded(text);
    let games = e
        .packages
        .iter()
        .map(|f| Arc::new(GamePackage::from_file(package::key_of(f), f, 1 << 26).unwrap()))
        .collect();
    Seed::open(bytes, games, 1 << 26)
}

#[test]
fn corrupt_truncated_and_hostile_files_are_errors_not_panics() {
    let text = small().to_string();
    let (_, e) = encoded(&text);
    let good = e.seed.clone();
    assert!(reopen(good.clone(), &text).is_ok());
    let mut flipped = good.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert!(reopen(flipped, &text).unwrap_err().to_string().contains("corrupt"));
    for cut in [0, 3, 50, HEADER, good.len() - 1] {
        assert!(reopen(good[..cut].to_vec(), &text).is_err(), "cut at {cut}");
    }
    let mut v1 = good.clone();
    v1[4] = 1;
    assert!(reopen(v1, &text).is_err(), "another version");
    let hostile: [(&str, usize, u8); 6] = [
        ("item field too wide", 7, 40),
        ("receiver field too wide", 8, 17),
        ("race mode not a flag", 6, 2),
        ("hint cost above 100", HEADER + 28, 101),
        ("remaining mode auto", HEADER + 31, 3),
        ("game index past the table", HEADER + ROOM + GAME + 16, 9),
    ];
    for (name, at, value) in hostile {
        let mut b = good.clone();
        b[at] = value;
        rehash(&mut b);
        assert!(reopen(b, &text).is_err(), "{name}");
    }
    // The second slot's first record: its item index becomes 1 in a 1-bit field, fine; set
    // its flags to 0b111 (valid) and its receiver past the slots by widening nothing: instead
    // point the presence offset of slot 1 at the string table.
    let mut b = good.clone();
    let slot0 = HEADER + ROOM + GAME;
    b[slot0 + 32..slot0 + 36].copy_from_slice(&u32::MAX.to_le_bytes());
    rehash(&mut b);
    assert!(reopen(b, &text).is_err(), "presence out of range");
    let mut b = good.clone();
    b[slot0 + 36..slot0 + 40].copy_from_slice(&5_u32.to_le_bytes());
    rehash(&mut b);
    assert!(reopen(b, &text).is_err(), "placed count disagrees with the presence set");
    let mut b = good;
    b[slot0 + 52..slot0 + 56].copy_from_slice(&u32::MAX.to_le_bytes());
    rehash(&mut b);
    assert!(reopen(b, &text).is_err(), "slot_data that would inflate past the cap");
}

#[test]
fn packages_must_be_the_ones_the_seed_names() {
    let text = small().to_string();
    let (_, e) = encoded(&text);
    assert!(Seed::open(e.seed.clone(), Vec::new(), 1 << 26).is_err());
    let other = GameData {
        item_name_to_id: BTreeMap::new(),
        location_name_to_id: BTreeMap::new(),
        checksum: String::new(),
        item_name_groups: BTreeMap::new(),
        location_name_groups: BTreeMap::new(),
    };
    let wrong = Arc::new(GamePackage::new([3; 32], "G", &other));
    assert!(Seed::open(e.seed, vec![wrong], 1 << 26).is_err());
}

#[test]
fn a_stored_seed_reads_back_on_its_packages_and_decodes_to_its_v1() {
    let text = fixture_text();
    let (_, e) = encoded(&text);
    // A blob store stand-in: package files by key.
    let store: BTreeMap<Key, Vec<u8>> =
        e.packages.iter().map(|f| (package::key_of(f), f.clone())).collect();
    let fetch = |k: &Key| store.get(k).cloned().ok_or_else(|| BundleError::Parse("missing".into()));
    let (imported, files) = crate::import_v1(text.as_bytes(), 1 << 26).unwrap();
    assert_eq!(files, e.packages, "the import hands back the package files to store");
    let stored = Seed::read(e.seed.clone(), fetch, 1 << 26).unwrap();
    assert_eq!(stored.bytes(), imported.bytes());
    // Both share this process's one copy of each package.
    assert!(Arc::ptr_eq(&stored.games()[0], &imported.games()[0]));
    // While a room holds the packages, a seed needs no fetch; once none does, it needs one.
    let none = |_: &Key| Err(BundleError::Parse("not in the store".into()));
    assert!(Seed::read(e.seed.clone(), none, 1 << 26).is_ok());
    drop((stored, imported));
    let missing = Seed::read(e.seed.clone(), none, 1 << 26).unwrap_err();
    assert!(missing.to_string().contains("not in the store"), "{missing}");
    assert!(crate::import_v1(b"{not json", 1 << 26).is_err());
    let back = decode(e.seed, fetch, 1 << 26).unwrap();
    let input: Value = serde_json::from_str(&fixture_text()).unwrap();
    assert_eq!(canonical_of(&back), package::canonical(input));
}

#[test]
fn the_world_view_matches_the_placements() {
    let (seed, _) = opened(&small().to_string());
    let w: &dyn World = &seed;
    assert_eq!(w.slots(), [SlotNo(1), SlotNo(2)]);
    assert_eq!((w.placed(0), w.placed(1), w.placed(2)), (1, 2, 0));
    // Slot 2 (position 1): location 101 is index 1 and sends item 7 to slot 1 (position 0).
    assert_eq!(w.find(1, 101), Some(1));
    assert_eq!(w.target(1, 1), Some((0, 7)));
    assert_eq!(w.target(1, 2), None);
    let start: Vec<i64> = seed.slot(2).unwrap().start_inventory().collect();
    assert_eq!(start, [8, 8, 7]);
}

/// A one-game bundle: `locations` locations, ids 1..; `slots` slots, of which only slot 1
/// places anything (its first `placed` locations, all item 1 to itself).
fn wide(locations: usize, slots: u32, placed: usize) -> String {
    let names: serde_json::Map<String, Value> =
        (1..=locations).map(|n| (format!("L{n}"), json!(n))).collect();
    let slot = |id: u32| {
        let locations: Vec<Value> = (1..=if id == 1 { placed } else { 0 })
            .map(|l| json!({"location": l, "item": 1, "receiver": 1, "flags": 0}))
            .collect();
        json!({"id": id, "team": 0, "name": format!("P{id}"), "game": "G", "kind": "player",
               "minimum_client_version": {"major": 0, "minor": 1, "build": 6},
               "group_members": [], "locations": locations, "slot_data": {},
               "start_inventory": []})
    };
    json!({
        "format": 1, "seed_name": "wide", "race_mode": 0,
        "generator_version": {"major": 0, "minor": 6, "build": 8},
        "server_options": {"password": null, "hint_cost": 10, "location_check_points": 1,
            "release_mode": "auto", "collect_mode": "auto", "remaining_mode": "goal"},
        "slots": (1..=slots).map(slot).collect::<Vec<_>>(),
        "connect_names": {},
        "games": {"G": {"item_name_to_id": {"One": 1}, "location_name_to_id": names,
                        "checksum": "w", "item_name_groups": {}, "location_name_groups": {}}}
    })
    .to_string()
}

/// Review finding: presence was sized by the game for every slot, so empty slots of a huge
/// game blew the seed (and the server's memory) up. Now an empty slot stores no presence
/// words, and the seed is sized before anything is allocated and refused over the cap.
#[test]
fn empty_slots_of_a_huge_game_cost_nothing_and_an_oversized_seed_is_refused() {
    // 30,000 locations: 469 presence words (3.7 KB) per slot that places something.
    let text = wide(30_000, 40, 1);
    let b = SeedBundle::from_json(&text).unwrap();
    let e = encode(&b, &hash_of(&text), 1 << 26).unwrap();
    // One slot's words plus 39 empty slot records, not 40 slots' words (150 KB).
    assert!(e.seed.len() < 8_000, "{} bytes", e.seed.len());
    let (seed, data) = opened(&text);
    assert_eq!(seed.to_v1(&data).unwrap().slots.len(), 40);
    assert_eq!(seed.slot(2).unwrap().placed(), 0);
    assert_eq!(seed.slot(2).unwrap().find(1), None);
    // Under a cap smaller than one slot's words: refused, before the words are written.
    let err = encode(&b, &hash_of(&text), 3_000).unwrap_err().to_string();
    assert!(err.contains("over the 3000 cap"), "{err}");
}

/// Review finding: with item width 0 a start inventory count cost no bytes, so a hostile
/// count up to 2^32 passed the range check and `open` iterated it (18.7 s measured). Start
/// entries now take at least one bit, so the count is bounded by the file's bytes.
#[test]
fn a_hostile_start_count_with_zero_item_bits_is_refused_at_once() {
    let text = wide(3, 1, 2);
    let (_, e) = encoded(&text);
    let (seed, _) = opened(&text);
    assert_eq!(seed.bit_widths(), (0, 0, 0), "one item, one receiver, no flags");
    let mut b = e.seed.clone();
    let slot0 = HEADER + ROOM + GAME;
    b[slot0 + 68..slot0 + 72].copy_from_slice(&u32::MAX.to_le_bytes());
    rehash(&mut b);
    let started = std::time::Instant::now();
    assert!(reopen(b, &text).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(1), "{:?}", started.elapsed());
}

/// Review finding: ordinals and selects walked every slot and every presence word per item.
/// Many slots, one game: every ordinal and index round trips (now through prefix sums and
/// rank tables).
#[test]
fn ordinals_and_indexes_round_trip_across_many_slots() {
    let mut v: Value = serde_json::from_str(&wide(700, 60, 0)).unwrap();
    for (k, s) in v["slots"].as_array_mut().unwrap().iter_mut().enumerate() {
        let locations: Vec<Value> = (0..k % 13)
            .map(|j| json!({"location": 1 + (k * 7 + j * 53) % 700, "item": 1, "receiver": 1 + (j % 60), "flags": j % 3}))
            .collect();
        s["locations"] = json!(locations);
    }
    let text = v.to_string();
    let b = SeedBundle::from_json(&text).unwrap();
    let (seed, _) = opened(&text);
    let mut ordinal = 0;
    for s in &b.slots {
        let view = seed.slot(s.id).unwrap();
        let mut sorted = s.locations.clone();
        sorted.sort_by_key(|p| p.location);
        for (i, p) in sorted.iter().enumerate() {
            assert_eq!(seed.ordinal(view.pos(), i), Some(ordinal));
            assert_eq!(seed.from_ordinal(ordinal), Some((view.pos(), i)));
            assert_eq!(view.location(i), Some(p.location));
            assert_eq!(view.find(p.location), Some(i));
            assert_eq!(view.flags(i), Some(p.flags));
            ordinal += 1;
        }
        assert_eq!(view.location(sorted.len()), None);
        assert_eq!(seed.ordinal(view.pos(), sorted.len()), None);
    }
    assert_eq!(seed.from_ordinal(ordinal), None);
}

/// Review finding: no test crossed games. Games A and B with disjoint item ids and different
/// item counts: slot 1 (A) finds an item of B for slot 2 (B), slot 2 one of A for slot 1, and
/// slot 3 (A) places nothing (`crates/e2e/fixtures/cross-games.json`, shared with the edge
/// tests). Item indexes resolve against the receiver's game.
fn cross_games() -> Value {
    serde_json::from_str(include_str!("../../../e2e/fixtures/cross-games.json")).unwrap()
}

#[test]
fn a_placement_across_games_resolves_against_the_receivers_game() {
    let text = cross_games().to_string();
    let (seed, data) = opened(&text);
    assert_eq!(seed.to_v1(&data).unwrap(), SeedBundle::from_json(&text).unwrap());
    let w: &dyn World = &seed;
    // Slot 1's location 11 (index 0) sends B's item 202 (index 2 in B, beyond A's 2 items).
    assert_eq!(w.target(0, 0), Some((1, 202)));
    assert_eq!(w.target(1, 0), Some((0, 2)));
    let p = seed.slot(1).unwrap().placement(0).unwrap();
    assert_eq!((p.item, p.receiver, p.flags), (202, 2, 1));
    assert_eq!(seed.slot(3).unwrap().placements().count(), 0);
    assert_eq!(w.placed(2), 0);
    // Item indexes up to 3 (B's b3 is unused; b2 is index 2): 2 bits.
    assert_eq!(seed.bit_widths().0, 2);
}
