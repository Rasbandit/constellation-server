//! Fill jobs for the pool's tests (`src/pool/tests.rs` without a process, `tests/pool.rs`
//! through real ones): small synthetic worlds as an upload prepares them, requests on them, and
//! what the one-process path answers.

use data_fill::Instance;
use data_fill::job::{self, Fill};
use data_fill::seed::Player;
use data_fill::synth::Shape;
use log::BlobKey;
use serde_json::json;

/// A small synthetic world as an upload prepares it: the key and file of its prepared world,
/// and a player's instance on it. Each `seed` is another world.
pub fn prepared(shape: &Shape, seed: u64) -> (BlobKey, Vec<u8>, Instance) {
    let (mut manifest, instance) = data_fill::synth::world(shape, seed, 0);
    manifest["game"] = json!(format!("Synth {seed}"));
    manifest["version"] = json!("1");
    let out = job::prepare(manifest.to_string().as_bytes()).unwrap();
    let (_, file) = job::unframe(&out).unwrap();
    (log::blob_key(file), file.to_vec(), instance)
}

pub fn small() -> Shape {
    Shape { checks: 200, ..Shape::default() }
}

/// A fill request for `names` on one instance.
pub fn head(names: &[&str], instance: &Instance, rng: u64) -> Vec<u8> {
    let players = names
        .iter()
        .map(|n| Player { name: (*n).to_owned(), instance: instance.clone() })
        .collect();
    let fill = Fill {
        seed_name: format!("s{rng}"),
        players,
        rng_seed: rng,
        seed_max_bytes: config::Limits::DESIGN.format.bundle_max_bytes,
    };
    serde_json::to_vec(&fill).unwrap()
}

/// What a job child of its own answers to the same request: the one-process path
/// (`sandbox::fill_job` runs exactly this).
pub fn alone(head: &[u8], file: &[u8]) -> Result<Vec<u8>, String> {
    job::run("fill", &job::frame(head, file).unwrap())
}
