//! Single home of limits and settings (doc 02 3.2, doc 05 6.2 rule 5).
//! Every number here is a DESIGN VALUE from the docs, not a measurement; replace after gate 1 (doc 05 section 20).
#![allow(clippy::disallowed_methods, reason = "this crate is the one place env vars are read")]
// `Limits` is needed by no_std core crates; only `load` needs std.
#![cfg_attr(not(feature = "std"), no_std)]

/// Runtime limits, all with design-value defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Per-connection memory design value (doc 05 20, weeks 11 to 12 replace it with a measurement).
    pub conn_mem_bytes: usize,
    /// Dedupe window for client message ids, 24 h (D-13).
    pub dedupe_window_secs: u64,
    /// How many seeds the bounded simulation test runs (`SIM_SEEDS`). A test setting, not a product limit.
    pub sim_seeds: u64,
    /// When set (`SIM_SEED`), the simulation test replays only this seed.
    pub sim_replay_seed: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            conn_mem_bytes: 32 * 1024,
            dedupe_window_secs: 24 * 60 * 60,
            sim_seeds: 64,
            sim_replay_seed: None,
        }
    }
}

/// The only place the process environment is read.
#[cfg(feature = "std")]
#[must_use]
pub fn load() -> Limits {
    fn num(key: &str) -> Option<u64> {
        std::env::var(key).ok().and_then(|s| s.parse().ok())
    }
    let mut l = Limits::default();
    if let Some(v) = num("CONSTELLATION_DEDUPE_WINDOW_SECS") {
        l.dedupe_window_secs = v;
    }
    if let Some(v) = num("SIM_SEEDS") {
        l.sim_seeds = v;
    }
    l.sim_replay_seed = num("SIM_SEED");
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_design_values() {
        let l = Limits::default();
        assert_eq!(l.conn_mem_bytes, 32768);
        assert_eq!(l.dedupe_window_secs, 86_400);
        assert_eq!(l.sim_seeds, 64);
        assert_eq!(l.sim_replay_seed, None);
    }
}
