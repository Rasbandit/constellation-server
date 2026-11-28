//! The fill's output as a seed: `data_fill::seed::generate` assembles a bundle that the v2
//! format carries back byte for byte (canonical JSON), with spheres and every item class.

use data_fill::Instance;
use data_fill::seed::{Player, generate};
use seed_bundle::{SeedBundle, bundle_hash, package, v2};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod support;

fn fixture() -> (Value, Vec<Instance>) {
    let doc = support::fixture("room_world");
    let (manifest, insts) = data_fill::room::from_doc(&doc).unwrap();
    let mut manifest = manifest.clone();
    manifest["game"] = json!("Fixture");
    manifest["version"] = json!("1");
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

fn make(rng: u64) -> SeedBundle {
    let (m, p) = players();
    generate(&m, "seed-one", &p, rng).unwrap()
}

#[test]
fn the_bundle_names_slots_games_spheres_and_every_item_class() {
    let b = make(7);
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
    let b = make(7);
    let text = serde_json::to_string(&b).unwrap();
    let hash: [u8; 32] = bundle_hash(&text).unwrap().try_into().unwrap();
    let enc = v2::encode(&b, &hash, data_fill::prepared::SEED_MAX_BYTES).unwrap();
    assert!(v2::is_v2(&enc.seed));
    let files: BTreeMap<_, _> =
        enc.packages.iter().map(|f| (package::key_of(f), f.clone())).collect();
    let back =
        v2::decode(enc.seed.clone(), |k| Ok(files[k].clone()), data_fill::prepared::SEED_MAX_BYTES)
            .unwrap();
    let canon = |b: &SeedBundle| package::canonical(serde_json::to_value(b).unwrap());
    assert_eq!(canon(&back), canon(&b));
    assert_eq!(bundle_hash(&serde_json::to_string(&back).unwrap()).unwrap(), hash);
}

#[test]
fn the_same_seed_gives_the_same_bundle_and_others_differ() {
    let canon = |b: &SeedBundle| package::canonical(serde_json::to_value(b).unwrap());
    assert_eq!(canon(&make(7)), canon(&make(7)));
    assert!((0..8).any(|r| canon(&make(r)) != canon(&make(7))));
}

#[test]
fn a_manifest_without_a_game_or_players_without_names_are_refused() {
    let (mut m, p) = players();
    let e = generate(&m, "s", &p, 1).map(|_| ());
    assert!(e.is_ok());
    m.as_object_mut().unwrap().remove("game");
    assert!(generate(&m, "s", &p, 1).unwrap_err().contains("game"));
    let (m, mut p) = players();
    p[1].name = "Ann".into();
    assert!(generate(&m, "s", &p, 1).unwrap_err().contains("repeats"));
    p[1].name = String::new();
    assert!(generate(&m, "s", &p, 1).unwrap_err().contains("name"));
    assert!(generate(&m, "s", &[], 1).is_err());
}

/// A stuck fill is a typed error naming the item and whose item it is, so a caller can say
/// it without reading text. Ann (player 0) holds the key in logic but her checks are all
/// excluded; Bo (player 1) reaches nothing without his key: Bo's key has nowhere to go.
#[test]
fn a_stuck_fill_names_the_item_and_its_player() {
    use data_fill::FillError;
    use data_fill::prepared::{Prepared, build};
    use data_fill::seed::{GenerateError, generate_on};
    let key = json!({"has": "key"});
    let locs: Vec<String> = (0..4).map(|l| format!("l{l}")).collect();
    let m = json!({
        "game": "Stuck", "version": "1",
        "options": [],
        "items": [{"key": "key", "id": 1}, {"key": "junk", "id": 2}],
        "locations": (0..4).map(|l| json!({"key": format!("l{l}"), "id": l + 1})).collect::<Vec<_>>(),
        "start": "a",
        "x.time": {"slices": 1, "start_mask": 1},
        "defs": {},
        "regions": [{"key": "a", "can_stay": false, "events": [], "stays": [],
                     "checks": locs.iter().map(|l| json!({"loc": l, "rule": key})).collect::<Vec<_>>(),
                     "exits": [{"to": "b", "rule": key}]},
                    {"key": "b", "can_stay": false, "checks": [], "exits": [], "events": [], "stays": []}],
        "goal": {"reach": "region:b"},
    });
    let class = [("key", "progression"), ("junk", "filler")]
        .map(|(k, c)| (k.to_owned(), c.to_owned()))
        .into_iter()
        .collect();
    let ann = Instance {
        locations: locs.clone(),
        excluded: locs.clone(),
        pool: vec!["junk".to_owned(); 4],
        pool_class: class,
        logic_start: [("key".to_owned(), 1)].into_iter().collect(),
        ..Instance::default()
    };
    let bo = Instance {
        pool: ["key", "junk", "junk", "junk"].map(String::from).to_vec(),
        excluded: Vec::new(),
        logic_start: Default::default(),
        ..ann.clone()
    };
    let p = vec![
        Player { name: "Ann".into(), instance: ann },
        Player { name: "Bo".into(), instance: bo },
    ];
    let on = Prepared::open(&build(&m).unwrap().0).unwrap();
    for seed in 1..=3 {
        match generate_on(&on, "s", &p, seed) {
            Err(GenerateError::Fill(FillError::Stuck { item, player, attempts })) => {
                assert_eq!(
                    (item.as_str(), player, attempts),
                    ("key", 1, config::FILL_MAX_ATTEMPTS)
                );
            }
            other => panic!("seed {seed}: {:?}", other.map(|x| x.1)),
        }
    }
    // As text, players count from 1 (Bo is the second).
    let text = String::from(generate_on(&on, "s", &p, 1).unwrap_err());
    assert!(
        text.contains("Stuck") && text.contains("\"key\"") && text.contains("player 2's item"),
        "{text}"
    );
}

/// The found record and the prepared world file of a manifest's bytes, as `job::prepare`
/// prints them.
fn prepare(manifest: &[u8]) -> Result<(Value, Vec<u8>), String> {
    let out = data_fill::job::prepare(manifest)?;
    let (found, file) = data_fill::job::unframe(&out)?;
    Ok((serde_json::from_slice(found).map_err(|e| e.to_string())?, file.to_vec()))
}

#[test]
fn the_child_process_jobs_give_the_same_bundle_and_check_a_manifest() {
    use data_fill::job::{Fill, fill, fill_reported, frame};
    let (m, p) = players();
    let direct = generate(&m, "seed-one", &p, 7).unwrap();
    let (found, file) = prepare(m.to_string().as_bytes()).unwrap();
    let game = data_fill::prepared::seed_game(&file).unwrap().to_vec();
    let bundle = |seed: Vec<u8>| {
        v2::decode(seed, |_| Ok(game.clone()), data_fill::prepared::SEED_MAX_BYTES).unwrap()
    };
    let head = Fill {
        seed_name: "seed-one".to_owned(),
        players: p.clone(),
        rng_seed: 7,
        seed_max_bytes: data_fill::prepared::SEED_MAX_BYTES,
    };
    let req = serde_json::to_value(&head).unwrap();
    let framed = |head: &Value| frame(head.to_string().as_bytes(), &file).unwrap();
    let out = fill(&framed(&req)).unwrap();
    let back: SeedBundle = bundle(out.clone());
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&direct).unwrap());
    assert!(fill(&frame(b"{", &file).unwrap()).unwrap_err().contains("bad fill request"));
    // A field the request does not know is refused: the fill mode it once named too.
    let mut asked = req.clone();
    asked["algorithm"] = json!("exact");
    assert!(fill(&framed(&asked)).unwrap_err().contains("bad fill request"));
    // The job says how the fill went, since the bundle cannot: what the library says.
    let on = data_fill::prepared::Prepared::open(&file).unwrap();
    let (_, report) = data_fill::seed::generate_on(&on, "seed-one", &p, 7).unwrap();
    assert_eq!(fill_reported(&framed(&req)).unwrap().1, report);

    assert_eq!(
        (&found["game"], &found["version"], &found["items"]),
        (&json!("Fixture"), &json!("1"), &json!(4))
    );
    let mut padded = m.clone();
    padded["game"] = json!("  Fixture ");
    assert_eq!(prepare(padded.to_string().as_bytes()).unwrap().0["game"], "Fixture");
    padded.as_object_mut().unwrap().remove("version");
    assert!(prepare(padded.to_string().as_bytes()).unwrap_err().contains("version"));
    assert!(prepare(b"[]").is_err());
}

#[test]
fn a_game_or_version_that_is_long_or_holds_control_characters_is_refused() {
    let (m, _) = players();
    let check = |key: &str, value: &str| {
        let mut m = m.clone();
        m[key] = json!(value);
        data_fill::job::prepare(m.to_string().as_bytes()).map(|_| ())
    };
    for key in ["game", "version"] {
        assert!(
            check(key, &"x".repeat(config::MANIFEST_NAME_MAX_BYTES)).is_ok(),
            "{key} at the cap"
        );
        let long = check(key, &"x".repeat(config::MANIFEST_NAME_MAX_BYTES + 1)).unwrap_err();
        assert!(long.contains("longer than"), "{key}: {long}");
        for bad in ["a\u{0}b", "a\nb", "\u{7f}"] {
            assert!(check(key, bad).unwrap_err().contains("control"), "{key} {bad:?}");
        }
    }
}

/// The fill report says what the round procedure did, and for a fill that needed repairs which
/// worlds leave fewer places early on than progression to seat (early capacity ratio under 1):
/// rooms of tight synthetic worlds (as much progression as locations that take it) that were
/// repaired flag every player, the others are not read; light rooms need no repair.
#[test]
fn the_fill_report_flags_worlds_with_little_room_early_on() {
    use data_fill::prepared::{Prepared, build};
    use data_fill::seed::{FillReport, generate_on};
    use data_fill::synth::{Hostile, Shape, players, uploaded};
    let report = |shape: &Shape, n: u32, seed: u64| -> FillReport {
        let (m, inst) = uploaded(shape, 23, 0);
        let players = players(&inst, n as usize);
        let on = Prepared::open(&build(&m).unwrap().0).unwrap();
        generate_on(&on, "s", &players, seed).unwrap().1
    };
    let tight = Shape { checks: 200, ..Shape::hostile(Hostile::Tight) };
    let mut repaired = 0;
    for (n, seed) in [(1, 1), (1, 2), (1, 3), (3, 1), (3, 2), (8, 1), (8, 2)] {
        let r = report(&tight, n, seed);
        if r.divided > 0 || r.swaps > 0 || r.attempts > 1 {
            repaired += 1;
            assert_eq!(r.tight_players, n, "{r:?}");
            assert!(r.capacity_milli < 1000, "{r:?}");
        } else {
            assert_eq!((r.tight_players, r.capacity_milli), (0, u32::MAX), "{r:?}");
        }
    }
    assert!(repaired > 0, "no tight room needed a repair");
    let light = report(&Shape { checks: 200, ..Shape::default() }, 3, 1);
    assert_eq!((light.tight_players, light.capacity_milli, light.attempts), (0, u32::MAX, 1));
}
