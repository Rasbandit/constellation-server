//! `rando`, the world tool (design doc 18 section 5.1; a working name, to be renamed with the
//! file extension):
//!
//! - `rando check [dir] [--allow-edited]`: read and check the world in `dir`, print every error
//!   and warning with its file, line and column.
//! - `rando fmt [dir] [--check]`: rewrite every `.rando` file under `dir` in the canonical
//!   layout; with `--check`, change nothing and fail when a file would change.
//! - `rando build [dir] [-o manifest.json] [--instance instance.json] [--allow-edited]
//!   [--publish]`: check, then write the canonical manifest (to standard output without `-o`)
//!   and `ledger.json` back into `dir`; `--instance` also writes the per-seed instance at the
//!   options' defaults.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use world_build::front::{Dir, LEDGER, Options, format_text};
use world_build::{Error, build};

fn usage() -> ExitCode {
    eprintln!("usage: rando check|fmt|build [dir] [options]; see the crate's docs/world-build.md");
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
            let p = e.map_err(|e| e.to_string())?.path();
            let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            if hidden {
                continue;
            }
            if p.is_dir() {
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first() else { return usage() };
    let mut dir = PathBuf::from(".");
    let (mut out, mut instance) = (None::<PathBuf>, None::<PathBuf>);
    let (mut opts, mut check_only) = (Options::default(), false);
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "-o" => out = rest.next().map(PathBuf::from),
            "--instance" => instance = rest.next().map(PathBuf::from),
            "--allow-edited" => opts.allow_edited = true,
            "--publish" => opts.publish = true,
            "--check" => check_only = true,
            s if s.starts_with('-') => return usage(),
            s => dir = PathBuf::from(s),
        }
    }
    if verb == "fmt" {
        return fmt(&dir, check_only);
    }
    if verb != "check" && verb != "build" {
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
    let write =
        |p: &Path, text: &str| std::fs::write(p, text).map_err(|e| format!("{}: {e}", p.display()));
    let mut steps = Vec::new();
    if let Some(l) = &built.ledger {
        steps.push(write(&dir.join(LEDGER), l));
    }
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
    let failed: Vec<String> = steps.into_iter().filter_map(Result::err).collect();
    failed.iter().for_each(|e| eprintln!("error: {e}"));
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
