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
    /// Chat and bounce messages a session may send in a burst (one shared token bucket).
    pub fanout_burst: u32,
    /// Refill of that bucket, in thousandths of a message per second (2000 = 2 per second).
    pub fanout_refill_milli_per_sec: u32,
    /// Longest chat text, bytes.
    pub chat_max_bytes: usize,
    /// Largest bounce packet as received, bytes (JSON).
    pub bounce_max_bytes: usize,
    /// Data package requests a session may send in a burst (each can be hundreds of KiB).
    pub datapackage_burst: u32,
    /// Refill of that bucket, thousandths per second (100 = one every 10 s).
    pub datapackage_refill_milli_per_sec: u32,
    /// `Connect` packets one client address may send in a burst (each re-reads the log).
    pub connect_burst: u32,
    /// Refill of that bucket, thousandths per second (1000 = one per second).
    pub connect_refill_milli_per_sec: u32,
    /// Most keys (slots, client addresses) a rate table tracks; past it, idle full buckets are
    /// dropped, and a new key is refused while every tracked one is still limited.
    pub rate_table_max_keys: usize,
    /// Open WebSocket connections per process; more get HTTP 503 (`CONSTELLATION_MAX_CONNECTIONS`).
    pub max_connections: usize,
    /// A socket that has not connected to a slot within this long is closed
    /// (`CONSTELLATION_CONNECT_TIMEOUT_MS`).
    pub connect_timeout_ms: u64,
    /// A socket that sends nothing (not even a pong) for this long is closed
    /// (`CONSTELLATION_IDLE_TIMEOUT_MS`).
    pub idle_timeout_ms: u64,
    /// The server pings each socket this often, so a quiet live client answers in time
    /// (`CONSTELLATION_PING_INTERVAL_MS`; keep it under the idle timeout).
    pub ping_interval_ms: u64,
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
        fanout_burst: 10,
        fanout_refill_milli_per_sec: 2000,
        chat_max_bytes: 2048,
        bounce_max_bytes: 16 * 1024,
        datapackage_burst: 4,
        datapackage_refill_milli_per_sec: 100,
        connect_burst: 10,
        connect_refill_milli_per_sec: 1000,
        rate_table_max_keys: 100_000,
        max_connections: 10_000,
        connect_timeout_ms: 30_000,
        idle_timeout_ms: 60_000,
        ping_interval_ms: 20_000,
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
    if let Some(v) = num("CONSTELLATION_MAX_CONNECTIONS").and_then(|v| usize::try_from(v).ok()) {
        l.max_connections = v;
    }
    for (key, field) in [
        ("CONSTELLATION_CONNECT_TIMEOUT_MS", &mut l.connect_timeout_ms),
        ("CONSTELLATION_IDLE_TIMEOUT_MS", &mut l.idle_timeout_ms),
        ("CONSTELLATION_PING_INTERVAL_MS", &mut l.ping_interval_ms),
    ] {
        if let Some(v) = num(key) {
            *field = v;
        }
    }
    l
}

/// Values that must not appear on a command line (visible to every local user in `ps`).
#[cfg(feature = "std")]
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secrets {
    /// Room password: `CONSTELLATION_ROOM_PASSWORD`, or the first line of the file named by
    /// `CONSTELLATION_ROOM_PASSWORD_FILE`. `None` keeps the seed's own.
    pub room_password: Option<String>,
}

#[cfg(feature = "std")]
impl core::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let pw = self.room_password.as_ref().map(|_| "<redacted>");
        f.debug_struct("Secrets").field("room_password", &pw).finish()
    }
}

/// Read [`Secrets`] from the environment.
///
/// # Errors
/// Both password variables set, an unreadable file, or an empty password.
#[cfg(feature = "std")]
pub fn load_secrets() -> Result<Secrets, String> {
    secrets_from(|k| std::env::var(k).ok(), |p| std::fs::read_to_string(p))
}

#[cfg(feature = "std")]
fn secrets_from(
    var: impl Fn(&str) -> Option<String>,
    read: impl Fn(&std::path::Path) -> std::io::Result<String>,
) -> Result<Secrets, String> {
    let direct = var("CONSTELLATION_ROOM_PASSWORD");
    let file = var("CONSTELLATION_ROOM_PASSWORD_FILE");
    let room_password = match (direct, file) {
        (Some(_), Some(_)) => {
            return Err("set CONSTELLATION_ROOM_PASSWORD or ..._FILE, not both".to_owned());
        }
        (Some(p), None) => Some(p),
        (None, Some(path)) => {
            let text = read(std::path::Path::new(&path)).map_err(|e| format!("{path}: {e}"))?;
            Some(text.lines().next().unwrap_or("").to_owned())
        }
        (None, None) => None,
    };
    if room_password.as_deref() == Some("") {
        return Err("the room password is empty".to_owned());
    }
    Ok(Secrets { room_password })
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

    #[test]
    fn the_password_comes_from_the_variable_or_the_files_first_line() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned())
        };
        let file = |p: &std::path::Path| {
            if p == std::path::Path::new("/pw") {
                Ok("s3cret\nignored\n".to_owned())
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
            }
        };
        assert_eq!(secrets_from(env(&[]), file), Ok(Secrets::default()));
        let direct = secrets_from(env(&[("CONSTELLATION_ROOM_PASSWORD", "pw")]), file);
        assert_eq!(direct.unwrap().room_password.as_deref(), Some("pw"));
        let from_file = secrets_from(env(&[("CONSTELLATION_ROOM_PASSWORD_FILE", "/pw")]), file);
        assert_eq!(from_file.unwrap().room_password.as_deref(), Some("s3cret"));
        let both: &'static [(&str, &str)] =
            &[("CONSTELLATION_ROOM_PASSWORD", "a"), ("CONSTELLATION_ROOM_PASSWORD_FILE", "/pw")];
        assert!(secrets_from(env(both), file).unwrap_err().contains("not both"));
        let missing = secrets_from(env(&[("CONSTELLATION_ROOM_PASSWORD_FILE", "/nope")]), file);
        assert!(missing.unwrap_err().contains("/nope"));
        let empty = secrets_from(env(&[("CONSTELLATION_ROOM_PASSWORD", "")]), file);
        assert!(empty.unwrap_err().contains("empty"));
    }

    #[test]
    fn secrets_print_redacted() {
        let s = Secrets { room_password: Some("hunter2".to_owned()) };
        let text = format!("{s:?}");
        assert!(!text.contains("hunter2") && text.contains("redacted"), "{text}");
    }

    #[test]
    fn connection_limits_are_design_values_and_the_ping_beats_the_idle_timeout() {
        let l = Limits::DESIGN;
        assert_eq!(l.max_connections, 10_000);
        assert_eq!(
            (l.connect_timeout_ms, l.idle_timeout_ms, l.ping_interval_ms),
            (30_000, 60_000, 20_000)
        );
        assert!(l.ping_interval_ms < l.idle_timeout_ms);
    }

    #[test]
    fn session_rate_limits_are_design_values() {
        let l = Limits::DESIGN;
        assert_eq!((l.fanout_burst, l.fanout_refill_milli_per_sec), (10, 2000));
        assert_eq!((l.chat_max_bytes, l.bounce_max_bytes), (2048, 16384));
        assert_eq!((l.datapackage_burst, l.datapackage_refill_milli_per_sec), (4, 100));
        assert_eq!((l.connect_burst, l.connect_refill_milli_per_sec), (10, 1000));
        assert_eq!(l.rate_table_max_keys, 100_000);
    }
}
