//! The two jobs the server runs in a child process, so a hostile or huge manifest can only hurt
//! a process that has a memory cap and a wall-clock kill: each takes its input as bytes and
//! gives bytes back (the server pipes them through stdin and stdout).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::seed::{Player, checked_name, generate_by as fill, manifest_game};
use crate::world::World;

/// A fill request: the manifest, who plays it, and the seed's name and random stream.
#[derive(Debug, Deserialize)]
struct Request {
    manifest: Value,
    seed_name: String,
    players: Vec<Player>,
    rng_seed: u64,
    workers: usize,
    /// How progression is placed; a request without it gets the exact fill.
    #[serde(default)]
    algorithm: crate::room::Algorithm,
}

/// Fill the room a request describes; the seed bundle as JSON text.
///
/// # Errors
/// A message for a request that does not parse, or for what [`crate::seed::generate`] refuses.
pub fn generate(input: &[u8]) -> Result<Vec<u8>, String> {
    let r: Request = serde_json::from_slice(input).map_err(|e| format!("bad fill request: {e}"))?;
    let bundle = fill(&r.manifest, &r.seed_name, &r.players, r.rng_seed, r.workers, r.algorithm)?;
    serde_json::to_vec(&bundle).map_err(|e| e.to_string())
}

/// Check a manifest (JSON bytes) the way an upload needs: a `game`, a `version`, and it compiles.
/// The answer is `{"game", "version", "items", "locations"}` as JSON text.
///
/// # Errors
/// A message naming what is wrong with it.
pub fn inspect(input: &[u8]) -> Result<Vec<u8>, String> {
    let m: Value = serde_json::from_slice(input).map_err(|e| format!("not valid JSON: {e}"))?;
    let game = manifest_game(&m)?;
    let version = match &m["version"] {
        Value::String(v) => checked_name("version", v)?.to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return Err("the manifest needs a \"version\"".to_owned()),
    };
    let w = World::load(&m)?;
    let found =
        json!({"game": game, "version": version, "items": w.items(), "locations": w.locations()});
    serde_json::to_vec(&found).map_err(|e| e.to_string())
}
