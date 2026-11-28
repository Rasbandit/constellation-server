// Hand-written bundles; the expected placement is read off the JSON by eye.
use super::*;
use serde_json::{Value, json};

fn good() -> Value {
    json!({
        "format": 1,
        "seed_name": "s1",
        "generator_version": {"major": 0, "minor": 6, "build": 8},
        "server_options": {
            "password": null, "hint_cost": 10, "location_check_points": 1,
            "release_mode": "auto", "collect_mode": "auto-enabled", "remaining_mode": "goal"
        },
        "slots": [
            {"id": 1, "team": 0, "name": "A", "game": "G", "kind": "player",
             "minimum_client_version": {"major": 0, "minor": 1, "build": 6},
             "locations": [{"location": 100, "item": 7, "receiver": 2, "flags": 1}],
             "slot_data": {"k": [1, 2]}},
            {"id": 2, "team": 0, "name": "B", "game": "G", "kind": "player",
             "minimum_client_version": {"major": 0, "minor": 1, "build": 6},
             "locations": [{"location": 100, "item": 8, "receiver": 2, "flags": 0},
                           {"location": 101, "item": 7, "receiver": 1, "flags": 4}],
             "slot_data": {}}
        ],
        "connect_names": {"A": [0, 1], "B": [0, 2]},
        "games": {"G": {
            "item_name_to_id": {"Seven": 7, "Eight": 8},
            "location_name_to_id": {"L100": 100, "L101": 101},
            "checksum": "abc"
        }}
    })
}

fn parse(v: &Value) -> Result<SeedBundle, BundleError> {
    SeedBundle::from_json(&v.to_string())
}

fn refused(edit: impl FnOnce(&mut Value)) -> String {
    let mut v = good();
    edit(&mut v);
    match parse(&v) {
        Err(BundleError::Invalid(m) | BundleError::Parse(m)) => m,
        Ok(_) => panic!("accepted {v}"),
    }
}

#[test]
fn a_good_bundle_loads_and_gives_a_per_slot_placement() {
    let b = parse(&good()).unwrap();
    assert_eq!(b.server_options.collect_mode, Mode::AutoEnabled);
    let p = b.placement();
    assert_eq!(p[&SlotNo(1)][&100], (SlotNo(2), 7));
    assert_eq!(p[&SlotNo(2)][&100], (SlotNo(2), 8));
    assert_eq!(p[&SlotNo(2)][&101], (SlotNo(1), 7));
    assert_eq!(b.slot_nos().collect::<Vec<_>>(), [SlotNo(1), SlotNo(2)]);
}

#[test]
fn a_bundle_survives_a_round_trip() {
    let b = parse(&good()).unwrap();
    assert_eq!(SeedBundle::from_json(&serde_json::to_string(&b).unwrap()).unwrap(), b);
}

#[test]
fn garbage_truncated_and_empty_input_are_parse_errors() {
    for text in ["", "{", "[]", "null", good().to_string().get(..40).unwrap()] {
        assert!(matches!(SeedBundle::from_json(text), Err(BundleError::Parse(_))), "{text:?}");
    }
}

#[test]
fn unknown_fields_are_refused_so_schema_drift_is_loud() {
    assert!(refused(|v| v["extra"] = json!(1)).contains("unknown field"));
}

#[test]
fn every_broken_rule_is_named() {
    type Edit = fn(&mut Value);
    let cases: [(&str, Edit); 17] = [
        ("format", |v| v["format"] = json!(2)),
        ("no slots", |v| v["slots"] = json!([])),
        ("hint_cost", |v| v["server_options"]["hint_cost"] = json!(101)),
        ("remaining_mode", |v| v["server_options"]["remaining_mode"] = json!("auto")),
        ("reserved", |v| v["slots"][0]["id"] = json!(0)),
        ("id 2 repeats", |v| v["slots"][0]["id"] = json!(2)),
        ("name", |v| v["slots"][1]["name"] = json!("A")),
        ("unknown game", |v| v["slots"][0]["game"] = json!("H")),
        ("slot_data", |v| v["slots"][0]["slot_data"] = json!([1])),
        ("repeats", |v| v["slots"][1]["locations"][1]["location"] = json!(100)),
        ("not in game", |v| v["slots"][0]["locations"][0]["location"] = json!(999)),
        ("flags", |v| v["slots"][0]["locations"][0]["flags"] = json!(8)),
        ("missing slot 9", |v| v["slots"][0]["locations"][0]["receiver"] = json!(9)),
        ("lacks", |v| v["slots"][0]["locations"][0]["item"] = json!(9)),
        ("connect name", |v| v["connect_names"]["C"] = json!([1, 1])),
        ("2^53", |v| v["games"]["G"]["item_name_to_id"]["Big"] = json!(1_i64 << 53)),
        ("starts with item 9", |v| v["slots"][0]["start_inventory"] = json!([7, 9])),
    ];
    for (want, edit) in cases {
        let msg = refused(edit);
        assert!(msg.contains(want), "{want:?} not in {msg:?}");
    }
}

#[test]
fn ids_at_the_documented_limit_are_accepted() {
    let mut v = good();
    v["games"]["G"]["item_name_to_id"]["Max"] = json!(MAX_ID);
    v["games"]["G"]["location_name_to_id"]["Min"] = json!(-MAX_ID);
    assert!(parse(&v).is_ok());
}

#[test]
fn group_slots_are_refused_until_supported() {
    let msg = refused(|v| v["slots"][1]["kind"] = json!("group"));
    assert!(msg.contains("group slot") && msg.contains("not supported"), "{msg}");
}

#[test]
fn race_mode_defaults_to_zero_and_is_kept() {
    assert_eq!(parse(&good()).unwrap().race_mode, 0);
    let mut v = good();
    v["race_mode"] = json!(1);
    assert_eq!(parse(&v).unwrap().race_mode, 1);
}

/// The importer's real output (`tools/ap-import`) must satisfy this schema: the cross-language
/// contract. Counts were read off the generator's spoiler log for the fixture seed.
#[test]
fn the_imported_fixture_bundle_is_valid() {
    let b = SeedBundle::from_json(include_str!("../../e2e/fixtures/two-slot.json")).unwrap();
    let names: Vec<_> =
        b.slots.iter().map(|s| (s.id, s.name.as_str(), s.locations.len())).collect();
    assert_eq!(names, [(1, "Alice", 15), (2, "Bob", 15)]);
    assert_eq!(b.server_options.password, None);
    let cross = b.slots[0].locations.iter().filter(|p| p.receiver == 2).count();
    assert!(cross > 0, "the fixture must send items across slots");
}

/// Protocol doc, "Data Package Contents": start items come from location -2 and player 0, so
/// they are item ids of the slot's own game, repeats allowed (two copies of one item).
#[test]
fn a_starting_inventory_is_kept_in_order_with_repeats() {
    let mut v = good();
    v["slots"][0]["start_inventory"] = json!([8, 7, 8]);
    let b = parse(&v).unwrap();
    assert_eq!(b.slots[0].start_inventory, [8, 7, 8]);
    assert!(b.slots[1].start_inventory.is_empty(), "absent means none");
}

/// The name groups a client reads as `_read_item_name_groups_<game>` and
/// `_read_location_name_groups_<game>` (protocol doc, "Get"), kept as given; absent is empty.
#[test]
fn name_groups_are_kept_per_game_and_default_to_empty() {
    let mut v = good();
    v["games"]["G"]["item_name_groups"] = json!({"Both": ["Seven", "Eight"]});
    v["games"]["G"]["location_name_groups"] = json!({"First": ["L100"]});
    let b = parse(&v).unwrap();
    let g = &b.games["G"];
    assert_eq!(g.item_name_groups["Both"], ["Seven", "Eight"]);
    assert_eq!(g.location_name_groups["First"], ["L100"]);
    assert!(parse(&good()).unwrap().games["G"].item_name_groups.is_empty());
}

/// The generic `Archipelago` game names location -2 and player 0 for start items; a bundle
/// may carry its package although no slot plays it.
#[test]
fn a_game_no_slot_plays_is_accepted() {
    let mut v = good();
    v["games"]["Archipelago"] = json!({"item_name_to_id": {"Nothing": -1},
        "location_name_to_id": {"Cheat Console": -1, "Server": -2}, "checksum": "ap"});
    assert!(parse(&v).unwrap().games.contains_key("Archipelago"));
}

#[test]
fn bundle_errors_display_which_check_failed() {
    assert_eq!(BundleError::Parse("eof".into()).to_string(), "seed bundle does not parse: eof");
    assert_eq!(BundleError::Invalid("x".into()).to_string(), "seed bundle is invalid: x");
}

#[test]
fn start_hints_and_the_hint_economy_are_kept() {
    let mut v = good();
    // B's location 101 holds A's item 7; A asked for it at generation (a start hint).
    v["start_hints"] = json!([{"finding": 2, "location": 101, "status": 30}]);
    let (seed, _) = import_v1(v.to_string().as_bytes(), 1 << 26).unwrap();
    assert_eq!(seed.start_hints().collect::<Vec<_>>(), [(SlotNo(2), 101, 30)]);
    assert_eq!((seed.hint_cost(), seed.location_check_points()), (10, 1));
    assert_eq!(parse(&good()).unwrap().start_hints, [], "absent means none");
}

#[test]
fn entrances_spheres_and_hint_blacklists_are_kept_and_default_to_empty() {
    let mut v = good();
    v["slots"][1]["entrances"] = json!({"101": "Cave"});
    v["slots"][1]["locations"][1]["sphere"] = json!(2);
    v["games"]["G"]["hint_blacklist"] = json!(["Eight"]);
    let b = parse(&v).unwrap();
    let bslot = b.slot(2).unwrap();
    assert_eq!(bslot.entrances.get(&101).map(String::as_str), Some("Cave"));
    assert_eq!((bslot.locations[0].sphere, bslot.locations[1].sphere), (None, Some(2)));
    assert!(b.games["G"].hint_blacklist.contains("Eight"));
    let plain = parse(&good()).unwrap();
    assert_eq!(plain.slot(2).unwrap().entrances.len(), 0);
    assert_eq!(plain.games["G"].hint_blacklist.len(), 0);
}

#[test]
fn hint_data_outside_a_world_or_with_a_bad_status_is_refused() {
    let m = refused(|v| v["start_hints"] = json!([{"finding": 1, "location": 101, "status": 0}]));
    assert!(m.contains("start hint"), "{m}");
    let m = refused(|v| v["start_hints"] = json!([{"finding": 9, "location": 100, "status": 0}]));
    assert!(m.contains("start hint"), "{m}");
    let m = refused(|v| v["start_hints"] = json!([{"finding": 2, "location": 101, "status": 5}]));
    assert!(m.contains("status 5"), "{m}");
    let m = refused(|v| v["slots"][0]["entrances"] = json!({"101": "Cave"}));
    assert!(m.contains("entrance"), "{m}");
}

/// What `validate` reads through its slot index (a receiver, a hint's slot, a connect name, an
/// entrance, a group member) is refused as a scan of the slots refused it, in the same words.
#[test]
fn what_a_slot_or_location_lookup_misses_is_refused_in_the_same_words() {
    type Edit = fn(&mut Value);
    let cases: [(&str, Edit); 12] = [
        ("slot name \"A\" repeats", |v| v["slots"][1]["name"] = json!("A")),
        ("slot 1 location 100 sends to missing slot 0", |v| {
            v["slots"][0]["locations"][0]["receiver"] = json!(0);
        }),
        ("slot 2 location 101 sends to missing slot 4294967295", |v| {
            v["slots"][1]["locations"][1]["receiver"] = json!(u32::MAX);
        }),
        ("slot 1 location 102 is not in game \"G\"", |v| {
            v["slots"][0]["locations"][0]["location"] = json!(102);
        }),
        ("group 1 names missing slot 3", |v| v["slots"][0]["group_members"] = json!([2, 3])),
        ("connect name \"C\" points at no slot", |v| v["connect_names"]["C"] = json!([0, 3])),
        ("connect name \"A\" points at no slot", |v| v["connect_names"]["A"] = json!([1, 1])),
        ("start hint for slot 3 location 100 is outside that world", |v| {
            v["start_hints"] = json!([{"finding": 3, "location": 100, "status": 0}]);
        }),
        ("slot 2 entrance for location 102, which its world lacks", |v| {
            v["slots"][1]["entrances"] = json!({"100": "Cave", "102": "Well"});
        }),
        // The receiver's game is what decides: item 8 is G's, and slot 2 now plays H.
        ("slot 1 location 100 places item 8 that slot 2's game lacks", |v| {
            v["games"]["H"] = json!({"item_name_to_id": {"Seven": 7}, "checksum": "h",
                                     "location_name_to_id": {"L100": 100, "L101": 101}});
            v["slots"][1]["game"] = json!("H");
            v["slots"][0]["locations"][0]["item"] = json!(8);
        }),
        // The first broken rule in file order is the one named: a location outside the game
        // before a later repeat, and a repeat before a later bad receiver.
        ("slot 2 location 999 is not in game \"G\"", |v| {
            v["slots"][1]["locations"] = json!([
                {"location": 999, "item": 7, "receiver": 1, "flags": 0},
                {"location": 100, "item": 7, "receiver": 1, "flags": 0},
                {"location": 100, "item": 7, "receiver": 1, "flags": 0}]);
        }),
        ("slot 2 location 100 repeats", |v| {
            v["slots"][1]["locations"] = json!([
                {"location": 100, "item": 7, "receiver": 1, "flags": 0},
                {"location": 100, "item": 7, "receiver": 1, "flags": 0},
                {"location": 101, "item": 7, "receiver": 9, "flags": 0}]);
        }),
    ];
    for (want, edit) in cases {
        assert_eq!(refused(edit), want);
    }
}

/// Slots in any order and with gaps between their ids are found by id.
#[test]
fn slots_out_of_order_with_gaps_in_their_ids_are_valid() {
    let mut v = good();
    v["slots"].as_array_mut().unwrap().reverse();
    v["slots"][0]["id"] = json!(40);
    v["slots"][1]["locations"][0]["receiver"] = json!(40);
    v["slots"][0]["locations"] = json!([{"location": 101, "item": 8, "receiver": 40, "flags": 0}]);
    v["connect_names"] = json!({"A": [0, 1], "B": [0, 40]});
    v["start_hints"] = json!([{"finding": 40, "location": 101, "status": 0}]);
    let b = parse(&v).unwrap();
    assert_eq!(b.slot(40).map(|s| s.name.as_str()), Some("B"));
    assert!(import_v1(v.to_string().as_bytes(), 1 << 26).is_ok());
}
