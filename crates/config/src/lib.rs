//! Single home of limits and settings (doc 02 3.2, doc 05 6.2 rule 5).
//! Every number here is a DESIGN VALUE from the docs, not a measurement; replace after gate 1 (doc 05 section 20).
#![allow(clippy::disallowed_methods, reason = "this crate is the one place env vars are read")]

use std::env;

/// Runtime limits, all with design-value defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Per-connection memory design value (doc 05 20, weeks 11 to 12 replace it with a measurement).
    pub conn_mem_bytes: usize,
    /// Dedupe window for client message ids, 24 h (D-13).
    pub dedupe_window_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { conn_mem_bytes: 32 * 1024, dedupe_window_secs: 24 * 60 * 60 }
    }
}

/// The only place the process environment is read.
#[must_use]
pub fn load() -> Limits {
    let mut l = Limits::default();
    if let Some(v) = env::var("CONSTELLATION_DEDUPE_WINDOW_SECS").ok().and_then(|s| s.parse().ok())
    {
        l.dedupe_window_secs = v;
    }
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
    }
}
