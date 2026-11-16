//! Deterministic simulator (doc 05 section 16, doc 02 section 6.3). One seed fixes the whole
//! schedule: commands (checks, repeated cids, acks, hostile input), a lossy and reordering
//! network, receiving clients, crash and rehydrate, a racing second owner with a higher epoch.
//! It drives `room-core` through `MemLogStore` and asserts the single-region invariants.
//!
//! Replay one failing seed: `SIM_SEED=<seed> cargo test -p sim`. Bound a run: `SIM_SEEDS=<n>`.
//! Both are read only by the `config` crate.

mod world;

use config::Limits;
use core::fmt;

pub use world::{Stats, World};

/// Steps per seed.
pub const STEPS: usize = 400;

/// A failed run. Displays the seed and how to replay it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimFailure {
    /// The seed that failed.
    pub seed: u64,
    /// The step (0-based) after which an invariant broke; `STEPS` means the final check.
    pub step: usize,
    /// What broke.
    pub message: String,
}

impl fmt::Display for SimFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sim seed {} failed at step {}: {} (replay: SIM_SEED={} cargo test -p sim)",
            self.seed, self.step, self.message, self.seed
        )
    }
}

impl core::error::Error for SimFailure {}

/// Run one seed. Even seeds use `limits` as given; odd seeds shrink the dedupe window to 50 s so
/// expiry is exercised too.
///
/// # Errors
/// [`SimFailure`] naming the broken invariant.
pub fn run_seed(seed: u64, limits: &Limits) -> Result<Stats, SimFailure> {
    let limits = if seed % 2 == 1 { Limits { dedupe_window_secs: 50, ..*limits } } else { *limits };
    let fail = |step, message| SimFailure { seed, step, message };
    let mut w = World::new(seed, limits).map_err(|m| fail(0, m))?;
    for step in 0..STEPS {
        w.step().map_err(|m| fail(step, m))?;
    }
    w.finish().map_err(|m| fail(STEPS, m))?;
    Ok(w.stats)
}

#[cfg(test)]
mod tests;
