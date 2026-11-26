//! The two jobs the server runs in a child process, so a hostile or huge manifest can only hurt
//! a process that has a memory cap and a wall-clock kill: each takes its input as bytes and
//! gives bytes back (the server pipes them through stdin and stdout).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::prepared::{self, Prepared};
use crate::seed::{Player, checked_name, encode_on, generate_by, generate_on, manifest_game};
use crate::world::World;

/// A fill request less its world: who plays, and the seed's name and random stream.
#[derive(Debug, Deserialize)]
struct Fill {
    seed_name: String,
    players: Vec<Player>,
    rng_seed: u64,
    workers: usize,
    /// How progression is placed; a request without it gets the exact fill.
    #[serde(default)]
    algorithm: crate::room::Algorithm,
    /// Most bytes the seed file may take ([`fill`] only); a request without it gets the design
    /// cap.
    #[serde(default = "seed_max_bytes")]
    seed_max_bytes: u64,
}

const fn seed_max_bytes() -> u64 {
    config::Limits::DESIGN.bundle_max_bytes
}

/// A fill request that carries the manifest to compile.
#[derive(Debug, Deserialize)]
struct Request {
    manifest: Value,
    #[serde(flatten)]
    fill: Fill,
}

/// Run job `job` on `input`: the one list of the jobs a child process takes.
///
/// # Errors
/// What the job refuses, or a job that is not one.
pub fn run(job: &str, input: &[u8]) -> Result<Vec<u8>, String> {
    match job {
        "prepare" => prepare(input),
        "fill" => fill(input),
        "generate" => generate(input),
        "inspect" => inspect(input),
        other => Err(format!("unknown job {other:?}")),
    }
}

/// A job's two-part input or output as one byte string: `head`'s length (4 bytes, little
/// endian), `head` (JSON), then `body` (a prepared world file) to the end.
#[must_use]
pub fn frame(head: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + head.len() + body.len());
    out.extend_from_slice(&u32::try_from(head.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(head);
    out.extend_from_slice(body);
    out
}

/// The head and body of a [`frame`].
///
/// # Errors
/// When the bytes are shorter than the head they announce.
pub fn unframe(framed: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let cut = || "a job frame is cut short".to_owned();
    let (len, rest) = framed.split_first_chunk::<4>().ok_or_else(cut)?;
    rest.split_at_checked(u32::from_le_bytes(*len) as usize).ok_or_else(cut)
}

/// Fill the room a request describes; the seed bundle as JSON text.
///
/// # Errors
/// A message for a request that does not parse, or for what [`crate::seed::generate`] refuses.
pub fn generate(input: &[u8]) -> Result<Vec<u8>, String> {
    let r: Request = serde_json::from_slice(input).map_err(|e| format!("bad fill request: {e}"))?;
    let f = r.fill;
    let bundle =
        generate_by(&r.manifest, &f.seed_name, &f.players, f.rng_seed, f.workers, f.algorithm)?;
    serde_json::to_vec(&bundle).map_err(|e| e.to_string())
}

/// Fill a room on a prepared world: the input is a [`frame`] of the request (JSON:
/// `seed_name`, `players`, `rng_seed`, `workers`, and if not the defaults `algorithm` and
/// `seed_max_bytes`) and the prepared world's file ([`prepare`]). Nothing is compiled. The
/// output is the seed file itself (v2), which names the game's package in the prepared world
/// (`crate::prepared::seed_game`) by key.
///
/// # Errors
/// A message for a frame or request that does not parse, a file that is not a prepared world
/// of this layout, what [`crate::seed::generate`] refuses, or a seed over its cap.
pub fn fill(input: &[u8]) -> Result<Vec<u8>, String> {
    let (head, file) = unframe(input)?;
    fill_on(&Prepared::open(file)?, head)
}

/// [`fill`] on a prepared world already loaded: `head` is the request alone. The work of one
/// seed and nothing else, for a worker that keeps loaded worlds.
///
/// # Errors
/// As [`fill`], less what loading the world refuses.
pub fn fill_on(world: &Prepared, head: &[u8]) -> Result<Vec<u8>, String> {
    let f: Fill = serde_json::from_slice(head).map_err(|e| format!("bad fill request: {e}"))?;
    let bundle = generate_on(world, &f.seed_name, &f.players, f.rng_seed, f.workers, f.algorithm)?;
    encode_on(world, &bundle, f.seed_max_bytes)
}

/// A manifest's game and version, checked.
fn named(m: &Value) -> Result<(&str, String), String> {
    let game = manifest_game(m)?;
    let version = match &m["version"] {
        Value::String(v) => checked_name("version", v)?.to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return Err("the manifest needs a \"version\"".to_owned()),
    };
    Ok((game, version))
}

/// What a check answers with: `{"game", "version", "items", "locations"}` as JSON text.
fn found((game, version): (&str, String), w: &World) -> Result<Vec<u8>, String> {
    let found =
        json!({"game": game, "version": version, "items": w.items(), "locations": w.locations()});
    serde_json::to_vec(&found).map_err(|e| e.to_string())
}

/// Check a manifest (JSON bytes) the way an upload needs: a `game`, a `version`, and it compiles.
/// The answer is `{"game", "version", "items", "locations"}` as JSON text.
///
/// # Errors
/// A message naming what is wrong with it.
pub fn inspect(input: &[u8]) -> Result<Vec<u8>, String> {
    let m: Value = serde_json::from_slice(input).map_err(|e| format!("not valid JSON: {e}"))?;
    found(named(&m)?, &World::load(&m)?)
}

/// Check a manifest as [`inspect`] does and keep what the check compiled: a [`frame`] of
/// [`inspect`]'s answer and the prepared world's file ([`crate::prepared`]), for the caller to
/// store and hand to [`fill`].
///
/// # Errors
/// As [`inspect`].
pub fn prepare(input: &[u8]) -> Result<Vec<u8>, String> {
    let m: Value = serde_json::from_slice(input).map_err(|e| format!("not valid JSON: {e}"))?;
    let name = named(&m)?;
    let (file, world) = prepared::build(&m)?;
    Ok(frame(&found(name, &world)?, &file))
}
