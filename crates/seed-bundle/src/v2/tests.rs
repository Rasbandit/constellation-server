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
    let e = encode(&b, &hash_of(text)).unwrap();
    (b, e)
}

fn opened(text: &str) -> (Seed, BTreeMap<Key, GameData>) {
    let (b, e) = encoded(text);
    let mut data = BTreeMap::new();
    let games = e
        .packages
        .iter()
        .map(|c| {
            let key = GamePackage::key_of(c);
            let (name, g) = GamePackage::parse(c).unwrap();
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
        .map(|c| {
            let (name, g) = GamePackage::parse(c).unwrap();
            Arc::new(GamePackage::new(GamePackage::key_of(c), &name, &g))
        })
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
fn a_seed_written_to_disk_reads_back_through_load() {
    let text = fixture_text();
    let dir = std::env::temp_dir().join(format!("v2-load-{}", std::process::id()));
    let (_, e) = encoded(&text);
    for c in &e.packages {
        package::write(&dir.join("packages"), c).unwrap();
    }
    let from_disk = crate::load(e.seed.clone(), &dir, 1 << 26).unwrap();
    let from_json = crate::load(text.into_bytes(), &dir, 1 << 26).unwrap();
    assert_eq!(from_disk.bytes(), from_json.bytes());
    // Both share this process's one copy of each package.
    assert!(Arc::ptr_eq(&from_disk.games()[0], &from_json.games()[0]));
    // While a room holds the packages, a seed needs no package files; once none does, it does.
    let empty_dir = std::env::temp_dir().join(format!("v2-none-{}", std::process::id()));
    assert!(crate::load(e.seed.clone(), &empty_dir, 1 << 26).is_ok());
    drop((from_disk, from_json));
    let empty = std::env::temp_dir().join(format!("v2-none-{}", std::process::id()));
    let missing = crate::load(e.seed.clone(), &empty, 1 << 26).unwrap_err();
    assert!(missing.contains(".pkg"), "{missing}");
    assert!(crate::load(b"{not json".to_vec(), &dir, 1 << 26).is_err());
    let back = decode(e.seed, &dir.join("packages"), 1 << 26).unwrap();
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
