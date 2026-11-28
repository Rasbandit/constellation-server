//! Generic shuffles, first slice (design doc 17 of the docs repo): a `none` shuffle whose
//! result rules read, and the `layout` entrance shuffle with coupled doors, one-way exits and
//! type groups, staged before the exact fill.

use std::collections::{BTreeMap, BTreeSet};

use data_fill::seed::{Player, generate};
use data_fill::shuffle::{Staged, order, room, stage, unshuffled};
use data_fill::synth::{Shape, world as synth_world};
use data_fill::{Instance, World, decode};
use serde_json::{Value, json};
use spec_rng::SpecRng;

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
        assert!(s.attempts <= rando_limits::SHUFFLE_MAX_ATTEMPTS);
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

/// Doors `d1` to region `a`, `d2` to the shrine and one more per name in `more`, all out of
/// the village. `d2` opens on a bell that rings in `a`, so a layout that puts `a` behind `d2`
/// closes both.
fn bell(more: &[&str], rows: Value) -> (Value, Instance) {
    let mut a = region("a", &["c_a"], json!([back("d1", "village")]));
    a["events"] = json!([{"event": "bell", "rule": t()}]);
    let rung = json!({"ge": [{"counter": "bell"}, 1]});
    let mut out = vec![door("d1", "a", "g", t()), door("d2", "shrine", "g", rung)];
    let mut names = vec!["d1".to_owned(), "d2".to_owned()];
    let mut regions = vec![a, region("shrine", &["c_shrine"], json!([back("d2", "village")]))];
    for (k, r) in more.iter().enumerate() {
        let key = format!("d{}", k + 3);
        out.push(door(&key, r, "g", t()));
        regions.push(region(r, &[], json!([back(&key, "village")])));
        names.push(key);
    }
    regions.insert(0, region("village", &["c1", "c2", "c3"], json!(out)));
    let shuffle =
        json!({"coupled": true, "slots": {"type": "exit", "names": names}, "allow": rows});
    manifest(regions, json!({"entrances": shuffle}))
}

#[test]
fn a_world_that_can_never_pass_the_check_errors_at_the_cap() {
    // The table lets the two doors only trade places, which puts the bell behind its own door.
    let swap = json!([{"slots": {"names": ["d1"]}, "only": {"names": ["d2"]}},
                      {"slots": {"names": ["d2"]}, "only": {"names": ["d1"]}}]);
    let (m, inst) = bell(&[], swap);
    let w = World::load(&m).unwrap();
    let e = stage(&w, &inst, 1, 0).unwrap_err();
    let cap = format!("{} attempts", rando_limits::SHUFFLE_MAX_ATTEMPTS);
    assert!(e.contains(&cap) && e.contains("entrances") && e.contains("region \"a\""), "{e}");
    // Through the seed path the error also names the player and the world.
    let p = [Player { name: "Ann".into(), instance: inst }];
    let e = generate(&m, "s", &p, 1).unwrap_err();
    assert!(e.contains("player 1") && e.contains("Doors") && e.contains("entrances"), "{e}");
}

#[test]
fn a_failed_layout_starts_again_and_the_next_attempt_can_pass() {
    // Four doors, free to trade: one layout in four puts the bell behind its own door.
    let (m, inst) = bell(&["b", "c"], json!([]));
    let w = World::load(&m).unwrap();
    let mut restarted = 0;
    for seed in 1..=60 {
        let s = staged(&w, &inst, seed);
        assert_ne!(s.table["entrances"]["d2"], "d1", "seed {seed}: the layout that cannot pass");
        assert_whole_world(&s, &inst, &format!("seed {seed}"));
        restarted += u32::from(s.attempts > 1);
        // Another attempt is another stream, and the same one every time.
        assert_eq!(s.attempts, staged(&w, &inst, seed).attempts, "seed {seed}");
    }
    assert!(restarted > 5, "{restarted} of 60 seeds restarted");
}

#[test]
fn a_none_shuffle_restarts_too_when_its_draw_closes_a_check() {
    // The ghost cannot be beaten (one sword exists); the prize is behind the spire's guardian.
    let mut shuffles = guardians();
    shuffles["guardians"]["allow"] = json!([]);
    shuffles["guardians"]["things"]["hydra"] = json!({"rule": {"has": "sword", "n": 2}});
    let (m, inst) = halls(shuffles);
    let w = World::load(&m).unwrap();
    let mut restarted = 0;
    for seed in 1..=60 {
        let s = staged(&w, &inst, seed);
        assert_ne!(s.table["guardians"]["spire_hall"], "hydra", "seed {seed}");
        restarted += u32::from(s.attempts > 1);
    }
    assert!(restarted > 5, "{restarted} of 60 seeds restarted");
}

#[test]
fn a_region_no_layout_can_open_is_an_error_at_once_not_at_the_cap() {
    // An island behind an option: closed by default, whatever the doors do.
    let mut regions = doors_regions();
    regions.push(region("island", &["c_island"], json!([])));
    let open = json!({"eq": [{"opt": "ferry"}, 1]});
    regions[0]["exits"].as_array_mut().unwrap().push(json!({"to": "island", "rule": open}));
    let (mut m, mut inst) = manifest(regions, entrances());
    m["options"] = json!([{"key": "ferry", "default": 0}]);
    let w = World::load(&m).unwrap();
    let e = stage(&w, &inst, 1, 0).unwrap_err();
    assert!(e.contains("island") && e.contains("unshuffled layout"), "{e}");
    assert!(!e.contains("attempts"), "no attempt can change it: {e}");
    inst.options.insert("ferry".into(), 1);
    assert_whole_world(&staged(&w, &inst, 1), &inst, "the ferry runs");
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
    // The two checks that test which guardian is there exist in no seed: a check that exists
    // has to be reachable, and each of them is closed for some guardian.
    let (m, mut inst) = manifest(regions, shuffles);
    inst.locations.truncate(4);
    inst.pool.truncate(4);
    (m, inst)
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
    assert_eq!(order(&w), ["guardians", "aaa"]);
    assert_eq!(staged(&w, &inst, 1).table["aaa"]["altar"], "cup");
    // The layout is one node after every shuffle a rule can read (design doc 17 section 4.3:
    // its check inspects every rule). The worked cycle of that section needs a `when` that
    // reads the entrances, which is refused by name until write edges exist.
    let mut both = guardians();
    both["entrances"] = entrances()["entrances"].clone();
    let w = World::load(&manifest(doors_regions(), both.clone()).0).unwrap();
    assert_eq!(order(&w), ["guardians", "layout"]);
    both["guardians"]["when"] = json!({"eq": [{"at": ["entrances", "chute"]}, "chute"]});
    let e = World::load(&manifest(doors_regions(), both).0).unwrap_err();
    assert!(e.contains("shuffles.guardians") && e.contains("`when`"), "{e}");
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
    for exit in m["regions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .flat_map(|r| r["exits"].as_array_mut().unwrap().iter_mut())
    {
        let exit = exit.as_object_mut().unwrap();
        for field in ["key", "door", "group"] {
            exit.remove(field);
        }
    }
    let w = World::load(&m).unwrap();
    assert!(stage(&w, &inst, 1, 0).unwrap().is_none());
    assert!(order(&w).is_empty());
    let p = [Player { name: "Ann".into(), instance: inst }];
    let b = generate(&m, "s", &p, 1).unwrap();
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
            let b = generate(&m, "s", players, seed).unwrap();
            for (k, slot) in b.slots.iter().enumerate() {
                assert!(slot.locations.iter().all(|l| l.sphere.is_some()), "seed {seed}");
                let want = stage(&w, &inst, seed, k as u32).unwrap().unwrap().table;
                assert_eq!(slot.slot_data, json!({"shuffles": want}), "seed {seed}, slot {k}");
                layouts.insert(slot.slot_data.to_string());
            }
        }
    }
    assert!(layouts.len() > 5, "{} layouts", layouts.len());
}

#[test]
fn a_prepared_world_carries_its_shuffles_through_the_child_jobs() {
    use data_fill::job::{Fill, fill, frame, prepare, unframe};
    let (m, inst) = doors();
    let p = vec![Player { name: "Ann".into(), instance: inst.clone() }];
    let direct = generate(&m, "s", &p, 9).unwrap();
    let out = prepare(m.to_string().as_bytes()).unwrap();
    let (_, file) = unframe(&out).unwrap();
    let head = Fill {
        seed_name: "s".to_owned(),
        players: p,
        rng_seed: 9,
        seed_max_bytes: data_fill::prepared::SEED_MAX_BYTES,
    };
    let seed = fill(&frame(&serde_json::to_vec(&head).unwrap(), file).unwrap()).unwrap();
    let game = data_fill::prepared::seed_game(file).unwrap().to_vec();
    let back =
        seed_bundle::v2::decode(seed, |_| Ok(game.clone()), data_fill::prepared::SEED_MAX_BYTES)
            .unwrap();
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
    let entrance: [(&str, Change); 30] = [
        ("shuffles.entrances.slots.type", |s| s["entrances"]["slots"] = json!({})),
        ("shuffles.entrances.slots.names", |s| {
            s["entrances"]["slots"].as_object_mut().unwrap().remove("names");
        }),
        ("shuffles.entrances.slots", |s| {
            s["entrances"].as_object_mut().unwrap().remove("slots");
        }),
        ("`scope`", |s| s["entrances"]["scope"] = json!("cosmetic")),
        ("`vanilla`", |s| s["entrances"]["vanilla"] = json!({"chute": "chute"})),
        ("without a dot", |s| *s = json!({"a.b": s["entrances"].take()})),
        ("48 bytes", |s| *s = json!({"n".repeat(49): s["entrances"].take()})),
        ("at most 64", |s| {
            let one = s["entrances"].take();
            *s = (0..65)
                .map(|k| (format!("s{k}"), one.clone()))
                .collect::<serde_json::Map<_, _>>()
                .into();
        }),
        ("rows", |s| {
            let row = s["entrances"]["allow"][0].clone();
            s["entrances"]["allow"] = json!(vec![row; rando_limits::SHUFFLE_MAX_ROWS + 1]);
        }),
        ("names and groups", |s| {
            let many = vec!["cave"; rando_limits::SHUFFLE_MAX_SLOTS + 1];
            s["entrances"]["allow"][0]["only"] = json!({"groups": many});
        }),
        ("only a shuffle that moves exits", |s| {
            *s = guardians();
            s["guardians"]["coupled"] = json!(true);
        }),
        ("is not supported yet", |s| s["entrances"]["coupled"] = json!({"opt": "x"})),
        ("the things of an exit shuffle", |s| {
            s["entrances"]["things"] = json!({"type": "exit", "names": ["chute"]});
        }),
        ("a row is", |s| s["entrances"]["allow"][0] = json!({"slots": {"groups": ["cave"]}})),
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
        ("reserved", |s| *s = json!({"layout": s["entrances"].take()})),
        ("in two shuffles", |s| s["other"] = s["entrances"].clone()),
    ];
    for (want, change) in entrance {
        let mut s = entrances();
        change(&mut s);
        let e = World::load(&manifest(doors_regions(), s).0).unwrap_err();
        assert!(e.contains(want) && e.contains("shuffles"), "{want}: {e}");
    }
    let hall: [(&str, Change); 16] = [
        ("`min`", |s| s["guardians"]["things"]["wisp"]["min"] = json!(1)),
        ("`weight`", |s| s["guardians"]["things"]["wisp"]["weight"] = json!(5)),
        ("`fill`", |s| s["guardians"]["things"]["wisp"]["fill"] = json!(true)),
        ("`grants`", |s| s["guardians"]["things"]["wisp"]["grants"] = json!(["bell"])),
        ("`item`", |s| s["guardians"]["things"]["wisp"]["item"] = json!("sword")),
        ("things", |s| {
            let many: Vec<String> =
                (0..=rando_limits::SHUFFLE_MAX_SLOTS).map(|i| format!("g{i}")).collect();
            s["guardians"]["things"] = json!(many);
        }),
        ("read as a rule", |s| {
            // More candidates than one rule can list.
            for k in 0..rando_limits::SHUFFLE_MAX_READ_THINGS {
                s["guardians"]["things"][format!("g{k}")] = json!({"rule": {"true": {}}});
            }
        }),
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
                (0..=rando_limits::SHUFFLE_MAX_SLOTS).map(|i| format!("h{i}")).collect();
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
    // The world's own fields this slice does not build are refused in every manifest, and the
    // exit fields of a shuffle in a manifest without one.
    for (field, value) in [("require", json!(["returnable"])), ("kinds", json!({}))] {
        for shuffles in [entrances(), json!({})] {
            let mut m = manifest(doors_regions(), shuffles.clone()).0;
            m[field] = value.clone();
            if shuffles == json!({}) {
                m["regions"] = json!([
                    region("village", &["c_village"], json!([])),
                    region("shrine", &[], json!([]))
                ]);
            }
            let e = World::load(&m).unwrap_err();
            assert!(e.contains(&format!("`{field}`")) && e.contains("not supported"), "{e}");
        }
    }
    let mut m = doors().0;
    m.as_object_mut().unwrap().remove("shuffles");
    let e = World::load(&m).unwrap_err();
    assert!(e.contains("cave_mouth") && e.contains("no shuffle"), "{e}");
    // Names the compiler keeps for shuffle results, and the cap on what rules can read.
    let mut m = halls(guardians()).0;
    m["options"] = json!([{"key": "at.guardians.spire_hall", "default": 0}]);
    assert!(World::load(&m).unwrap_err().contains("`at.` names"), "an option");
    let mut m = halls(guardians()).0;
    m["defs"] = json!({"at.guardians.spire_hall": t()});
    assert!(World::load(&m).unwrap_err().contains("`at.` names"), "a def");
    let mut s = guardians();
    let many: Vec<String> = (0..300).map(|i| format!("h{i}")).collect();
    s["guardians"]["slots"]["names"] = json!(many);
    s["guardians"]["things"] = json!(many);
    s["guardians"]["allow"] = json!([]);
    let mut m = halls(s).0;
    m["regions"][1]["checks"] = json!([]);
    let e = World::load(&m).unwrap_err();
    assert!(e.contains("at most 256"), "{e}");
    // Rules compiled with shuffle results and loaded without the shuffles would read every
    // slot as its first thing.
    let (rules, game) = data_fill::section::sections(&halls(guardians()).0).unwrap();
    let e = World::from_sections(rules, game).unwrap_err();
    assert!(e.contains("shuffle section"), "{e}");
    // An error does not carry a name of any length back.
    let mut s = entrances();
    s["entrances"]["slots"]["names"][0] = json!("x".repeat(1 << 20));
    let e = World::load(&manifest(doors_regions(), s).0).unwrap_err();
    assert!(e.len() <= rando_limits::SHUFFLE_ERROR_MAX_BYTES + 8, "{} bytes", e.len());
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

/// Synthetic worlds with shuffled two-way doors: light with a door as the only way into a
/// region, light with a second fixed way in, and heavy without a time context. Every room
/// stages and fills beatable, and each table says what the staged world's exits do. The
/// restarts are counted and printed. (Heavy worlds without `doors` can be unfillable before
/// any shuffle, issue 209; `doors` keeps the start region's checks open.)
#[test]
fn synthetic_worlds_with_shuffled_doors_all_fill_beatable() {
    let (mut staged_worlds, mut restarts) = (0, 0);
    let light = Shape { checks: 60, layers: 3, doors: true, timeless: true, ..Shape::default() };
    for (shape, players) in [
        (Shape { branching: 1, ..light }, 3u32),
        (light, 2),
        (Shape { checks: 60, layers: 3, doors: true, timeless: true, ..Shape::heavy() }, 2),
    ] {
        for seed in 1..=40u64 {
            let (mut m, inst) = synth_world(&shape, seed, 0);
            m["game"] = json!("Synth");
            let names = m["shuffles"]["entrances"]["slots"]["names"].as_array().unwrap();
            assert!(names.len() > 5);
            let p: Vec<Player> = (0..players)
                .map(|k| Player { name: format!("P{k}"), instance: inst.clone() })
                .collect();
            let b = generate(&m, "s", &p, seed).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            let w = World::load(&m).unwrap();
            let vanilla = graph(&m, &w);
            for (k, slot) in b.slots.iter().enumerate() {
                let s = stage(&w, &inst, seed, k as u32).unwrap().unwrap();
                assert_eq!(slot.slot_data, json!({"shuffles": s.table}), "seed {seed}");
                assert_whole_world(&s, &inst, &format!("seed {seed}"));
                // The table against the exits the fill walked: `e<r>` leads where its thing
                // led, and that region's way back `b<r>` leads here.
                let (real, table) = (graph(&m, &s.world), &s.table["entrances"]);
                for name in names.iter().map(|n| n.as_str().unwrap()) {
                    let thing = &table[name];
                    assert_eq!(real[name].1, vanilla[thing].1, "seed {seed}, {name}");
                    let back = &real[&thing.replacen('e', "b", 1)];
                    assert_eq!(*back, (real[name].1.clone(), real[name].0.clone()), "seed {seed}");
                }
                restarts += s.attempts - 1;
                staged_worlds += 1;
            }
        }
    }
    println!("120 rooms, {staged_worlds} worlds staged, {restarts} restarts (counted)");
    assert_eq!(staged_worlds, 40 * (3 + 2 + 2));
    // Off, the shape draws the world it always drew.
    let off = Shape { checks: 60, layers: 3, ..Shape::default() };
    assert!(synth_world(&off, 1, 0).0.get("shuffles").is_none());
}

#[test]
fn a_synthetic_none_shuffle_fills_beatable_and_a_time_context_refuses_a_layout_shuffle() {
    let shape = Shape { checks: 60, layers: 3, guards: true, ..Shape::default() };
    for seed in 1..=20u64 {
        let (mut m, inst) = synth_world(&shape, seed, 0);
        m["game"] = json!("Synth");
        let p = [Player { name: "P".into(), instance: inst }];
        let b = generate(&m, "s", &p, seed).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(b.slots[0].slot_data["shuffles"]["guards"].as_object().unwrap().len(), 8);
    }
    // More than one time slice, or clocks: an exit shuffle is refused until exits that narrow
    // time are settled (design doc 17 section 9). A `none` shuffle is not.
    for shape in [
        Shape { checks: 60, layers: 3, doors: true, ..Shape::default() },
        Shape { checks: 60, layers: 3, doors: true, ..Shape::heavy() },
    ] {
        let e = World::load(&synth_world(&shape, 1, 0).0).unwrap_err();
        assert!(e.contains("shuffles.entrances") && e.contains("time"), "{e}");
    }
    let (mut m, _) = doors();
    m["x.time"] = json!({"slices": 2, "start_mask": 1});
    assert!(World::load(&m).unwrap_err().contains("time"));
}

/// The tables of fixed seeds, pinned: a change to a draw, a stream or the layout solve
/// changes every shuffled seed and has to show here.
#[test]
fn the_results_of_fixed_seeds_are_pinned() {
    let pin = |(m, inst): (Value, Instance)| {
        let w = World::load(&m).unwrap();
        let out: String =
            (1..=8).map(|seed| format!("{:?}\n", staged(&w, &inst, seed).table)).collect();
        data_fill::check::fnv1a(out.as_bytes())
    };
    // The entrance shuffle, the guardian shuffle, and a layout that restarts.
    let got = [pin(doors()), pin(halls(guardians())), pin(bell(&["b", "c"], json!([])))];
    assert_eq!(got, ["a4521a4a03bded05", "7e7645c7fd61def5", "7b5053b20fdcc435"]);
}

/// A wider pin for the layout solve and the check behind it: per seed, the attempts taken and
/// the table, on worlds whose layouts fail the check (the bell worlds, where the old check
/// rejected one layout in four and one in eight), on three shuffles solved together, and on
/// a synthetic world with more doors than fit one machine word. An attempt count that moves
/// means a layout was judged differently; a table that moves means a draw changed.
#[test]
fn layouts_that_fail_the_check_are_rejected_on_the_same_attempts_as_before() {
    let pin = |(m, inst): (Value, Instance), seeds: u64| {
        let w = World::load(&m).unwrap();
        let (mut out, mut restarted) = (String::new(), 0u32);
        for seed in 1..=seeds {
            // A seed whose every attempt fails is pinned by its error.
            let s = stage(&w, &inst, seed, 0).map(|s| s.map(|s| (s.attempts, s.table)));
            restarted += u32::from(s.as_ref().is_ok_and(|s| s.as_ref().unwrap().0 > 1));
            out += &format!("{s:?}\n");
        }
        (data_fill::check::fnv1a(out.as_bytes()), restarted)
    };
    let wide = Shape { checks: 400, doors: true, timeless: true, ..Shape::heavy() };
    let (synth, inst) = synth_world(&wide, 7, 0);
    let doors = synth["shuffles"]["entrances"]["slots"]["names"].as_array().unwrap().len();
    assert!(doors > 64, "{doors} doors");
    let got = [
        pin(bell(&["b", "c"], json!([])), 300),
        pin(bell(&["b", "c", "e", "f", "g", "h"], json!([])), 300),
        pin(three_shuffles(), 100),
        pin((synth, inst), 6),
    ];
    assert!(got[0].1 > 30 && got[1].1 > 15, "{got:?}: both bell worlds restart");
    assert_eq!(
        got.map(|x| x.0),
        ["0807942c8978390a", "2f61edeb98a2c309", "fcd03d6e1bbef245", "6ea4add6e2bef5d5"]
    );
}

/// A staged world is an ordinary world to the room fill: the fill, with its journal, and its
/// cross-check, a fresh solve per round (`room::fill_room_fresh`), both read the seed's exits
/// and the seed's `none` results, and decide the same placements and spheres. Hand-written and synthetic worlds, an entrance shuffle and a `none` shuffle, 1 and
/// 10 players, 30 seeds each. A room of worlds without shuffles staged through the same call
/// fills as it does resolved directly.
#[test]
fn a_shuffled_room_fills_the_same_by_a_solve_per_round() {
    use data_fill::room::{Compiled, fill_room_fresh, fill_room_spheres};
    let fill = |c: &Compiled, seed: u64, fresh: bool| {
        let mut rng = SpecRng::from_u64(seed);
        let (f, sp) = if fresh {
            fill_room_fresh(&c.seats(), &mut rng, rando_limits::FILL_MAX_ATTEMPTS)
        } else {
            fill_room_spheres(&c.seats(), &mut rng, rando_limits::FILL_MAX_ATTEMPTS)
        }
        .unwrap_or_else(|e| panic!("seed {seed}, fresh {fresh}: {e:?}"));
        assert!(f.all_beatable(), "seed {seed}, fresh {fresh}");
        (f.placed, sp, f.attempts)
    };
    let doors_shape =
        Shape { checks: 60, layers: 3, doors: true, timeless: true, ..Shape::default() };
    let guards_shape = Shape { checks: 60, layers: 3, guards: true, ..Shape::heavy() };
    let mut both = guardians();
    both["entrances"] = entrances()["entrances"].clone();
    let worlds = [
        doors(),
        manifest(doors_regions(), both),
        synth_world(&Shape { branching: 1, ..doors_shape }, 3, 0),
        synth_world(&doors_shape, 4, 0),
        synth_world(&guards_shape, 7, 0),
    ];
    let (mut rooms, mut layouts) = (0, BTreeSet::new());
    for (m, inst) in &worlds {
        let w = World::load(m).unwrap();
        for players in [1usize, 10] {
            let insts = vec![inst; players];
            for seed in 1..=30 {
                let (c, tables) = room(&[&w], &insts, seed).unwrap();
                let fresh = fill(&c, seed, true);
                assert_eq!(fresh, fill(&c, seed, false), "{players} players, seed {seed}");
                rooms += 1;
                layouts.insert(format!("{tables:?}"));
            }
        }
    }
    assert!(rooms == 300 && layouts.len() > 200, "{rooms} rooms, {} layouts", layouts.len());
    // Without shuffles nothing is staged: the same room as resolving it directly.
    let (m, inst) = synth_world(&Shape { checks: 60, layers: 3, ..Shape::default() }, 5, 0);
    let w = World::load(&m).unwrap();
    let insts = vec![&inst; 10];
    let direct = Compiled::shared(w.clone(), &insts).unwrap();
    let (staged, tables) = room(&[&w], &insts, 9).unwrap();
    assert!(tables.iter().all(|t| t.is_empty()));
    for fresh in [false, true] {
        assert_eq!(fill(&direct, 9, fresh), fill(&staged, 9, fresh), "fresh {fresh}");
    }
}

/// The comparison form: the same world with every exit where the manifest has it.
#[test]
fn the_unshuffled_form_of_a_world_keeps_every_exit_and_passes_the_same_check() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    let plain = unshuffled(&w, &inst).unwrap().unwrap();
    assert_eq!(graph(&m, &plain.world), graph(&m, &w));
    assert!(plain.table.is_empty() && plain.world.shares_with(&w));
    assert_whole_world(&plain, &inst, "unshuffled");
    let (m, inst) = halls(guardians());
    let e = unshuffled(&World::load(&m).unwrap(), &inst).unwrap_err();
    assert!(e.contains("guardians") && e.contains("no unshuffled form"), "{e}");
}

/// Both sides of each door listed in a shuffle that is not coupled: every side is its own
/// slot, and a door need not lead back.
#[test]
fn decoupled_doors_are_four_slots_and_need_not_lead_back() {
    let regions = vec![
        region(
            "village",
            &["c1", "c2", "c3"],
            json!([door("cave_in", "cave", "g", t()), door("shrine_in", "shrine", "g", t())]),
        ),
        region("cave", &["c_cave"], json!([back("cave_in", "village")])),
        region("shrine", &["c_shrine"], json!([back("shrine_in", "village")])),
    ];
    let names = ["cave_in", "cave_in_back", "shrine_in", "shrine_in_back"];
    let shuffles =
        json!({"entrances": {"coupled": false, "slots": {"type": "exit", "names": names}}});
    let (m, inst) = manifest(regions, shuffles);
    let w = World::load(&m).unwrap();
    let vanilla = graph(&m, &w);
    let mut one_way = 0;
    for seed in 1..=100 {
        let s = staged(&w, &inst, seed);
        let (real, table) = (graph(&m, &s.world), &s.table["entrances"]);
        assert_eq!(table.len(), 4, "seed {seed}: no side follows another");
        for name in names {
            assert_eq!(real[name].1, vanilla[&table[name]].1, "seed {seed}, {name}");
        }
        assert_whole_world(&s, &inst, &format!("seed {seed}"));
        let back = |door: &str| real[&format!("{door}_back")].1 == "village";
        one_way += u32::from(!back("cave_in") || !back("shrine_in"));
    }
    assert!(one_way > 0, "some door leads somewhere else than back");
}

/// The doors world with its doors split over three coupled shuffles.
fn three_shuffles() -> (Value, Instance) {
    let slots =
        |names: &[&str]| json!({"coupled": true, "slots": {"type": "exit", "names": names}});
    let shuffles = json!({
        "caves": slots(&["cave_mouth", "mill_door", "deep_door"]),
        "drops": slots(&["well_drop", "chute"]),
        "dungeons": slots(&["tower_gate", "crypt_gate"]),
    });
    manifest(doors_regions(), shuffles)
}

/// Three shuffles over the doors of one world: one joint solve, each exit in its own table.
#[test]
fn several_layout_shuffles_of_a_world_are_solved_together() {
    let (m, inst) = three_shuffles();
    let w = World::load(&m).unwrap();
    assert_eq!(order(&w), ["layout"]);
    let mut seen = BTreeSet::new();
    for seed in 1..=100 {
        let s = staged(&w, &inst, seed);
        let own = |shuffle: &str, names: &[&str]| {
            let t = &s.table[shuffle];
            names.iter().all(|n| names.contains(&t[*n].as_str()))
        };
        assert!(own("caves", &["cave_mouth", "mill_door", "deep_door"]), "seed {seed}");
        assert!(
            own("drops", &["well_drop", "chute"]) && own("dungeons", &["tower_gate", "crypt_gate"])
        );
        assert_whole_world(&s, &inst, &format!("seed {seed}"));
        seen.insert(format!("{:?}", s.table));
    }
    assert!(seen.len() > 5, "{} layouts", seen.len());
}

#[test]
fn a_coupled_door_listed_from_its_far_side_is_a_load_error_that_names_it() {
    let (mut m, _) = tree();
    m["shuffles"]["entrances"]["slots"]["names"][0] = json!("to_hub_back");
    let e = World::load(&m).unwrap_err();
    assert!(e.contains("shuffles.entrances") && e.contains("to_hub_back"), "{e}");
    assert!(e.contains("region \"hub\"") && e.contains("far side"), "{e}");
}

#[test]
fn a_room_stages_the_same_every_time() {
    let (m, inst) = doors();
    let w = World::load(&m).unwrap();
    let insts = vec![&inst; 9];
    let (once, tables) = room(&[&w], &insts, 5).unwrap();
    let (again, same) = room(&[&w], &insts, 5).unwrap();
    assert_eq!(tables, same);
    let exits = |c: &data_fill::room::Compiled| {
        c.worlds.iter().map(|w| decode::manifest(w)["regions"].clone()).collect::<Vec<_>>()
    };
    assert_eq!(exits(&once), exits(&again));
    assert!(tables.iter().any(|t| *t != tables[0]), "players differ");
    // A staged world shares what the seed did not change with the world it came from.
    assert!(once.worlds.iter().all(|s| s.shares_with(&w)));
}

/// More fixed regions behind the shuffled exits than the layout index may hold.
#[test]
fn a_world_whose_fixed_regions_around_its_doors_are_too_many_is_refused() {
    let mut regions = vec![region(
        "village",
        &["c1", "c2"],
        json!([{"to": "r1", "rule": t()},
                                                                   {"to": "shrine", "rule": t()}]),
    )];
    regions.push(region("shrine", &[], json!([])));
    let mut names = Vec::new();
    for k in 1..=1100 {
        let mut exits = vec![json!({"to": format!("r{}", k + 1), "rule": t()})];
        if k > 76 {
            exits.push(door(&format!("e{k}"), &format!("x{k}"), "g", t()));
            regions.push(region(
                &format!("x{k}"),
                &[],
                json!([back(&format!("e{k}"), &format!("r{k}"))]),
            ));
            names.push(format!("e{k}"));
        }
        regions.push(region(&format!("r{k}"), &[], json!(exits)));
    }
    regions.push(region("r1101", &[], json!([])));
    assert_eq!(names.len(), rando_limits::SHUFFLE_MAX_SLOTS);
    let shuffles =
        json!({"entrances": {"coupled": true, "slots": {"type": "exit", "names": names}}});
    let e = World::load(&manifest(regions, shuffles).0).unwrap_err();
    assert!(e.contains("not shuffled") && e.contains("shuffles"), "{e}");
}

fn below(rng: &mut SpecRng, n: usize) -> usize {
    usize::try_from(rng.below(n as u64).unwrap_or(0)).unwrap_or(0)
}

/// One random change to a JSON value: a member dropped, repeated or replaced by something of
/// another shape.
fn mutate(v: &mut Value, rng: &mut SpecRng, depth: u32) {
    let children = match v {
        Value::Object(o) => o.len(),
        Value::Array(a) => a.len(),
        _ => 0,
    };
    if children > 0 && depth < 12 && below(rng, 4) != 0 {
        let (k, what) = (below(rng, children), below(rng, 8));
        match v {
            Value::Object(o) => {
                let key = o.keys().nth(k).cloned().unwrap_or_default();
                match (what, o.get_mut(&key)) {
                    (0, _) | (_, None) => drop(o.remove(&key)),
                    (_, Some(child)) => mutate(child, rng, depth + 1),
                }
            }
            Value::Array(a) => match what {
                0 => drop(a.remove(k)),
                1 => a.push(a[k].clone()),
                _ => mutate(&mut a[k], rng, depth + 1),
            },
            _ => {}
        }
        return;
    }
    *v = match below(rng, 12) {
        0 => Value::Null,
        1 => json!(true),
        2 => json!(-1),
        3 => json!(u64::MAX),
        4 => json!(""),
        5 => json!("layout"),
        6 => json!([]),
        7 => json!({}),
        8 => json!("x".repeat(300)),
        9 => json!({"at": ["guardians", "spire_hall"]}),
        10 => json!(["cave_mouth", "cave_mouth_back", 7]),
        _ => json!({"names": ["chute"], "groups": ["cave"], "type": "exit"}),
    };
}

/// Malformed input is an error, never a panic: thousands of random changes to the shuffles
/// and regions of manifests, and to the stored shuffle section of a compiled world.
#[test]
fn random_damage_to_a_manifest_or_a_stored_section_is_an_error_never_a_panic() {
    let mut both = guardians();
    both["entrances"] = entrances()["entrances"].clone();
    let bases =
        [doors(), halls(guardians()), manifest(doors_regions(), both), bell(&["b"], json!([]))];
    let mut rng = SpecRng::from_u64(7);
    let (mut loaded, mut refused) = (0, 0);
    for case in 0..4000u64 {
        let (mut m, inst) = bases[(case % 4) as usize].clone();
        for _ in 0..=case % 3 {
            let part = ["shuffles", "shuffles", "regions"][(case / 4 % 3) as usize];
            mutate(&mut m[part], &mut rng, 0);
        }
        match World::load(&m) {
            // Whatever loads also stages or says why not.
            Ok(w) => loaded += u32::from(stage(&w, &inst, case, 0).is_ok()),
            Err(e) => {
                assert!(!e.is_empty());
                refused += 1;
            }
        }
    }
    assert!(loaded > 100 && refused > 1000, "{loaded} staged, {refused} refused");
    // The stored form of a compiled world, as a prepared world file carries it.
    let (mut opened, mut refused) = (0, 0);
    for case in 0..2000u64 {
        let (m, inst) = &bases[(case % 4) as usize];
        let (rules, game) = data_fill::section::sections(m).unwrap();
        let stored = data_fill::shuffle::section(m).unwrap().unwrap();
        let mut v: Value = serde_json::from_slice(&stored).unwrap();
        mutate(&mut v, &mut rng, 0);
        match World::from_parts(rules, game, Some(v.to_string().as_bytes())) {
            Ok(w) => opened += u32::from(stage(&w, inst, case, 0).is_ok()),
            Err(_) => refused += 1,
        }
    }
    assert!(opened > 0 && refused > 500, "{opened} staged, {refused} refused");
}

/// The tools that resolve a plan themselves say that a world with shuffles has to be staged.
#[test]
fn the_tools_refuse_a_world_with_shuffles_by_name() {
    let (m, inst) = doors();
    let dir = std::env::temp_dir().join(format!("shuffle-tools-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |name: &str, v: &Value| {
        let p = dir.join(name);
        std::fs::write(&p, v.to_string()).unwrap();
        p.to_str().unwrap().to_owned()
    };
    let (mp, ip) = (path("m.json", &m), path("i.json", &serde_json::to_value(&inst).unwrap()));
    let out = dir.join("out.json").to_str().unwrap().to_owned();
    let runs: [(&str, Vec<&str>); 3] = [
        (env!("CARGO_BIN_EXE_data-fill"), vec![&mp, &ip]),
        (env!("CARGO_BIN_EXE_fill-fairness"), vec!["--manifest", &mp, "--instance", &ip]),
        (
            env!("CARGO_BIN_EXE_port-check"),
            vec!["--manifest", &mp, "--legacy", "true", "--out", &out],
        ),
    ];
    for (bin, args) in runs {
        let o = std::process::Command::new(bin).args(&args).output().unwrap();
        let said =
            String::from_utf8_lossy(&o.stderr).into_owned() + &String::from_utf8_lossy(&o.stdout);
        assert!(
            !o.status.success() && said.contains("shuffles") && said.contains("stage"),
            "{bin}: {said}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
