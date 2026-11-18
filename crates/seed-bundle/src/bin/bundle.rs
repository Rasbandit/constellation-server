//! Seed file converter and decoder for debugging (`docs/seed-bundle-v2.md`). It reads and
//! writes plain files; the server keeps seeds and packages only in its store.
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

/// A package file in a debugging folder: `<hex key>.pkg`. The server never writes these; it
/// keeps packages in its blob store.
fn package_file(dir: &Path, key: &[u8; 32]) -> PathBuf {
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{hex}.pkg"))
}

/// Read a package from a debugging folder and check it against its key.
fn fetch(dir: &Path, key: &[u8; 32]) -> Result<Vec<u8>, seed_bundle::BundleError> {
    let path = package_file(dir, key);
    let bytes = std::fs::read(&path)
        .map_err(|e| seed_bundle::BundleError::Parse(format!("{}: {e}", path.display())))?;
    if seed_bundle::package::key_of(&bytes) != *key {
        return Err(seed_bundle::BundleError::Invalid(format!(
            "{} does not match its key",
            path.display()
        )));
    }
    Ok(bytes)
}

fn encode(input: &Path, out: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let b = seed_bundle::SeedBundle::from_json(&text).map_err(|e| e.to_string())?;
    let hash: [u8; 32] =
        seed_bundle::bundle_hash(&text)?.try_into().map_err(|_| "hash length".to_owned())?;
    let e = seed_bundle::v2::encode(&b, &hash, MAX_BYTES).map_err(|e| e.to_string())?;
    let packages = out.join("packages");
    std::fs::create_dir_all(&packages).map_err(|e| e.to_string())?;
    for f in &e.packages {
        let path = package_file(&packages, &seed_bundle::package::key_of(f));
        std::fs::write(&path, f).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let stem = input.file_stem().map_or("bundle".into(), |s| s.to_string_lossy());
    let path = out.join(format!("{stem}.seed"));
    std::fs::write(&path, &e.seed).map_err(|e| format!("{}: {e}", path.display()))?;
    let seed = seed_bundle::Seed::read(e.seed.clone(), |k| fetch(&packages, k), MAX_BYTES)
        .map_err(|e| e.to_string())?;
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
    let b = seed_bundle::v2::decode(bytes, |k| fetch(packages, k), MAX_BYTES)
        .map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = include_str!("../../../e2e/fixtures/cross-games.json");

    /// A fresh folder for one test.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bundle-cli-{name}-{}", std::process::id()));
        if d.exists() {
            std::fs::remove_dir_all(&d).unwrap();
        }
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cli(args: &[&Path]) -> Result<String, String> {
        run(&args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>())
    }

    fn same_bundle(json: &str) -> bool {
        seed_bundle::SeedBundle::from_json(json).unwrap()
            == seed_bundle::SeedBundle::from_json(V1).unwrap()
    }

    #[test]
    fn a_seed_decodes_back_to_its_v1_file_with_either_packages_folder() {
        let d = scratch("round-trip");
        let input = d.join("cross-games.json");
        std::fs::write(&input, V1).unwrap();
        let out = d.join("out");
        let said = cli(&[Path::new("v2-encode"), &input, &out]).unwrap();
        let seed = out.join("cross-games.seed");
        assert!(said.starts_with(&seed.display().to_string()), "{said}");
        let decode = Path::new("v2-decode");
        assert!(same_bundle(&cli(&[decode, &seed]).unwrap()), "packages beside the seed");
        let moved = d.join("elsewhere");
        std::fs::rename(out.join("packages"), &moved).unwrap();
        assert!(cli(&[decode, &seed]).is_err(), "no packages beside the seed any more");
        let flag = Path::new("--packages");
        assert!(same_bundle(&cli(&[decode, &seed, flag, &moved]).unwrap()), "--packages");
        assert_eq!(cli(&[decode, &seed, Path::new("--pkgs"), &moved]), Err(USAGE.to_owned()));
        assert_eq!(cli(&[Path::new("v2-decod"), &seed]), Err(USAGE.to_owned()));
    }

    #[test]
    fn a_package_that_does_not_match_its_key_is_refused() {
        let d = scratch("tampered");
        let input = d.join("cross-games.json");
        std::fs::write(&input, V1).unwrap();
        let out = d.join("out");
        cli(&[Path::new("v2-encode"), &input, &out]).unwrap();
        let pkg = std::fs::read_dir(out.join("packages")).unwrap().next().unwrap().unwrap().path();
        std::fs::write(&pkg, b"other bytes").unwrap();
        let err = cli(&[Path::new("v2-decode"), &out.join("cross-games.seed")]).unwrap_err();
        assert!(err.contains("does not match its key"), "{err}");
    }
}
