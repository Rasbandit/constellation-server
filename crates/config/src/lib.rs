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
    /// WebSocket read buffer per connection (doc 11 R1: 4 KiB, never the crate default).
    pub ws_read_buffer_bytes: usize,
    /// WebSocket write buffer per connection (doc 11 R1: small).
    pub ws_write_buffer_bytes: usize,
    /// Largest incoming frame payload on the Archipelago-compatible edge, wire or inflated.
    /// The native 64 KiB cap (doc 09) is too small for this protocol: a client may send every
    /// location of a big world in one `LocationChecks`. 1 MiB matches the default message limit
    /// of the Python `websockets` library that servers for this protocol commonly use (that
    /// library's documented default, not measured here).
    pub compat_max_frame_bytes: usize,
    /// Largest incoming message (all fragments, inflated) on the compatible edge.
    pub compat_max_message_bytes: usize,
    /// Idle compressors kept per window size in the shared permessage-deflate pool (doc 11 2.1).
    pub ws_deflate_pool: usize,
    /// Bounded room mailbox depth (doc 11 R3).
    pub room_mailbox: usize,
    /// Bounded per-session queue of published room events (doc 05 8.1: 256).
    pub session_queue: usize,
    /// Largest seed bundle file the server will read.
    pub bundle_max_bytes: u64,
}

impl Limits {
    /// The design values (see each field).
    pub const DESIGN: Self = Self {
        conn_mem_bytes: 32 * 1024,
        dedupe_window_secs: 24 * 60 * 60,
        sim_seeds: 64,
        sim_replay_seed: None,
        ws_read_buffer_bytes: 4 * 1024,
        ws_write_buffer_bytes: 4 * 1024,
        compat_max_frame_bytes: 1024 * 1024,
        compat_max_message_bytes: 1024 * 1024,
        ws_deflate_pool: 256,
        room_mailbox: 1024,
        session_queue: 256,
        bundle_max_bytes: 64 * 1024 * 1024,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DESIGN
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

    /// Doc 11 R1: the socket buffers and message caps are set here, explicitly, so a crate
    /// default never decides them.
    #[test]
    fn socket_buffers_and_caps_are_pinned() {
        let l = Limits::default();
        assert_eq!((l.ws_read_buffer_bytes, l.ws_write_buffer_bytes), (4096, 4096));
        assert_eq!((l.compat_max_frame_bytes, l.compat_max_message_bytes), (1 << 20, 1 << 20));
        assert_eq!((l.room_mailbox, l.session_queue, l.ws_deflate_pool), (1024, 256, 256));
        assert_eq!(l.bundle_max_bytes, 64 << 20);
    }
}
