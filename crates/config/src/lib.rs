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
    /// Most rooms one process lists (in memory or not); further bundles are skipped with a
    /// warning.
    pub max_rooms: usize,
    /// Most rooms one process holds in memory at once (`CONSTELLATION_MAX_RESIDENT_ROOMS`).
    /// A cold room that must load past it puts the resident room unused for the longest to
    /// sleep first; a room with a connection open is never put to sleep, so when every one
    /// has one the load is refused. Doc 02 section 5.0 assumes 15% of rooms resident
    /// (`r = 0.15`, 150 of `max_rooms`); this allows 250. A cost trade, not a design value,
    /// until #38 measures memory per room.
    pub max_resident_rooms: usize,
    /// How often the server looks in the rooms folder for new seed bundles and checks for idle
    /// and abandoned rooms (`CONSTELLATION_ROOMS_SCAN_MS`).
    pub rooms_scan_ms: u64,
    /// A room with no connection for this long leaves memory, and its next connect loads it
    /// again (doc 02 section 4.3: Resident to Draining after idle 15 min, a design value;
    /// `CONSTELLATION_ROOM_IDLE_SECS`).
    pub room_idle_secs: u64,
    /// A room with no connection for this long ends as abandoned and is never served again
    /// (D-30: 30 days, a design value; `CONSTELLATION_ROOM_ABANDON_SECS`).
    pub room_abandon_secs: u64,
    /// A room in use has the time it was last used written to the database at most this
    /// often, so a crash loses at most this much of it (a cost trade, not a design value;
    /// `CONSTELLATION_ROOM_TOUCH_SECS`). A room leaving memory, and a clean stop, write it
    /// at once.
    pub room_touch_secs: u64,
    /// A room that fails to load, or whose task ends within `room_healthy_secs` of its load,
    /// waits this long before it loads again, doubled for each such failure in a row up to
    /// `room_retry_max_ms` (#64). Doc 01 D1.7's reconnect backoff numbers: 1 s, then
    /// doubling, at most 60 s, reset after 30 s healthy (`CONSTELLATION_ROOM_RETRY_FIRST_MS`).
    pub room_retry_first_ms: u64,
    /// The longest wait between loads of a failing room (`CONSTELLATION_ROOM_RETRY_MAX_MS`).
    pub room_retry_max_ms: u64,
    /// A room in memory this long is healthy: its failures in a row are forgotten
    /// (`CONSTELLATION_ROOM_HEALTHY_SECS`).
    pub room_healthy_secs: u64,
    /// Draws of a new room key before a new room gives up until the next scan; each draw
    /// collides with an existing key only with odds of about one in 2^50 per stored room.
    pub room_key_draws: usize,
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
    /// Durable events a host's room view stream replays at most when it opens or resumes; an
    /// older start gets the newest ones and a reset (the web UI keeps 400 lines).
    pub view_history_events: u64,
    /// Room view streams (admin API, server-sent events) open at once per process; more get
    /// HTTP 503.
    pub view_streams: usize,
    /// How often an open room view stream checks that its account still has a role on the room
    /// (a removed admin or deleted account loses the stream within this long; design value).
    pub view_access_recheck_ms: u64,
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
    /// Packets that write hints (scouts that create hints, `UpdateHint`) a slot may send in a
    /// burst. Each can log events, so it has its own bucket; `!hint` shares chat's (estimate,
    /// docs/protocol-plan.md 2.5).
    pub hint_burst: u32,
    /// Refill of that bucket, thousandths per second (1000 = 1 per second).
    pub hint_refill_milli_per_sec: u32,
    /// Shortest account password, in characters (design value; NIST SP 800-63B says at least 8).
    pub password_min_chars: usize,
    /// Longest account password, in bytes: argon2 hashes all of it, so it is capped (design value).
    pub password_max_bytes: usize,
    /// Shortest account name, in characters (design value).
    pub account_name_min_chars: usize,
    /// Longest account name, in bytes (design value).
    pub account_name_max_bytes: usize,
    /// How long a login lasts without another one, seconds (design value: 14 days).
    pub session_ttl_secs: u64,
    /// Failed logins one client address may make in a burst; more get HTTP 429 until the bucket
    /// refills (design value). A good login is never counted.
    pub login_fail_burst: u32,
    /// Refill of that bucket, thousandths per second (100 = one every 10 s).
    pub login_fail_refill_milli_per_sec: u32,
    /// Passwords hashed or checked at once (design value); more get HTTP 503. Each is a core and
    /// ~19 MiB, and the per-address counts only apply once the work is done.
    pub password_hash_parallel: usize,
    /// Whether anyone may make an account (`CONSTELLATION_SIGNUP=open|closed`, default open).
    /// Closed: the sign-up page and `POST /api/signup` refuse; existing accounts still log in.
    pub signup_open: bool,
    /// Accounts one client address may make in a burst; more get HTTP 429 until the bucket
    /// refills (design value).
    pub signup_burst: u32,
    /// Refill of that bucket, thousandths per second (10 = one every 100 s).
    pub signup_refill_milli_per_sec: u32,
    /// Largest `.apworld` upload, bytes (design value: real worlds are a few MB; a few bundled
    /// ones are tens of MB).
    pub apworld_max_bytes: usize,
    /// Most bytes an `.apworld` may unpack to; more is refused as a zip bomb (design value).
    pub apworld_max_unpacked_bytes: u64,
    /// Most entries in an `.apworld` zip (design value).
    pub apworld_max_entries: u64,
    /// Largest player `.yaml` upload, bytes (design value: real ones are a few KB).
    pub yaml_max_bytes: usize,
    /// Largest world manifest upload, bytes (design value: the large test world is 830 KB).
    pub manifest_max_bytes: usize,
    /// Bytes of manifests the server keeps in memory for generation (least recently used go
    /// first); the database holds every one (design value).
    pub manifest_cache_bytes: usize,
    /// Most records one listing page returns, whatever the caller asks (design value).
    pub list_page_max: usize,
    /// Longest catalogue game name, in bytes (design value: real game names are under 60).
    pub catalogue_name_max_bytes: usize,
    /// Longest catalogue game description, in bytes (design value).
    pub catalogue_description_max_bytes: usize,
    /// Longest catalogue version number, in bytes (design value).
    pub catalogue_version_max_bytes: usize,
    /// Longest catalogue version note ("what changed"), in bytes (design value).
    pub catalogue_note_max_bytes: usize,
    /// Test seeds a catalogue version must generate without an error before it goes live, each
    /// with a random seed and a default player (design value, agreed in #189: a real world took
    /// about 22 s, so 20 take about 7 minutes one at a time).
    pub catalogue_check_seeds: usize,
    /// Test seeds generating or waiting to, server-wide. Each also waits its turn for a
    /// generation slot (`gen_concurrency`) in the same line as players, so a player is behind
    /// at most this many; at most `gen_concurrency` (design value).
    pub catalogue_check_concurrency: usize,
    /// Manifest checks (a child process each) running at once; more are refused as busy.
    pub manifest_check_concurrency: usize,
    /// Address-space cap of a fill or manifest check child process, bytes (design value).
    pub fill_memory_bytes: u64,
    /// Process and thread cap of a fill or manifest check child process. The system counts every
    /// task of the server's user against it, so it is a brake on runaway spawning, not a tight
    /// bound; 0 leaves it off (design value).
    pub fill_max_processes: u64,
    /// CPU-seconds cap of a fill or manifest check child process (design value).
    pub fill_cpu_secs: u64,
    /// Wall-clock limit of one fill child process, after which it is killed (design value).
    pub fill_timeout_ms: u64,
    /// Fill worker processes that stay up and take one fill job after another
    /// (`server::pool`). 0, the default, is no pool: each fill runs in a process of its own.
    /// With a pool, a fill needs a worker, so there must be one for every generation that may
    /// run at once: the valid settings are 0, or `gen_concurrency` and up
    /// ([`Limits::check`]; workers past `gen_concurrency` are never started). One worker is
    /// one core ([`GENERATION_WORKERS`] thread per fill). `CONSTELLATION_FILL_POOL_WORKERS`.
    /// Each worker runs under the same caps as a fill child process, and a job past
    /// `fill_timeout_ms` ends its worker.
    pub fill_pool_workers: usize,
    /// Fill jobs a pooled worker takes before it is replaced by a fresh process (design
    /// value, not measured).
    pub fill_worker_jobs: usize,
    /// Most memory the loaded worlds one pooled worker keeps between jobs may hold
    /// ([`PREPARED_CACHE_BYTES`]).
    pub fill_worker_cache_bytes: usize,
    /// A pooled worker whose address space after a job is over this share of
    /// `fill_memory_bytes`, in percent, is replaced by a fresh process: what earlier fills
    /// left allocated must leave the next job at least the rest of the cap (design value).
    pub fill_worker_recycle_percent: u64,
    /// Generations running at once (legacy and manifest worlds alike; the cap #77 names).
    pub gen_concurrency: usize,
    /// Most generations queued or running at once; a new one past it is refused as busy
    /// (design value).
    pub gen_queue_max: usize,
    /// Uploads and generation requests one client address may make in a burst, then
    /// `admin_write_refill_milli_per_sec` per second after (design values).
    pub admin_write_burst: u32,
    /// Refill of the per-address write allowance, thousandths of a request per second.
    pub admin_write_refill_milli_per_sec: u32,
    /// Wall-clock limit of one sandboxed inspect call, after which it is killed (design value).
    pub inspect_timeout_ms: u64,
    /// Wall-clock limit of one sandboxed generation, after which it is killed
    /// (`CONSTELLATION_GEN_TIMEOUT_MS`; design value: one real world took about 22 s here).
    pub gen_timeout_ms: u64,
    /// A queued or running generation marks itself alive in the store this often, so another
    /// server instance can tell it from one whose process stopped (#119; a cost trade, not a
    /// design value).
    pub gen_heartbeat_secs: u64,
    /// A queued or running generation not marked alive for this long reads as `stalled`: its
    /// server stopped, or lost the store for a while and may come back. Several heartbeats, so
    /// one slow write never shows a live job as stalled.
    pub gen_stale_secs: u64,
    /// A generation not marked alive for this long is failed by any instance's sweep: its
    /// server is gone. Longer than `gen_timeout_ms` ([`Limits::check`]), so a server that
    /// lost the store mid-job, and could not write its heartbeats, can still end the job by
    /// its own timeout before the sweep fails it (the heartbeat covers a job of any length;
    /// this covers only failed writes). A cost trade, not a design value. A server that does
    /// come back still writes how its job really ended.
    pub gen_lost_secs: u64,
    /// Most files a job folder's log line names (a removed folder, or one a stopped server
    /// left); past it the line says how many more (a cost trade: log line length).
    pub job_log_files_max: usize,
    /// Address space cap of the sandboxed Python process, bytes (`prlimit --as`,
    /// `CONSTELLATION_GEN_MEMORY_BYTES`; design value).
    pub gen_memory_bytes: u64,
    /// Most bytes of the sidecar's error output kept in a failed job's status; the rest is
    /// read and dropped, never held (design value).
    pub gen_error_max_bytes: usize,
    /// Most bytes the sidecar may print as its result (the bundle travels this way); more
    /// fails the call (design value: the bundle cap plus room for the report).
    pub gen_stdout_max_bytes: usize,
    /// Size of each writable tmpfs in the sandbox (`/tmp`, the generator's scratch output),
    /// bytes (design value).
    pub gen_tmpfs_bytes: u64,
    /// Largest file the sandboxed process may write (`prlimit --fsize`), bytes (design value).
    pub gen_file_max_bytes: u64,
    /// Most processes and threads in the sandbox (`prlimit --nproc`, counted per sandbox
    /// because each has its own user namespace; design value).
    pub gen_max_processes: u64,
    /// Most open files in the sandbox (`prlimit --nofile`; bounds memfd use; design value).
    pub gen_max_open_files: u64,
    /// Memory cap of each sandbox's own cgroup (`systemd-run --user --scope -p MemoryMax`),
    /// bytes. It counts what the address space cap misses: tmpfs, memfd and shared memory
    /// pages, and all processes together (`CONSTELLATION_GEN_CGROUP_MEMORY_BYTES`; design value).
    pub gen_cgroup_memory_bytes: u64,
    /// Task cap of each sandbox's cgroup (`TasksMax`; bwrap's own processes included; design
    /// value).
    pub gen_cgroup_tasks: u64,
}

/// Deepest rule nesting a world's rules section may hold, counting every node and every def a
/// rule reaches through `ref`: the loader rejects deeper rules, so the recursive evaluators
/// stay well inside a thread's stack (design value; real worlds nest under 20).
pub const RULES_MAX_DEPTH: u32 = 256;

/// Most terms a rule may take as a bit rule (`data_fill`'s fold: an OR of terms, each a set of
/// atom bits that must all be set). Turning AND over OR into that form multiplies terms, so a
/// rule that would need more, at any step of the way, stays bytecode (a design value: the real
/// test world's rules are counted in the PR that added this).
pub const RULE_BITS_MAX_TERMS: usize = 16;

/// Most tests (atom bits and time windows) one term of a bit rule may hold. A def's terms are
/// copied into every rule that refers to it and AND joins the lists, so without this a long
/// def referred to from many sites grows the bit rules by sites times its length; a rule with a
/// longer term, at any step of the way, stays bytecode (a design value).
pub const RULE_BITS_MAX_TERM_TESTS: usize = 32;

/// Most tests a loaded world's bit rules may hold in all (two bytes each), def terms copied
/// into sites counted: once reached, the rules not yet compiled stay bytecode. A site's rule
/// is at most [`RULE_BITS_MAX_TERMS`] terms of [`RULE_BITS_MAX_TERM_TESTS`] tests, so this
/// bounds a load whatever the manifest (a design value: the heavy synthetic world of 1,000
/// checks holds under a tenth of it).
pub const RULE_BITS_MAX_WORLD_TESTS: usize = 1 << 18;

/// Full fills a seed may start before the data-only fill gives up as stuck: a net for what the
/// exact fill's repairs in place cannot mend, not its way of coping (each retry takes the next
/// draws of the seed's random stream and goes deepest first; how often it is entered is in the
/// fill report, `attempts`). A design value. Counted on 9,926 rooms with 20 allowed (tight
/// synthetic worlds of 60 to 1,000 checks at 1, 3, 10 and 100 players, both fixture worlds,
/// 3,000 seeds each): none needed more than 3, one needed 3, and none that drawn order fills in
/// 20 attempts is stuck here in 3.
pub const FILL_MAX_ATTEMPTS: u32 = 3;

/// Version of a prepared world's layout beside its rules section (`data_fill::prepared`): which
/// sections the file holds and what is in them. Raise it with any change to that. A world is
/// also prepared again by every new server build, so this guards builds that share a version
/// text (local and test builds).
pub const PREPARED_WORLD_LAYOUT: u16 = 2;

/// Attempts one world's shuffles get per seed (`data_fill::shuffle`): a failed layout or
/// whole-world check starts every shuffle of that world again on the next attempt's random
/// streams, and at the cap the room's generation fails naming the world and the shuffle (a
/// design value, doc 17 section 4.7 of the design docs).
pub const SHUFFLE_MAX_ATTEMPTS: u32 = 4;

/// Most shuffles one world may declare (a design value).
pub const SHUFFLES_MAX: usize = 64;

/// Most slots over all shuffles of one world, and most things of one shuffle (a design value:
/// the largest entrance shuffle we know of is reported at 611 exits). Bounds the permission
/// tables, which hold a cell per slot and thing.
pub const SHUFFLE_MAX_SLOTS: usize = 1024;

/// Longest shuffle name in bytes: a name keys a random stream, and a stream label is short
/// (`spec_rng::derive`).
pub const SHUFFLE_NAME_MAX_BYTES: usize = 48;

/// Most steps the load of a world's layout index may take: per shuffled exit, the regions
/// that reach its region by exits that are not shuffled (what the layout solve tells a branch
/// from a dead end by, about 2 bytes each), and the exits walked to find them (a design
/// value).
pub const SHUFFLE_MAX_BEHIND: usize = 1 << 20;

/// Most permission rows one shuffle may carry: a row is applied to the whole table of its
/// shuffle when the world is compiled (a design value).
pub const SHUFFLE_MAX_ROWS: usize = 64;

/// Most things a shuffle may have when a rule reads its result as a rule: such a read is one
/// shared rule listing every candidate, and a rule lists at most this many parts (the
/// engine's limit, one byte).
pub const SHUFFLE_MAX_READ_THINGS: usize = 255;

/// Longest error text the shuffle checks give back, bytes: a message may quote names from the
/// manifest, and a name's length is not known to be small when it is quoted (a design value).
pub const SHUFFLE_ERROR_MAX_BYTES: usize = 4096;

/// Most memory the loaded worlds one pooled fill worker keeps between jobs may hold
/// (`server::pool`; the design value of `Limits::fill_worker_cache_bytes`), the least recently
/// used going first. A kept world counts what it holds (`data_fill::prepared::Prepared::
/// held_bytes`, checked against a counting allocator in CI). From issue 200.
pub const PREPARED_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// CPU time kept in hand when a pooled fill worker is judged fit for one more job: its
/// process CPU cap (`fill_cpu_secs`) must cover what its jobs so far may have used, one more
/// whole job, and this much for its start and what it does between jobs (design value).
pub const FILL_WORKER_CPU_MARGIN_MS: u64 = 1000;

/// Threads one generation asks its fill for (`workers` of the fill request): one, so a
/// generation costs one core and many run side by side. A room's fill runs on one thread on
/// the path a room takes by itself (`data_fill::room::Path::of`). The forward path, which can
/// use threads, stays for a caller that names it, up to [`ROOM_FORWARD_WORKERS`] (results
/// never depend on the number).
pub const GENERATION_WORKERS: usize = 1;

/// Worker threads that compile worlds and resolve instances of a room's players in parallel
/// before a multiworld fill (a design value; results never depend on this number).
pub const FILL_SETUP_WORKERS: usize = 4;

/// Most threads the players' states of a room run on while a round is solved forward: a fill
/// takes the worker count of its request, at most this many and at most the cores the process
/// may use (a design value; results never depend on the number).
pub const ROOM_FORWARD_WORKERS: usize = 4;

/// Rule evaluations ahead of one step of a forward solve (queued regions and sites over the
/// room's states; every rule site of the room for the first step of a solve) from which the
/// step starts threads; below it the states run on the caller's thread, since starting threads
/// would cost more than the step (a design value, not measured).
pub const ROOM_FORWARD_MIN_PENDING: usize = 2048;

/// Placed items an exact fill tries to move, one fresh solve of the room each, for one item
/// that has nowhere to go (`data_fill::room`, `displace`), before the attempt counts as stuck
/// (a design value).
pub const FILL_DISPLACE_TRIES: usize = 32;

/// Rule evaluations an exact fill may spend on repair, over all its attempts: the fresh solves
/// of moves, one solve's worth more per move for the pass over the rounds after it, and per
/// divided round the evaluations of the attempt's first solve in rank order with none of the
/// rounds' items (`data_fill::room`, `charge_division`), all counted the same on every path.
/// Within the budget means at most this much spent: once more is spent, checked before each
/// solve and at each division, the attempt is stuck (so a fill ends at most one solve past
/// it), and so is any later one that needs a move or a division. A budget of work, not of
/// time: a seed is the same on every machine and path. A design value with a margin over the
/// most repaired bench room, keys at 100 players, which charges 34.9 to 37.5 million
/// (counted, `bench-room` run 45948967615, seeds 1 to 3): 1.7 times that. By the growth of
/// that charge over room size (counted at 22 and 100 players) it lets key rooms up to about
/// 120 players and chain rooms up to about 200 fill without a restart (estimated). Time is
/// not bounded by it: on that run keys at 100 players took 4.3 to 4.4 s for 78 to 83 million
/// evaluations in all (measured), about 55 ns an evaluation; at that rate the budget is about
/// 3.5 s of repair, and at the heavy profile's 186 ns about 12 s (both estimated).
pub const FILL_REPAIR_EVALS: u64 = 64_000_000;

/// The least p value the product's fill may show against uniform beatable placements on the
/// one-player fixture world (`data-fill` `tests/fairness.rs`; a design value: drawn order gives
/// 0.39 there, each player's items deepest first gave 0.00008, counted over 300 seeds).
pub const FILL_FAIRNESS_MIN_P: f64 = 0.01;

/// Random orders of a world's progression the early capacity ratio is the mean over
/// (`data_fill::capacity`), the setup-time warning that a world leaves little room early on
/// (as many orders as the measurement that found the ratio used; a design value).
pub const FILL_CAPACITY_ORDERS: u32 = 30;

/// Most threads any one parallel setup takes, whatever worker count a caller asks for (a design
/// value).
pub const FILL_MAX_WORKERS: usize = 64;

/// Longest game or version name a manifest may carry, bytes (a design value; it is kept in
/// listing columns and an index). Also the longest name in a shuffle's vocabulary (a slot, a
/// thing, a group, an exit key, a door).
pub const MANIFEST_NAME_MAX_BYTES: usize = 128;

/// The generator version a data-only seed records (what clients of the compatible protocol
/// expect; a format value, not a limit).
pub const SEED_GENERATOR_VERSION: (u32, u32, u32) = (0, 6, 8);

/// Hint cost, percent of a slot's locations, that a data-only seed starts with (design value).
pub const SEED_HINT_COST_PCT: u8 = 10;

/// Hint points a data-only seed gives per checked location (design value).
pub const SEED_CHECK_POINTS: u32 = 1;

/// Most players one room's fill takes (a design value; player numbers are 16 bits).
pub const ROOM_MAX_PLAYERS: usize = 1024;

/// Writes of a generation's last status while the store is down: how many tries (a cost trade:
/// with the doubling wait about 15 s in all, one `gen_heartbeat_secs`, which rides out a store
/// restart).
pub const GEN_FINAL_WRITE_TRIES: u32 = 6;

/// The wait after the first failed write of a generation's last status, ms; it doubles after
/// each further one ([`GEN_FINAL_WRITE_TRIES`]).
pub const GEN_FINAL_WRITE_WAIT_MS: u64 = 500;

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
        max_resident_rooms: 250,
        rooms_scan_ms: 2_000,
        room_idle_secs: 15 * 60,
        room_abandon_secs: 30 * 24 * 60 * 60,
        room_touch_secs: 60,
        room_retry_first_ms: 1_000,
        room_retry_max_ms: 60_000,
        room_healthy_secs: 30,
        room_key_draws: 3,
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
        view_history_events: 400,
        view_streams: 64,
        view_access_recheck_ms: 5000,
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
        hint_burst: 10,
        hint_refill_milli_per_sec: 1000,
        password_min_chars: 10,
        password_max_bytes: 256,
        account_name_min_chars: 3,
        account_name_max_bytes: 64,
        session_ttl_secs: 14 * 24 * 60 * 60,
        login_fail_burst: 5,
        login_fail_refill_milli_per_sec: 100,
        password_hash_parallel: 4,
        signup_open: true,
        signup_burst: 3,
        signup_refill_milli_per_sec: 10,
        apworld_max_bytes: 64 * 1024 * 1024,
        apworld_max_unpacked_bytes: 256 * 1024 * 1024,
        apworld_max_entries: 20_000,
        yaml_max_bytes: 1024 * 1024,
        manifest_max_bytes: 8 * 1024 * 1024,
        manifest_cache_bytes: 64 * 1024 * 1024,
        list_page_max: 200,
        catalogue_name_max_bytes: 100,
        catalogue_description_max_bytes: 2000,
        catalogue_version_max_bytes: 32,
        catalogue_note_max_bytes: 4000,
        catalogue_check_seeds: 20,
        catalogue_check_concurrency: 1,
        manifest_check_concurrency: 2,
        fill_memory_bytes: 1024 * 1024 * 1024,
        fill_cpu_secs: 120,
        fill_max_processes: 8192,
        fill_timeout_ms: 60_000,
        fill_pool_workers: 0,
        fill_worker_jobs: 256,
        fill_worker_cache_bytes: PREPARED_CACHE_BYTES,
        fill_worker_recycle_percent: 50,
        gen_concurrency: 1,
        gen_queue_max: 64,
        admin_write_burst: 30,
        admin_write_refill_milli_per_sec: 1000,
        inspect_timeout_ms: 30_000,
        gen_timeout_ms: 15 * 60 * 1000,
        gen_heartbeat_secs: 15,
        gen_stale_secs: 60,
        gen_lost_secs: 20 * 60,
        job_log_files_max: 32,
        gen_memory_bytes: 4 * 1024 * 1024 * 1024,
        gen_error_max_bytes: 16 * 1024,
        gen_stdout_max_bytes: 64 * 1024 * 1024 + 1024 * 1024,
        gen_tmpfs_bytes: 256 * 1024 * 1024,
        gen_file_max_bytes: 256 * 1024 * 1024,
        gen_max_processes: 64,
        gen_max_open_files: 256,
        gen_cgroup_memory_bytes: 4 * 1024 * 1024 * 1024,
        gen_cgroup_tasks: 80,
    };
}

impl Limits {
    /// Whether the limits fit together; the one place the rules between them are written.
    ///
    /// # Errors
    /// The first rule broken, as a message naming the limits.
    pub const fn check(&self) -> Result<(), &'static str> {
        if self.gen_stale_secs < 3 * self.gen_heartbeat_secs {
            return Err("gen_stale_secs must be at least 3 times gen_heartbeat_secs");
        }
        if self.gen_lost_secs < self.gen_stale_secs {
            return Err("gen_lost_secs must not be less than gen_stale_secs");
        }
        if self.gen_lost_secs.saturating_mul(1000) <= self.gen_timeout_ms {
            return Err("gen_lost_secs (in ms) must exceed gen_timeout_ms \
                 (CONSTELLATION_GEN_TIMEOUT_MS)");
        }
        if self.password_min_chars == 0 || self.password_min_chars > self.password_max_bytes {
            return Err("password_min_chars must be 1 to password_max_bytes");
        }
        if self.catalogue_check_concurrency == 0
            // Generation treats 0 as 1.
            || (self.catalogue_check_concurrency > self.gen_concurrency
                && self.catalogue_check_concurrency > 1)
        {
            return Err("catalogue_check_concurrency must be 1 to gen_concurrency");
        }
        if self.catalogue_check_seeds == 0 {
            return Err("catalogue_check_seeds must be at least 1");
        }
        if self.fill_worker_jobs == 0 {
            return Err("fill_worker_jobs must be at least 1");
        }
        // Generation treats a gen_concurrency of 0 as 1.
        let generations = if self.gen_concurrency == 0 { 1 } else { self.gen_concurrency };
        if self.fill_pool_workers != 0 && self.fill_pool_workers < generations {
            return Err("fill_pool_workers must be 0 (no pool) or at least gen_concurrency: \
                 with fewer, generations that could fill side by side would wait for a worker");
        }
        if self.fill_worker_recycle_percent == 0 || self.fill_worker_recycle_percent > 100 {
            return Err("fill_worker_recycle_percent must be 1 to 100");
        }
        if self.password_hash_parallel == 0 {
            return Err("password_hash_parallel must be at least 1");
        }
        Ok(())
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::DESIGN
    }
}

/// The only place the process environment is read.
///
/// # Errors
/// Overrides that break a rule between the limits ([`Limits::check`]).
#[cfg(feature = "std")]
pub fn load() -> Result<Limits, String> {
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
    l.signup_open = match std::env::var("CONSTELLATION_SIGNUP").ok().as_deref() {
        None | Some("" | "open") => true,
        Some("closed") => false,
        Some(_) => return Err("CONSTELLATION_SIGNUP must be `open` or `closed`".into()),
    };
    for (key, field) in [
        ("CONSTELLATION_MAX_CONNECTIONS", &mut l.max_connections),
        ("CONSTELLATION_MAX_RESIDENT_ROOMS", &mut l.max_resident_rooms),
        ("CONSTELLATION_GEN_CONCURRENCY", &mut l.gen_concurrency),
        ("CONSTELLATION_GEN_QUEUE_MAX", &mut l.gen_queue_max),
        ("CONSTELLATION_FILL_POOL_WORKERS", &mut l.fill_pool_workers),
        ("CONSTELLATION_MANIFEST_CACHE_BYTES", &mut l.manifest_cache_bytes),
        ("CONSTELLATION_CATALOGUE_CHECK_SEEDS", &mut l.catalogue_check_seeds),
        ("CONSTELLATION_CATALOGUE_CHECK_CONCURRENCY", &mut l.catalogue_check_concurrency),
    ] {
        if let Some(v) = num(key).and_then(|v| usize::try_from(v).ok()) {
            *field = v;
        }
    }
    for (key, field) in [
        ("CONSTELLATION_CONNECT_TIMEOUT_MS", &mut l.connect_timeout_ms),
        ("CONSTELLATION_IDLE_TIMEOUT_MS", &mut l.idle_timeout_ms),
        ("CONSTELLATION_PING_INTERVAL_MS", &mut l.ping_interval_ms),
        ("CONSTELLATION_GEN_TIMEOUT_MS", &mut l.gen_timeout_ms),
        ("CONSTELLATION_GEN_MEMORY_BYTES", &mut l.gen_memory_bytes),
        ("CONSTELLATION_FILL_MEMORY_BYTES", &mut l.fill_memory_bytes),
        ("CONSTELLATION_FILL_TIMEOUT_MS", &mut l.fill_timeout_ms),
        ("CONSTELLATION_GEN_CGROUP_MEMORY_BYTES", &mut l.gen_cgroup_memory_bytes),
        ("CONSTELLATION_ROOMS_SCAN_MS", &mut l.rooms_scan_ms),
        ("CONSTELLATION_ROOM_IDLE_SECS", &mut l.room_idle_secs),
        ("CONSTELLATION_ROOM_ABANDON_SECS", &mut l.room_abandon_secs),
        ("CONSTELLATION_ROOM_TOUCH_SECS", &mut l.room_touch_secs),
        ("CONSTELLATION_ROOM_RETRY_FIRST_MS", &mut l.room_retry_first_ms),
        ("CONSTELLATION_ROOM_RETRY_MAX_MS", &mut l.room_retry_max_ms),
        ("CONSTELLATION_ROOM_HEALTHY_SECS", &mut l.room_healthy_secs),
    ] {
        if let Some(v) = num(key) {
            *field = v;
        }
    }
    l.check().map_err(|e| format!("limits: {e}"))?;
    Ok(l)
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
    /// Postgres URL for the migrations, as the role that owns the tables:
    /// `CONSTELLATION_MIGRATE_DATABASE_URL`, or the first line of the file named by
    /// `CONSTELLATION_MIGRATE_DATABASE_URL_FILE`. `None`: the migrations run as the runtime
    /// role (what a deployment from before the split does; the server logs it).
    pub migrate_database_url: Option<String>,
}

#[cfg(feature = "std")]
impl core::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let pw = self.room_password.as_ref().map(|_| "<redacted>");
        let db = self.database_url.as_ref().map(|_| "<redacted>");
        let migrate = self.migrate_database_url.as_ref().map(|_| "<redacted>");
        f.debug_struct("Secrets")
            .field("room_password", &pw)
            .field("database_url", &db)
            .field("migrate_database_url", &migrate)
            .finish()
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
    let migrate_database_url = var_or_file("CONSTELLATION_MIGRATE_DATABASE_URL", &var, &read)?;
    if let Some(url) = &migrate_database_url {
        check_database_url(url)?;
    }
    Ok(Secrets {
        room_password: var_or_file("CONSTELLATION_ROOM_PASSWORD", &var, &read)?,
        database_url,
        migrate_database_url,
    })
}

/// Where seed generation finds Archipelago and our sidecar. All three are needed for the
/// upload and generation API (`--data-dir`).
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    /// An Archipelago checkout (`Generate.py`, `custom_worlds/`): `CONSTELLATION_AP_DIR`.
    pub ap_dir: std::path::PathBuf,
    /// A Python with Archipelago's dependencies installed: `CONSTELLATION_AP_PYTHON`.
    pub python: std::path::PathBuf,
    /// The sidecar script, normally `tools/ap-gen/ap_gen.py`: `CONSTELLATION_AP_GEN_SCRIPT`.
    pub sidecar: std::path::PathBuf,
    /// Host paths no sandbox mount may contain: the server user's home (`HOME`) and the secret
    /// files (`CONSTELLATION_*_FILE`).
    pub private: Vec<std::path::PathBuf>,
    /// `CONSTELLATION_SANDBOX_MODE=container`: no per-job cgroup (`systemd-run --user`); the
    /// container's own memory and process limits bound the jobs instead. Never detected, only
    /// asked for. Default (`systemd`, or unset): a cgroup per job.
    pub container_mode: bool,
}

/// The Postgres role the server runs as: SELECT, INSERT, UPDATE and DELETE on our tables and
/// USAGE on their sequences, nothing else (no CREATE, no DDL, no superuser). The migrations run
/// as [`DB_MIGRATE_ROLE`] (`CONSTELLATION_MIGRATE_DATABASE_URL`), which owns the tables and
/// grants this role its rights after each migration. `docker/db-roles.sh` creates both.
pub const DB_APP_ROLE: &str = "constellation_app";

/// The Postgres role that owns the tables and runs the migrations (see [`DB_APP_ROLE`]).
pub const DB_MIGRATE_ROLE: &str = "constellation_migrate";

/// The secret file variables (see [`Secrets`]); their files must never be visible to a sandbox.
#[cfg(feature = "std")]
const SECRET_FILE_VARS: [&str; 3] = [
    "CONSTELLATION_ROOM_PASSWORD_FILE",
    "CONSTELLATION_DATABASE_URL_FILE",
    "CONSTELLATION_MIGRATE_DATABASE_URL_FILE",
];

/// Read [`Generation`] from the environment: `None` when none of its variables is set.
///
/// # Errors
/// Some but not all set, or a path that is not absolute (the sandbox mounts them by path).
#[cfg(feature = "std")]
pub fn load_generation() -> Result<Option<Generation>, String> {
    generation_from(|k| std::env::var(k).ok())
}

#[cfg(feature = "std")]
fn generation_from(var: impl Fn(&str) -> Option<String>) -> Result<Option<Generation>, String> {
    const KEYS: [&str; 3] =
        ["CONSTELLATION_AP_DIR", "CONSTELLATION_AP_PYTHON", "CONSTELLATION_AP_GEN_SCRIPT"];
    let container_mode = match var("CONSTELLATION_SANDBOX_MODE").as_deref() {
        None | Some("" | "systemd") => false,
        Some("container") => true,
        Some(_) => return Err("CONSTELLATION_SANDBOX_MODE must be `systemd` or `container`".into()),
    };
    let [ap, py, side] = KEYS.map(|k| var(k).filter(|v| !v.is_empty()));
    let (Some(ap), Some(py), Some(side)) = (&ap, &py, &side) else {
        if ap.is_none() && py.is_none() && side.is_none() {
            return Ok(None);
        }
        return Err(format!("set all of {} or none", KEYS.join(", ")));
    };
    let path = |v: &str, k: &str| {
        let p = std::path::PathBuf::from(v);
        if p.is_absolute() { Ok(p) } else { Err(format!("{k} must be an absolute path")) }
    };
    let private = ["HOME"]
        .iter()
        .chain(SECRET_FILE_VARS.iter())
        .filter_map(|k| var(k).filter(|v| !v.is_empty()))
        .map(std::path::PathBuf::from)
        .collect();
    Ok(Some(Generation {
        ap_dir: path(ap, KEYS[0])?,
        python: path(py, KEYS[1])?,
        sidecar: path(side, KEYS[2])?,
        private,
        container_mode,
    }))
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
        assert_eq!((l.max_rooms, l.max_resident_rooms), (1000, 250));
        assert_eq!(l.rooms_scan_ms, 2_000);
        assert_eq!((l.room_idle_secs, l.room_abandon_secs), (900, 2_592_000));
        assert_eq!((l.room_touch_secs, l.room_key_draws), (60, 3));
        assert_eq!((l.room_retry_first_ms, l.room_retry_max_ms), (1_000, 60_000));
        assert_eq!(l.room_healthy_secs, 30);
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

    /// The role names live here; the roles script and the compose files (which cannot import a
    /// constant) must spell them the same.
    #[test]
    fn the_roles_script_and_compose_use_the_role_names_from_here() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
        let init = read("docker/db-roles.sh");
        for role in [DB_APP_ROLE, DB_MIGRATE_ROLE] {
            assert!(
                init.contains(&format!("CREATE ROLE {role} LOGIN")),
                "{role} in the roles script"
            );
        }
        let env_example = read(".env.example");
        assert!(env_example.contains("CONSTELLATION_MIGRATE_DATABASE_URL_FILE"));
        let smoke = read(".github/workflows/release.yml");
        assert!(smoke.contains(&format!("postgres://{DB_APP_ROLE}:")));
        assert!(smoke.contains(&format!("postgres://{DB_MIGRATE_ROLE}:")));
    }

    #[test]
    fn the_migration_url_comes_from_its_own_variable_or_file_and_prints_redacted() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned())
        };
        let file = |_: &std::path::Path| Ok("postgres://m:p@h/db\n".to_owned());
        let direct =
            secrets_from(env(&[("CONSTELLATION_MIGRATE_DATABASE_URL", "postgres://m@h/db")]), file)
                .unwrap();
        assert_eq!(direct.migrate_database_url.as_deref(), Some("postgres://m@h/db"));
        assert_eq!(direct.database_url, None, "the runtime URL is its own");
        let from_file =
            secrets_from(env(&[("CONSTELLATION_MIGRATE_DATABASE_URL_FILE", "/m")]), file).unwrap();
        assert_eq!(from_file.migrate_database_url.as_deref(), Some("postgres://m:p@h/db"));
        assert!(!format!("{from_file:?}").contains("postgres://"), "{from_file:?}");
        assert_eq!(secrets_from(env(&[]), file).unwrap().migrate_database_url, None);
        let bad = secrets_from(
            env(&[("CONSTELLATION_MIGRATE_DATABASE_URL", "postgres://h/db?bogus=1")]),
            file,
        );
        assert!(bad.is_err());
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
            database_url: Some("postgres://u:hunter2@h".to_owned()),
            ..Secrets::default()
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
            .env("CONSTELLATION_MAX_RESIDENT_ROOMS", "7")
            .env("CONSTELLATION_CONNECT_TIMEOUT_MS", "11")
            .env("CONSTELLATION_IDLE_TIMEOUT_MS", "not-a-number")
            .env("CONSTELLATION_PING_INTERVAL_MS", "-4")
            .env("CONSTELLATION_GEN_TIMEOUT_MS", "1234")
            .env("CONSTELLATION_GEN_MEMORY_BYTES", "5678")
            .env("CONSTELLATION_SIGNUP", "closed")
            .env("CONSTELLATION_ROOM_PASSWORD", "pw-probe")
            .env("CONSTELLATION_AP_DIR", "/probe/ap")
            .env("CONSTELLATION_AP_PYTHON", "/probe/python")
            .env("CONSTELLATION_AP_GEN_SCRIPT", "/probe/ap_gen.py")
            .env("CONSTELLATION_DATABASE_URL_FILE", &url_file)
            .env_remove("CONSTELLATION_ROOM_PASSWORD_FILE")
            .env_remove("CONSTELLATION_DATABASE_URL")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text}");
        let d = Limits::DESIGN;
        // Bad numbers keep the design value.
        let want = format!(
            "probe limits 77 5 Some(9) 3 11 {} {} 1234 5678 7",
            d.idle_timeout_ms, d.ping_interval_ms
        );
        assert!(text.contains(&want), "want {want:?} in {text}");
        assert!(text.contains("probe signup false"), "closed by CONSTELLATION_SIGNUP: {text}");
        assert!(
            text.contains("probe secrets Some(\"pw-probe\") Some(\"postgres://probe@h/db\")"),
            "{text}"
        );
        assert!(
            text.contains("probe generation /probe/ap /probe/python /probe/ap_gen.py"),
            "{text}"
        );
    }

    /// The child half of the test above; does nothing unless that test started it.
    #[test]
    fn env_probe() {
        if std::env::var("CONFIG_ENV_PROBE").is_err() {
            return;
        }
        let l = load().unwrap();
        println!("probe signup {}", l.signup_open);
        println!(
            "probe limits {} {} {:?} {} {} {} {} {} {} {}",
            l.dedupe_window_secs,
            l.sim_seeds,
            l.sim_replay_seed,
            l.max_connections,
            l.connect_timeout_ms,
            l.idle_timeout_ms,
            l.ping_interval_ms,
            l.gen_timeout_ms,
            l.gen_memory_bytes,
            l.max_resident_rooms
        );
        let s = load_secrets().unwrap();
        println!("probe secrets {:?} {:?}", s.room_password, s.database_url);
        let g = load_generation().unwrap().expect("the paths are set");
        println!(
            "probe generation {} {} {}",
            g.ap_dir.display(),
            g.python.display(),
            g.sidecar.display()
        );
    }

    #[test]
    fn secrets_print_redacted() {
        let s = Secrets { room_password: Some("hunter2".to_owned()), ..Secrets::default() };
        let text = format!("{s:?}");
        assert!(!text.contains("hunter2") && text.contains("redacted"), "{text}");
    }

    #[test]
    fn connection_limits_are_design_values_and_the_ping_beats_the_idle_timeout() {
        let l = Limits::DESIGN;
        assert_eq!(l.max_connections, 10_000);
        assert_eq!((l.view_history_events, l.view_streams), (400, 64), "the room view caps");
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

    /// The fill worker pool is off unless asked for (zero workers: each fill in a process of
    /// its own). Asked for, it must have a worker for every generation that may run at once,
    /// or fills that ran side by side would wait in line for a worker.
    #[test]
    fn the_fill_pool_is_off_by_default_and_never_smaller_than_gen_concurrency() {
        let l = Limits::DESIGN;
        assert_eq!((l.fill_pool_workers, l.fill_worker_jobs), (0, 256));
        assert_eq!((PREPARED_CACHE_BYTES, l.fill_worker_cache_bytes), (32 << 20, 32 << 20));
        assert_eq!((l.fill_worker_recycle_percent, FILL_WORKER_CPU_MARGIN_MS), (50, 1000));
        assert!(l.check().is_ok(), "off");
        // On: 1 worker for 1 generation at a time, 4 or 6 for 4; never 1, 2 or 3 for 4.
        let with = |fill_pool_workers, gen_concurrency| {
            Limits { fill_pool_workers, gen_concurrency, ..l }.check()
        };
        for (workers, generations) in [(1, 1), (1, 0), (4, 4), (6, 4), (0, 4)] {
            assert!(with(workers, generations).is_ok(), "{workers} for {generations}");
        }
        for (workers, generations) in [(1, 4), (3, 4), (1, 2)] {
            let e = with(workers, generations).unwrap_err();
            assert!(e.contains("fill_pool_workers"), "{workers} for {generations}: {e}");
        }
        let e = Limits { fill_worker_jobs: 0, ..l }.check().unwrap_err();
        assert!(e.contains("fill_worker_jobs"), "{e}");
        for percent in [0, 101] {
            let e = Limits { fill_worker_recycle_percent: percent, ..l }.check().unwrap_err();
            assert!(e.contains("fill_worker_recycle_percent"), "{e}");
        }
    }

    #[test]
    fn upload_and_generation_limits_are_design_values() {
        let l = Limits::DESIGN;
        assert_eq!((l.apworld_max_bytes, l.yaml_max_bytes), (64 << 20, 1 << 20));
        assert_eq!(l.manifest_max_bytes, 8 << 20);
        assert_eq!((l.gen_concurrency, l.gen_queue_max), (1, 64));
        assert_eq!(
            (l.fill_memory_bytes, l.fill_cpu_secs, l.fill_timeout_ms),
            (1 << 30, 120, 60_000)
        );
        assert_eq!(l.fill_max_processes, 8192);
        assert_eq!((l.admin_write_burst, l.admin_write_refill_milli_per_sec), (30, 1000));
        assert_eq!(
            (l.password_min_chars, l.password_max_bytes, l.account_name_max_bytes),
            (10, 256, 64)
        );
        assert_eq!((l.session_ttl_secs, l.login_fail_burst), (1_209_600, 5));
        assert!(l.signup_open, "sign-up is open unless the operator closes it");
        assert_eq!((l.apworld_max_unpacked_bytes, l.apworld_max_entries), (256 << 20, 20_000));
        assert_eq!((l.inspect_timeout_ms, l.gen_timeout_ms), (30_000, 900_000));
        assert_eq!((l.gen_memory_bytes, l.gen_error_max_bytes), (4 << 30, 16 * 1024));
        assert_eq!(l.gen_stdout_max_bytes, (64 << 20) + (1 << 20));
        assert!(l.gen_stdout_max_bytes as u64 > l.bundle_max_bytes, "a full bundle fits");
        assert_eq!(
            (l.gen_tmpfs_bytes, l.gen_file_max_bytes, l.gen_max_processes),
            (256 << 20, 256 << 20, 64)
        );
        assert_eq!(
            (l.gen_max_open_files, l.gen_cgroup_memory_bytes, l.gen_cgroup_tasks),
            (256, 4 << 30, 80)
        );
        assert!(l.gen_cgroup_tasks > l.gen_max_processes, "room for bwrap's own processes");
        assert_eq!((l.gen_heartbeat_secs, l.gen_stale_secs), (15, 60));
        assert_eq!((l.gen_lost_secs, l.job_log_files_max), (1200, 32));
        assert_eq!(l.check(), Ok(()));
    }

    /// An operator's override must keep the rules between the limits, not only the defaults.
    #[test]
    fn limits_that_break_a_rule_are_refused() {
        let d = Limits::DESIGN;
        let timeout = Limits { gen_timeout_ms: d.gen_lost_secs * 1000, ..d };
        assert!(timeout.check().unwrap_err().contains("gen_timeout_ms"));
        let beats = Limits { gen_stale_secs: 2 * d.gen_heartbeat_secs, ..d };
        assert!(beats.check().unwrap_err().contains("gen_heartbeat_secs"));
        let order = Limits { gen_lost_secs: d.gen_stale_secs - 1, ..d };
        assert!(order.check().unwrap_err().contains("gen_stale_secs"));
    }

    #[test]
    fn generation_paths_come_from_the_environment_and_are_all_or_nothing() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned())
        };
        assert_eq!(generation_from(env(&[])), Ok(None));
        let all: &'static [(&str, &str)] = &[
            ("CONSTELLATION_AP_DIR", "/ap"),
            ("CONSTELLATION_AP_PYTHON", "/venv/bin/python"),
            ("CONSTELLATION_AP_GEN_SCRIPT", "/tools/ap-gen/ap_gen.py"),
        ];
        let g = generation_from(env(all)).unwrap().unwrap();
        assert_eq!(g.ap_dir, std::path::PathBuf::from("/ap"));
        assert_eq!(g.python, std::path::PathBuf::from("/venv/bin/python"));
        assert_eq!(g.sidecar, std::path::PathBuf::from("/tools/ap-gen/ap_gen.py"));
        assert!(g.private.is_empty());
        let with_private: &'static [(&str, &str)] = &[
            ("CONSTELLATION_AP_DIR", "/ap"),
            ("CONSTELLATION_AP_PYTHON", "/venv/bin/python"),
            ("CONSTELLATION_AP_GEN_SCRIPT", "/tools/ap-gen/ap_gen.py"),
            ("HOME", "/home/u"),
            ("CONSTELLATION_ROOM_PASSWORD_FILE", "/etc/c/token"),
            ("CONSTELLATION_DATABASE_URL_FILE", "/etc/c/db"),
        ];
        let g = generation_from(env(with_private)).unwrap().unwrap();
        let want: Vec<std::path::PathBuf> =
            ["/home/u", "/etc/c/token", "/etc/c/db"].iter().map(Into::into).collect();
        assert_eq!(g.private, want);
        let part = generation_from(env(&[("CONSTELLATION_AP_DIR", "/ap")])).unwrap_err();
        assert!(part.contains("CONSTELLATION_AP_PYTHON"), "{part}");
        let rel: &'static [(&str, &str)] = &[
            ("CONSTELLATION_AP_DIR", "ap"),
            ("CONSTELLATION_AP_PYTHON", "/p"),
            ("CONSTELLATION_AP_GEN_SCRIPT", "/s"),
        ];
        assert!(generation_from(env(rel)).unwrap_err().contains("absolute"));
        // The sandbox mode: systemd by default, container only when asked, anything else refused.
        macro_rules! mode {
            ($m:expr) => {
                generation_from(env(&[
                    ("CONSTELLATION_AP_DIR", "/ap"),
                    ("CONSTELLATION_AP_PYTHON", "/venv/bin/python"),
                    ("CONSTELLATION_AP_GEN_SCRIPT", "/tools/ap_gen.py"),
                    ("CONSTELLATION_SANDBOX_MODE", $m),
                ]))
                .map(|g| g.unwrap().container_mode)
            };
        }
        assert_eq!(mode!(""), Ok(false));
        assert_eq!(mode!("systemd"), Ok(false));
        assert_eq!(mode!("container"), Ok(true));
        assert!(mode!("docker").unwrap_err().contains("SANDBOX_MODE"));
        assert!(generation_from(env(&[("CONSTELLATION_SANDBOX_MODE", "x")])).is_err());
    }

    #[test]
    fn session_rate_limits_are_design_values() {
        let l = Limits::DESIGN;
        assert_eq!((l.hint_burst, l.hint_refill_milli_per_sec), (10, 1000));
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
