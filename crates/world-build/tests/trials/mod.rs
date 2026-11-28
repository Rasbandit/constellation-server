//! The fixture worlds end to end (a module of `build.rs`). Every folder in `tests/fixtures`
//! builds with no warnings, keeps the ids in its committed ledger, is in canonical layout, and
//! fills beatable for seeds 1 to 50 at 1 and 4 players: a new folder is covered by being there.
//! Beside that, the trial worlds of doc 18 (section 8, question 9), `cinderfall` (an adventure
//! with keys, progressive items, options that reshape rules and a shuffle of interiors) and
//! `neon_cadence` (a catalog game: a generated base, a hand overlay, families and weighted
//! sums), have their own tests, and the genre examples fill at their option extremes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use data_fill::seed::{Player, generate};
use world_build::front::{Dir, Files, Mem, Options, format_text, write_generated};
use world_build::{Built, build};

fn root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Every file of a fixture folder (`.rando`, `.csv`, `rando.lock`) by relative path.
fn read_all(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().to_str().unwrap().to_owned();
                out.insert(rel, std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Every folder in `tests/fixtures`, by name. Each must be a world (have a `rando.toml`), so a
/// folder missing it fails here instead of leaving its folder untested.
fn fixtures() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root(""))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .map(|p| {
            assert!(p.join("rando.toml").is_file(), "{} has no rando.toml", p.display());
            p.file_name().unwrap().to_str().unwrap().to_owned()
        })
        .collect();
    names.sort();
    names
}

fn ok(files: &dyn Files) -> Built {
    build(files, &Options::default()).unwrap_or_else(|e| {
        panic!("{}", e.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
    })
}

fn errors(files: &dyn Files) -> Vec<String> {
    build(files, &Options::default())
        .map(|_| ())
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// The checks the manifest has, by region.
fn checks(b: &Built, region: &str) -> Vec<String> {
    b.manifest["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == region)
        .map(|r| {
            r["checks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["loc"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// `generate` refuses a world a player cannot finish, so every seed passing means every seed
/// is beatable.
pub(crate) fn fills(b: &Built, options: &HashMap<String, i64>, seeds: u64) {
    let m: serde_json::Value = serde_json::from_str(b.json()).unwrap();
    let inst = b.instance(options).unwrap();
    for players in [1usize, 4] {
        let p: Vec<Player> = (0..players)
            .map(|k| Player { name: format!("P{k}"), instance: inst.clone() })
            .collect();
        for seed in 1..=seeds {
            generate(&m, "s", &p, seed)
                .unwrap_or_else(|e| panic!("{options:?}, {players} players, seed {seed}: {e}"));
        }
    }
}

fn opts(pairs: &[(&str, i64)]) -> HashMap<String, i64> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
}

/// Every `.rando` file of a fixture is already in the layout `fmt` writes.
fn canonical(name: &str) {
    for (path, text) in read_all(&root(name)) {
        if path.ends_with(".rando") {
            assert_eq!(format_text(&text, &path).unwrap(), text, "{name}/{path}");
        }
    }
}

#[test]
fn every_fixture_world_builds_clean_in_canonical_layout_and_fills_beatable() {
    let names = fixtures();
    assert!(names.len() >= 8, "{names:?}");
    for name in names {
        let b = ok(&Dir(root(&name)));
        assert!(b.warnings.is_empty(), "{name}: {:?}", b.warnings);
        assert_eq!(b.meta.as_ref().map(|m| m.name.as_str()), b.manifest["game"].as_str(), "{name}");
        // The committed ledger is what the build writes: no id moved.
        let committed = std::fs::read_to_string(root(&name).join("rando.lock"))
            .unwrap_or_else(|e| panic!("{name}/rando.lock: {e}"));
        assert_eq!(b.ledger.as_deref(), Some(committed.as_str()), "{name}");
        canonical(&name);
        fills(&b, &HashMap::new(), 50);
    }
}

/// What a world is at `options`, less the option values themselves: its per-seed instance (which
/// checks exist, the pool, start items, where progression may go), and what its logic answers
/// for fixed inventories (64 random ones, where inventory `t` holds each copy of a progression
/// item of the pool with chance `t` in 65 by a seeded draw, so they run from nearly empty to
/// nearly full, and every progression copy but one, once per item):
/// which locations are reached and whether the goal holds. An option that changes a check, a
/// rule, the pool, a start item or the goal changes some part; one that does nothing, none.
fn logic(b: &Built, options: &HashMap<String, i64>) -> (serde_json::Value, Vec<(Vec<bool>, bool)>) {
    let w = data_fill::World::load(&b.manifest).unwrap();
    let inst = b.instance(options).unwrap();
    let plan = data_fill::Plan::new(&w, &inst).unwrap();
    let mut held: Vec<u16> = inst
        .pool
        .iter()
        .filter(|i| inst.pool_class.get(*i).is_some_and(|c| c == "progression"))
        .map(|i| w.item(i).unwrap())
        .collect();
    held.sort_unstable();
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut inventories: Vec<Vec<u16>> = (1..=64u64)
        .map(|t| {
            let mut draw = |_: &u16| {
                // xorshift: the same draws every run.
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x % 65 < t
            };
            held.iter().copied().filter(|i| draw(i)).collect()
        })
        .collect();
    // Everything but one copy of each item: where a count at the top (all four companions, the
    // last shard) decides.
    for k in 0..held.len() {
        if k == 0 || held[k] != held[k - 1] {
            let mut less = held.clone();
            less.remove(k);
            inventories.push(less);
        }
    }
    // Fixed placements (`holds`) are collected where they sit.
    let locked = plan.locked_placed(&w);
    let answers = inventories
        .iter()
        .map(|extra| {
            let r = plan.state(&w, extra, &locked);
            let reached =
                (0..u32::try_from(w.locations()).unwrap()).map(|l| r.reached(l)).collect();
            (reached, r.goal())
        })
        .collect();
    let mut seed = b.instance_json(options).unwrap();
    seed.as_object_mut().unwrap().remove("options");
    (seed, answers)
}

/// The genre examples at their defaults and at their options' ends, as an author writes them:
/// each setting has the number of checks its README states, and fills beatable. Each option of a
/// case, set alone, also changes what the world's logic answers ([`logic`]), so an option that
/// changes only rules or the goal is caught doing nothing too.
#[test]
fn the_genre_examples_have_their_check_counts_and_fill_at_their_option_extremes() {
    let cases: &[(&str, &[(&str, &str)], usize)] = &[
        ("meridian_front", &[], 51),
        ("meridian_front", &[("chapters", "2"), ("side_ops", "off")], 18),
        ("meridian_front", &[("chapters", "3"), ("campaign_order", "open")], 39),
        ("cobalt_warrens", &[], 44),
        ("cobalt_warrens", &[("tricks", "advanced"), ("bosses_needed", "0")], 44),
        ("cobalt_warrens", &[("tricks", "basic"), ("start_thruster", "on")], 44),
        ("gloam_delve", &[], 40),
        ("gloam_delve", &[("depths", "5"), ("hardships", "0"), ("wins_needed", "1")], 15),
        ("gloam_delve", &[("depths", "50"), ("hardships", "4"), ("wins_needed", "5")], 80),
        ("oathbound_tale", &[], 46),
        (
            "oathbound_tale",
            &[("shop_slots", "1"), ("safe_shops", "off"), ("party_needed", "4")],
            38,
        ),
        ("oathbound_tale", &[("shop_slots", "5"), ("party_needed", "1")], 54),
        ("bramble_isles", &[], 65),
        ("bramble_isles", &[("marbles_for_summit", "30"), ("pods_per_world", "2")], 45),
        ("bramble_isles", &[("marbles_for_summit", "15"), ("pods_per_world", "8")], 75),
    ];
    for (name, pairs, checks) in cases {
        let b = ok(&Dir(root(name)));
        let options =
            pairs.iter().map(|(k, v)| ((*k).to_owned(), b.option_value(k, v).unwrap())).collect();
        assert_eq!(b.instance(&options).unwrap().locations.len(), *checks, "{name} {pairs:?}");
        fills(&b, &options, 10);
        let default = logic(&b, &HashMap::new());
        for (k, v) in *pairs {
            let alone = HashMap::from([((*k).to_owned(), b.option_value(k, v).unwrap())]);
            assert_ne!(logic(&b, &alone), default, "{name}: {k} = {v} changes nothing");
        }
    }
}

#[test]
fn the_adventure_trial_world_builds_keeps_its_ids_and_confines_its_keys() {
    let b = ok(&Dir(root("cinderfall")));
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
    let m = &b.manifest;
    assert_eq!(
        (m["items"].as_array().unwrap().len(), m["locations"].as_array().unwrap().len()),
        (24, 173)
    );
    assert_eq!(m["regions"].as_array().unwrap().len(), 58);
    // Dungeon keys go only to their own dungeon's checks.
    let inst = b.default_instance().unwrap();
    assert_eq!(inst.item_groups.get("mire_key").map(String::as_str), Some("mire_keys"));
    assert_eq!(inst.location_groups.get("great_chest").map(String::as_str), Some("mire_keys"));
    assert!(!inst.location_groups.contains_key("well_bucket"));
    // The optional dungeon's checks exist only with its option on.
    assert!(!inst.locations.contains(&"core_chest".to_owned()));
    let on = b.instance(&opts(&[("sunken_vault", 1)])).unwrap();
    assert_eq!(on.locations.len(), inst.locations.len() + 9);
}

#[test]
fn the_adventure_trial_world_fills_beatable_at_its_option_extremes() {
    let b = ok(&Dir(root("cinderfall")));
    fills(&b, &opts(&[("difficulty", 0), ("sunken_vault", 1), ("start_lantern", 1)]), 10);
    fills(&b, &opts(&[("difficulty", 2), ("relics_needed", 1)]), 10);
}

/// The catalog's generator, as a tool outside the language would be: it reads
/// `data/songs.csv` and writes `catalog.rando` with its header.
fn catalog(csv: &str, folder: &dyn Files) -> String {
    let mut body = String::from("table song(key: name, tier: int, pack: name) {\n");
    for line in csv.lines().skip(1) {
        let cells: Vec<&str> = line.split(',').collect();
        body += &format!("  {} {} {}\n", cells[0], cells[1], cells[2]);
    }
    body += "}\n\nregion song_select {\n  for s in song {\n    exit {s.key}: has({s.pack}_pack) and count(tier_pass) >= s.tier - 1\n  }\n}\n\n";
    body += "for s in song {\n  region {s.key} {\n    for d in chart where d.focus <= 1 or opt.expert_charts, g in objective {\n      check {s.key}_{d.key}_{g.key}: count(focus_lens) >= d.focus and g.need\n    }\n    event song_cleared: true\n    exit song_select\n  }\n}\n";
    write_generated("catalog-export 1", &[("data/songs.csv", csv)], &[("song", 60)], &body, folder)
        .unwrap()
}

fn neon() -> BTreeMap<String, String> {
    read_all(&root("neon_cadence"))
}

#[test]
fn the_catalog_trial_world_builds_from_its_generated_base_and_overlay() {
    let files = neon();
    // The committed catalog is exactly what its generator writes from the committed CSV, so
    // its header hash is right and the overlay sits on an untouched base.
    let mem = Mem(files.clone());
    assert_eq!(catalog(&files["data/songs.csv"], &mem), files["catalog.rando"]);
    let b = ok(&mem);
    // 64 songs, one cut by the overlay: 63 x 3 charts x 2 objectives, one more check removed,
    // and the lobby's, the encore's and the jukebox's.
    assert_eq!(b.manifest["locations"].as_array().unwrap().len(), 63 * 6 - 1 + 3 + 2 + 1);
    let jukebox = checks(&b, "song_select");
    assert_eq!(jukebox, ["jukebox_secret"]);
    assert!(!checks(&b, "velvet_echo").contains(&"velvet_echo_hard_perfect".to_owned()));
    assert!(checks(&b, "jade_lantern").is_empty());
    // Expert charts exist only with their option on (the family's `where`).
    let inst = b.default_instance().unwrap();
    assert_eq!(inst.locations.len(), 63 * 4 - 1 + 6);
    assert!(!inst.locations.contains(&"amber_drift_expert_clear".to_owned()));
}

#[test]
fn the_catalog_trial_world_fills_beatable_with_its_options_up() {
    let b = ok(&Mem(neon()));
    fills(&b, &opts(&[("expert_charts", 1), ("songs_needed", 48), ("star_target", 60)]), 10);
}

/// Regenerate the catalog from a changed CSV, as its tool would, over the committed ledger:
/// every key that stays keeps its id, a new key gets a new id, and an overlay whose target is
/// gone is an error at the overlay.
#[test]
fn regenerating_the_catalog_keeps_ids_and_a_vanished_target_is_an_error() {
    let files = neon();
    let before = ok(&Mem(files.clone()));
    let ids = |b: &Built| -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(b.ledger.as_deref().unwrap()).unwrap()
    };
    let old = ids(&before);
    let regen = |csv: &str| {
        let mut f = files.clone();
        f.insert("data/songs.csv".into(), csv.to_owned());
        let text = catalog(csv, &Mem(f.clone()));
        f.insert("catalog.rando".into(), text);
        f
    };
    // A song inserted in the middle and the rows reordered: ids follow keys, not positions.
    let csv = &files["data/songs.csv"];
    let mut lines: Vec<&str> = csv.lines().collect();
    lines[1..].reverse();
    lines.insert(30, "teal_reverie,2,glass");
    let changed = regen(&(lines.join("\n") + "\n"));
    let after = ok(&Mem(changed.clone()));
    let new = ids(&after);
    let mut kept = 0;
    for (k, v) in old["checks"].as_object().unwrap() {
        assert_eq!(new["checks"][k]["id"], v["id"], "{k}");
        kept += 1;
    }
    assert_eq!(kept, 63 * 6 - 1 + 6);
    let largest = old["checks"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v["id"].as_i64().unwrap())
        .max()
        .unwrap();
    assert!(new["checks"]["teal_reverie_normal_clear"]["id"].as_i64().unwrap() > largest);
    // The same build twice writes the same ledger.
    assert_eq!(ok(&Mem(changed)).ledger, after.ledger);

    // The tutorial song leaves the catalog: the overlay that replaces its chart fails by name.
    let gone = regen(
        &csv.lines().filter(|l| !l.starts_with("amber_drift,")).collect::<Vec<_>>().join("\n"),
    );
    let errs = errors(&Mem(gone));
    assert!(
        errs.iter().any(|e| e.starts_with("fixes.rando:7:1: ")
            && e.contains("amber_drift.amber_drift_hard_clear")
            && e.contains("overlay.target_missing")),
        "{errs:#?}"
    );

    // A hand edit of the generated file is refused.
    let mut edited = files;
    let text = edited["catalog.rando"].replace("  amber_drift 1 dawn\n", "  amber_drift 2 dawn\n");
    edited.insert("catalog.rando".into(), text);
    assert!(errors(&Mem(edited)).iter().any(|e| e.contains("edited after it was generated")));
}
