//! Generic shuffles, first slice (design doc 17 of the docs repo): a `none` shuffle whose
//! result rules read, and the `layout` entrance shuffle with coupled doors, one-way exits and
//! type groups, staged before the exact fill.

use std::collections::{BTreeMap, BTreeSet};

use data_fill::seed::{Player, generate};
use data_fill::shuffle::{Staged, stage};
use data_fill::synth::{Shape, world as synth_world};
use data_fill::{Instance, World, decode};
use serde_json::{Value, json};

fn t() -> Value {
    json!({"true": {}})
}

fn has(item: &str) -> Value {
    json!({"has": item})
}

/// A region with one always-open check `c_<key>` per name in `checks`.
fn region(key: &str, checks: &[&str], exits: Value) -> Value {
    let checks: Vec<Value> = checks.iter().map(|c| json!({"loc": c, "rule": t()})).collect();
    json!({"key": key, "can_stay": false, "checks": checks, "exits": exits, "events": [], "stays": []})
}

/// Both sides of a two-way door named after its outside exit.
fn door(key: &str, to: &str, group: &str, rule: Value) -> Value {
    json!({"to": to, "rule": rule, "key": key, "door": key, "group": group})
}

fn back(door: &str, to: &str) -> Value {
    json!({"to": to, "rule": t(), "key": format!("{door}_back"), "door": door})
}

/// A manifest of `regions` with one location per check, the items `lantern`, `sword` and
/// `junk`, and an instance whose pool fills every location.
fn manifest(regions: Vec<Value>, shuffles: Value) -> (Value, Instance) {
    let locs: Vec<String> = regions
        .iter()
        .flat_map(|r| r["checks"].as_array().unwrap().iter())
        .map(|c| c["loc"].as_str().unwrap().to_owned())
        .collect();
    let m = json!({
        "game": "Doors", "version": "1",
        "options": [],
        "items": [{"key": "lantern", "id": 1}, {"key": "sword", "id": 2}, {"key": "junk", "id": 3}],
        "locations": locs.iter().enumerate().map(|(i, k)| json!({"key": k, "id": i + 1})).collect::<Vec<_>>(),
        "start": "village",
        "x.time": {"slices": 1, "start_mask": 1},
        "defs": {},
        "regions": regions,
        "goal": {"reach": "region:shrine"},
        "shuffles": shuffles,
    });
    let mut pool = vec!["lantern".to_owned(), "sword".to_owned()];
    pool.resize(locs.len(), "junk".to_owned());
    let pool_class = [("lantern", "progression"), ("sword", "progression"), ("junk", "filler")]
        .map(|(k, c)| (k.to_owned(), c.to_owned()))
        .into();
    (m, Instance { locations: locs, pool, pool_class, ..Instance::default() })
}

const SLOTS: [&str; 7] =
    ["cave_mouth", "mill_door", "deep_door", "tower_gate", "crypt_gate", "well_drop", "chute"];

/// A village with two cave doors and two dungeon doors, a door deeper in the cave (so the cave
/// is a branch and the mill a dead end), an attic fixed behind the tower, and two one-way
/// drops whose landings lead back to the village by fixed exits.
fn doors_regions() -> Vec<Value> {
    vec![
        region(
            "village",
            &["c_village", "c_square", "c_well", "c_gate"],
            json!([
                door("cave_mouth", "cave", "cave", has("lantern")),
                door("mill_door", "mill", "cave", t()),
                door("tower_gate", "tower", "dungeon", t()),
                door("crypt_gate", "crypt", "dungeon", has("sword")),
                {"to": "pond", "rule": t(), "key": "well_drop", "group": "drop"},
            ]),
        ),
        region(
            "cave",
            &["c_cave"],
            json!([back("cave_mouth", "village"), door("deep_door", "depths", "cave", t())]),
        ),
        region("depths", &["c_depths"], json!([back("deep_door", "cave")])),
        region("mill", &["c_mill"], json!([back("mill_door", "village")])),
        region(
            "tower",
            &["c_tower"],
            json!([
                back("tower_gate", "village"),
                {"to": "attic", "rule": t()},
                {"to": "shrine", "rule": t(), "key": "chute", "group": "drop"},
            ]),
        ),
        region("attic", &["c_attic"], json!([{"to": "tower", "rule": t()}])),
        region("crypt", &["c_crypt"], json!([back("crypt_gate", "village")])),
        region("pond", &["c_pond"], json!([{"to": "village", "rule": t()}])),
        region("shrine", &["c_shrine"], json!([{"to": "village", "rule": t()}])),
    ]
}

fn entrances() -> Value {
    json!({"entrances": {
        "check": "layout",
        "coupled": true,
        "slots": {"type": "exit", "names": SLOTS},
        "allow": [
            {"slots": {"groups": ["cave"]}, "only": {"groups": ["cave"]}},
            {"slots": {"groups": ["dungeon"]}, "only": {"groups": ["dungeon"]}},
        ],
    }})
}

fn doors() -> (Value, Instance) {
    manifest(doors_regions(), entrances())
}

fn staged(w: &World, inst: &Instance, seed: u64) -> Staged {
    stage(w, inst, seed, 0).unwrap().expect("the world declares shuffles")
}

/// Where every keyed exit of manifest `m` really leads in `w`: exit key to (from, to).
fn graph(m: &Value, w: &World) -> BTreeMap<String, (String, String)> {
    let real = decode::manifest(w);
    let mut out = BTreeMap::new();
    for (r, region) in m["regions"].as_array().unwrap().iter().enumerate() {
        for (i, e) in region["exits"].as_array().unwrap().iter().enumerate() {
            if let Some(key) = e["key"].as_str() {
                let to = real["regions"][r]["exits"][i]["to"].as_str().unwrap().to_owned();
                out.insert(key.to_owned(), (region["key"].as_str().unwrap().to_owned(), to));
            }
        }
    }
    out
}

/// Every region and every location is reached holding the whole pool, and the goal holds.
fn assert_whole_world(s: &Staged, inst: &Instance, what: &str) {
    let w = &s.world;
    let held: Vec<u16> = inst.pool.iter().map(|k| w.item(k).unwrap()).collect();
    let r = s.plan.solve(w, &held);
    assert!(r.masks.iter().all(|m| *m != 0), "{what}: a region is not reached");
    assert!(s.plan.included.iter().all(|l| r.reached(*l)), "{what}: a check is not reached");
    assert!(r.goal(), "{what}: the goal does not hold");
}

#[test]
fn the_same_seed_gives_the_same_layout_and_other_seeds_differ() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    let mut seen = BTreeSet::new();
    for seed in 1..=40 {
        let (a, b) = (staged(&w, &inst, seed), staged(&w, &inst, seed));
        assert_eq!(a.table, b.table, "seed {seed}");
        assert_eq!(decode::manifest(&a.world), decode::manifest(&b.world), "seed {seed}");
        assert_eq!(a.table["entrances"].len(), 12, "7 slots and the 5 back sides that follow");
        seen.insert(format!("{:?}", a.table));
    }
    assert!(seen.len() > 5, "40 seeds gave {} layouts", seen.len());
    // Another player of the same room draws from another stream.
    let other = |p| stage(&w, &inst, 3, p).unwrap().unwrap().table;
    assert!((1..20).any(|p| other(p) != other(0)));
}

#[test]
fn every_shuffled_layout_passes_the_whole_world_check() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    for seed in 1..=300 {
        let s = staged(&w, &inst, seed);
        assert!(s.attempts <= config::SHUFFLE_MAX_ATTEMPTS);
        assert_whole_world(&s, &inst, &format!("seed {seed}"));
    }
}

#[test]
fn coupled_doors_lead_back_through_the_door_they_were_entered_by() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    let vanilla = graph(&m, &w);
    let two_way = |k: &str| vanilla.contains_key(&format!("{k}_back"));
    for seed in 1..=200 {
        let s = staged(&w, &inst, seed);
        let (real, table) = (graph(&m, &s.world), &s.table["entrances"]);
        for slot in SLOTS {
            let thing = &table[slot];
            // The table says what the world does: the slot leads where its thing led.
            assert_eq!(real[slot].1, vanilla[thing].1, "seed {seed}, {slot}");
            assert_eq!(real[slot].0, vanilla[slot].0, "an exit never moves, seed {seed}");
            if two_way(slot) {
                // Walking back out of where the slot leads arrives where the slot is.
                let (back_thing, back_slot) = (format!("{thing}_back"), format!("{slot}_back"));
                assert_eq!(real[&back_thing], (real[slot].1.clone(), real[slot].0.clone()));
                assert_eq!(table[&back_thing], back_slot, "seed {seed}, {slot}");
            }
        }
    }
}

#[test]
fn type_groups_and_sides_are_respected_and_every_slot_really_moves() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    let group = |k: &str| match k {
        "cave_mouth" | "mill_door" | "deep_door" => "cave",
        "tower_gate" | "crypt_gate" => "dungeon",
        _ => "drop",
    };
    let mut got: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for seed in 1..=200 {
        let s = staged(&w, &inst, seed);
        let things: BTreeSet<&String> = SLOTS.iter().map(|k| &s.table["entrances"][*k]).collect();
        assert_eq!(things.len(), SLOTS.len(), "seed {seed}: every thing is used once");
        for slot in SLOTS {
            let thing = &s.table["entrances"][slot];
            assert_eq!(group(slot), group(thing), "seed {seed}: {slot} got {thing}");
            got.entry(slot).or_default().insert(thing.clone());
        }
    }
    // A one-way drop takes only a one-way landing: `drop` is no row of the table.
    assert!(SLOTS.iter().all(|k| got[k].len() > 1), "a slot never moved: {got:?}");
}

/// One exit out of the start, a hub with three exits behind one thing and leaves behind the
/// others: a leaf placed first would leave no exit to go on from.
fn tree() -> (Value, Instance) {
    let leaf = |k: &str| region(k, &[], json!([back(&format!("to_{k}"), "hub")]));
    let regions = vec![
        region("village", &["c1", "c2", "c3"], json!([door("to_hub", "hub", "g", t())])),
        region(
            "hub",
            &[],
            json!([
                back("to_hub", "village"),
                door("to_a", "a", "g", t()),
                door("to_b", "b", "g", t()),
                door("to_shrine", "shrine", "g", t()),
            ]),
        ),
        leaf("a"),
        leaf("b"),
        leaf("shrine"),
    ];
    let names = ["to_hub", "to_a", "to_b", "to_shrine"];
    let shuffles =
        json!({"entrances": {"coupled": true, "slots": {"type": "exit", "names": names}}});
    manifest(regions, shuffles)
}

#[test]
fn dead_ends_are_placed_after_branches_so_no_attempt_is_wasted() {
    let (m, inst) = tree();
    let w = World::load(&m).unwrap();
    for seed in 1..=200 {
        let s = staged(&w, &inst, seed);
        assert_eq!(s.attempts, 1, "seed {seed}");
        assert_eq!(s.table["entrances"]["to_hub"], "to_hub", "the only branch, seed {seed}");
        assert_whole_world(&s, &inst, &format!("seed {seed}"));
    }
}

#[test]
fn a_world_that_can_never_pass_the_check_errors_at_the_cap() {
    let mut regions = doors_regions();
    regions.push(region("island", &["c_island"], json!([])));
    let (m, inst) = manifest(regions, entrances());
    let w = World::load(&m).unwrap();
    let e = stage(&w, &inst, 1, 0).unwrap_err();
    let cap = format!("{} attempts", config::SHUFFLE_MAX_ATTEMPTS);
    assert!(e.contains(&cap) && e.contains("entrances") && e.contains("island"), "{e}");
    // Through the seed path the error also names the player and the world.
    let p = [Player { name: "Ann".into(), instance: inst }];
    let e = generate(&m, "s", &p, 1, 1).unwrap_err();
    assert!(e.contains("player 1") && e.contains("Doors") && e.contains("entrances"), "{e}");
}

/// Three halls, three guardians; the large one may not stand in the spire.
fn guardians() -> Value {
    json!({"guardians": {
        "check": "none",
        "slots": {"type": "name", "names": ["marsh_hall", "spire_hall", "vault_hall"]},
        "things": {
            "golem": {"rule": has("sword")},
            "hydra": {"rule": {"all": [has("sword"), has("lantern")]}, "group": "large"},
            "wisp": {"rule": has("lantern")},
        },
        "allow": [{"slots": {"names": ["spire_hall"]}, "deny": {"groups": ["large"]}}],
    }})
}

fn halls(shuffles: Value) -> (Value, Instance) {
    let at = json!({"at": ["guardians", "spire_hall"]});
    let mut spire = region("shrine", &[], json!([]));
    spire["checks"] = json!([
        {"loc": "spire_prize", "rule": at},
        {"loc": "echo_stone", "rule": {"all": [at, {"ne": [at, "wisp"]}]}},
        {"loc": "wisp_mark", "rule": {"not": {"ne": [at, "wisp"]}}},
    ]);
    let regions =
        vec![region("village", &["c1", "c2", "c3"], json!([{"to": "shrine", "rule": t()}])), spire];
    manifest(regions, shuffles)
}

#[test]
fn a_none_shuffle_keeps_its_table_and_rules_read_its_result() {
    let (m, inst) = halls(guardians());
    let w = World::load(&m).unwrap();
    let mut in_spire = BTreeSet::new();
    for seed in 1..=100 {
        let s = staged(&w, &inst, seed);
        assert_eq!(s.table, staged(&w, &inst, seed).table, "seed {seed}");
        let g = &s.table["guardians"];
        assert_eq!(g.values().collect::<BTreeSet<_>>().len(), 3, "each guardian once: {g:?}");
        let here = g["spire_hall"].as_str();
        assert_ne!(here, "hydra", "seed {seed}: the table forbids it");
        in_spire.insert(here.to_owned());
        // The rule form is the rule of the guardian that stands there, and nothing else.
        let (mine, other) =
            if here == "golem" { ("sword", "lantern") } else { ("lantern", "sword") };
        let reached = |item: &str, loc: &str| {
            let r = s.plan.solve(&s.world, &[s.world.item(item).unwrap()]);
            r.reached(s.world.loc(loc).unwrap())
        };
        assert!(reached(mine, "spire_prize") && !reached(other, "spire_prize"), "seed {seed}");
        // The name test is a constant of the seed.
        assert_eq!(reached(mine, "echo_stone"), here != "wisp", "seed {seed}");
        assert_eq!(reached("junk", "wisp_mark"), here == "wisp", "seed {seed}");
    }
    assert_eq!(in_spire.len(), 2, "both allowed guardians stood in the spire: {in_spire:?}");
}

#[test]
fn a_shuffle_runs_after_the_shuffles_its_rules_read_and_a_cycle_is_a_load_error() {
    let relics = |rule: Value| {
        json!({"slots": {"type": "name", "names": ["altar"]},
                                       "things": {"cup": {"rule": rule}}})
    };
    // `aaa` sorts first and reads `guardians`: the order comes from the read, not the name.
    let mut shuffles = guardians();
    shuffles["aaa"] = relics(json!({"at": ["guardians", "marsh_hall"]}));
    let (m, inst) = halls(shuffles.clone());
    let w = World::load(&m).unwrap();
    assert_eq!(staged(&w, &inst, 1).table["aaa"]["altar"], "cup");
    // The guardians now read `aaa` back.
    shuffles["guardians"]["things"]["golem"]["rule"] = json!({"at": ["aaa", "altar"]});
    let e = World::load(&halls(shuffles).0).unwrap_err();
    assert!(e.contains("cycle"), "{e}");
    assert!(e.contains("aaa reads guardians") && e.contains("guardians reads aaa"), "{e}");
    assert!(e.contains("shuffles.aaa.things.cup.rule"), "the edge names its place: {e}");
    // A shuffle that reads itself is the shortest cycle.
    let own = json!({"own": relics(json!({"at": ["own", "altar"]}))});
    assert!(World::load(&halls(own).0).unwrap_err().contains("own reads own"));
}

#[test]
fn a_world_without_shuffles_is_not_staged_and_its_seed_says_nothing_new() {
    let (mut m, inst) = doors();
    m.as_object_mut().unwrap().remove("shuffles");
    let w = World::load(&m).unwrap();
    assert!(stage(&w, &inst, 1, 0).unwrap().is_none());
    let p = [Player { name: "Ann".into(), instance: inst }];
    let b = generate(&m, "s", &p, 1, 1).unwrap();
    assert_eq!(b.slots[0].slot_data, json!({}));
}

#[test]
fn filled_seeds_are_beatable_on_a_shuffled_layout_and_record_it() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    let p: Vec<Player> =
        ["Ann", "Bo", "Cy"].map(|n| Player { name: n.to_owned(), instance: inst.clone() }).into();
    let mut layouts = BTreeSet::new();
    for seed in 1..=150 {
        for players in [&p[..1], &p[..]] {
            // `generate` refuses a seed a player cannot finish.
            let b = generate(&m, "s", players, seed, 1).unwrap();
            for (k, slot) in b.slots.iter().enumerate() {
                assert!(slot.locations.iter().all(|l| l.sphere.is_some()), "seed {seed}");
                let want = stage(&w, &inst, seed, k as u32).unwrap().unwrap().table;
                assert_eq!(slot.slot_data, json!({"shuffles": want}), "seed {seed}, slot {k}");
                layouts.insert(slot.slot_data.to_string());
            }
        }
    }
    assert!(layouts.len() > 20, "{} layouts", layouts.len());
}

#[test]
fn a_prepared_world_carries_its_shuffles_through_the_child_jobs() {
    use data_fill::job::{Fill, fill, frame, prepare, unframe};
    let (m, inst) = doors();
    let p = vec![Player { name: "Ann".into(), instance: inst.clone() }];
    let direct = generate(&m, "s", &p, 9, 1).unwrap();
    let out = prepare(m.to_string().as_bytes()).unwrap();
    let (_, file) = unframe(&out).unwrap();
    let head = Fill {
        seed_name: "s".to_owned(),
        players: p,
        rng_seed: 9,
        workers: 1,
        algorithm: data_fill::room::Algorithm::Exact,
        seed_max_bytes: 1 << 24,
    };
    let seed = fill(&frame(&serde_json::to_vec(&head).unwrap(), file).unwrap()).unwrap();
    let game = data_fill::prepared::seed_game(file).unwrap().to_vec();
    let back = seed_bundle::v2::decode(seed, |_| Ok(game.clone()), 1 << 24).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&direct).unwrap());
    assert!(back.slots[0].slot_data["shuffles"]["entrances"].is_object());
    // The loaded world stages as the compiled one does.
    let w = data_fill::prepared::Prepared::open(file).unwrap();
    let table = staged(w.world(), &inst, 9).table;
    assert_eq!(table, staged(&World::load(&m).unwrap(), &inst, 9).table);
}

#[test]
fn a_fill_on_a_world_whose_shuffles_did_not_run_is_refused() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    let e = data_fill::Plan::new(&w, &inst).unwrap_err();
    assert!(format!("{e:?}").contains("shuffle"), "{e:?}");
}

#[test]
fn what_is_outside_the_slice_or_malformed_is_refused_by_name() {
    type Change = fn(&mut Value);
    let entrance: [(&str, Change); 16] = [
        ("`when`", |s| s["entrances"]["when"] = json!({"opt": "x"})),
        ("`pin`", |s| s["entrances"]["pin"] = json!([{"slot": "chute", "thing": "chute"}])),
        ("`pool`", |s| s["entrances"]["pool"] = json!({"file": "layouts.bin"})),
        ("`kind`", |s| s["entrances"]["kind"] = json!("doors")),
        ("`after`", |s| s["entrances"]["after"] = json!(["x"])),
        ("`again`", |s| s["entrances"]["allow"][0]["again"] = json!(true)),
        ("\"reach\"", |s| s["entrances"]["check"] = json!("reach")),
        ("moves exits", |s| s["entrances"]["check"] = json!("none")),
        ("unknown field \"speed\"", |s| s["entrances"]["speed"] = json!(1)),
        ("\"start\"", |s| s["entrances"]["slots"]["type"] = json!("start")),
        ("unknown exit \"nowhere\"", |s| s["entrances"]["slots"]["names"][0] = json!("nowhere")),
        ("unknown group \"castle\"", |s| {
            s["entrances"]["allow"][0]["only"] = json!({"groups": ["castle"]});
        }),
        ("the other side follows", |s| {
            s["entrances"]["slots"]["names"][0] = json!("mill_door_back");
        }),
        ("no assignment", |s| {
            s["entrances"]["allow"][1]["only"] = json!({"groups": ["cave"]});
        }),
        ("reserved", |s| s["layout"] = s["entrances"].take()),
        ("in two shuffles", |s| s["other"] = s["entrances"].clone()),
    ];
    for (want, change) in entrance {
        let mut s = entrances();
        change(&mut s);
        let e = World::load(&manifest(doors_regions(), s).0).unwrap_err();
        assert!(e.contains(want) && e.contains("shuffles"), "{want}: {e}");
    }
    let hall: [(&str, Change); 9] = [
        ("`to`", |s| s["guardians"]["things"]["wisp"]["to"] = json!("village")),
        ("`count`", |s| s["guardians"]["things"]["wisp"]["count"] = json!(2)),
        ("`value`", |s| s["guardians"]["things"]["wisp"]["value"] = json!(3)),
        ("no assignment", |s| {
            s["guardians"]["things"].as_object_mut().unwrap().remove("golem");
        }),
        ("has no rule", |s| {
            s["guardians"]["things"]["wisp"].as_object_mut().unwrap().remove("rule");
        }),
        ("unknown item", |s| s["guardians"]["things"]["wisp"]["rule"] = json!({"has": "wand"})),
        ("no slot \"spire_hall\"", |s| {
            s["guardians"]["slots"]["names"][1] = json!("spire");
            s["guardians"]["allow"] = json!([]);
        }),
        ("twice", |s| s["guardians"]["slots"]["names"][1] = json!("marsh_hall")),
        ("at most", |s| {
            let many: Vec<String> =
                (0..=config::SHUFFLE_MAX_SLOTS).map(|i| format!("h{i}")).collect();
            s["guardians"]["slots"]["names"] = json!(many);
        }),
    ];
    for (want, change) in hall {
        let mut s = guardians();
        change(&mut s);
        let e = World::load(&halls(s).0).unwrap_err();
        assert!(e.contains(want), "{want}: {e}");
    }
    // Reads the engine cannot fold in this slice.
    let rule_errors = [
        (json!({"at": ["entrances", "chute"]}), "layout"),
        (json!({"at": ["nobody", "x"]}), "unknown shuffle \"nobody\""),
        (json!({"eq": [{"at": ["guardians", "spire_hall"]}, "ghost"]}), "no thing \"ghost\""),
        (json!({"ge": [{"at": ["guardians", "spire_hall"]}, "wisp"]}), "`eq` or `ne`"),
        (json!({"ge": [{"at": ["guardians", "spire_hall"]}, 2]}), "thing name"),
    ];
    for (rule, want) in rule_errors {
        let mut shuffles = guardians();
        shuffles["entrances"] = entrances()["entrances"].clone();
        let mut regions = doors_regions();
        regions[0]["checks"][0]["rule"] = rule;
        let e = World::load(&manifest(regions, shuffles).0).unwrap_err();
        assert!(e.contains(want), "{want}: {e}");
    }
    // A door with three sides, a side that is not the other's way back, a group without a key.
    let mut regions = doors_regions();
    regions[4]["exits"][1] = json!({"to": "attic", "rule": t(), "key": "x", "door": "cave_mouth"});
    let e = World::load(&manifest(regions, entrances()).0).unwrap_err();
    assert!(e.contains("door \"cave_mouth\"") && e.contains("two sides"), "{e}");
    let mut regions = doors_regions();
    regions[3]["exits"][0]["to"] = json!("cave");
    let e = World::load(&manifest(regions, entrances()).0).unwrap_err();
    assert!(e.contains("door \"mill_door\"") && e.contains("way back"), "{e}");
    let mut regions = doors_regions();
    regions[5]["exits"][0]["group"] = json!("cave");
    let e = World::load(&manifest(regions, entrances()).0).unwrap_err();
    assert!(e.contains("needs a key"), "{e}");
}

/// Synthetic worlds whose every way into a region is a shuffled two-way door: rooms fill
/// beatable for every seed that stages, and the restarts are counted.
#[test]
fn synthetic_worlds_with_shuffled_doors_fill_beatable() {
    let (mut staged_worlds, mut restarts, mut capped) = (0, 0, 0);
    for (shape, players) in [
        (Shape { checks: 60, layers: 3, branching: 1, doors: true, ..Shape::default() }, 3),
        (Shape { checks: 60, layers: 3, doors: true, ..Shape::heavy() }, 2),
    ] {
        for seed in 1..=40u64 {
            let (mut m, inst) = synth_world(&shape, seed, 0);
            m["game"] = json!("Synth");
            assert!(m["shuffles"]["entrances"]["slots"]["names"].as_array().unwrap().len() > 5);
            let p: Vec<Player> = (0..players)
                .map(|k| Player { name: format!("P{k}"), instance: inst.clone() })
                .collect();
            match generate(&m, "s", &p, seed, 1) {
                Ok(b) => {
                    let w = World::load(&m).unwrap();
                    for k in 0..players {
                        let s = stage(&w, &inst, seed, k).unwrap().unwrap();
                        assert_whole_world(&s, &inst, &format!("seed {seed}"));
                        restarts += s.attempts - 1;
                        staged_worlds += 1;
                    }
                    assert!(b.slots.iter().all(|s| s.slot_data["shuffles"].is_object()));
                }
                // The only refusal allowed: a world out of attempts, named.
                Err(e) => {
                    assert!(e.contains("attempts") && e.contains("entrances"), "seed {seed}: {e}");
                    capped += 1;
                }
            }
        }
    }
    println!(
        "{staged_worlds} worlds staged, {restarts} restarts, {capped} rooms at the cap (counted)"
    );
    assert!(staged_worlds > 150 && capped <= 4, "{staged_worlds} staged, {capped} capped");
    // Off, the shape draws the world it always drew.
    let off = Shape { checks: 60, layers: 3, ..Shape::default() };
    assert!(synth_world(&off, 1, 0).0.get("shuffles").is_none());
}
