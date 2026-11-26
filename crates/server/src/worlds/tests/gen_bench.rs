//! Where the time of a data-only generation goes, for a room of 1 and of 10 players on one
//! synthetic world (`data_fill::synth`), through the real server binary of this build's profile:
//!
//! ```sh
//! cargo build --release -p server --bin constellation-server
//! cargo test --release -p server --lib --features gen-bench gen_bench -- --nocapture
//! ```
//!
//! One JSON line per room and job runner, each number the median of [`REPS`] runs:
//! - `start_ms`: a job process that does nothing, from its start to its exit (`prlimit`, the
//!   server binary, an unknown job);
//! - `pipe_in_ms`: the same with the fill's whole input on its stdin, less `start_ms`;
//! - `load_ms`, `fill_ms`, `encode_ms`: the job's own steps, timed in this process;
//! - `job_ms`: the fill job as the server runs it; `rest_ms` is what the parts above leave of
//!   it (the request parsed, the seed piped back, noise);
//! - `request_ms`: a whole request, from `create_world` to its status reading done (polled),
//!   and `phase_ms`: that generation's own laps (its report's `ms`).
//!
//! Then the same room through the fill pool (runner `pool`, see [`by_pool`]), and one line of
//! a worker's resident memory as its cache fills ([`memory`]).
//!
//! The store is the in-memory one and no HTTP is parsed. Only CI's numbers count (the `bench`
//! workflow); a debug build runs a small room, to show the bench itself works.

use super::*;
use crate::sandbox::run_job;
use data_fill::job::Fill;
use data_fill::prepared::Prepared;
use data_fill::seed::{Player, encode_on, generate_on};
use data_fill::synth::{Shape, world};
use std::collections::BTreeMap;

/// Shared with the pool's tests: worlds and requests made without a server.
#[path = "../../../tests/support/fills.rs"]
mod fills;

const REPS: usize = if cfg!(debug_assertions) { 3 } else { 21 };
const CHECKS: usize = if cfg!(debug_assertions) { 100 } else { 1000 };
const PLAYERS: [usize; 2] = [1, 10];

/// The server binary cargo built for this test binary's profile: in the nearest folder above
/// it (`target/<profile>/`; how deep the test binary sits under it depends on the cargo).
fn server_binary() -> PathBuf {
    let test = std::env::current_exe().unwrap();
    let bin = test.ancestors().map(|dir| dir.join("constellation-server")).find(|b| b.is_file());
    bin.unwrap_or_else(|| panic!("build the server binary of this profile first: {test:?}"))
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}

/// The median, to a thousandth of a millisecond.
fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(f64::total_cmp);
    (times[times.len() / 2] * 1e3).round() / 1e3
}

/// The median time of `f` over [`REPS`] runs, and its last result.
fn timed<T>(mut f: impl FnMut() -> T) -> (f64, T) {
    let mut run = || {
        let t = Instant::now();
        let out = f();
        (ms(t), out)
    };
    let (first, mut last) = run();
    let mut times = vec![first];
    for _ in 1..REPS {
        let (t, out) = run();
        times.push(t);
        last = out;
    }
    (median(times), last)
}

/// The median time of job `job` on `input` in a child process, and its last result.
async fn job_timed(exe: &Path, job: &str, input: &[u8]) -> (f64, Result<Vec<u8>, Failure>) {
    let l = Limits::DESIGN;
    let timeout = Duration::from_millis(l.fill_timeout_ms);
    let (mut times, mut last) = (Vec::new(), Err(Failure::Broken("never ran".into())));
    for _ in 0..REPS {
        let t = Instant::now();
        last = run_job(exe, job, input, &l, timeout).await;
        times.push(ms(t));
    }
    (median(times), last)
}

/// One generation through `w`, from the request to its status reading done: its wall time and
/// its report's laps.
async fn request(w: &Arc<Worlds>, body: &Value) -> (f64, Value) {
    let t = Instant::now();
    let queued = create_world(State(Arc::clone(w)), me(), Bytes::from(body.to_string()));
    let id = json_of(queued.await.unwrap()).await["id"].as_str().unwrap().to_owned();
    loop {
        let status = read_status(w, &id).await.unwrap();
        match status["status"].as_str() {
            Some("done") => return (ms(t), status["generation"]["ms"].clone()),
            Some("failed") => panic!("{status}"),
            _ => tokio::task::yield_now().await,
        }
    }
}

/// The medians of [`REPS`] whole requests: wall time, and each lap.
async fn requests(w: &Arc<Worlds>, body: &Value) -> (f64, BTreeMap<String, f64>) {
    let (mut walls, mut laps) = (Vec::new(), BTreeMap::<String, Vec<f64>>::new());
    for _ in 0..REPS {
        let (wall, ms) = request(w, body).await;
        walls.push(wall);
        for (phase, v) in ms.as_object().unwrap() {
            laps.entry(phase.clone()).or_default().push(v.as_f64().unwrap());
        }
    }
    (median(walls), laps.into_iter().map(|(k, v)| (k, median(v))).collect())
}

/// A server with one synthetic world uploaded and `players` player files on it, and a
/// generation's fill request for them as the server builds it.
struct Room {
    w: Arc<Worlds>,
    /// The body of the request for a world of these players.
    asks: Value,
    prepared: PreparedWorld,
    head: Vec<u8>,
}

async fn room(name: &str, shape: &Shape, players: usize, exe: &Path, pool: Option<Pool>) -> Room {
    let root = temp(&format!("gen-bench-{name}-{players}"));
    let limits = Limits::DESIGN;
    let w = hosting_on(&root, limits, JobRunner::Child(exe.to_owned()), pool).await;
    let (mut manifest, instance) = world(shape, 1, 0);
    manifest["game"] = json!("Synth");
    manifest["version"] = json!("1");
    let body = manifest.to_string();
    let up = upload_manifest(&w, Some("name=m.json"), body.as_bytes()).await.unwrap();
    let manifest_id = json_of(up).await["id"].as_str().unwrap().to_owned();
    let instance = serde_json::to_value(&instance).unwrap();
    let mut yamls = Vec::new();
    for p in 0..players {
        let meta = player_yaml(&w, &format!("P{p}"), "Synth", &instance).await;
        yamls.push(meta["id"].as_str().unwrap().to_owned());
    }
    let files = player_files(&w, &yamls).await.unwrap();
    let prepared = prepared_world(&w, &manifest_id).await.unwrap();
    let head = fill_head("bench", &files, (7, limits.bundle_max_bytes)).unwrap();
    Room { w, asks: json!({"manifest": manifest_id, "yamls": yamls}), prepared, head }
}

/// Step by step through a job process of its own: the row of runner `child`, and the seed.
async fn by_child(exe: &Path, profile: &str, shape: &Shape, players: usize) -> (Value, Vec<u8>) {
    let Room { w, asks, prepared, head } = room(profile, shape, players, exe, None).await;
    let input = data_fill::job::frame(&head, &prepared).unwrap();

    let (start_ms, nothing) = job_timed(exe, "none", &[]).await;
    assert!(matches!(&nothing, Err(Failure::Refused(e)) if e.contains("unknown job")));
    let (fed_ms, _) = job_timed(exe, "none", &input).await;
    let (job_ms, seed) = job_timed(exe, "fill", &input).await;
    let seed = seed.unwrap();

    let f: Fill<Player> = serde_json::from_slice(&head).unwrap();
    let (load_ms, loaded) = timed(|| Prepared::open(&prepared).unwrap());
    let fill =
        || generate_on(&loaded, &f.seed_name, &f.players, f.rng_seed, f.workers, f.algorithm);
    let (fill_ms, filled) = timed(|| fill().unwrap());
    let mut bundle = filled.0;
    let (encode_ms, encoded) = timed(|| encode_on(&loaded, &mut bundle, f.seed_max_bytes).unwrap());
    assert_eq!(encoded, seed, "the child's seed is the one the steps timed here make");

    let (request_ms, phase_ms) = requests(&w, &asks).await;
    let pipe_in_ms = fed_ms - start_ms;
    let rest_ms = job_ms - start_ms - pipe_in_ms - load_ms - fill_ms - encode_ms;
    let row = json!({
        "bench": "gen", "runner": "child", "profile": profile, "players": players,
        "checks": shape.checks, "reps": REPS, "prepared_bytes": prepared.len(),
        "request_bytes": head.len(), "seed_bytes": seed.len(),
        "start_ms": start_ms, "pipe_in_ms": fine(pipe_in_ms), "load_ms": load_ms,
        "fill_ms": fill_ms, "encode_ms": encode_ms, "job_ms": job_ms, "rest_ms": fine(rest_ms),
        "request_ms": request_ms, "phase_ms": phase_ms,
    });
    (row, seed)
}

fn fine(ms: f64) -> f64 {
    (ms * 1e3).round() / 1e3
}

/// The resident memory of process `pid`, bytes (`VmRSS`).
fn rss(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let kb = status.lines().find_map(|l| l.strip_prefix("VmRSS:")).unwrap();
    kb.trim().trim_end_matches("kB").trim().parse::<u64>().unwrap() * 1024
}

/// The same room through the pool: the row of runner `pool`. Its jobs: `cold_ms` on a pool
/// that has no worker yet (a process start and the world's load), `miss_ms` on a worker that
/// is up and does not hold the world, `warm_ms` on one that holds it. Its requests:
/// `request_first_ms` is the server's first (one sample: worker start and load), `request_ms`
/// the ones after. `rss_idle_bytes` is the worker's resident memory after those jobs, holding
/// the one world. `seed` is what the job in a process of its own made: the pool must too.
async fn by_pool(exe: &Path, profile: &str, shape: &Shape, players: usize, seed: &[u8]) -> Value {
    let l = Limits::DESIGN;
    let pool = || Pool::new(exe.to_owned(), l);
    let name = format!("{profile}-pool");
    let Room { w, asks, prepared, head } = room(&name, shape, players, exe, Some(pool())).await;
    let (key, file) = (&prepared.key, &prepared.file);
    assert_eq!(fills::alone(&head, file).unwrap(), seed);
    // Another world, to have a worker up that does not hold this room's.
    let (other_key, other_file, instance) = fills::prepared(&fills::small(), 2);
    let other = fills::head(&["P0"], &instance, 1, data_fill::room::Algorithm::Exact);

    let (mut cold, mut miss, mut warm) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..REPS {
        let fresh = pool();
        let t = Instant::now();
        assert_eq!(fresh.fill(&head, key, file).await.unwrap(), seed);
        cold.push(ms(t));
        let up = pool();
        up.fill(&other, &other_key, &other_file).await.unwrap();
        let t = Instant::now();
        assert_eq!(up.fill(&head, key, file).await.unwrap(), seed);
        miss.push(ms(t));
    }
    let kept = pool();
    kept.fill(&head, key, file).await.unwrap();
    for _ in 0..REPS {
        let t = Instant::now();
        assert_eq!(kept.fill(&head, key, file).await.unwrap(), seed);
        warm.push(ms(t));
    }
    let pids = kept.idle_pids();
    assert_eq!(pids.len(), 1, "one worker took every warm job");

    let (request_first_ms, _) = request(&w, &asks).await;
    let (request_ms, phase_ms) = requests(&w, &asks).await;
    json!({
        "bench": "gen", "runner": "pool", "profile": profile, "players": players,
        "checks": shape.checks, "reps": REPS, "cold_ms": median(cold), "miss_ms": median(miss),
        "warm_ms": median(warm), "request_first_ms": fine(request_first_ms),
        "request_ms": request_ms, "phase_ms": phase_ms, "rss_idle_bytes": rss(pids[0]),
    })
}

/// A worker's resident memory as its cache fills: one worker (never replaced here, so this is
/// the most a worker can come to hold) fills one player on world after world until the files
/// of the worlds it was sent pass `config::PREPARED_CACHE_BYTES` and the cache has begun to
/// drop the oldest. Every world is the heavy world of the rows above under another game name:
/// another file and key, loaded apart, and a fill that is known to succeed (a refusal would
/// end the worker). A debug build stops after a few worlds.
async fn memory(exe: &Path) -> Value {
    let l = Limits { fill_worker_jobs: usize::MAX, ..Limits::DESIGN };
    let pool = Pool::new(exe.to_owned(), l);
    let shape = Shape { checks: CHECKS, ..Shape::heavy() };
    let (mut manifest, instance) = world(&shape, 1, 0);
    manifest["version"] = json!("1");
    let head = fills::head(&["P0"], &instance, 7, data_fill::room::Algorithm::Exact);
    let most = if cfg!(debug_assertions) { 3 } else { usize::MAX };
    let full = config::PREPARED_CACHE_BYTES;
    let (mut worlds, mut sent, mut file_bytes) = (0_usize, 0_usize, 0);
    let (mut pid, mut rss_one_world) = (0, 0);
    // A tenth past the cap, so the cache is full and has dropped worlds.
    while sent <= full + full / 10 && worlds < most {
        manifest["game"] = json!(format!("Synth {worlds}"));
        let out = data_fill::job::prepare(manifest.to_string().as_bytes()).unwrap();
        let (_, file) = data_fill::job::unframe(&out).unwrap();
        pool.fill(&head, &log::blob_key(file), file).await.unwrap();
        let pids = pool.idle_pids();
        if worlds == 0 {
            (pid, rss_one_world) = (pids[0], rss(pids[0]));
        }
        assert_eq!(pids, [pid], "the same worker throughout");
        worlds += 1;
        sent += file.len();
        file_bytes = file.len();
    }
    json!({
        "bench": "gen-memory", "profile": "heavy", "checks": shape.checks, "worlds": worlds,
        "bytes_per_prepared_file": file_bytes, "prepared_bytes_sent": sent,
        "cache_cap_bytes": full, "cache_full": sent > full,
        "rss_one_world_bytes": rss_one_world, "rss_full_cache_bytes": rss(pid),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gen_bench_times_a_generation_part_by_part() {
    let exe = server_binary();
    let shapes = [("light", Shape::default()), ("heavy", Shape::heavy())];
    for (profile, shape) in shapes {
        let shape = Shape { checks: CHECKS, ..shape };
        for players in PLAYERS {
            let (row, seed) = by_child(&exe, profile, &shape, players).await;
            println!("{row}");
            println!("{}", by_pool(&exe, profile, &shape, players, &seed).await);
        }
    }
    println!("{}", memory(&exe).await);
}
