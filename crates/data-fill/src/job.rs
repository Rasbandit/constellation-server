//! The two jobs the server runs in a child process, so a hostile or huge manifest can only hurt
//! a process that has a memory cap and a wall-clock kill: [`prepare`] checks an uploaded
//! manifest and keeps what it compiled, [`fill`] fills a room on that. Each takes its input as
//! bytes and gives bytes back (the server pipes them through stdin and stdout).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::prepared::{self, Prepared, SEED_MAX_BYTES};
use crate::room::Algorithm;
use crate::seed::{FillReport, Player, checked_name, encode_on, generate_on, manifest_game};
use crate::world::World;

/// How [`fill`]'s refusal starts when the prepared world it was given does not load (made by
/// another engine, or damaged): the caller prepares the world again and asks once more.
pub const UNLOADABLE: &str = "the prepared world does not load: ";

/// The head of a fill request: who plays (`P`: a name and an instance, typed as [`Player`]
/// where it is read), and the seed's name and random stream. The one definition for the side
/// that writes it and the side that reads it; a field the reader does not know is refused.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fill<P> {
    /// The seed's name.
    pub seed_name: String,
    /// The players, in slot order.
    pub players: Vec<P>,
    /// The seed's random stream.
    pub rng_seed: u64,
    /// Threads the fill may use.
    pub workers: usize,
    /// How progression is placed; a request without it gets the exact fill.
    #[serde(default)]
    pub algorithm: Algorithm,
    /// Most bytes the seed file may take; a request without it gets the design cap.
    #[serde(default = "seed_max_bytes")]
    pub seed_max_bytes: u64,
}

const fn seed_max_bytes() -> u64 {
    SEED_MAX_BYTES
}

impl<P: Serialize> Fill<P> {
    /// This request on the prepared world `file`, as [`fill`] reads it.
    ///
    /// # Errors
    /// A request that has no JSON form, or is too long for a frame.
    pub fn framed(&self, file: &[u8]) -> Result<Vec<u8>, String> {
        frame(&serde_json::to_vec(self).map_err(|e| e.to_string())?, file)
    }
}

/// Run job `job` on `input`: the one list of the jobs a child process takes.
///
/// # Errors
/// What the job refuses, or a job that is not one.
pub fn run(job: &str, input: &[u8]) -> Result<Vec<u8>, String> {
    match job {
        "prepare" => prepare(input),
        "fill" => fill(input),
        other => Err(format!("unknown job {other:?}")),
    }
}

/// A job's two-part input or output as one byte string: `head`'s length (4 bytes, little
/// endian), `head` (JSON), then `body` (a prepared world file) to the end.
///
/// # Errors
/// A head of 4 GiB or more.
pub fn frame(head: &[u8], body: &[u8]) -> Result<Vec<u8>, String> {
    let len = u32::try_from(head.len()).map_err(|_| "a job frame's head is too long")?;
    let mut out = Vec::with_capacity(4 + head.len() + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(head);
    out.extend_from_slice(body);
    Ok(out)
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

/// Fill a room on a prepared world: the input is a [`frame`] of the request ([`Fill`] as
/// JSON) and the prepared world's file ([`prepare`]). Nothing is compiled. The output is the
/// seed file itself (v2), which names the game's package in the prepared world
/// (`crate::prepared::seed_game`) by key. When the asked mode fell back to the exact fill, one
/// line on stderr says so (stdout is the seed and nothing else).
///
/// # Errors
/// A message for a frame or request that does not parse, a prepared world that does not load
/// (the message then starts with [`UNLOADABLE`]), what [`crate::seed::generate`] refuses, or a
/// seed over its cap.
pub fn fill(input: &[u8]) -> Result<Vec<u8>, String> {
    let (seed, report) = fill_reported(input)?;
    if report.fallbacks > 0 {
        eprintln!("fill: the asked mode fell back to the exact fill: {report:?}");
    }
    Ok(seed)
}

/// [`fill`], and how the progression was placed (whether the asked mode fell back).
///
/// # Errors
/// As [`fill`].
pub fn fill_reported(input: &[u8]) -> Result<(Vec<u8>, FillReport), String> {
    let (head, file) = unframe(input)?;
    let f = request(head)?;
    let world = Prepared::open(file).map_err(|e| format!("{UNLOADABLE}{e}"))?;
    fill_on(&world, &f)
}

/// The request of a fill: `head` ([`Fill`] as JSON), read. Apart from the fill, so a worker
/// can refuse a bad request before it asks for the world.
///
/// # Errors
/// A head that is not a fill request.
pub fn request(head: &[u8]) -> Result<Fill<Player>, String> {
    serde_json::from_slice(head).map_err(|e| format!("bad fill request: {e}"))
}

/// [`fill_reported`] on a world already loaded, for a worker that stays up and keeps its
/// worlds between jobs. The world is only read.
///
/// # Errors
/// As [`fill`], less what the frame, the request's text and loading the world can refuse.
pub fn fill_on(world: &Prepared, f: &Fill<Player>) -> Result<(Vec<u8>, FillReport), String> {
    let (mut bundle, report) =
        generate_on(world, &f.seed_name, &f.players, f.rng_seed, f.workers, f.algorithm)?;
    Ok((encode_on(world, &mut bundle, f.seed_max_bytes)?, report))
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

/// Check a manifest (JSON bytes) the way an upload needs (a `game`, a `version`, and it
/// compiles) and keep what the check compiled: a [`frame`] of the answer, `{"game", "version",
/// "items", "locations"}` as JSON text, and the prepared world's file ([`crate::prepared`]),
/// for the caller to store and hand to [`fill`].
///
/// # Errors
/// A message naming what is wrong with the manifest.
pub fn prepare(input: &[u8]) -> Result<Vec<u8>, String> {
    let m: Value = serde_json::from_slice(input).map_err(|e| format!("not valid JSON: {e}"))?;
    let name = named(&m)?;
    let (file, world) = prepared::build(&m)?;
    frame(&found(name, &world)?, &file)
}
