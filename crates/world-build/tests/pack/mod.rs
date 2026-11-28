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
