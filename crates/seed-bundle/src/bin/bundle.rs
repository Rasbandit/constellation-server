//! Seed file converter and decoder (`docs/seed-bundle-v2.md`).
//!
//! - `bundle v2-encode <v1.json> <out-dir>`: writes `<out-dir>/<name>.seed` and the game
//!   packages in `<out-dir>/packages/`, and prints the sizes and bit widths.
//! - `bundle v2-decode <seed> [--packages <dir>]`: prints the v1 JSON (packages default to
//!   the `packages` folder beside the seed).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What a seed or one part of it may take: the server's bundle cap, from `config`.
const MAX_BYTES: u64 = config::Limits::DESIGN.bundle_max_bytes;

const USAGE: &str =
    "usage: bundle v2-encode <v1.json> <out-dir> | bundle v2-decode <seed> [--packages <dir>]";

fn encode(input: &Path, out: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let b = seed_bundle::SeedBundle::from_json(&text).map_err(|e| e.to_string())?;
    let hash: [u8; 32] =
        seed_bundle::bundle_hash(&text)?.try_into().map_err(|_| "hash length".to_owned())?;
    let e = seed_bundle::v2::encode(&b, &hash, MAX_BYTES).map_err(|e| e.to_string())?;
    let packages = out.join("packages");
    for c in &e.packages {
        seed_bundle::package::write(&packages, c).map_err(|e| e.to_string())?;
    }
    let stem = input.file_stem().map_or("bundle".into(), |s| s.to_string_lossy());
    let path = out.join(format!("{stem}.seed"));
    std::fs::write(&path, &e.seed).map_err(|e| format!("{}: {e}", path.display()))?;
    let seed = seed_bundle::load(e.seed.clone(), out, MAX_BYTES)?;
    let (wi, wr, wf) = seed.bit_widths();
    Ok(format!(
        "{} bytes={} json_bytes={} item_bits={wi} receiver_bits={wr} flag_bits={wf} packages={}",
        path.display(),
        e.seed.len(),
        text.len(),
        e.packages.len()
    ))
}

fn decode(seed: &Path, packages: &Path) -> Result<String, String> {
    let bytes = std::fs::read(seed).map_err(|e| format!("{}: {e}", seed.display()))?;
    let b = seed_bundle::v2::decode(bytes, packages, MAX_BYTES).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&b).map_err(|e| e.to_string())
}

fn run(args: &[String]) -> Result<String, String> {
    match args {
        [cmd, input, out] if cmd == "v2-encode" => encode(Path::new(input), Path::new(out)),
        [cmd, seed] if cmd == "v2-decode" => {
            let dir = Path::new(seed).parent().unwrap_or_else(|| Path::new(".")).join("packages");
            decode(Path::new(seed), &dir)
        }
        [cmd, seed, flag, dir] if cmd == "v2-decode" && flag == "--packages" => {
            decode(Path::new(seed), &PathBuf::from(dir))
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
