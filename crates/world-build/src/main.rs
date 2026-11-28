//! `rando`, the world tool (design doc 18 section 5.1; a working name, to be renamed with the
//! file extension):
//!
//! - `rando check [dir] [--allow-edited]`: read and check the world in `dir`, print every error
//!   and warning with its file, line and column.
//! - `rando fmt [dir] [--check]`: rewrite every `.rando` file under `dir` in the canonical
//!   layout; with `--check`, change nothing and fail when a file would change.
//! - `rando build [dir] [-o manifest.json] [--instance instance.json] [--allow-edited]
//!   [--publish]`: check, then write the canonical manifest (to standard output without `-o`)
//!   and `rando.lock` back into `dir` (warning loudly when it had none); `--instance` also writes the per-seed instance at the
//!   options' defaults.
//! - `rando generate [dir] [--seed N] [--players N] [--option name=value ...] [--options file]
//!   [-o out/] [--format text|json] [--check-seeds K]`: build in memory with the checks of
//!   `build`, fill a room of N copies with the engine's fill and write `<world>-<seed>.seed` and
//!   a spoiler log into `out/`; `--check-seeds K` fills K seeds instead and reports how it went.
//! - `rando stamp FILE --tool "NAME VERSION" [--source PATH ...] [--expect TABLE>=N ...]`:
//!   write or refresh the `generated` header of FILE (a generator's output in any language),
//!   with the SHA-256 of each source (paths relative to the world's folder, the nearest folder
//!   above FILE with a `world.rando`) and of the content.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use world_build::front::{Dir, LEDGER, Options, format_text};
use world_build::{Error, build};

fn usage() -> ExitCode {
    eprintln!(
        "usage: rando check|fmt|build|generate [dir] [options], or rando stamp FILE --tool NAME [--source PATH] [--expect TABLE>=N]; see docs/world-build.md"
    );
    ExitCode::from(2)
}

fn report(list: &[Error], what: &str) {
    for e in list {
        eprintln!("{what}: {e}");
    }
}

/// Every `.rando` file under `dir`, hidden folders skipped, in path order.
fn rando_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))? {
            let e = e.map_err(|e| e.to_string())?;
            let p = e.path();
            let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            // Links are not followed: one could lead outside the folder, or round in a loop.
            let kind = e.file_type().map_err(|e| e.to_string())?;
            if hidden || kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                todo.push(p);
            } else if p.extension().is_some_and(|x| x == "rando") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn fmt(dir: &Path, check_only: bool) -> ExitCode {
    let files = match rando_files(dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut bad = false;
    for p in files {
        let name = p.strip_prefix(dir).unwrap_or(&p).display().to_string();
        let text = match std::fs::read_to_string(&p) {
            Ok(t) if t.len() <= config::WORLD_SOURCE_MAX_BYTES => t,
            Ok(_) => {
                eprintln!("error: {name} is over {} bytes", config::WORLD_SOURCE_MAX_BYTES);
                bad = true;
                continue;
            }
            Err(e) => {
                eprintln!("error: {name}: {e}");
                bad = true;
                continue;
            }
        };
        match format_text(&text, &name) {
            Err(list) => {
                report(&list, "error");
                bad = true;
            }
            Ok(f) if f == text => {}
            Ok(_) if check_only => {
                eprintln!("{name}: not in canonical layout");
                bad = true;
            }
            Ok(f) => {
                if let Err(e) = std::fs::write(&p, f) {
                    eprintln!("error: {name}: {e}");
                    bad = true;
                }
            }
        }
    }
    if bad { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// `rando stamp`: write or refresh a generated file's header.
fn stamp(args: &[String]) -> ExitCode {
    let (mut file, mut tool) = (None::<PathBuf>, None::<String>);
    let (mut sources, mut expect) = (Vec::<String>::new(), Vec::<(String, i64)>::new());
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--tool" | "--source" | "--expect" => {
                let Some(v) = rest.next() else { return usage() };
                match a.as_str() {
                    "--tool" => tool = Some(v.clone()),
                    "--source" => sources.push(v.clone()),
                    _ => {
                        let parsed = v
                            .split_once(">=")
                            .and_then(|(t, n)| Some((t.trim().to_owned(), n.trim().parse().ok()?)));
                        let Some(e) = parsed else {
                            eprintln!("error: --expect takes TABLE>=N, not `{v}`");
                            return ExitCode::from(2);
                        };
                        expect.push(e);
                    }
                }
            }
            s if s.starts_with('-') => return usage(),
            s => file = Some(PathBuf::from(s)),
        }
    }
    let (Some(file), Some(tool)) = (file, tool) else { return usage() };
    // The world's folder: the nearest folder above the file with an entry file.
    let Some(root) = file
        .ancestors()
        .skip(1)
        .find(|d| d.join(world_build::front::ENTRY).is_file())
        .map(Path::to_path_buf)
    else {
        eprintln!("error: no folder above {} has a `world.rando`", file.display());
        return ExitCode::FAILURE;
    };
    let folder = Dir(root);
    let mut texts = Vec::new();
    for p in &sources {
        match world_build::front::Files::read(&folder, p) {
            Some(Ok(t)) => texts.push((p.as_str(), t)),
            Some(Err(e)) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
            None => {
                eprintln!("error: source `{p}` does not exist in the world's folder");
                return ExitCode::FAILURE;
            }
        }
    }
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {}: {e}", file.display());
            return ExitCode::FAILURE;
        }
    };
    let srcs: Vec<(&str, &str)> = texts.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let exp: Vec<(&str, i64)> = expect.iter().map(|(t, n)| (t.as_str(), *n)).collect();
    match world_build::front::stamp(&text, &tool, &srcs, &exp, &folder) {
        Ok(out) => match write_file(&file, out.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Err(list) => {
            report(&list, "error");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first() else { return usage() };
    if verb == "stamp" {
        return stamp(&args[1..]);
    }
    if verb == "generate" && !cfg!(feature = "engine") {
        eprintln!(
            "error: this rando was built without the engine (feature `engine`), so it cannot generate seeds; build it with default features"
        );
        return ExitCode::FAILURE;
    }
    let mut dir = PathBuf::from(".");
    let (mut out, mut instance) = (None::<PathBuf>, None::<PathBuf>);
    let (mut opts, mut check_only) = (Options::default(), false);
    #[cfg(feature = "engine")]
    let (mut g, mut gen_flags) = (Gen::default(), false);
    #[cfg(not(feature = "engine"))]
    let gen_flags = false;
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            #[cfg(feature = "engine")]
            "--seed" | "--players" | "--option" | "--options" | "--format" | "--check-seeds" => {
                let Some(v) = rest.next() else { return usage() };
                gen_flags = true;
                if let Err(e) = g.set(a, v) {
                    eprintln!("error: {e}");
                    return ExitCode::from(2);
                }
            }
            "-o" | "--instance" => {
                let Some(p) = rest.next() else { return usage() };
                if a == "-o" {
                    out = Some(PathBuf::from(p));
                } else {
                    instance = Some(PathBuf::from(p));
                }
            }
            "--allow-edited" => opts.allow_edited = true,
            "--publish" => opts.publish = true,
            "--check" => check_only = true,
            s if s.starts_with('-') => return usage(),
            s => dir = PathBuf::from(s),
        }
    }
    // A flag of one command given to another is a mistake, not something to ignore.
    if (gen_flags && verb != "generate")
        || (verb == "generate" && (instance.is_some() || check_only))
    {
        return usage();
    }
    if verb == "fmt" {
        return fmt(&dir, check_only);
    }
    if !matches!(verb.as_str(), "check" | "build" | "generate") {
        return usage();
    }
    let built = match build(&Dir(dir.clone()), &opts) {
        Ok(b) => b,
        Err(list) => {
            report(&list, "error");
            return ExitCode::FAILURE;
        }
    };
    report(&built.warnings, "warning");
    if opts.publish && !built.warnings.is_empty() {
        eprintln!("error: publishing refuses a world with warnings");
        return ExitCode::FAILURE;
    }
    if verb == "check" {
        return ExitCode::SUCCESS;
    }
    #[cfg(feature = "engine")]
    if verb == "generate" {
        return generate(&built, &g, out.as_deref());
    }
    let write = |p: &Path, text: &str| write_file(p, text.as_bytes());
    let mut steps = Vec::new();
    match &out {
        Some(p) => steps.push(write(p, built.json())),
        None => print!("{}", built.json()),
    }
    if let Some(p) = &instance {
        steps.push(
            built
                .instance_json(&std::collections::HashMap::new())
                .and_then(|i| world_build::canon::to_string(&i))
                .and_then(|t| write(p, &t)),
        );
    }
    // The ledger last, and only when the rest is written: a failed build records and publishes
    // no ids. A manifest written before a failed `--instance` keeps its ids, which the next
    // build gives again, since the ledger did not move.
    if steps.iter().all(Result::is_ok)
        && let Some(l) = &built.ledger
    {
        let lock = dir.join(LEDGER);
        let fresh = std::fs::symlink_metadata(&lock).is_err();
        steps.push(write(&lock, l));
        if fresh && steps.iter().all(Result::is_ok) {
            eprintln!(
                "warning: there was no {LEDGER}, so new ids were assigned to every item and check and written to {}: commit it, and never delete it. Seeds and clients carry these ids, so a published world must keep them.",
                lock.display()
            );
        }
    }
    let failed: Vec<String> = steps.into_iter().filter_map(Result::err).collect();
    failed.iter().for_each(|e| eprintln!("error: {e}"));
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// What `rando generate` was asked for.
#[cfg(feature = "engine")]
#[derive(Default)]
struct Gen {
    seed: Option<u64>,
    players: Option<u32>,
    /// `name=value` pairs, in the order given (`--options` files first where they came first).
    options: Vec<(String, String)>,
    json: bool,
    check_seeds: Option<u64>,
}

#[cfg(feature = "engine")]
impl Gen {
    fn set(&mut self, flag: &str, v: &str) -> Result<(), String> {
        let num = |what: &str| {
            v.parse::<u64>().map_err(|_| format!("{what} takes a whole number, not `{v}`"))
        };
        match flag {
            "--seed" => {
                let n = Some(num("--seed")?).filter(|n| *n <= config::SEED_NUMBER_MAX);
                self.seed = Some(n.ok_or_else(|| {
                    format!("--seed takes a whole number from 0 to {}", config::SEED_NUMBER_MAX)
                })?);
            }
            "--players" => {
                let n = u32::try_from(num("--players")?).ok().filter(|n| {
                    *n >= 1 && usize::try_from(*n).is_ok_and(|n| n <= config::ROOM_MAX_PLAYERS)
                });
                self.players = Some(n.ok_or_else(|| {
                    format!("--players takes a number from 1 to {}", config::ROOM_MAX_PLAYERS)
                })?);
            }
            "--check-seeds" => {
                let n = Some(num("--check-seeds")?).filter(|n| *n >= 1);
                self.check_seeds = Some(n.ok_or("--check-seeds takes a number of at least 1")?);
            }
            "--format" => match v {
                "json" => self.json = true,
                "text" => self.json = false,
                _ => return Err(format!("--format is text or json, not `{v}`")),
            },
            "--option" => self.options.push(pair(v)?),
            _ => {
                // An options file: one `name = value` per line, `#` starts a comment.
                let text = std::fs::read_to_string(v).map_err(|e| format!("{v}: {e}"))?;
                for l in text
                    .lines()
                    .map(|l| l.split('#').next().unwrap_or("").trim())
                    .filter(|l| !l.is_empty())
                {
                    self.options.push(pair(l).map_err(|e| format!("{v}: {e}"))?);
                }
            }
        }
        Ok(())
    }
}

#[cfg(feature = "engine")]
fn pair(v: &str) -> Result<(String, String), String> {
    let (k, x) =
        v.split_once('=').ok_or_else(|| format!("an option is `name=value`, not `{v}`"))?;
    Ok((k.trim().to_owned(), x.trim().to_owned()))
}

/// A seed when none was asked for: from the process's random hash keys, printed so the seed
/// can be made again.
#[cfg(feature = "engine")]
fn random_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    // `SEED_NUMBER_MAX` is 2^53 - 1, all ones, so the mask keeps the number in range.
    std::collections::hash_map::RandomState::new().build_hasher().finish() & config::SEED_NUMBER_MAX
}

#[cfg(feature = "engine")]
fn generate(built: &world_build::Built, g: &Gen, out: Option<&Path>) -> ExitCode {
    use world_build::generate::{Generator, spoiler_text};
    let mut options = std::collections::HashMap::new();
    for (k, v) in &g.options {
        match built.option_value(k, v) {
            Ok(x) => {
                options.insert(k.clone(), x);
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let generator = match Generator::new(built) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let players = g.players.unwrap_or(1);
    if let Some(count) = g.check_seeds {
        let from = g.seed.unwrap_or(1);
        let c = match generator.check_seeds(&options, players, from, count) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        };
        let last = from + (c.tried - 1);
        let mut ms: Vec<f64> = c.times.iter().map(|t| t.as_secs_f64() * 1000.0).collect();
        ms.sort_by(f64::total_cmp);
        let s = if players == 1 { "" } else { "s" };
        println!(
            "{} of {} seeds fill (seeds {from} to {last}, {players} player{s})",
            c.filled, c.tried
        );
        println!(
            "fill attempts over the seeds that filled: {} (restarts: {})",
            c.attempts, c.restarts
        );
        if let (Some(lo), Some(hi)) = (ms.first(), ms.last()) {
            println!(
                "time per seed (fill, spoiler and seed file; measured): fastest {lo:.0} ms, median {:.0} ms, slowest {hi:.0} ms",
                ms[ms.len() / 2]
            );
        }
        return match c.first_failure {
            None => ExitCode::SUCCESS,
            Some((seed, e)) => {
                eprintln!("error: seed {seed}: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let seed = g.seed.unwrap_or_else(random_seed);
    let made = match generator.run(&options, players, seed) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: seed {seed}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = out.unwrap_or(Path::new("."));
    let spoiler = if g.json {
        match world_build::canon::to_string(&made.spoiler) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        spoiler_text(&made.spoiler)
    };
    let ext = if g.json { "json" } else { "txt" };
    let seed_path = dir.join(format!("{}.seed", made.name));
    let spoiler_path = dir.join(format!("{}-spoiler.{ext}", made.name));
    let written = std::fs::create_dir_all(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))
        .and_then(|()| write_file(&seed_path, &made.file))
        .and_then(|()| {
            // A seed without its spoiler is not left behind.
            write_file(&spoiler_path, spoiler.as_bytes()).map_err(|e| {
                match std::fs::remove_file(&seed_path) {
                    Ok(()) => format!("{e}; the seed file {} was removed", seed_path.display()),
                    Err(r) => format!("{e}; {} is left behind: {r}", seed_path.display()),
                }
            })
        });
    if let Err(e) = written {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    println!("seed {seed}: wrote {} and {}", seed_path.display(), spoiler_path.display());
    ExitCode::SUCCESS
}

/// Write `bytes` to `p`, never through a link (it could lead out of the folder).
fn write_file(p: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{} is a link; not writing through it", p.display()));
    }
    std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}
