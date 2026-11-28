//! `rando.toml` and `rando pack` (design doc 19; a module of `build.rs`): the one fixed file
//! of a world folder, and the package `rando pack` writes from it.

use world_build::front::{Mem, Options};
use world_build::{Built, build};

use super::lang::{errors, mem, ok};

const WORLD: &str = "world tide \"Tide\" version 3\nstart a\nitem k progression\nregion a {\n  check c\n}\ngoal reach(a)\n";

const TOML: &str = "[world]\nname = \"tide\"\nversion = \"1.2.0\"\nentry = \"src/tide.rando\"\nauthors = [\"A. Author\"]\nlicense = \"MIT\"\ndescription = \"A test world.\"\n";

fn with_toml(toml: &str) -> Mem {
    mem(&[("rando.toml", toml), ("src/tide.rando", WORLD)])
}

fn warnings(b: &Built) -> String {
    b.warnings.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
}

/// `rando.toml` names the entry file (any name, any folder) and the world's metadata.
#[test]
fn rando_toml_names_the_entry_file_and_the_metadata() {
    let b = ok(&with_toml(TOML));
    assert!(b.warnings.is_empty(), "{}", warnings(&b));
    let m = b.meta.as_ref().unwrap();
    assert_eq!((m.name.as_str(), m.version.as_str()), ("tide", "1.2.0"));
    assert_eq!(m.entry, "src/tide.rando");
    assert_eq!(m.authors, ["A. Author"]);
    // Without `entry` the entry file is `world.rando`.
    let plain = mem(&[
        ("rando.toml", "[world]\nname = \"tide\"\nversion = \"0.1.0\"\n"),
        ("world.rando", WORLD),
    ]);
    assert_eq!(ok(&plain).meta.unwrap().entry, "world.rando");
}

/// Every mistake in `rando.toml` is an error in that file that names the key.
#[test]
fn rando_toml_refuses_unknown_keys_bad_versions_and_a_name_that_is_not_the_game_key() {
    let has = |toml: &str, needle: &str| {
        let all = errors(&with_toml(toml)).join("\n");
        assert!(all.contains(needle), "want {needle:?} in {all}");
        assert!(all.contains("rando.toml: "), "{all}");
    };
    has(&format!("{TOML}licence = \"MIT\"\n"), "unknown key `world.licence`");
    has(&format!("{TOML}[extra]\n"), "unknown table or key `extra`");
    has(&TOML.replace("1.2.0", "1.2.0+build.5"), "has build metadata");
    has(&TOML.replace("1.2.0", "1.2"), "is not Semantic Versioning");
    has(
        &TOML.replace("name = \"tide\"", "name = \"other\""),
        "`world.name` `other` is not the game key `tide`",
    );
    has(
        &TOML.replace("src/tide.rando", "../tide.rando"),
        "must be a relative path inside the folder",
    );
    has(
        &format!("{TOML}homepage = \"http://example.com\"\n"),
        "`world.homepage` must be an `https://` URL",
    );
    has("[world\n", "rando.toml: ");
}

/// `check` and `build` only warn when a field `rando pack` requires is missing, so a new world
/// builds before it has a license.
#[test]
fn a_missing_publish_field_is_a_warning_not_an_error() {
    let b = ok(&with_toml(&TOML.replace("license = \"MIT\"\n", "")));
    let w = warnings(&b);
    assert!(w.contains("rando.toml: `world.license` is missing; `rando pack` requires it"), "{w}");
    // `--publish` refuses any warning, so the field is required there too.
    let publish = Options { publish: true, ..Options::default() };
    assert!(
        !build(&with_toml(&TOML.replace("license = \"MIT\"\n", "")), &publish)
            .unwrap()
            .warnings
            .is_empty()
    );
}

fn temp_world(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rando-{tag}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&root));
    for (p, t) in files {
        let p = root.join(p);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    }
    root
}

fn rando(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_rando"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

/// With no folder given, `rando` walks up to the nearest `rando.toml`, as Cargo does; a folder
/// with only `world.rando` still builds, with a warning that shows a minimal `rando.toml`.
#[test]
fn rando_finds_the_world_by_walking_up_and_a_folder_without_rando_toml_warns() {
    let root = temp_world("walk", &[("rando.toml", TOML), ("src/tide.rando", WORLD)]);
    let run = rando(&root.join("src"), &["check"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    std::fs::remove_dir_all(&root).unwrap();
    let old = temp_world("old", &[("world.rando", WORLD)]);
    let run = rando(&old, &["check", "."]);
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{err}");
    assert!(err.contains("warning: ./rando.toml is missing"), "{err}");
    assert!(err.contains("name = \"tide\"\nversion = \"3.0.0\""), "{err}");
    std::fs::remove_dir_all(&old).unwrap();
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000IHDR";

/// A world ready to pack: a module, a table file, an icon and two assets.
fn packable(tag: &str) -> std::path::PathBuf {
    let root = temp_world(
        tag,
        &[
            (
                "rando.toml",
                &format!(
                    "{TOML}icon = \"art/icon.png\"\nrepository = \"https://example.com/tide\"\n\n[assets]\nfiles = [\"art/map.png\", \"CREDITS.txt\"]\n"
                ),
            ),
            (
                "src/tide.rando",
                &WORLD
                    .replace("start a\n", "use more\nstart a\n")
                    .replace("check c\n", "check c\n  exit b\n"),
            ),
            (
                "more.rando",
                "table more(key: name) from \"data/more.csv\"\nregion b {\n  for m in more { check {m.key}_m }\n}\n",
            ),
            ("data/more.csv", "key\nx\n"),
            ("CREDITS.txt", "Made for a test.\n"),
            ("notes.py", "print('never packed')\n"),
        ],
    );
    std::fs::create_dir_all(root.join("art")).unwrap();
    std::fs::write(root.join("art/icon.png"), PNG).unwrap();
    std::fs::write(root.join("art/map.png"), PNG).unwrap();
    root
}

fn entries(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let name = f.name().unwrap().into_owned();
            assert!(f.is_file(), "{name}");
            assert_eq!(f.extra_data().map_or(0, |x| x.len()), 0, "{name} has an extra field");
            assert_eq!(f.unix_mode(), Some(0o100_644), "{name}");
            let mut b = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut b).unwrap();
            (name, b)
        })
        .collect()
}

/// `rando pack` writes the layout of doc 19 section 4.1, with a SHA-256 for every entry, and
/// the same input gives the same bytes.
#[test]
fn pack_writes_the_layout_with_hashes_and_the_same_bytes_every_time() {
    use sha2::Digest;
    let root = packable("pack");
    let dir = world_build::front::Dir(root.clone());
    let a = world_build::pack::pack(&dir, true, &config::Limits::DESIGN)
        .unwrap_or_else(|e| panic!("{e:#?}"));
    let b = world_build::pack::pack(&dir, true, &config::Limits::DESIGN).unwrap();
    assert_eq!(a.bytes, b.bytes, "two packs of one folder differ");
    assert_eq!(a.file_name, "tide-1.2.0.randopack");
    let e = entries(&a.bytes);
    let names: Vec<&str> = e.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "assets/CREDITS.txt",
            "assets/art/icon.png",
            "assets/art/map.png",
            "manifest.json",
            "meta.json",
            "settings.json",
            "sha256.txt",
            "source/data/more.csv",
            "source/more.rando",
            "source/rando.lock",
            "source/rando.toml",
            "source/src/tide.rando",
        ]
    );
    // Every other entry is listed once, sorted by path, with its hash.
    let sums = String::from_utf8(e["sha256.txt"].clone()).unwrap();
    let want: String = e
        .iter()
        .filter(|(p, _)| *p != "sha256.txt")
        .map(|(p, b)| {
            let hex: String = sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect();
            format!("{hex}  {p}\n")
        })
        .collect();
    assert_eq!(sums, want);
    // The manifest is what `build` writes; the lock in the package marks the ids published.
    let built = build(&dir, &Options { publish: true, ..Options::default() }).unwrap();
    assert_eq!(e["manifest.json"], built.json().as_bytes());
    assert_eq!(e["settings.json"], built.settings_schema().unwrap().as_bytes());
    assert_eq!(e["source/rando.lock"], a.lock.as_bytes());
    assert!(a.lock.contains("\"published\":true"), "{}", a.lock);
    // `meta.json` is the normalised `rando.toml`: canonical, defaults filled, asset paths
    // under `assets/`, and the tool version.
    let meta = String::from_utf8(e["meta.json"].clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(world_build::canon::to_string(&v).unwrap(), meta);
    assert_eq!(v["world"]["icon"], "assets/art/icon.png");
    assert_eq!(v["world"]["entry"], "src/tide.rando");
    assert_eq!(
        v["assets"]["files"],
        serde_json::json!(["assets/art/map.png", "assets/CREDITS.txt"])
    );
    assert_eq!(v["tool"], format!("rando {}", env!("CARGO_PKG_VERSION")));
    // Without the source, `source/` is left out.
    let bare =
        entries(&world_build::pack::pack(&dir, false, &config::Limits::DESIGN).unwrap().bytes);
    assert!(bare.keys().all(|k| !k.starts_with("source/")), "{:?}", bare.keys());
    std::fs::remove_dir_all(&root).unwrap();
}

/// `rando pack` refuses what the portal would: a missing publish field, any warning, an asset
/// that is not PNG, WebP or plain text, one over its size cap, and a `LicenseRef` with no
/// `LICENSE` file. Nothing is written when it fails.
#[test]
fn pack_refuses_missing_fields_bad_assets_and_oversized_files() {
    let root = packable("refuse");
    let dir = world_build::front::Dir(root.clone());
    let refused = |needle: &str| {
        let e = world_build::pack::pack(&dir, true, &config::Limits::DESIGN)
            .err()
            .unwrap_or_default()
            .join("\n");
        assert!(e.contains(needle), "want {needle:?} in {e}");
    };
    let toml = std::fs::read_to_string(root.join("rando.toml")).unwrap();
    std::fs::write(root.join("rando.toml"), toml.replace("license = \"MIT\"\n", "")).unwrap();
    refused("`world.license` is missing");
    std::fs::write(root.join("rando.toml"), toml.replace("\"MIT\"", "\"LicenseRef-Mine\""))
        .unwrap();
    refused("needs a LICENSE file");
    std::fs::write(root.join("LICENSE"), "All mine.\n").unwrap();
    let e = entries(&world_build::pack::pack(&dir, true, &config::Limits::DESIGN).unwrap().bytes);
    assert!(e.contains_key("assets/LICENSE"), "{:?}", e.keys());
    std::fs::write(root.join("rando.toml"), &toml).unwrap();
    std::fs::write(root.join("art/map.png"), b"GIF89a\x00\x01").unwrap();
    refused("art/map.png is not a PNG, WebP or plain-text file");
    std::fs::write(root.join("art/map.png"), vec![b'a'; 2 * 1024 * 1024 + 1]).unwrap();
    refused("art/map.png is over 2097152 bytes");
    std::fs::write(root.join("art/map.png"), PNG).unwrap();
    let mut big = PNG.to_vec();
    big.resize(256 * 1024 + 1, 0);
    std::fs::write(root.join("art/icon.png"), big).unwrap();
    refused("art/icon.png is over 262144 bytes");
    std::fs::remove_dir_all(&root).unwrap();
}

/// `rando pack` on the command line writes `<name>-<version>.randopack` and marks the ids
/// published in `rando.lock`; a folder with no `rando.toml` is refused.
#[test]
fn rando_pack_writes_the_package_and_the_published_lock() {
    let root = packable("cli");
    let run = rando(&root, &["pack"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert!(root.join("tide-1.2.0.randopack").is_file());
    assert!(
        std::fs::read_to_string(root.join("rando.lock")).unwrap().contains("\"published\":true")
    );
    let out = root.join("out/x.randopack");
    std::fs::create_dir_all(root.join("out")).unwrap();
    let run = rando(&root, &["pack", ".", "-o", out.to_str().unwrap(), "--no-source"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert!(entries(&std::fs::read(&out).unwrap()).keys().all(|k| !k.starts_with("source/")));
    std::fs::remove_dir_all(&root).unwrap();
    let old = temp_world("cli-old", &[("world.rando", WORLD)]);
    let run = rando(&old, &["pack"]);
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("rando pack requires a rando.toml"));
    assert!(!old.join("rando.lock").exists(), "a refused pack wrote the lock");
    std::fs::remove_dir_all(&old).unwrap();
}

/// The zip settings doc 19 section 3 promises, read back from the entries: the zip epoch as
/// every time, mode 0644, deflate at level 9 (the compressed bytes are exactly what deflate at
/// level 9 makes), and a generated file's non-table source left out of `source/`.
#[test]
fn pack_pins_time_mode_compression_and_the_source_filter() {
    let root = packable("pin");
    // A generated module stamped from a script's input: the build reads `gen/input.json`
    // to check the header, and the package must still leave it out.
    std::fs::create_dir_all(root.join("gen")).unwrap();
    std::fs::write(root.join("gen/input.json"), "{\"rows\": 1}\n").unwrap();
    let dir = world_build::front::Dir(root.clone());
    let made = world_build::front::write_generated(
        "gen 1",
        &[("gen/input.json", "{\"rows\": 1}\n")],
        &[],
        "item extra filler\n",
        &dir,
    )
    .unwrap();
    std::fs::write(root.join("made.rando"), made).unwrap();
    let toml = std::fs::read_to_string(root.join("src/tide.rando")).unwrap();
    std::fs::write(root.join("src/tide.rando"), toml.replace("use more\n", "use more\nuse made\n"))
        .unwrap();
    let limits = config::Limits::DESIGN;
    let a = world_build::pack::pack(&dir, true, &limits).unwrap_or_else(|e| panic!("{e:#?}"));
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(&a.bytes)).unwrap();
    let mut names = Vec::new();
    for i in 0..z.len() {
        let f = z.by_index_raw(i).unwrap();
        let name = f.name().unwrap().into_owned();
        let t = f.last_modified().unwrap();
        assert_eq!(
            (t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second()),
            (1980, 1, 1, 0, 0, 0),
            "{name}"
        );
        assert_eq!(f.unix_mode(), Some(0o100_644), "{name}");
        assert_eq!(f.compression(), zip::CompressionMethod::Deflated, "{name}");
        names.push(name);
    }
    assert!(names.contains(&"source/made.rando".to_owned()), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("input.json")), "{names:?}");
    // Level 9: the stored bytes of the manifest are raw deflate at level 9 of its text.
    let manifest = entries(&a.bytes)["manifest.json"].clone();
    let mut d = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    std::io::Write::write_all(&mut d, &manifest).unwrap();
    let want = d.finish().unwrap();
    let idx = names.iter().position(|n| n == "manifest.json").unwrap();
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut z.by_index_raw(idx).unwrap(), &mut raw).unwrap();
    assert_eq!(raw, want, "manifest.json is not deflate at level 9");
    std::fs::remove_dir_all(&root).unwrap();
}

/// The package caps refuse a pack that passes everything else: too many entries, a path too
/// long, too much source, and a package too large. Each cap is `config::Limits`, set low here.
#[test]
fn pack_refuses_past_the_package_caps() {
    let root = packable("caps");
    let dir = world_build::front::Dir(root.clone());
    let d = config::Limits::DESIGN;
    let refused = |l: config::Limits, needle: &str| {
        let e = world_build::pack::pack(&dir, true, &l).err().unwrap_or_default().join("\n");
        assert!(e.contains(needle), "want {needle:?} in {e}");
    };
    assert!(world_build::pack::pack(&dir, true, &d).is_ok());
    refused(config::Limits { package_max_entries: 10, ..d }, "more than 10 entries");
    refused(
        config::Limits { package_max_path_bytes: 20, ..d },
        "source/src/tide.rando is a path over 20 bytes",
    );
    refused(config::Limits { package_max_source_bytes: 100, ..d }, "the source is over 100 bytes");
    refused(
        config::Limits { package_max_settings_bytes: 50, ..d },
        "settings.json is over 50 bytes",
    );
    refused(config::Limits { package_max_bytes: 1000, ..d }, "the package is over 1000 bytes");
    refused(
        config::Limits { world_assets_max_bytes: 20, ..d },
        "the assets are over 20 bytes in all",
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// `rando.toml`'s caps come from `config::Limits` too, and an asset listed twice is refused.
#[test]
fn rando_toml_caps_are_limits_and_a_repeated_asset_is_refused() {
    let d = config::Limits::DESIGN;
    let has = |toml: &str, l: &config::Limits, needle: &str| {
        let e = world_build::meta::parse(toml, l).unwrap_err().join("\n");
        assert!(e.contains(needle), "want {needle:?} in {e}");
    };
    assert!(world_build::meta::parse(TOML, &d).is_ok());
    has(TOML, &config::Limits { world_name_max_bytes: 3, ..d }, "`world.name`");
    for name in ["_", "__", "_9", "a.b"] {
        has(
            &TOML.replace("name = \"tide\"", &format!("name = \"{name}\"")),
            &d,
            "`world.name` is required",
        );
    }
    has(TOML, &config::Limits { world_version_max_bytes: 3, ..d }, "`world.version`");
    has(TOML, &config::Limits { world_authors_max: 0, ..d }, "`world.authors` takes 1 to 0");
    has(TOML, &config::Limits { world_author_max_chars: 3, ..d }, "1 to 3 characters");
    has(TOML, &config::Limits { world_description_max_chars: 3, ..d }, "1 to 3 characters");
    has(TOML, &config::Limits { world_license_max_bytes: 2, ..d }, "`world.license`");
    has(TOML, &config::Limits { world_toml_max_bytes: 10, ..d }, "is over 10 bytes");
    let url = format!("{TOML}homepage = \"https://example.com/a\"\n");
    has(&url, &config::Limits { world_url_max_bytes: 10, ..d }, "at most 10 bytes");
    let twice = format!("{TOML}\n[assets]\nfiles = [\"a.png\", \"a.png\"]\n");
    has(&twice, &d, "asset \"a.png\" is listed twice");
    has(&twice, &config::Limits { world_assets_max: 1, ..d }, "more than 1 files");
}

/// With no `-o`, the package is written beside `rando.toml`, wherever `rando` was run from.
#[test]
fn the_package_lands_beside_rando_toml() {
    let root = packable("where");
    let run = rando(&root.join("src"), &["pack"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert!(root.join("tide-1.2.0.randopack").is_file());
    assert!(!root.join("src/tide-1.2.0.randopack").exists());
    std::fs::remove_dir_all(&root).unwrap();
}

/// `rando.toml`'s `name` is checked by the game-key rule of the `world` line (`-` and a
/// leading digit allowed), not the identifier rule, so the two never disagree. The `.rando`
/// world line cannot write such a key today (its key is an identifier token), so this is
/// checked on `rando.toml` alone.
#[test]
fn rando_toml_name_follows_the_game_key_rule() {
    for key in ["tide-well", "9lives", "tide"] {
        let toml = TOML.replace("name = \"tide\"", &format!("name = \"{key}\""));
        let m = world_build::meta::parse(&toml, &config::Limits::DESIGN)
            .unwrap_or_else(|e| panic!("{key}: {e:#?}"));
        assert_eq!(m.name, key);
    }
    for bad in ["Tide", "__", "tide well", "-9"] {
        let toml = TOML.replace("name = \"tide\"", &format!("name = \"{bad}\""));
        let e = world_build::meta::parse(&toml, &config::Limits::DESIGN).unwrap_err().join("\n");
        assert!(e.contains("`world.name` is required"), "{bad}: {e}");
    }
}

/// `settings.json` (doc 20 section 5): every option with its kind, values or bounds, default,
/// scope (`seed` when none is written) and label, then the item, place and door names, in
/// canonical JSON. Each part after an option's type is optional, in the order default, scope,
/// label.
#[test]
fn the_settings_schema_keeps_option_scope_and_label() {
    let body = "option lamp_color: choice(teal, amber) = teal cosmetic \"Lamp color\"\n\
                option hints: toggle knowledge\noption n: range 1..3 = 2 \"Count\"\n\
                option plain: toggle = on seed\n";
    let b = ok(&mem(&[("rando.toml", TOML), ("src/tide.rando", &format!("{body}{WORLD}"))]));
    let text = b.settings_schema().unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(world_build::canon::to_string(&v).unwrap(), text);
    assert_eq!(
        v,
        serde_json::json!({
            "format": "schema/1", "world": "tide", "version": "1.2.0",
            "options": [
                {"key": "lamp_color", "kind": "choice", "values": ["teal", "amber"],
                 "default": "teal", "scope": "cosmetic", "label": "Lamp color"},
                {"key": "hints", "kind": "toggle", "default": false, "scope": "knowledge"},
                {"key": "n", "kind": "range", "min": 1, "max": 3, "default": 2, "scope": "seed",
                 "label": "Count"},
                {"key": "plain", "kind": "toggle", "default": true, "scope": "seed"},
            ],
            "items": ["k"], "places": ["c"], "doors": [],
        })
    );
    // Without `rando.toml` there is no name or version to put in it.
    assert!(ok(&mem(&[("world.rando", WORLD)])).settings_schema().is_err());
}
