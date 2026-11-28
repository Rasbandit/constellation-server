//! `rando check`, `rando settings json` and `rando fmt` on settings files (`.randoset`,
//! design doc 20 section 8.2), a module of `build.rs`: the world is found by `--world` (a
//! folder or a `.randopack`), by the header's `from`, or by walking up to `rando.toml`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cinderfall")
}

fn rando(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rando")).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A fresh folder for one test, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rando-settings-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&p));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, rel: &str, t: &str) -> String {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, t).unwrap();
        p.to_str().unwrap().to_owned()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

#[test]
fn check_reports_each_error_with_its_place_and_help() {
    let t = Temp::new("check");
    let f = t
        .write("mira.randoset", "settings for cinderfall\ndifficulty = expret\nrelics_neded = 2\n");
    let world = fixture();
    let o = rando(&["check", &f, "--world", world.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(err.contains("mira.randoset:2:14: 'Difficulty' can be casual, standard or expert. Your file says 'expret'."), "{err}");
    assert!(err.contains("help: did you mean `expert`?"), "{err}");
    assert!(
        err.contains("mira.randoset:3:1: Cinderfall has no setting called 'relics_neded'."),
        "{err}"
    );
    let ok = t.write("ok.randoset", "settings for cinderfall\ndifficulty = expert\n");
    let o = rando(&["check", &ok, "--world", world.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o.stderr));
}

#[test]
fn settings_json_prints_canonical_json_and_the_hash() {
    let t = Temp::new("json");
    let f = t.write(
        "mira.randoset",
        "settings for cinderfall\nplayer \"Mira\"\nrelics_needed = between(1, 3)\n",
    );
    let world = fixture();
    let o = rando(&["settings", "json", &f, "--world", world.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    let mut lines = out.lines();
    let json: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(json["surprise"]["relics_needed"], serde_json::json!({"between": [1, 3]}));
    assert_eq!(json["options"]["difficulty"], "standard");
    assert_eq!(
        (json["world"].as_str(), json["version"].as_str()),
        (Some("cinderfall"), Some("1.0.0"))
    );
    assert_eq!(lines.next().unwrap().len(), 64);
}

#[test]
fn the_world_is_found_by_from_and_by_walking_up() {
    let t = Temp::new("find");
    copy_dir(&fixture(), &t.0.join("cinderfall"));
    let preset = t.write(
        "cinderfall/presets/quick.randoset",
        "preset quick for cinderfall\nrelics_needed = 1\n",
    );
    // A preset inside the world's folder finds it by walking up.
    let o = rando(&["check", &preset]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    // A player file finds it by `from`, and its preset in the world's `presets/`.
    let f = t.write("mira.randoset", "settings for cinderfall from \"cinderfall\"\npreset quick\n");
    let o = rando(&["settings", "json", &f]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("\"relics_needed\":1"), "{}", text(&o.stdout));
    // With nowhere to look, the error says what to do.
    let lost = t.write("lost/ana.randoset", "settings for cinderfall\n");
    let o = rando(&["check", &lost]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(err.contains("ana.randoset: No world 'cinderfall' was found"), "{err}");
    assert!(err.contains("--world"), "{err}");
    // A world by another name is refused.
    let other = t.write("other.randoset", "settings for tidewell from \"cinderfall\"\n");
    let o = rando(&["check", &other]);
    assert!(
        text(&o.stderr)
            .contains("This file is for 'tidewell', and the world found is 'cinderfall'."),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn a_randopack_serves_as_the_world() {
    let t = Temp::new("pack");
    copy_dir(&fixture(), &t.0.join("w"));
    t.write("w/LICENSE", "All rights reserved.\n");
    let pack = t.0.join("w.randopack");
    let o = rando(&["pack", t.0.join("w").to_str().unwrap(), "-o", pack.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let f = t.write("mira.randoset", "settings for cinderfall\nsunken_vault = on\n");
    let o = rando(&["check", &f, "--world", pack.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let bad = t.write("bad.randoset", "settings for cinderfall\nsunken_vault = maybe\n");
    let o = rando(&["check", &bad, "--world", pack.to_str().unwrap()]);
    assert!(
        text(&o.stderr).contains("bad.randoset:2:16: 'Include the Sunken Vault' is on or off."),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn fmt_formats_settings_files_too() {
    let t = Temp::new("fmt");
    let f = t.write("mira.randoset", "settings for cinderfall\ndifficulty=expert # mine\n");
    let o = rando(&["fmt", t.0.to_str().unwrap(), "--check"]);
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains("mira.randoset: not in canonical layout"),
        "{}",
        text(&o.stderr)
    );
    let o = rando(&["fmt", t.0.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "settings for cinderfall\n\ndifficulty = expert # mine\n"
    );
    let o = rando(&["fmt", t.0.to_str().unwrap(), "--check"]);
    assert!(o.status.success());
    t.write("broken.randoset", "settings for cinderfall\nk = [a,\n");
    let o = rando(&["fmt", t.0.to_str().unwrap(), "--check"]);
    assert!(text(&o.stderr).contains("broken.randoset:2:"), "{}", text(&o.stderr));
}
