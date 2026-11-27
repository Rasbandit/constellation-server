//! The `.rando` language end to end (a module of `build.rs`, so the two share `support`) (design doc 18): the example world in its own files builds
//! to the same manifest as its model and fills beatable; parsing, modules, families and
//! tables, `match`, parameters, the ledger, generated headers, overlays and the formatter.

use std::collections::BTreeMap;
use std::path::PathBuf;

use data_fill::seed::{Player, generate};
use world_build::front::{Dir, Mem, Options, format_text, write_generated};
use world_build::{Built, build, build_model};

fn example() -> Dir {
    Dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tidewell"))
}

fn mem(files: &[(&str, &str)]) -> Mem {
    Mem(files.iter().map(|(p, t)| ((*p).to_owned(), (*t).to_owned())).collect::<BTreeMap<_, _>>())
}

fn ok(files: &Mem) -> Built {
    build(files, &Options::default()).unwrap_or_else(|e| {
        panic!("{}", e.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
    })
}

fn errors(files: &Mem) -> Vec<String> {
    build(files, &Options::default())
        .map(|_| ())
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// Some error of `files` starts with `at` (`file:line:col`) and contains `needle`.
fn fails(files: &Mem, at: &str, needle: &str) {
    let all = errors(files);
    assert!(
        all.iter().any(|e| e.starts_with(&format!("{at}: ")) && e.contains(needle)),
        "want {at}: ..{needle}.. in {all:#?}"
    );
}

/// A world: a header, the body given, and enough to build.
fn world(body: &str) -> Mem {
    let text = format!(
        "world t\nstart a\nitem k progression\nitem junk filler\npool fill {{ junk 1 }}\n{body}\ngoal reach(a)\n"
    );
    mem(&[("world.rando", &text)])
}

const HEAD: usize = 5; // lines before `body` in `world`

fn line(n: usize) -> usize {
    HEAD + n
}

fn region(key: &Built, name: &str) -> serde_json::Value {
    key.manifest["regions"].as_array().unwrap().iter().find(|r| r["key"] == name).unwrap().clone()
}

#[test]
fn the_example_world_in_rando_builds_to_the_same_manifest_as_its_model() {
    let from_files = build(&example(), &Options::default()).unwrap();
    let from_model = build_model(&super::support::tidewell(), "w").unwrap();
    assert_eq!(from_files.json(), from_model.json());
    assert!(from_files.warnings.is_empty(), "{:?}", from_files.warnings);
    // Its ledger: ids in the order the files declare them, nothing retired.
    let ledger: serde_json::Value =
        serde_json::from_str(from_files.ledger.as_ref().unwrap()).unwrap();
    assert_eq!(ledger["items"]["oar"], serde_json::json!({"id": 1, "retired": false}));
    assert_eq!(ledger["checks"]["lantern_room"]["id"], 17);
}

/// Build the example from its `.rando` files, load it in the engine, fill seeds 1 to 50 at 1
/// and 4 players: `generate` refuses a world a player cannot finish, so every seed passing
/// means every seed is beatable.
#[test]
fn the_example_world_in_rando_fills_beatable_for_seeds_1_to_50_at_1_and_4_players() {
    let b = build(&example(), &Options::default()).unwrap();
    let m: serde_json::Value = serde_json::from_str(b.json()).unwrap();
    let inst = b.default_instance().unwrap();
    for players in [1usize, 4] {
        let p: Vec<Player> = (0..players)
            .map(|k| Player { name: format!("P{k}"), instance: inst.clone() })
            .collect();
        for seed in 1..=50 {
            generate(&m, "s", &p, seed)
                .unwrap_or_else(|e| panic!("{players} players, seed {seed}: {e}"));
        }
    }
}

#[test]
fn the_example_files_are_already_in_canonical_layout() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tidewell");
    for f in ["world.rando", "items.rando", "regions/coast.rando", "regions/lighthouse.rando"] {
        let text = std::fs::read_to_string(root.join(f)).unwrap();
        assert_eq!(format_text(&text, f).unwrap(), text, "{f}");
    }
}

#[test]
fn every_syntax_error_in_a_file_is_reported_with_its_line_and_column() {
    let files =
        world("region a {\n  check x: has(\n}\noption o toggle\nregion b {\n  check y: k k\n}");
    let all = errors(&files);
    assert_eq!(all.len(), 3, "{all:#?}");
    assert!(
        all[0].starts_with(&format!("world.rando:{}:1: expected a value", line(3))),
        "{all:#?}"
    );
    assert!(all[1].starts_with(&format!("world.rando:{}:10: expected `:`", line(4))), "{all:#?}");
    assert!(
        all[2].starts_with(&format!("world.rando:{}:14: expected `check`", line(6))),
        "{all:#?}"
    );
}

#[test]
fn a_reserved_word_is_a_name_in_backticks() {
    let b = ok(&world(
        "item `group` progression\nregion a {\n  check `check`: has(`group`)\n  check key\n}",
    ));
    assert_eq!(region(&b, "a")["checks"][0]["rule"], serde_json::json!({"has": "group"}));
    fails(&world("item group progression"), &format!("world.rando:{}:6", line(1)), "reserved word");
}

#[test]
fn table_rows_are_one_per_line() {
    let files = world(
        "table t(key: name, n: int) {\n  a 1\n  b\n  c 3 4\n}\nregion a {\n  for r in t { check {r.key}_x }\n}",
    );
    fails(&files, &format!("world.rando:{}:4", line(3)), "row ends before column `n`");
    fails(&files, &format!("world.rando:{}:7", line(4)), "more values than");
}

#[test]
fn modules_load_by_use_and_a_name_needs_the_use_of_its_file() {
    let entry = "world t\nuse items\nuse rooms/a\nstart a\ngoal reach(a)\n";
    let items = "item k progression\nitem junk filler\npool fill { junk 1 }\n";
    let good = mem(&[
        ("world.rando", entry),
        ("items.rando", items),
        ("rooms/a.rando", "use items\nregion a {\n  check x: has(k)\n}\n"),
    ]);
    ok(&good);
    let bad = mem(&[
        ("world.rando", entry),
        ("items.rando", items),
        ("rooms/a.rando", "region a {\n  check x: has(k)\n}\n"),
    ]);
    fails(
        &bad,
        "rooms/a.rando:2:16",
        "`k` is declared in `items.rando`, which this file does not use",
    );
    assert!(errors(&bad).iter().any(|e| e.contains("add `use items`")));
    let missing =
        mem(&[("world.rando", "world t\nuse nowhere\nstart a\nregion a {\n}\ngoal reach(a)\n")]);
    fails(&missing, "world.rando:2:5", "`nowhere.rando` does not exist");
    // A cycle of uses loads each file once.
    let cycle = mem(&[
        ("world.rando", entry),
        ("items.rando", &format!("use rooms/a\n{items}")),
        ("rooms/a.rando", "use items\nregion a {\n  check x: has(k)\n}\n"),
    ]);
    ok(&cycle);
}

#[test]
fn a_family_over_a_table_and_ranges_expands_to_plain_checks() {
    // Doc 18 section 2.6's invented "Card Tower".
    let files = world(
        "option stakes_on: range 1..8 = 4\nitem ash_deck progression\nitem elm_deck progression\nitem joker progression x 12\n\
         table deck(key: name, unlock: rule) {\n  oak true\n  ash has(ash_deck)\n  elm has(elm_deck)\n}\n\
         region a {\n  for d in deck, ante in 1..8, stake in 1..8 where stake <= opt.stakes_on {\n    check {d.key}_ante{ante}_stake{stake}:\n      d.unlock and count(joker) >= max(0, ante - 3)\n  }\n}",
    );
    let b = ok(&files);
    let checks = region(&b, "a")["checks"].as_array().unwrap().clone();
    assert_eq!(checks.len(), 192);
    let rule = |k: &str| checks.iter().find(|c| c["loc"] == k).unwrap()["rule"].clone();
    assert_eq!(
        rule("ash_ante1_stake1"),
        serde_json::json!({"all": [{"has": "ash_deck"}, {"ge": [{"count": "joker"}, 0]}]})
    );
    assert_eq!(
        rule("oak_ante8_stake2"),
        serde_json::json!({"all": [{"true": {}}, {"ge": [{"count": "joker"}, 5]}]})
    );
    assert_eq!(b.default_instance().unwrap().locations.len(), 96);
    // A `where` that folds at build time drops the combinations it rules out.
    let folded =
        world("region a {\n  for i in 1..4, j in 1..4 where j > i {\n    check c{i}_{j}\n  }\n}");
    assert_eq!(region(&ok(&folded), "a")["checks"].as_array().unwrap().len(), 6);
}

#[test]
fn family_errors_name_where_they_come_from() {
    fails(
        &world("region a {\n  for i in [x, x] {\n    check c_{i}\n  }\n}"),
        &format!("world.rando:{}:11", line(3)),
        "`c_x` is already a check",
    );
    fails(
        &world("region a {\n  for i in nope {\n    check c_{i}\n  }\n}"),
        &format!("world.rando:{}:12", line(2)),
        "unknown table `nope`",
    );
    fails(
        &world("region a {\n  for i in 1..3 {\n    check c_{j}\n  }\n}"),
        &format!("world.rando:{}:11", line(3)),
        "`{j}` is not a family variable",
    );
    let big = world("region a {\n  for i in 1..1000, j in 1..1000 {\n    check c{i}_{j}\n  }\n}");
    assert!(errors(&big).iter().any(|e| e.contains("families expand to more than")));
}

#[test]
fn a_table_from_a_csv_file_is_read_with_its_own_line_numbers() {
    let entry = "world t\nstart a\nitem junk filler\nitem gem progression\npool fill { junk 1 }\n\
                 table song(key: name, tier: int, need: rule) from \"data/songs.csv\"\n\
                 region a {\n  for s in song {\n    check {s.key}_clear: s.need and count(gem) >= s.tier\n  }\n}\ngoal reach(a)\n";
    let csv = "key, tier, need\nmorning_bell, 1, true\nriver_run, 2, has(gem)\n";
    let b = ok(&mem(&[("world.rando", entry), ("data/songs.csv", csv)]));
    assert_eq!(region(&b, "a")["checks"].as_array().unwrap().len(), 2);
    let bad = "key, tier, need\nmorning_bell, one, true\nriver_run, 2, has(\n";
    let files = mem(&[("world.rando", entry), ("data/songs.csv", bad)]);
    fails(&files, "data/songs.csv:2:15", "`one` in column `tier` is not a whole number");
    fails(&files, "data/songs.csv:3:19", "the rule ends where a value was expected");
    let header = mem(&[("world.rando", entry), ("data/songs.csv", "key, tier\n")]);
    fails(&header, "data/songs.csv:1:1", "does not name the columns key, tier, need");
}

#[test]
fn match_and_defs_with_parameters_in_the_language() {
    let files = world(
        "option d: choice(easy, normal, hard) = normal\ndef enough(n) = count(k) >= n\n\
         region a {\n  check x: match opt.d {\n    easy => true\n    normal => enough(1)\n    _ => enough(2)\n  }\n}",
    );
    let b = ok(&files);
    assert!(
        b.manifest["defs"].get("enough__1_1").is_some()
            && b.manifest["defs"].get("enough__1_2").is_some()
    );
    fails(
        &world(
            "option d: choice(easy, hard) = easy\nregion a {\n  check x: match opt.d {\n    easy => true\n  }\n}",
        ),
        &format!("world.rando:{}:12", line(3)),
        "does not cover hard",
    );
}

#[test]
fn the_ledger_keeps_ids_across_reordering_renames_and_removals() {
    let v1 = world("region a {\n  check one\n  check two\n  check three\n}");
    let b1 = ok(&v1);
    let ledger = b1.ledger.clone().unwrap();
    let id = |b: &Built, k: &str| {
        b.manifest["locations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["key"] == k)
            .map(|l| l["id"].as_i64().unwrap())
    };
    // Reordered, `two` renamed with `was`, `one` removed, a new check added: every old id kept.
    let mut v2 = world("region a {\n  check three\n  check second was two\n  check four\n}");
    v2.0.insert("ledger.json".into(), ledger);
    let b2 = ok(&v2);
    assert_eq!((id(&b2, "three"), id(&b2, "second")), (id(&b1, "three"), id(&b1, "two")));
    assert_eq!(id(&b2, "four"), Some(4));
    let l2: serde_json::Value = serde_json::from_str(b2.ledger.as_ref().unwrap()).unwrap();
    assert_eq!(l2["checks"]["one"], serde_json::json!({"id": 1, "retired": true}));
    assert!(l2["checks"].get("two").is_none());
    // Bringing `one` back gives its old id; a new name never takes a retired one.
    let mut v3 = world("region a {\n  check fresh\n  check one\n}");
    v3.0.insert("ledger.json".into(), b2.ledger.clone().unwrap());
    let b3 = ok(&v3);
    assert_eq!((id(&b3, "one"), id(&b3, "fresh")), (Some(1), Some(5)));
    // A written key keeps an id when a generator renames a row.
    let mut v4 = world("region a {\n  check renamed key \"row-7\"\n}");
    v4.0.insert(
        "ledger.json".into(),
        "{\"checks\":{\"row-7\":{\"id\":40,\"retired\":false}},\"items\":{},\"world\":\"t\"}"
            .into(),
    );
    assert_eq!(id(&ok(&v4), "renamed"), Some(40));
    // The ledger is checked, not trusted.
    let mut bad = world("region a {\n  check one\n}");
    bad.0.insert(
        "ledger.json".into(),
        "{\"checks\":{\"a\":{\"id\":3},\"b\":{\"id\":3}},\"world\":\"other\"}".into(),
    );
    let all = errors(&bad).join("\n");
    assert!(
        all.contains("two checks have id 3") && all.contains("belongs to world `other`"),
        "{all}"
    );
}

#[test]
fn a_generated_file_refuses_hand_edits_and_keeps_its_promises() {
    let body = "table song(key: name) {\n  a\n  b\n}\ntable more(key: name) from \"data/more.csv\"\nregion a {\n  for s in song { check {s.key}_c }\n  for m in more { check {m.key}_m }\n}\n";
    let (csv, more) = ("whatever", "key\nx\n");
    let entry =
        "world t\nuse songs\nstart a\nitem junk filler\npool fill { junk 1 }\ngoal reach(a)\n";
    let files = |g: &str, src: &str, more: &str| {
        mem(&[
            ("world.rando", entry),
            ("songs.rando", g),
            ("data/in.csv", src),
            ("data/more.csv", more),
        ])
    };
    let folder = files("", csv, more);
    let generated =
        write_generated("songs-export 1", &[("data/in.csv", csv)], &[("song", 2)], body, &folder)
            .unwrap();
    let b = ok(&files(&generated, csv, more));
    assert_eq!(region(&b, "a")["checks"].as_array().unwrap().len(), 3);
    // Edited by hand: an error, a warning with `--allow-edited`, an error again when publishing.
    let edited = generated.replace("  a\n", "  a\n  c\n");
    fails(&files(&edited, csv, more), "songs.rando:5:18", "edited after it was generated");
    let local =
        build(&files(&edited, csv, more), &Options { allow_edited: true, publish: false }).unwrap();
    assert!(local.warnings.iter().any(|w| w.msg.contains("edited after")));
    assert!(
        build(&files(&edited, csv, more), &Options { allow_edited: true, publish: true }).is_err()
    );
    // A table file it reads, and its promises, are covered too; a comment is not an edit.
    fails(
        &files(&generated, csv, "key\nx\ny\n"),
        "songs.rando:5:18",
        "edited after it was generated",
    );
    fails(
        &files(&generated.replace("song >= 2", "song >= 1"), csv, more),
        "songs.rando:5:18",
        "edited after",
    );
    ok(&files(&generated.replace("region a {", "# a note\nregion a {"), csv, more));
    // Its source changed: a warning to regenerate.
    let stale = build(&files(&generated, "changed", more), &Options::default()).unwrap();
    assert!(stale.warnings.iter().any(|w| w.msg.contains("changed after this file was generated")));
    // A promised size it does not reach is an error.
    let short = write_generated("songs-export 1", &[], &[("song", 40)], body, &folder).unwrap();
    fails(
        &files(&short, csv, more),
        "songs.rando:3:10",
        "has 2 rows, and the generator promised at least 40",
    );
}

#[test]
fn overlays_change_a_base_by_name_and_a_missing_target_is_an_error() {
    let base =
        "region a {\n  check x: has(k)\n  check y\n  exit b: has(k)\n}\nregion b {\n  check z\n}\n";
    let entry = "world t\nuse base\nuse tweaks\nstart a\nitem k progression\nitem junk filler\npool fill { junk 1 }\ngoal reach(a)\n";
    let tweaks = "use base\nextend region b {\n  check w: has(k)\n}\nreplace check a.x: true\nreplace exit a.b: true\nremove check a.y\n";
    let b = ok(&mem(&[("world.rando", entry), ("base.rando", base), ("tweaks.rando", tweaks)]));
    let (a, rb) = (region(&b, "a"), region(&b, "b"));
    assert_eq!(a["checks"], serde_json::json!([{"loc": "x", "rule": {"true": {}}}]));
    assert_eq!(a["exits"][0]["rule"], serde_json::json!({"true": {}}));
    assert_eq!(rb["checks"].as_array().unwrap().len(), 2);
    let dangling = "use base\nreplace exit a.cliff: true\n";
    let files = mem(&[("world.rando", entry), ("base.rando", base), ("tweaks.rando", dangling)]);
    fails(&files, "tweaks.rando:2:1", "the base no longer has this exit (overlay.target_missing)");
    // Two overlays replacing one target need one to use the other.
    let entry2 = entry.replace("use tweaks\n", "use tweaks\nuse more\n");
    let one = "use base\nreplace check a.x: true\n";
    let clash = mem(&[
        ("world.rando", &entry2),
        ("base.rando", base),
        ("tweaks.rando", one),
        ("more.rando", one),
    ]);
    fails(&clash, "more.rando:2:1", "neither file uses the other");
    let ordered = mem(&[
        ("world.rando", &entry2),
        ("base.rando", base),
        ("tweaks.rando", one),
        ("more.rando", "use tweaks\nreplace check a.x: false\n"),
    ]);
    assert_eq!(region(&ok(&ordered), "a")["checks"][0]["rule"], serde_json::json!({"false": {}}));
}

#[test]
fn fmt_is_canonical_idempotent_keeps_comments_and_adds_parentheses() {
    let messy = "# top\nworld   t\nregion a{check x:has(k) or has(j) and has(l)   # why\n  exit b door d group g}\n\n\noption o:choice( a,b )=a\n";
    let once = format_text(messy, "m").unwrap();
    assert_eq!(
        once,
        "# top\nworld t\nregion a {\n  check x: has(k) or (has(j) and has(l)) # why\n  exit b door d group g\n}\n\noption o: choice(a, b) = a\n"
    );
    assert_eq!(format_text(&once, "m").unwrap(), once);
    let m = "region a {\n  check x: match opt.d { easy => true hard => has(k) }\n}\n";
    let f = format_text(m, "m").unwrap();
    assert_eq!(
        f,
        "region a {\n  check x: match opt.d {\n    easy => true\n    hard => has(k)\n  }\n}\n"
    );
    assert_eq!(format_text(&f, "m").unwrap(), f);
    assert!(format_text("region a {", "m").is_err());
}

#[test]
fn constructs_this_version_does_not_build_are_named() {
    fails(
        &world("score army { k 1 }"),
        &format!("world.rando:{}:1", line(1)),
        "`score` is not supported yet",
    );
    fails(
        &world("shuffle s {\n  when opt.x\n  slots p, q\n  thing a\n  thing b\n}"),
        &format!("world.rando:{}:3", line(2)),
        "`when` is not supported yet",
    );
}

#[test]
fn doc_16s_smallest_world_in_rando_builds_to_the_same_bytes_as_its_model() {
    let text = "world two_rooms\nstart hall\nitem `key` progression\nregion hall {\n  check shelf\n  exit vault: has(`key`)\n}\nregion vault {\n  check chest\n}\ngoal reach(vault)\n";
    let b = ok(&mem(&[("world.rando", text)]));
    assert_eq!(b.json(), build_model(&super::support::two_rooms(), "w").unwrap().json());
}

#[test]
fn hostile_families_stop_at_their_limits_without_crashing() {
    let huge = world(
        "region a {\n  for i in -9223372036854775807..9223372036854775807 {\n    check c{i}\n  }\n}",
    );
    assert!(errors(&huge).iter().any(|e| e.contains("empty or too long")));
    // Few combinations, many declarations each: the count is of declarations.
    let wide_body: String = (0..200).map(|k| format!("    check c{k}_{{i}}\n")).collect();
    let wide = world(&format!("region a {{\n  for i in 1..99999 {{\n{wide_body}  }}\n}}"));
    assert!(
        errors(&wide)
            .iter()
            .any(|e| e.contains("families expand to more than 100000 declarations"))
    );
    let grants: String = (0..200).map(|_| "  start_item junk\n".to_owned()).collect();
    let granted = world(&format!("for i in 1..99999 {{\n{grants}}}"));
    assert!(errors(&granted).iter().any(|e| e.contains("families expand to more than")));
    let nest = |n: usize| {
        format!("{}region r {{\n  check q\n}}{}", "for i in 1..1 {\n".repeat(n), "}\n".repeat(n))
    };
    assert!(!errors(&world(&nest(31))).iter().any(|e| e.contains("blocks nest")));
    assert!(errors(&world(&nest(32))).iter().any(|e| e.contains("blocks nest more than 32")));
    let deep = format!(
        "{}region r {{\n  check q\n}}{}",
        "for i in 1..1 {\n".repeat(2000),
        "}\n".repeat(2000)
    );
    assert!(errors(&world(&deep)).iter().any(|e| e.contains("blocks nest more than")));
    assert!(format_text(&deep, "d").is_err());
    // One mistake a family repeats is reported once.
    let repeated = world("region a {\n  for i in 1..100 {\n    check c_{nope}\n  }\n}");
    assert_eq!(errors(&repeated).len(), 1);
}

#[test]
fn a_family_where_is_put_in_once() {
    let files = world(
        "option k: choice(blue, red) = red\nregion a {\n  check spare\n  for v in [blue], blue in [red] where opt.k == v {\n    check c_{v}_{blue}\n  }\n}",
    );
    let b = ok(&files);
    assert_eq!(b.default_instance().unwrap().locations, ["spare"]);
}

#[test]
fn declarations_a_family_cannot_repeat_and_misplaced_keywords_are_errors() {
    fails(
        &world("for i in 1..2 {\n  table t(key: name) {\n    a\n  }\n}"),
        &format!("world.rando:{}:3", line(2)),
        "cannot be inside a family",
    );
    fails(
        &world("check q"),
        &format!("world.rando:{}:1", line(1)),
        "`check` belongs inside a region",
    );
    fails(
        &world("item y\nregion b {\n  check z\n}"),
        &format!("world.rando:{}:1", line(2)),
        "an item needs a class",
    );
}

#[test]
fn an_unclosed_block_does_not_hide_the_errors_after_it() {
    let all =
        errors(&world("region x {\n  check c1\nregion b {\n  check c3 ::\n}\ngoal has(k) and"));
    assert!(all.iter().any(|e| e.contains("not closed")), "{all:#?}");
    assert!(all.iter().any(|e| e.starts_with(&format!("world.rando:{}:", line(4)))), "{all:#?}");
    assert!(all.iter().any(|e| e.starts_with(&format!("world.rando:{}:", line(7)))), "{all:#?}");
}

#[test]
fn the_ledger_refuses_renames_and_keys_that_would_move_an_id() {
    let mut w = world("region a {\n  check c3 was c1\n  check c1\n}");
    w.0.insert("ledger.json".into(), "{\"checks\":{\"c1\":{\"id\":1,\"retired\":false},\"c2\":{\"id\":2,\"retired\":false}},\"items\":{},\"world\":\"t\"}".into());
    fails(&w, &format!("world.rando:{}:9", line(2)), "`was c1` names a key that is still declared");
    let mut twice = world("region a {\n  check n1 was gone\n  check n2 was gone\n}");
    twice.0.insert(
        "ledger.json".into(),
        "{\"checks\":{\"gone\":{\"id\":1,\"retired\":false}},\"items\":{},\"world\":\"t\"}".into(),
    );
    assert!(
        errors(&twice)
            .iter()
            .any(|e| e.contains("`was gone` is also written on another declaration"))
    );
    let k = world("region a {\n  check c1 key \"c2\"\n  check c2\n}");
    assert!(errors(&k).iter().any(|e| e.contains("ledger key `c2` is used twice")));
    // A removed check is retired.
    let base = "world t\nuse tweaks\nstart a\nitem junk filler\npool fill { junk 1 }\nregion a {\n  check x\n  check y\n}\ngoal reach(a)\n";
    let b = ok(&mem(&[("world.rando", base), ("tweaks.rando", "remove check a.y\n")]));
    let l: serde_json::Value = serde_json::from_str(b.ledger.as_ref().unwrap()).unwrap();
    assert!(l["checks"].get("y").is_none_or(|e| e["retired"] == true), "{l}");
}

#[test]
fn defs_with_arguments_of_different_kinds_never_share_a_written_def() {
    let files = world(
        "option k: toggle = off\nregion a {\n  check c1: need(opt.k)\n  check c2: need(opt_k)\n  check c3: need(m1)\n  check c4: atleast(-1) or atleast(1)\n  event opt_k: true\n  event m1: true\n}\ndef need(z) = z\ndef atleast(n) = count(k) >= n",
    );
    let b = ok(&files);
    let mut names: Vec<&String> = b.manifest["defs"].as_object().unwrap().keys().collect();
    names.sort();
    assert_eq!(
        names,
        ["atleast__1_1", "atleast__2_1n", "need__2_m1", "need__5_opt_k", "need__6_0opt_k"]
    );
    // Names that start or end with `_` do not run into each other.
    let edges = ok(&world(
        "region a {\n  check c1: two(p_, q) and two(p, _q)\n  event p_: true\n  event q: true\n  event p: true\n  event _q: true\n}\ndef two(x, y) = x and y",
    ));
    assert_eq!(edges.manifest["defs"].as_object().unwrap().len(), 2);
    assert_eq!(
        b.manifest["defs"]["need__5_opt_k"],
        serde_json::json!({"ge": [{"counter": "opt_k"}, 1]})
    );
}

#[test]
fn an_overlay_conflict_covers_remove_and_extend_too() {
    let base = "region a {\n  check x\n}\n";
    let entry = "world t\nuse base\nuse one\nuse two\nstart a\nitem junk filler\npool fill { junk 1 }\ngoal reach(a)\n";
    let clash = mem(&[
        ("world.rando", entry),
        ("base.rando", base),
        ("one.rando", "use base\nreplace check a.x: true\n"),
        ("two.rando", "use base\nremove check a.x\n"),
    ]);
    fails(&clash, "two.rando:2:1", "neither file uses the other");
    let added = mem(&[
        ("world.rando", entry),
        ("base.rando", base),
        ("one.rando", "use base\nextend region a {\n  check n\n}\n"),
        ("two.rando", "use base\nremove check a.n\n"),
    ]);
    fails(&added, "two.rando:2:1", "neither file uses the other");
    let in_family = mem(&[
        ("world.rando", entry),
        ("base.rando", base),
        ("one.rando", "use base\nextend region a {\n  for i in 1..2 {\n    check n{i}\n  }\n}\n"),
        ("two.rando", "use base\nremove check a.n1\n"),
    ]);
    fails(&in_family, "two.rando:2:1", "neither file uses the other");
    let whole = mem(&[
        ("world.rando", entry),
        ("base.rando", "region a {\n  check x\n}\nregion b {\n  check d\n}\n"),
        ("one.rando", "use base\nreplace check b.d: true\n"),
        ("two.rando", "use base\nremove region b\n"),
    ]);
    fails(&whole, "two.rando:2:1", "neither file uses the other");
    let both_extend = mem(&[
        ("world.rando", entry),
        ("base.rando", base),
        ("one.rando", "use base\nextend region a {\n  check n\n}\n"),
        ("two.rando", "use base\nextend region a {\n  check m\n}\n"),
    ]);
    ok(&both_extend);
}

#[test]
fn a_link_out_of_the_world_folder_is_refused() {
    let root = std::env::temp_dir().join(format!("rando-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("w/data")).unwrap();
    std::fs::write(root.join("secret.txt"), "SECRET=1\n").unwrap();
    std::fs::write(root.join("w/world.rando"), "world t\nstart a\nitem junk filler\npool fill { junk 1 }\ntable t(key: name) from \"data/t.csv\"\nregion a {\n  check x\n}\ngoal reach(a)\n").unwrap();
    std::os::unix::fs::symlink(root.join("secret.txt"), root.join("w/data/t.csv")).unwrap();
    let e = build(&Dir(root.join("w")), &Options::default()).unwrap_err();
    let text = e.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(
        text.contains("leads outside the world's folder") && !text.contains("SECRET"),
        "{text}"
    );
    // A link to nothing is refused too, and `build` never writes through a link.
    std::fs::remove_file(root.join("w/data/t.csv")).unwrap();
    std::os::unix::fs::symlink(root.join("nothing.csv"), root.join("w/data/t.csv")).unwrap();
    let e = build(&Dir(root.join("w")), &Options::default()).unwrap_err();
    assert!(e.iter().any(|x| x.msg.contains("leads nowhere")), "{e:?}");
    std::fs::remove_file(root.join("w/data/t.csv")).unwrap();
    std::fs::write(root.join("w/data/t.csv"), "key\nx\n").unwrap();
    std::os::unix::fs::symlink(root.join("outside.json"), root.join("w/ledger.json")).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_rando"))
        .args([
            "build",
            root.join("w").to_str().unwrap(),
            "-o",
            root.join("w/out.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!run.status.success(), "{run:?}");
    assert!(!root.join("outside.json").exists(), "wrote through the ledger link");
    std::fs::remove_dir_all(&root).unwrap();
}
