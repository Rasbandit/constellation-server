//! `rando generate` end to end (a module of `build.rs`): seeds and spoilers are reproducible,
//! the spoiler lists every placement, `--check-seeds` passes on the example and fails clearly
//! on a world that cannot fill, and options are checked by name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use world_build::build;
use world_build::front::{Dir, Options};
use world_build::generate::{Generator, spoiler_text};

fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tidewell")
}

fn rando(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rando")).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A fresh folder of the system's temp folder for one test, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rando-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&p));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn s(&self, rel: &str) -> String {
        self.0.join(rel).to_str().unwrap().to_owned()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// Every file of `dir` by name, with its bytes.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (p.file_name().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap())
        })
        .collect();
    out.sort();
    out
}

/// A world `check` accepts that no seed can fill: every check needs the one key.
const STUCK: &str = "world stuck \"Stuck\" version 1\nstart a\nitem k progression\nitem junk filler\npool fill { junk 1 }\nregion a {\n  check c1: has(k)\n  check c2: has(k)\n}\ngoal has(k)\n";

#[test]
fn the_same_seed_and_options_write_the_same_files() {
    let t = Temp::new("same");
    let ex = example();
    let ex = ex.to_str().unwrap();
    for format in ["text", "json"] {
        let run = |out: &str| {
            let r = rando(&[
                "generate",
                ex,
                "--seed",
                "11",
                "--players",
                "2",
                "--option",
                "start_gift=lamp",
                "--format",
                format,
                "-o",
                out,
            ]);
            assert!(r.status.success(), "{}", text(&r.stderr));
        };
        let (a, b) = (t.s(&format!("a-{format}")), t.s(&format!("b-{format}")));
        run(&a);
        run(&b);
        let (fa, fb) = (files(Path::new(&a)), files(Path::new(&b)));
        assert_eq!(
            fa.len(),
            2,
            "a seed and a spoiler: {:?}",
            fa.iter().map(|f| &f.0).collect::<Vec<_>>()
        );
        assert_eq!(fa, fb, "{format}");
        let ext = if format == "json" { "json" } else { "txt" };
        assert_eq!(fa[0].0, format!("tidewell_isle-11-spoiler.{ext}"));
        assert_eq!(fa[1].0, "tidewell_isle-11.seed");
    }
    // Another seed gives another layout.
    let c = t.s("c");
    assert!(
        rando(&[
            "generate",
            ex,
            "--seed",
            "12",
            "--players",
            "2",
            "--option",
            "start_gift=lamp",
            "-o",
            &c
        ])
        .status
        .success()
    );
    let spoiler = |d: &str, s: u32| {
        std::fs::read(Path::new(d).join(format!("tidewell_isle-{s}-spoiler.txt"))).unwrap()
    };
    assert_ne!(spoiler(&c, 12)[20..], spoiler(&t.s("a-text"), 11)[20..]);
}

#[test]
fn no_seed_given_prints_a_random_one_that_makes_the_same_files_again() {
    let t = Temp::new("random");
    let ex = example();
    let ex = ex.to_str().unwrap();
    let r = rando(&["generate", ex, "-o", &t.s("a")]);
    assert!(r.status.success(), "{}", text(&r.stderr));
    let out = text(&r.stdout);
    let seed: u64 = out
        .strip_prefix("seed ")
        .and_then(|s| s.split(':').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no seed printed: {out}"));
    let again = rando(&["generate", ex, "--seed", &seed.to_string(), "-o", &t.s("b")]);
    assert!(again.status.success());
    assert_eq!(files(&t.0.join("a")), files(&t.0.join("b")));
}

/// Every location of every player is in the spoiler once, with its region, and the text lists
/// each by region and under its item; the play order holds every progression item placed.
#[test]
fn the_spoiler_lists_every_placement() {
    let built = build(&Dir(example()), &Options::default()).unwrap();
    let g = Generator::new(&built).unwrap();
    let opts = HashMap::new();
    let seed = g.run(&opts, 3, 5).unwrap();
    let s = &seed.spoiler;
    let locations = built.default_instance().unwrap().locations;
    let placements = s["placements"].as_array().unwrap();
    assert_eq!(placements.len(), locations.len() * 3);
    for player in ["P1", "P2", "P3"] {
        let mut got: Vec<&str> = placements
            .iter()
            .filter(|p| p["player"] == player)
            .map(|p| p["location"].as_str().unwrap())
            .collect();
        got.sort_unstable();
        let mut want: Vec<&str> = locations.iter().map(String::as_str).collect();
        want.sort_unstable();
        assert_eq!(got, want, "{player}");
    }
    assert!(placements.iter().all(|p| !p["region"].as_str().unwrap().is_empty()), "{s}");
    let t = spoiler_text(s);
    for p in placements {
        let (pl, loc, item, to) = (&p["player"], &p["location"], &p["item"], &p["receiver"]);
        let (pl, loc, item, to) = (
            pl.as_str().unwrap(),
            loc.as_str().unwrap(),
            item.as_str().unwrap(),
            to.as_str().unwrap(),
        );
        assert!(t.contains(&format!("    {loc}: {item} for {to}\n")), "{loc} by region in\n{t}");
        assert!(t.contains(&format!("{pl}: {loc}")), "{loc} by item in\n{t}");
    }
    let progression = placements.iter().filter(|p| p["class"] == "progression").count();
    let order = t.split("Play order").nth(1).unwrap().split("\n\n").next().unwrap();
    assert_eq!(order.matches(" at ").count(), progression, "{order}");
    for want in [
        "Seed 5 ",
        "Options\n  market_stalls: on\n",
        "Shuffle results\n",
        "wardens\n",
        "Fill\n  attempts: ",
    ] {
        assert!(t.contains(want), "{want:?} in\n{t}");
    }
    assert_eq!(s["fill"]["attempts"], seed.report.attempts);
    // In a room of 10 the text lists players in room order: P2 before P10.
    let ten = spoiler_text(&g.run(&opts, 10, 5).unwrap().spoiler);
    let at = |h: &str| ten.find(h).unwrap_or_else(|| panic!("{h} in\n{ten}"));
    assert!(at("  P2 dock\n") < at("  P10 dock\n"));
    let shuffles = &ten[at("Shuffle results")..];
    assert!(shuffles.find("  P2\n").unwrap() < shuffles.find("  P10\n").unwrap(), "{shuffles}");
    // The library refuses what the CLI refuses, before any copy is made.
    assert!(g.run(&opts, 1025, 1).unwrap_err().contains("1 to 1024 players"));
    assert!(g.run(&opts, 1, 1 << 53).is_err());
    assert!(g.check_seeds(&opts, 1, u64::MAX, 2).is_err());
    assert!(g.check_seeds(&opts, 1, 1, 0).is_err());
}

#[test]
fn check_seeds_50_passes_on_the_example_world() {
    let ex = example();
    let r = rando(&["generate", ex.to_str().unwrap(), "--check-seeds", "50"]);
    let out = text(&r.stdout);
    assert!(r.status.success(), "{out}{}", text(&r.stderr));
    assert!(out.starts_with("50 of 50 seeds fill (seeds 1 to 50, 1 player)\n"), "{out}");
    assert!(
        out.contains("fill attempts over the seeds that filled: 50 (restarts: ")
            && out.contains("time per seed (fill, spoiler and seed file; measured)"),
        "{out}"
    );
    assert!(r.stderr.is_empty(), "{}", text(&r.stderr));
    // From `--seed`, at 4 players.
    let r = rando(&[
        "generate",
        ex.to_str().unwrap(),
        "--check-seeds",
        "5",
        "--seed",
        "100",
        "--players",
        "4",
    ]);
    assert!(
        text(&r.stdout).starts_with("5 of 5 seeds fill (seeds 100 to 104, 4 players)\n"),
        "{}",
        text(&r.stdout)
    );
}

/// A world that cannot fill fails `--check-seeds` with a non-zero exit, naming the first
/// failing seed and the stuck item, and writes nothing.
#[test]
fn a_world_that_cannot_fill_fails_check_seeds_clearly() {
    let t = Temp::new("stuck");
    std::fs::write(t.0.join("world.rando"), STUCK).unwrap();
    let r = rando(&["check", &t.s("")]);
    assert!(r.status.success(), "check accepts it: {}", text(&r.stderr));
    let r = rando(&["generate", &t.s(""), "--check-seeds", "4"]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stdout).starts_with("0 of 4 seeds fill"), "{}", text(&r.stdout));
    let err = text(&r.stderr);
    assert!(
        err.starts_with("error: seed 1: the fill could not place item `k` after 3 attempts"),
        "{err}"
    );
    assert!(err.contains("help: "), "{err}");
    // One seed: the same message, and no file is written.
    let r = rando(&["generate", &t.s(""), "--seed", "9", "--players", "2", "-o", &t.s("out")]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("seed 9: the fill could not place P"), "{}", text(&r.stderr));
    assert!(!t.0.join("out").exists());
}

/// When the spoiler cannot be written, the seed written just before it is removed.
#[test]
fn a_seed_is_not_left_without_its_spoiler() {
    let t = Temp::new("half");
    std::fs::create_dir_all(t.0.join("out/tidewell_isle-3-spoiler.txt")).unwrap();
    let ex = example();
    let r = rando(&["generate", ex.to_str().unwrap(), "--seed", "3", "-o", &t.s("out")]);
    assert_eq!(r.status.code(), Some(1));
    assert!(!t.0.join("out/tidewell_isle-3.seed").exists(), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("the seed file"), "{}", text(&r.stderr));
}

#[test]
fn bad_options_are_refused_by_name() {
    let t = Temp::new("opts");
    let ex = example();
    let ex = ex.to_str().unwrap();
    let refused = |args: &[&str], needle: &str| {
        let r = rando(&[&["generate", ex, "-o", &t.s("out")], args].concat());
        assert!(!r.status.success(), "{args:?} passed");
        let err = text(&r.stderr);
        assert!(err.contains(needle), "{args:?}: {err}");
        assert!(!t.0.join("out").exists(), "{args:?} wrote files");
    };
    refused(&["--option", "pearl_count=2"], "unknown option `pearl_count`; the options are");
    refused(
        &["--option", "pearls_needed=9"],
        "option `pearls_needed` cannot be `9`; it takes a whole number from 1 to 3",
    );
    refused(
        &["--option", "start_gift=sword"],
        "option `start_gift` cannot be `sword`; it takes one of none, lamp",
    );
    refused(&["--option", "tricky_climbs=maybe"], "it takes on or off");
    refused(&["--option", "tricky_climbs"], "an option is `name=value`");
    refused(&["--seed", "x"], "--seed takes a whole number");
    refused(&["--players", "0"], "--players takes a number from 1 to 1024");
    refused(&["--players", "1025"], "--players takes a number from 1 to 1024");
    refused(
        &["--seed", "9007199254740992"],
        "--seed takes a whole number from 0 to 9007199254740991",
    );
    refused(
        &["--seed", "9007199254740991", "--check-seeds", "2"],
        "2 seeds from 9007199254740991 go past the largest seed number, 9007199254740991",
    );
    refused(&["--check-seeds", "0"], "--check-seeds takes a number of at least 1");
    refused(&["--format", "yaml"], "--format is text or json");
    std::fs::write(t.0.join("o.txt"), "# options\npearls_needed = 3\nnope = 1\n").unwrap();
    refused(&["--options", &t.s("o.txt")], "unknown option `nope`");
    // A good options file is read, and the spoiler says what was used.
    std::fs::write(
        t.0.join("o.txt"),
        "# options\npearls_needed = 3 # all of them\ntricky_climbs=on\n",
    )
    .unwrap();
    let r = rando(&["generate", ex, "--seed", "1", "--options", &t.s("o.txt"), "-o", &t.s("good")]);
    assert!(r.status.success(), "{}", text(&r.stderr));
    let s = std::fs::read_to_string(t.0.join("good/tidewell_isle-1-spoiler.txt")).unwrap();
    assert!(s.contains("  pearls_needed: 3\n  start_gift: none\n  tricky_climbs: on\n"), "{s}");
    // Flags of `generate` are not taken by the other commands.
    assert!(!rando(&["check", ex, "--seed", "1"]).status.success());
}

/// A world with errors prints exactly what `check` prints, and nothing is written.
#[test]
fn world_errors_print_exactly_as_check_prints_them() {
    let t = Temp::new("errs");
    std::fs::write(t.0.join("world.rando"), STUCK.replace("has(k)\n}", "has(q)\n}")).unwrap();
    let check = rando(&["check", &t.s("")]);
    let generate = rando(&["generate", &t.s(""), "-o", &t.s("out")]);
    assert!(!check.status.success() && !check.stderr.is_empty());
    assert_eq!(generate.status.code(), check.status.code());
    assert_eq!(text(&generate.stderr), text(&check.stderr));
    assert!(!t.0.join("out").exists());
}
