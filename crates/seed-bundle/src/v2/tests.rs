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
    reopen_capped(bytes, text, 1 << 26)
}

/// Open `bytes` on `text`'s packages with `max_bytes` for its `slot_data`.
fn reopen_capped(bytes: Vec<u8>, text: &str, max_bytes: u64) -> Result<Seed, BundleError> {
    let (_, e) = encoded(text);
    let games = e
        .packages
        .iter()
        .map(|f| Arc::new(GamePackage::from_file(package::key_of(f), f, 1 << 26).unwrap()))
        .collect();
    Seed::open(bytes, games, max_bytes)
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
    let other = GameData::default();
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

/// `small` with every hint field set: start hints, spheres (one location without), entrances,
/// and the hint blacklist.
fn hinted() -> Value {
    let mut v = small();
    v["start_hints"] = json!([
        {"finding": 2, "location": 101, "status": 30},
        {"finding": 1, "location": 100, "status": 0}
    ]);
    v["slots"][0]["locations"][0]["sphere"] = json!(3);
    v["slots"][1]["locations"][0]["sphere"] = json!(0);
    v["slots"][0]["entrances"] = json!({"100": "Back Door", "101": "Cave"});
    v["games"]["G"]["hint_blacklist"] = json!(["Eight"]);
    v
}

#[test]
fn hint_data_round_trips_v1_to_v2_to_v1() {
    let text = hinted().to_string();
    let (seed, data) = opened(&text);
    let mut want = SeedBundle::from_json(&text).unwrap();
    want.slots.sort_by_key(|s| s.id);
    for s in &mut want.slots {
        s.locations.sort_by_key(|p| p.location);
    }
    assert_eq!(seed.to_v1(&data).unwrap(), want);
    let b = seed.slot(2).unwrap();
    assert_eq!((b.sphere(101), b.sphere(100), b.sphere(102)), (Some(3), None, None));
    assert_eq!(
        (b.entrance(100), b.entrance(101), b.entrance(102)),
        (Some("Back Door"), Some("Cave"), None)
    );
    assert_eq!(seed.slot(1).unwrap().entrance(100), None);
    let start: Vec<_> = seed.start_hints().collect();
    assert_eq!(start, [(SlotNo(2), 101, 30), (SlotNo(1), 100, 0)]);
    assert!(seed.game("G").unwrap().hint_blacklisted("Eight"));
    assert!(!seed.game("G").unwrap().hint_blacklisted("Seven"));
}

#[test]
fn hint_fields_take_the_fewest_bits_and_no_hint_data_means_no_block() {
    // Spheres up to 3 plus the "none" value: 3 bits. Location indexes up to 1 (L101): 1 bit.
    // Entrance indexes up to 1 (Back Door, Cave): 1 bit. Slot positions up to 1: 1 bit.
    let (_, e) = encoded(&hinted().to_string());
    let at = u32::from_le_bytes(e.seed[28..32].try_into().unwrap()) as usize;
    assert_ne!(at, 0);
    assert_eq!(e.seed[at..at + 4], [3, 1, 1, 1]);
    // Without hint data the reserved field stays 0: a seed made before hints is unchanged,
    // and so is its package (no empty blacklist or entrance list is written).
    let (_, plain) = encoded(&small().to_string());
    assert_eq!(plain.seed[28..32], [0; 4]);
    let content = package::game_of(&plain.packages[0], 1 << 20).unwrap();
    assert!(
        !String::from_utf8(GamePackage::content(&content.0, &content.1)).unwrap().contains("hint")
    );
}

#[test]
fn a_hostile_hint_block_is_an_error_not_a_panic() {
    let text = hinted().to_string();
    let (_, e) = encoded(&text);
    let good = e.seed;
    let at = u32::from_le_bytes(good[28..32].try_into().unwrap()) as usize;
    let start = u32::from_le_bytes(good[at + 8..at + 12].try_into().unwrap()) as usize;
    // Slot 2 (position 1): its entrance count is the third field of its hint entry.
    let count = at + HINTS + HINT_SLOT + 8;
    let field =
        |b: &mut Vec<u8>, off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    type Edit<'a> = Box<dyn Fn(&mut Vec<u8>) + 'a>;
    let cases: [(&str, Edit<'_>); 9] = [
        ("block past the data", Box::new(|b| field(b, 28, u32::MAX - 4))),
        ("sphere field too wide", Box::new(move |b| b[at] = 17)),
        ("start hint count past the data", Box::new(move |b| field(b, at + 4, 1 << 20))),
        // The first start hint's status code (bits 2 to 4 of its first byte) becomes 7.
        ("start hint status code", Box::new(move |b| b[start] |= 0b1_1100)),
        ("entrance count past the data", Box::new(move |b| field(b, count, 1 << 20))),
        ("entrances out of order", Box::new(move |b| field(b, count, 3))),
        ("spheres with zero bits", Box::new(move |b| b[at] = 0)),
        ("entrance names past the data", Box::new(move |b| field(b, at + 12, 1 << 20))),
        (
            "entrance name ref out of range",
            Box::new(move |b| {
                let names = u32::from_le_bytes(b[at + 16..at + 20].try_into().unwrap()) as usize;
                field(b, names, u32::MAX - 1);
            }),
        ),
    ];
    for (name, edit) in cases {
        let mut b = good.clone();
        edit(&mut b);
        rehash(&mut b);
        assert!(reopen(b, &text).is_err(), "{name}");
    }
    assert!(reopen(good.clone(), &text).is_ok());
    // The entrance name table is sorted by name, as the format says: swapping its two
    // references ("Back Door", "Cave") is refused.
    let mut b = good;
    let names = u32::from_le_bytes(b[at + 16..at + 20].try_into().unwrap()) as usize;
    let (first, second) = (b[names..names + 8].to_vec(), b[names + 8..names + 16].to_vec());
    b[names..names + 8].copy_from_slice(&second);
    b[names + 8..names + 16].copy_from_slice(&first);
    rehash(&mut b);
    assert!(reopen(b, &text).unwrap_err().to_string().contains("entrance names"));
}

/// Review finding: the encoder expanded every slot's spheres to one value per location of its
/// game before it checked the cap (8 bytes each in memory), so a small bundle of many slots of
/// a huge game, each with one sphere, allocated gigabytes before it was refused. Now spheres
/// stay sparse until the seed is sized: 400 slots of a 50,000-location game would need 20 M
/// sphere values, and the cap refuses them from the counts alone.
#[test]
fn many_slots_with_one_sphere_each_are_sized_from_counts_and_refused() {
    let mut v: Value = serde_json::from_str(&wide(50_000, 400, 0)).unwrap();
    for s in v["slots"].as_array_mut().unwrap() {
        s["locations"] =
            json!([{"location": 1, "item": 1, "receiver": 1, "flags": 0, "sphere": 0}]);
    }
    let text = v.to_string();
    let b = SeedBundle::from_json(&text).unwrap();
    let err = encode(&b, &hash_of(&text), 1 << 20).unwrap_err().to_string();
    assert!(err.contains("cap"), "{err}");
    // What the encoder holds before the cap check: one entry per sphere given, not per location.
    let tables = vec![GamePackage::new([0; 32], "G", &game(&text))];
    let mut slots: Vec<&Slot> = b.slots.iter().collect();
    slots.sort_by_key(|s| s.id);
    let indexed: Vec<Indexed> =
        slots.iter().map(|_| Indexed { game: 0, placed: Vec::new(), start: Vec::new() }).collect();
    let held = hint_data(&b, &slots, &indexed, &tables).unwrap();
    assert_eq!(held.spheres.iter().map(|s| s.1.len()).sum::<usize>(), 400);
}

fn game(text: &str) -> GameData {
    SeedBundle::from_json(text).unwrap().games["G"].clone()
}

/// Review finding: entrance names were in the package, so every seed with shuffled entrances
/// made its own package of the same game. Now they are in the seed's string table.
#[test]
fn seeds_with_different_entrances_share_one_package() {
    let mut other = hinted();
    other["slots"][0]["entrances"] = json!({"100": "Attic"});
    let (_, a) = encoded(&hinted().to_string());
    let (_, b) = encoded(&other.to_string());
    assert_eq!(package::key_of(&a.packages[0]), package::key_of(&b.packages[0]));
    let (seed, data) = opened(&other.to_string());
    assert_eq!(seed.slot(2).unwrap().entrance(100), Some("Attic"));
    assert_eq!(seed.to_v1(&data).unwrap().slots[1].entrances[&100], "Attic");
}

fn get32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

fn put32(b: &mut [u8], at: usize, v: usize) {
    b[at..at + 4].copy_from_slice(&u32::try_from(v).unwrap().to_le_bytes());
}

/// The slot table of a one-game seed.
const SLOT0: usize = HEADER + ROOM + GAME;

/// Why `text`'s seed, edited by `edit` (its body hash rewritten), does not open; "opened"
/// when it does.
fn refused(text: &str, edit: impl FnOnce(&mut Vec<u8>)) -> String {
    let (_, e) = encoded(text);
    let mut b = e.seed;
    edit(&mut b);
    rehash(&mut b);
    reopen(b, text).map_or_else(|e| e.to_string(), |_| "opened".to_owned())
}

#[test]
fn a_header_alone_is_read_and_anything_else_is_not_a_seed_file() {
    let mut h = vec![0_u8; HEADER];
    h[..4].copy_from_slice(&MAGIC);
    h[4..6].copy_from_slice(&VERSION.to_le_bytes());
    put32(&mut h, 24, HEADER);
    rehash(&mut h);
    assert_eq!(bundle_hash_of(&h).unwrap(), [0; 32], "exactly a header");
    let not_v2 = |b: &[u8]| matches!(bundle_hash_of(b), Err(BundleError::Parse(_)));
    assert!(not_v2(&h[..HEADER - 1]), "one byte short");
    assert!(not_v2(&MAGIC), "the magic alone");
    let mut other = h;
    other[0] = b'X';
    assert!(not_v2(&other), "a whole header with another magic");
}

#[test]
fn field_widths_fit_up_to_the_readers_limits() {
    assert!(placement_fits((0, 0, 0)) && placement_fits((32, 16, 3)));
    for w in [(33, 16, 3), (32, 17, 3), (32, 16, 4)] {
        assert!(!placement_fits(w), "{w:?}");
    }
    assert!(hint_fits((0, 0, 0, 0)) && hint_fits((16, 32, 24, 16)));
    for w in [(17, 32, 24, 16), (16, 33, 24, 16), (16, 32, 25, 16), (16, 32, 24, 17)] {
        assert!(!hint_fits(w), "{w:?}");
    }
}

#[test]
fn every_mode_and_kind_byte_reads_back() {
    for m in [Mode::Disabled, Mode::Enabled, Mode::Goal, Mode::Auto, Mode::AutoEnabled] {
        assert_eq!(byte_mode(mode_byte(m)), Some(m));
    }
    for k in [SlotKind::Spectator, SlotKind::Player, SlotKind::Group] {
        assert_eq!(byte_kind(kind_byte(k)), Some(k));
    }
    assert_eq!((byte_mode(5), byte_kind(3)), (None, None));
}

#[test]
fn the_seed_is_sized_from_its_counts_before_it_is_built() {
    let mut v = small();
    v["server_options"]["password"] = json!("pwd");
    v["slots"][0]["group_members"] = json!([1]);
    let text = v.to_string();
    let b = SeedBundle::from_json(&text).unwrap();
    let h = hash_of(&text);
    // Tables 96 + 36 + 32 + 2 * 72 + 3 * 16 = 356. Strings: "A" "B", "A" "B" "Bee", "s1",
    // "pwd": 12. Slot 1: one presence word 8, one 5-bit record 1, slot_data {"k":[1,2]} 11.
    // Slot 2: a word 8, two records 2, slot_data {} 2, one group member 4, three 1-bit start
    // items 1.
    let need: usize = 356 + 12 + (8 + 1 + 11) + (8 + 2 + 2 + 4 + 1);
    let err = encode(&b, &h, 0).unwrap_err().to_string();
    assert!(err.contains(&format!("would take {need} bytes")), "{err}");
    let e = encode(&b, &h, need as u64).unwrap();
    assert!(e.seed.len() <= need, "the bound holds: {}", e.seed.len());
    assert!(encode(&b, &h, need as u64 - 1).is_err());
}

#[test]
fn the_hint_block_size_is_its_byte_count() {
    assert_eq!(HintData::default().size(), 0);
    let h = HintData {
        // Slot positions up to 1 (1 bit) and location indexes up to 3 (2 bits): 6-bit hints.
        start: (0..8).map(|k| (k % 2, k / 2, k % 5)).collect(),
        spheres: vec![(4, vec![(0, 2), (3, 0)]), (4, vec![])],
        entrances: vec![vec![(1, 0), (2, 2)], vec![(3, 1)]],
        names: ["A", "B", "C"].map(String::from).to_vec(),
    };
    // Fixed 20 + 2 * 12 + 3 * 8 = 68; eight hints 6 bytes; slot 0's four 2-bit spheres 1 and
    // its two 4-bit entrances 1; slot 1's entrance 1.
    assert_eq!(h.size(), 68 + 6 + 1 + 1 + 1);
    assert_eq!(h.size(), h.bytes(0, &mut Strings::default()).unwrap().len());
}

/// Two team-1 slots of one game G (items and locations 1 to 4) that place every location, in
/// groups, flags up to 7, spheres and entrances (slot 1 only), eight start hints and options
/// at their edges: every field the reader decodes holds a value a wrong read would change.
fn rich() -> Value {
    let at = |l: i64, item: i64, to: u32, flags: u8| json!({"location": l, "item": item, "receiver": to, "flags": flags});
    let mut p1 = vec![at(1, 2, 2, 7), at(2, 1, 1, 0), at(3, 4, 2, 5), at(4, 3, 1, 2)];
    p1[2]["sphere"] = json!(1);
    p1[3]["sphere"] = json!(0);
    let p2 = vec![at(1, 4, 1, 1), at(2, 3, 2, 6), at(3, 2, 1, 3), at(4, 1, 2, 4)];
    let slot = |id: u32, members: Vec<u32>, locations: Vec<Value>| {
        json!({"id": id, "team": 1, "name": format!("P{id}"), "game": "G", "kind": "player",
               "minimum_client_version": {"major": 0, "minor": 1, "build": 6},
               "group_members": members, "locations": locations, "slot_data": {},
               "start_inventory": [4, 1]})
    };
    let mut one = slot(1, vec![2], p1);
    one["entrances"] = json!({"1": "Gate", "2": "Attic", "3": "Gate", "4": "Well"});
    let start: Vec<Value> = RICH_START
        .iter()
        .map(|&(f, l, s)| json!({"finding": f.0, "location": l, "status": s}))
        .collect();
    json!({
        "format": 1, "seed_name": "rich", "race_mode": 0,
        "generator_version": {"major": 1, "minor": 2, "build": 3},
        "server_options": {"password": null, "hint_cost": 100, "location_check_points": 2,
            "release_mode": "disabled", "collect_mode": "enabled", "remaining_mode": "goal"},
        "slots": [one, slot(2, vec![1, 2], p2)],
        "connect_names": {"P1": [1, 1], "P2": [1, 2]},
        "start_hints": start,
        "games": {"G": {"item_name_to_id": {"I1": 1, "I2": 2, "I3": 3, "I4": 4},
                        "location_name_to_id": {"L1": 1, "L2": 2, "L3": 3, "L4": 4},
                        "checksum": "r", "item_name_groups": {}, "location_name_groups": {}}}
    })
}

/// `rich`'s start hints: every (slot, location) once, statuses in no order.
const RICH_START: [(SlotNo, i64, u8); 8] = [
    (SlotNo(1), 1, 0),
    (SlotNo(2), 1, 10),
    (SlotNo(1), 2, 20),
    (SlotNo(2), 2, 30),
    (SlotNo(1), 3, 30),
    (SlotNo(2), 3, 20),
    (SlotNo(1), 4, 10),
    (SlotNo(2), 4, 0),
];

#[test]
fn every_field_of_a_rich_seed_reads_back() {
    let text = rich().to_string();
    let (seed, data) = opened(&text);
    assert_eq!(seed.to_v1(&data).unwrap(), SeedBundle::from_json(&text).unwrap());
    assert_eq!(seed.slot_nos().collect::<Vec<_>>(), [SlotNo(1), SlotNo(2)]);
    let debug = format!("Seed {{ seed_name: \"rich\", bytes: {}, .. }}", seed.bytes().len());
    assert_eq!(format!("{seed:?}"), debug);
    assert_eq!(seed.start_hints().collect::<Vec<_>>(), RICH_START);
    let (one, two) = (seed.slot(1).unwrap(), seed.slot(2).unwrap());
    assert_eq!((one.team(), one.group_members(), two.group_members()), (1, vec![2], vec![1, 2]));
    assert_eq!((one.sphere(1), one.sphere(3), one.sphere(4)), (None, Some(1), Some(0)));
    assert!((1..=4).all(|l| two.sphere(l).is_none()), "slot 2 records no spheres");
    let names: Vec<_> = (1..=4).map(|l| one.entrance(l)).collect();
    assert_eq!(names, [Some("Gate"), Some("Attic"), Some("Gate"), Some("Well")]);
    assert_eq!(two.entrance(1), None);
    assert_eq!((one.flags(0), one.flags(3), one.flags(4)), (Some(7), Some(2), None));
    assert_eq!(seed.ranks.len(), 2, "one rank entry per presence word");
}

#[test]
fn one_entrance_name_takes_no_bits() {
    let mut v = rich();
    v["slots"][0]["entrances"] = json!({"2": "Gate", "4": "Gate"});
    let text = v.to_string();
    let (seed, data) = opened(&text);
    assert_eq!(seed.hint_widths().2, 0);
    let one = seed.slot(1).unwrap();
    let names: Vec<_> = (1..=4).map(|l| one.entrance(l)).collect();
    assert_eq!(names, [None, Some("Gate"), None, Some("Gate")]);
    assert_eq!(seed.to_v1(&data).unwrap(), SeedBundle::from_json(&text).unwrap());
}

#[test]
fn start_hints_fill_their_bytes_and_one_more_is_refused() {
    // `rich` without spheres and entrances: the start hint bits end the data.
    let mut v = rich();
    for s in v["slots"].as_array_mut().unwrap() {
        s.as_object_mut().unwrap().remove("entrances");
        for p in s["locations"].as_array_mut().unwrap() {
            p.as_object_mut().unwrap().remove("sphere");
        }
    }
    let text = v.to_string();
    let (seed, _) = opened(&text);
    assert_eq!(seed.hint_widths(), (0, 2, 0, 1));
    assert_eq!(seed.start_hints().collect::<Vec<_>>(), RICH_START);
    // Eight 6-bit hints fill 6 bytes, right before the strings; a ninth needs a seventh.
    let err = refused(&text, |b| {
        let at = get32(b, 28);
        put32(b, at + 4, 9);
    });
    assert!(err.contains("start hints out of range"), "{err}");
}

#[test]
fn each_table_rule_refuses_with_its_own_reason() {
    let text = small().to_string();
    let (_, e) = encoded(&text);
    let (len, strings) = (e.seed.len(), get32(&e.seed, 20));
    let slot = |pos: usize| SLOT0 + SLOT * pos;
    let connect = slot(2);
    type Edit = Box<dyn Fn(&mut Vec<u8>)>;
    let cases: [(&str, Edit, &str); 14] = [
        ("strings past the end", Box::new(move |b| put32(b, 20, len + 1)), "overlap"),
        ("connect table past the strings", Box::new(|b| put32(b, 16, 1000)), "overlap"),
        ("strings at the very end", Box::new(move |b| put32(b, 20, len)), "room string"),
        ("seed name with no string", Box::new(|b| put32(b, HEADER + 4, NONE as usize)), "room"),
        ("seed name past the strings", Box::new(|b| put32(b, HEADER, 1 << 20)), "room string"),
        ("password past the strings", Box::new(|b| put32(b, HEADER + 8, 1 << 20)), "room"),
        ("no slots", Box::new(|b| b[10..12].copy_from_slice(&[0, 0])), "slots must be"),
        ("a repeated slot id", Box::new(move |b| put32(b, slot(1), 1)), "slots must be"),
        ("slot id 0", Box::new(move |b| put32(b, slot(0), 0)), "slots must be"),
        (
            "slot name with no string",
            Box::new(move |b| put32(b, slot(0) + 12, NONE as usize)),
            "slot 1 name",
        ),
        (
            "presence past the data",
            Box::new(move |b| put32(b, slot(0) + 32, NONE as usize)),
            "slot 1 presence",
        ),
        (
            "a location the game lacks",
            Box::new(move |b| {
                let p = get32(b, slot(0) + 32);
                b[p] |= 1 << 5;
            }),
            "its game lacks",
        ),
        (
            "records past the data",
            Box::new(move |b| put32(b, slot(1) + 40, strings - 1)),
            "slot 2 placements",
        ),
        (
            "connect name past the strings",
            Box::new(move |b| put32(b, connect + 4, 1000)),
            "connect name",
        ),
    ];
    for (name, edit, why) in cases {
        let err = refused(&text, edit);
        assert!(err.contains(why), "{name}: {err}");
    }
    // The strings may start right after the tables (an empty data area): not an overlap.
    let tables = connect + 3 * CONNECT;
    let err = refused(&text, move |b| put32(b, 20, tables));
    assert!(!err.contains("overlap"), "{err}");
}

#[test]
fn an_item_index_past_the_receivers_game_is_refused() {
    let text = cross_games().to_string();
    let (seed, _) = opened(&text);
    assert_eq!(seed.bit_widths(), (2, 1, 3));
    // Slot 1's second record (bits 6 to 11) sends A's a1 (index 0) to slot 1; A has 2 items,
    // so index 3 is past them.
    let err = refused(&text, |b| {
        let records = get32(b, HEADER + ROOM + 2 * GAME + 40);
        b[records] |= 0b1100_0000;
    });
    assert!(err.contains("slot 1 placement 1 is out of range"), "{err}");
}

#[test]
fn slot_data_may_inflate_to_exactly_the_cap() {
    let mut v = small();
    v["slots"][1]["slot_data"] = json!({"text": "x".repeat(4000)});
    let text = v.to_string();
    let (_, e) = encoded(&text);
    // {"text":"x...x"}: 4,011 bytes inflated.
    assert!(reopen_capped(e.seed.clone(), &text, 4011).is_ok());
    let err = reopen_capped(e.seed, &text, 4010).unwrap_err().to_string();
    assert!(err.contains("slot 1 slot_data"), "{err}");
}

#[test]
fn slot_data_that_deflate_does_not_shrink_is_kept_raw() {
    // The first such object (short text deflates to about its own length).
    let data = (0_u64..2000)
        .map(|n| json!({"k": format!("{:x}", n.wrapping_mul(0x9e37_79b9_7f4a_7c15))}))
        .find(|v| package::deflate(v.to_string().as_bytes()).unwrap().len() == v.to_string().len())
        .unwrap();
    let mut v = small();
    v["slots"][1]["slot_data"] = data.clone();
    let text = v.to_string();
    let (seed, _) = opened(&text);
    assert_eq!(get32(seed.bytes(), SLOT0 + 52), 0, "stored raw: no inflated length");
    assert_eq!(seed.slot(1).unwrap().slot_data().unwrap(), data);
}

#[test]
fn a_package_under_another_key_is_refused_even_with_the_right_data() {
    let (b, e) = encoded(&small().to_string());
    let other = Arc::new(GamePackage::new([3; 32], "G", &b.games["G"]));
    let err = Seed::open(e.seed, vec![other], 1 << 26).unwrap_err().to_string();
    assert!(err.contains("not the ones the seed names"), "{err}");
}

#[test]
fn each_hint_rule_refuses_with_its_own_reason() {
    let text = hinted().to_string();
    let (_, e) = encoded(&text);
    let strings = get32(&e.seed, 20);
    // The block's fixed part and two slot entries take 20 + 2 * 12 = 44 bytes; 43 are left.
    let err = refused(&text, |b| put32(b, 28, strings - 43));
    assert!(err.contains("hint block out of range"), "{err}");
    // Slot 1 places only location index 0. Its entrance list becomes one record read from
    // slot 2's presence byte 0b11: location index 1 (bit 0), entrance 1 (bit 1).
    let err = refused(&text, |b| {
        let (at, presence) = (get32(b, 28), get32(b, SLOT0 + SLOT + 32));
        put32(b, at + HINTS + 4, presence);
        put32(b, at + HINTS + 8, 1);
    });
    assert!(err.contains("slot 1 entrance 0 is out of range"), "{err}");
}

#[test]
fn a_group_member_that_is_no_slot_is_refused() {
    let text = rich().to_string();
    let err = refused(&text, |b| {
        let members = get32(b, SLOT0 + 56);
        put32(b, members, 9);
    });
    assert!(err.contains("slot 1 group members out of range"), "{err}");
}

/// Entrance names end at the table's count, even where the next bytes read as a name.
#[test]
fn no_entrance_name_past_the_table() {
    let (mut seed, _) = opened(&rich().to_string());
    let (count, table) = (seed.entrance_count(), get32(seed.bytes(), get32(seed.bytes(), 28) + 16));
    assert_eq!((count, seed.entrance_name(0)), (3, Some("Attic")));
    seed.bytes.copy_within(table..table + 8, table + 8 * count);
    assert_eq!(seed.entrance_name(count), None);
}

/// Fails without `encode_on`: a seed encoded on packages made earlier (once per world) is the
/// seed `encode` makes, and packages of other games than the bundle's are refused.
#[test]
fn encoding_on_packages_made_earlier_gives_the_same_seed() {
    let text = fixture_text();
    let b = SeedBundle::from_json(&text).unwrap();
    let hash = b.hash().unwrap();
    assert_eq!(hash.to_vec(), crate::bundle_hash(&text).unwrap(), "no text round trip needed");
    let whole = encode(&b, &hash, 1 << 24).unwrap();
    let tables: Vec<GamePackage> = b
        .games
        .iter()
        .zip(&whole.packages)
        .map(|((name, g), file)| GamePackage::new(package::key_of(file), name, g))
        .collect();
    assert_eq!(encode_on(&b, &hash, 1 << 24, &tables).unwrap(), whole.seed);
    assert!(encode_on(&b, &hash, 1 << 24, &tables[1..]).is_err(), "a game's package is missing");
    let other = GamePackage::new([0; 32], "Another Game", &GameData::default());
    let mut wrong = vec![other];
    wrong.extend(tables.into_iter().skip(1));
    assert!(encode_on(&b, &hash, 1 << 24, &wrong).is_err(), "not the bundle's game");
}
