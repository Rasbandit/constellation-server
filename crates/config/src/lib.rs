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
    /// Most rooms one process loads from a rooms folder (`--rooms`); more is refused at start.
    pub max_rooms: usize,
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
    /// Postgres connections the log store's pool may open (one box, Postgres on the same
    /// box, doc 12 Stage 0 and 1; design value, not measured).
    pub db_pool_connections: u32,
    /// Postgres `statement_timeout` for the log store's sessions (design value).
    pub db_statement_timeout_ms: u64,
    /// Postgres `lock_timeout`: a claim or a write waiting on a row lock held by a stuck
    /// writer errors after this instead of hanging (design value).
    pub db_lock_timeout_ms: u64,
    /// Postgres `idle_in_transaction_session_timeout`: the server ends a session that sits in
    /// an open transaction this long, releasing its locks (design value).
    pub db_idle_in_transaction_timeout_ms: u64,
    /// Longest data storage key, bytes (estimate, docs/protocol-plan.md 1.5).
    pub kv_max_key_bytes: usize,
    /// Largest stored value, serialized as JSON, bytes (estimate).
    pub kv_max_value_bytes: usize,
    /// Largest key-value store per room: keys plus serialized values, bytes (estimate).
    pub kv_max_room_bytes: u64,
    /// Most operations in one `Set` (estimate).
    pub kv_max_ops: usize,
    /// Deepest JSON nesting of a stored value or operand, counting each list and object. The
    /// same number as the frame depth cap of the reference server (`NetUtils.py:179-205`).
    pub kv_max_depth: usize,
    /// Most keys in one `Get` or `SetNotify` (estimate).
    pub kv_max_keys_per_request: usize,
    /// Most keys one connection may watch with `SetNotify` (estimate).
    pub kv_max_watched_keys: usize,
    /// `Set` packets a slot may send in a burst; its own bucket, so frequent shared-value writes
    /// never use up chat (estimate).
    pub kv_set_burst: u32,
    /// Refill of that bucket, thousandths per second (20000 = 20 per second).
    pub kv_set_refill_milli_per_sec: u32,
    /// Stored bytes (key plus value as JSON) a slot's data storage writes may log in a burst.
    /// Every write is logged in full, so this, not the write count, bounds log growth (estimate).
    pub kv_write_bytes_burst: u32,
    /// Refill of that byte bucket, thousandths of a byte per second (4096000 = 4 KiB/s).
    pub kv_write_bytes_refill_milli_per_sec: u32,
    /// Most data storage bytes a room may ever log (the log is not compacted yet), bytes
    /// (estimate; a room past it refuses writes with `room.storage_full`).
    pub kv_max_room_log_bytes: u64,
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
        max_rooms: 1000,
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
        db_pool_connections: 8,
        db_statement_timeout_ms: 10_000,
        db_lock_timeout_ms: 5_000,
        db_idle_in_transaction_timeout_ms: 10_000,
        kv_max_key_bytes: 1024,
        kv_max_value_bytes: 64 * 1024,
        kv_max_room_bytes: 16 * 1024 * 1024,
        kv_max_ops: 64,
        kv_max_depth: 16,
        kv_max_keys_per_request: 1024,
        kv_max_watched_keys: 4096,
        kv_set_burst: 50,
        kv_set_refill_milli_per_sec: 20_000,
        kv_write_bytes_burst: 256 * 1024,
        kv_write_bytes_refill_milli_per_sec: 4096 * 1000,
        kv_max_room_log_bytes: 512 * 1024 * 1024,
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
    /// Postgres URL for the log (it can hold a password): `CONSTELLATION_DATABASE_URL`, or the
    /// first line of the file named by `CONSTELLATION_DATABASE_URL_FILE`. `None` runs on the
    /// in-memory log, which a restart loses.
    pub database_url: Option<String>,
}

#[cfg(feature = "std")]
impl core::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let pw = self.room_password.as_ref().map(|_| "<redacted>");
        let db = self.database_url.as_ref().map(|_| "<redacted>");
        f.debug_struct("Secrets").field("room_password", &pw).field("database_url", &db).finish()
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

/// `NAME`, or the first line of the file named by `NAME_FILE`; never both, never empty.
#[cfg(feature = "std")]
fn var_or_file(
    name: &str,
    var: &impl Fn(&str) -> Option<String>,
    read: &impl Fn(&std::path::Path) -> std::io::Result<String>,
) -> Result<Option<String>, String> {
    let value = match (var(name), var(&format!("{name}_FILE"))) {
        (Some(_), Some(_)) => return Err(format!("set {name} or {name}_FILE, not both")),
        (Some(v), None) => Some(v),
        (None, Some(path)) => {
            let text = read(std::path::Path::new(&path)).map_err(|e| format!("{path}: {e}"))?;
            Some(text.lines().next().unwrap_or("").to_owned())
        }
        (None, None) => None,
    };
    if value.as_deref() == Some("") {
        return Err(format!("{name} is empty"));
    }
    Ok(value)
}

#[cfg(feature = "std")]
fn secrets_from(
    var: impl Fn(&str) -> Option<String>,
    read: impl Fn(&std::path::Path) -> std::io::Result<String>,
) -> Result<Secrets, String> {
    let database_url = var_or_file("CONSTELLATION_DATABASE_URL", &var, &read)?;
    if let Some(url) = &database_url {
        check_database_url(url)?;
    }
    Ok(Secrets {
        room_password: var_or_file("CONSTELLATION_ROOM_PASSWORD", &var, &read)?,
        database_url,
    })
}

/// URL parameters sqlx 0.9.0 understands (`sqlx-postgres` `options/parse.rs`), plus
/// `options[<setting>]`. sqlx logs any other one with its value, so those are refused.
#[cfg(feature = "std")]
const DATABASE_URL_PARAMS: &[&str] = &[
    "sslmode",
    "ssl-mode",
    "sslrootcert",
    "ssl-root-cert",
    "ssl-ca",
    "sslcert",
    "ssl-cert",
    "sslkey",
    "ssl-key",
    "statement-cache-capacity",
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "password",
    "application_name",
    "options",
];

/// Refuse a parameter sqlx would ignore and log. The error never shows the URL or the key: a
/// misplaced secret must not reach the log.
#[cfg(feature = "std")]
fn check_database_url(url: &str) -> Result<(), String> {
    let query = url.split_once('?').map_or("", |(_, q)| q);
    let query = query.split_once('#').map_or(query, |(q, _)| q);
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let key = pair.split_once('=').map_or(pair, |(k, _)| k);
        let bracketed = key
            .strip_prefix("options[")
            .and_then(|k| k.strip_suffix(']'))
            .is_some_and(|k| !k.is_empty());
        if !(bracketed || DATABASE_URL_PARAMS.contains(&key)) {
            return Err(format!(
                "CONSTELLATION_DATABASE_URL has a parameter sqlx does not recognise (not \
                 shown, so it cannot leak into the log); allowed: {}, options[<setting>]",
                DATABASE_URL_PARAMS.join(", ")
            ));
        }
    }
    Ok(())
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
        assert_eq!(l.max_rooms, 1000);
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
    fn the_database_url_comes_from_the_variable_or_a_file_like_the_password() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned())
        };
        let file = |_: &std::path::Path| Ok("postgres://u:p@h/db\n".to_owned());
        let url = "postgres://localhost/c";
        let direct =
            secrets_from(env(&[("CONSTELLATION_DATABASE_URL", "postgres://localhost/c")]), file);
        assert_eq!(direct.unwrap().database_url.as_deref(), Some(url));
        let from_file = secrets_from(env(&[("CONSTELLATION_DATABASE_URL_FILE", "/u")]), file);
        assert_eq!(from_file.unwrap().database_url.as_deref(), Some("postgres://u:p@h/db"));
        let both: &'static [(&str, &str)] =
            &[("CONSTELLATION_DATABASE_URL", "x"), ("CONSTELLATION_DATABASE_URL_FILE", "/u")];
        assert!(secrets_from(env(both), file).unwrap_err().contains("not both"));
        let empty = secrets_from(env(&[("CONSTELLATION_DATABASE_URL", "")]), file);
        assert!(empty.unwrap_err().contains("empty"));
        assert_eq!(secrets_from(env(&[]), file).unwrap().database_url, None);
        let s = Secrets {
            room_password: None,
            database_url: Some("postgres://u:hunter2@h".to_owned()),
        };
        assert!(!format!("{s:?}").contains("hunter2"), "the URL can hold a password");
    }

    /// sqlx logs a connect parameter it does not know, value included, so such URLs are
    /// refused here, and the error never echoes the URL. Known names are sqlx 0.9.0's
    /// (`sqlx-postgres` `options/parse.rs`).
    #[test]
    fn a_database_url_with_a_parameter_sqlx_ignores_is_refused_without_echoing_it() {
        let url = |u: &'static str| {
            secrets_from(
                move |k: &str| (k == "CONSTELLATION_DATABASE_URL").then(|| u.to_owned()),
                |_| Ok(String::new()),
            )
        };
        for bad in [
            "postgres://h/db?pasword=s3cret",
            "postgres://h/db?sslmode=require&s3cret=1",
            "postgres://h/db?=s3cret",
            "postgres://h/db?options[=s3cret",
        ] {
            let e = url(bad).unwrap_err();
            assert!(e.contains("parameter") && !e.contains("s3cret"), "{bad}: {e}");
        }
        for good in [
            "postgres://u:p@h:5432/db",
            "postgres://h/db?sslmode=require&application_name=c&options[search_path]=t_1",
            "postgres://h/db?options=-c%20x%3D1&user=u&password=p&port=1&dbname=d",
        ] {
            assert_eq!(url(good).unwrap().database_url.as_deref(), Some(good), "{good}");
        }
    }

    /// `load` and `load_secrets` read the real process environment, which a test may not
    /// change (no `unsafe` for `set_var`). So this test runs its own binary again with a set
    /// environment, and the child (`env_probe`) prints what the two functions read.
    #[test]
    fn load_and_load_secrets_read_the_process_environment() {
        let dir = std::env::temp_dir().join(format!("config-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let url_file = dir.join("url");
        std::fs::write(&url_file, "postgres://probe@h/db\nignored\n").unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::env_probe", "--nocapture", "--test-threads=1"])
            .env("CONFIG_ENV_PROBE", "1")
            .env("CONSTELLATION_DEDUPE_WINDOW_SECS", "77")
            .env("SIM_SEEDS", "5")
            .env("SIM_SEED", "9")
            .env("CONSTELLATION_MAX_CONNECTIONS", "3")
            .env("CONSTELLATION_CONNECT_TIMEOUT_MS", "11")
            .env("CONSTELLATION_IDLE_TIMEOUT_MS", "not-a-number")
            .env("CONSTELLATION_PING_INTERVAL_MS", "-4")
            .env("CONSTELLATION_ROOM_PASSWORD", "pw-probe")
            .env("CONSTELLATION_DATABASE_URL_FILE", &url_file)
            .env_remove("CONSTELLATION_ROOM_PASSWORD_FILE")
            .env_remove("CONSTELLATION_DATABASE_URL")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text}");
        let d = Limits::DESIGN;
        // Bad numbers keep the design value.
        let want =
            format!("probe limits 77 5 Some(9) 3 11 {} {}", d.idle_timeout_ms, d.ping_interval_ms);
        assert!(text.contains(&want), "want {want:?} in {text}");
        assert!(
            text.contains("probe secrets Some(\"pw-probe\") Some(\"postgres://probe@h/db\")"),
            "{text}"
        );
    }

    /// The child half of the test above; does nothing unless that test started it.
    #[test]
    fn env_probe() {
        if std::env::var("CONFIG_ENV_PROBE").is_err() {
            return;
        }
        let l = load();
        println!(
            "probe limits {} {} {:?} {} {} {} {}",
            l.dedupe_window_secs,
            l.sim_seeds,
            l.sim_replay_seed,
            l.max_connections,
            l.connect_timeout_ms,
            l.idle_timeout_ms,
            l.ping_interval_ms
        );
        let s = load_secrets().unwrap();
        println!("probe secrets {:?} {:?}", s.room_password, s.database_url);
    }

    #[test]
    fn secrets_print_redacted() {
        let s = Secrets { room_password: Some("hunter2".to_owned()), database_url: None };
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
    fn data_storage_limits_are_the_planned_estimates() {
        let l = Limits::DESIGN;
        assert_eq!((l.kv_max_key_bytes, l.kv_max_value_bytes), (1024, 65_536));
        assert_eq!(l.kv_max_room_bytes, 16_777_216);
        assert_eq!((l.kv_max_ops, l.kv_max_depth), (64, 16));
        assert_eq!((l.kv_max_keys_per_request, l.kv_max_watched_keys), (1024, 4096));
        assert_eq!((l.kv_set_burst, l.kv_set_refill_milli_per_sec), (50, 20_000));
        assert_eq!(
            (l.kv_write_bytes_burst, l.kv_write_bytes_refill_milli_per_sec),
            (262_144, 4_096_000)
        );
        assert_eq!(l.kv_max_room_log_bytes, 512 * 1024 * 1024);
    }

    #[test]
    fn session_rate_limits_are_design_values() {
        let l = Limits::DESIGN;
        assert_eq!((l.fanout_burst, l.fanout_refill_milli_per_sec), (10, 2000));
        assert_eq!((l.chat_max_bytes, l.bounce_max_bytes), (2048, 16384));
        assert_eq!((l.datapackage_burst, l.datapackage_refill_milli_per_sec), (4, 100));
        assert_eq!((l.connect_burst, l.connect_refill_milli_per_sec), (10, 1000));
        assert_eq!(l.rate_table_max_keys, 100_000);
        assert_eq!(l.db_pool_connections, 8);
        assert_eq!(
            (l.db_statement_timeout_ms, l.db_lock_timeout_ms, l.db_idle_in_transaction_timeout_ms),
            (10_000, 5_000, 10_000)
        );
    }
}
