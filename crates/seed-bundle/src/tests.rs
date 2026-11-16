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
    let cases: [(&str, Edit); 16] = [
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
fn a_group_must_name_real_slots() {
    assert!(refused(|v| v["slots"][0]["group_members"] = json!([5])).contains("missing slot 5"));
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
