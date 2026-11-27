//! Where the time of a data-only generation goes, and what the fill pool changes, for a room
//! of 1 and of 10 players on one synthetic world (`data_fill::synth`), through the real server
//! binary of this build's profile:
//!
//! ```sh
//! cargo build --release -p server --bin constellation-server
//! cargo test --release -p server --lib --features gen-bench gen_bench -- --nocapture
//! ```
//!
//! One JSON line per room (`"bench":"gen"`):
//! - `split`: the parts of a job in a process of its own, each the median of [`REPS`] runs on
//!   one fixed seed: `start_ms` (a job process that does nothing, from start to exit),
//!   `pipe_in_ms` (the fill's input on its stdin, by difference), `load_ms`, `fill_ms`,
//!   `encode_ms` (the job's own steps, timed in this process), `job_ms`, and `rest_ms` (what
//!   the parts leave of the job: inferred);
//! - `job`: the fill job timed [`REPS`] times, each time on another seed and each time by
//!   every runner in turn, so runner noise falls on all alike: `own` (a process of its own),
//!   `cold` (a pool with no worker yet), `miss` (a worker that is up and does not hold the
//!   world) and `warm` (a worker that holds it). Each is `{min, med, max}` in ms;
//! - `request`: a whole request, from `create_world` to its status reading done (polled), on
//!   a server without a pool (`own`) and on one with a pool (`pool`), in turn, with the
//!   median of each phase the server logged (`laps`).
//!
//! Then one line per world shape of a worker's memory as its cache fills
//! (`"bench":"gen-memory"`, see [`memory`]).
//!
//! The store is the in-memory one and no HTTP is parsed. Only CI's numbers count (the `bench`
//! workflow runs this three times); a debug build runs a small room, to show the bench works.

use super::pool_path::{ON as POOLED, server_binary};
use super::*;
use crate::pool::{Job, status_bytes};
use crate::sandbox::run_job;
use data_fill::prepared::Prepared;
use data_fill::room::Algorithm;
use data_fill::seed::{encode_on, generate_on};
use data_fill::synth::{Shape, world};
use std::collections::BTreeMap;

/// Shared with the pool's tests: worlds and requests made without a server.
#[path = "../../../tests/support/fills.rs"]
mod fills;

const REPS: usize = if cfg!(debug_assertions) { 3 } else { 21 };
const CHECKS: usize = if cfg!(debug_assertions) { 100 } else { 1000 };
const PLAYERS: [usize; 2] = [1, 10];

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}

fn fine(ms: f64) -> f64 {
    (ms * 1e3).round() / 1e3
}

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(f64::total_cmp);
    fine(times[times.len() / 2])
}

/// The least, the median and the most of `times`.
fn spread(times: &[f64]) -> Value {
    let least = times.iter().copied().fold(f64::MAX, f64::min);
    let most = times.iter().copied().fold(0.0, f64::max);
    json!({"min": fine(least), "med": median(times.to_vec()), "max": fine(most)})
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

/// Job `job` on `input` in a child process: its time and its result.
async fn child(exe: &Path, job: &str, input: &[u8]) -> (f64, Result<Vec<u8>, Failure>) {
    let l = Limits::DESIGN;
    let t = Instant::now();
    let out = run_job(exe, job, input, &l, Duration::from_millis(l.fill_timeout_ms)).await;
    (ms(t), out)
}

/// The median time of job `job` on `input` in a child process, and its last result.
async fn child_timed(exe: &Path, job: &str, input: &[u8]) -> (f64, Result<Vec<u8>, Failure>) {
    let (mut times, mut last) = (Vec::new(), Err(Failure::Broken("never ran".into())));
    for _ in 0..REPS {
        let (t, out) = child(exe, job, input).await;
        times.push(t);
        last = out;
    }
    (median(times), last)
}

/// One generation through `w`, from the request to its status reading done: its wall time,
/// and the phases the server logged for it.
async fn request(w: &Arc<Worlds>, body: &Value) -> (f64, serde_json::Map<String, Value>) {
    let t = Instant::now();
    let queued = create_world(State(Arc::clone(w)), me(), Bytes::from(body.to_string()));
    let id = json_of(queued.await.unwrap()).await["id"].as_str().unwrap().to_owned();
    loop {
        let status = read_status(w, &id).await.unwrap();
        match status["status"].as_str() {
            Some("done") => return (ms(t), w.laps.lock().unwrap().clone()),
            Some("failed") => panic!("{status}"),
            _ => tokio::task::yield_now().await,
        }
    }
}

/// A server with one synthetic world uploaded and `players` player files on it.
struct Room {
    w: Arc<Worlds>,
    /// The body of the request for a world of these players.
    asks: Value,
    prepared: PreparedWorld,
    files: Vec<(String, Vec<u8>)>,
}

impl Room {
    async fn new(name: &str, shape: &Shape, players: usize, exe: &Path, limits: Limits) -> Self {
        let root = temp(&format!("gen-bench-{name}-{players}"));
        let pool = Pool::new(exe.to_owned(), limits);
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
        Self { w, asks: json!({"manifest": manifest_id, "yamls": yamls}), prepared, files }
    }

    /// A generation's fill request for these players on random stream `rng`.
    fn head(&self, rng: u64) -> Vec<u8> {
        fill_head("bench", &self.files, (rng, Limits::DESIGN.bundle_max_bytes)).unwrap()
    }
}

/// The parts of one job in a process of its own, on one seed (see the module's `split`).
async fn split(exe: &Path, room: &Room) -> Value {
    let head = room.head(7);
    let input = data_fill::job::frame(&head, &room.prepared).unwrap();
    let (start_ms, nothing) = child_timed(exe, "none", &[]).await;
    assert!(matches!(&nothing, Err(Failure::Refused(e)) if e.contains("unknown job")));
    let (fed_ms, _) = child_timed(exe, "none", &input).await;
    let (job_ms, seed) = child_timed(exe, "fill", &input).await;
    let seed = seed.unwrap();

    let f = data_fill::job::request(&head).unwrap();
    let (load_ms, loaded) = timed(|| Prepared::open(&room.prepared).unwrap());
    let fill =
        || generate_on(&loaded, &f.seed_name, &f.players, f.rng_seed, f.workers, f.algorithm);
    let (fill_ms, filled) = timed(|| fill().unwrap());
    let mut bundle = filled.0;
    let (encode_ms, encoded) = timed(|| encode_on(&loaded, &mut bundle, f.seed_max_bytes).unwrap());
    assert_eq!(encoded, seed, "the child's seed is the one the steps timed here make");
    let pipe_in_ms = fed_ms - start_ms;
    let rest_ms = job_ms - start_ms - pipe_in_ms - load_ms - fill_ms - encode_ms;
    json!({
        "start_ms": start_ms, "pipe_in_ms": fine(pipe_in_ms), "load_ms": load_ms,
        "fill_ms": fill_ms, "encode_ms": encode_ms, "job_ms": job_ms, "rest_ms": fine(rest_ms),
        "prepared_bytes": room.prepared.len(), "request_bytes": head.len(),
        "seed_bytes": seed.len(),
    })
}

/// One room's line: the split, then the job by every runner in turn and the request with and
/// without the pool in turn, [`REPS`] times, each time on another seed. Every pooled seed is
/// compared with the seed of the job in its own process.
async fn room(exe: &Path, profile: &str, shape: &Shape, players: usize) -> Value {
    let own = Room::new(profile, shape, players, exe, Limits::DESIGN).await;
    let pooled = Room::new(&format!("{profile}-pool"), shape, players, exe, POOLED).await;
    let split = split(exe, &own).await;
    let (key, file) = (&own.prepared.key, &own.prepared.file);
    let pool = || Pool::new(exe.to_owned(), POOLED).expect("one worker");
    // Another world, to have a worker up that does not hold this room's.
    let (other_key, other_file, instance) = fills::prepared(&fills::small(), 2);
    let other = fills::head(&["P0"], &instance, 1, Algorithm::Exact);
    let other = Job { head: &other, threads: 1, key: &other_key, file: &other_file };

    let kept = pool();
    let mut jobs: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut requests: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut laps: BTreeMap<&str, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    let (mut rng, mut done) = (0, 0);
    while done < REPS {
        rng += 1;
        assert!(rng <= 10 * REPS as u64, "too many seeds of this room cannot be filled");
        let head = own.head(rng);
        let input = data_fill::job::frame(&head, file).unwrap();
        // A seed the fill refuses (a synthetic world can be stuck) would end every worker.
        let (own_ms, Ok(seed)) = child(exe, "fill", &input).await else { continue };
        let job = Job { head: &head, threads: fill_threads(), key, file };
        if done == 0 {
            assert_eq!(fills::alone(&head, file).unwrap(), seed);
            kept.fill(job).await.unwrap();
        }
        let mut pooled_job = async |name, pool: &Pool| {
            let t = Instant::now();
            let filled = pool.fill(job).await.unwrap();
            jobs.entry(name).or_default().push(ms(t));
            assert_eq!(filled, seed, "{name}: not the seed of the job in its own process");
        };
        pooled_job("cold", &pool()).await;
        let up = pool();
        up.fill(other).await.unwrap();
        pooled_job("miss", &up).await;
        pooled_job("warm", &kept).await;
        jobs.entry("own").or_default().push(own_ms);
        for (name, room) in [("own", &own), ("pool", &pooled)] {
            let (wall, phases) = request(&room.w, &room.asks).await;
            requests.entry(name).or_default().push(wall);
            for (phase, v) in phases {
                let lap = laps.entry(name).or_default().entry(phase).or_default();
                lap.push(v.as_f64().unwrap());
            }
        }
        done += 1;
    }
    let pids = kept.idle_pids();
    assert_eq!(pids.len(), 1, "one worker took every warm job");
    let spreads = |all: &BTreeMap<&str, Vec<f64>>| -> BTreeMap<String, Value> {
        all.iter().map(|(name, times)| ((*name).to_owned(), spread(times))).collect()
    };
    let laps: BTreeMap<&str, BTreeMap<String, f64>> = laps
        .into_iter()
        .map(|(name, phases)| (name, phases.into_iter().map(|(p, v)| (p, median(v))).collect()))
        .collect();
    json!({
        "bench": "gen", "profile": profile, "players": players, "checks": shape.checks,
        "reps": REPS, "seeds_tried": rng, "split": split, "job": spreads(&jobs),
        "request": spreads(&requests), "laps": laps,
        "rss_idle_bytes": status_bytes(pids[0], "VmRSS:"),
        "vm_idle_bytes": status_bytes(pids[0], "VmSize:"),
    })
}

/// A worker's memory as its cache fills: one worker (never replaced here, so this is the
/// most a worker can come to hold) fills one player on world after world of one shape until
/// what the cache counts for the worlds it was sent passes `config::PREPARED_CACHE_BYTES` by
/// a tenth, so the cache is full and has dropped worlds. Every world is the same synthetic
/// world under another game name: another file and key, loaded apart, and a fill that is
/// known to succeed (a refusal would end the worker). The worker's resident memory must not
/// have grown by more than twice the cap: the cap is one in real bytes. A debug build stops
/// after a few worlds.
async fn memory(exe: &Path, profile: &str, shape: &Shape) -> Value {
    let l = Limits { fill_worker_jobs: usize::MAX, fill_worker_recycle_percent: 100, ..POOLED };
    let pool = Pool::new(exe.to_owned(), l).expect("one worker");
    let (mut manifest, instance) = world(shape, 1, 0);
    manifest["version"] = json!("1");
    let prepare = |manifest: &Value| {
        let out = data_fill::job::prepare(manifest.to_string().as_bytes()).unwrap();
        data_fill::job::unframe(&out).unwrap().1.to_vec()
    };
    // A seed this world fills on.
    manifest["game"] = json!("Synth");
    let first = prepare(&manifest);
    let head = (1..=40)
        .map(|rng| fills::head(&["P0"], &instance, rng, Algorithm::Exact))
        .find(|head| fills::alone(head, &first).is_ok())
        .expect("a seed this world fills on");
    let held = Prepared::open(&first).unwrap().held_bytes();
    let most = if cfg!(debug_assertions) { 3 } else { usize::MAX };
    let full = config::PREPARED_CACHE_BYTES;
    let (mut worlds, mut pid) = (0_usize, 0);
    let (mut rss_one, mut vm_one) = (None, None);
    while worlds * held <= full + full / 10 && worlds < most {
        manifest["game"] = json!(format!("Synth {worlds}"));
        let file = prepare(&manifest);
        let job = Job { head: &head, threads: 1, key: &log::blob_key(&file), file: &file };
        pool.fill(job).await.unwrap();
        let pids = pool.idle_pids();
        if worlds == 0 {
            pid = pids[0];
            (rss_one, vm_one) = (status_bytes(pid, "VmRSS:"), status_bytes(pid, "VmSize:"));
        }
        assert_eq!(pids, [pid], "the same worker throughout");
        worlds += 1;
    }
    let (rss_full, vm_full) = (status_bytes(pid, "VmRSS:"), status_bytes(pid, "VmSize:"));
    let cache_full = worlds * held > full;
    let grown = rss_full.unwrap_or(0).saturating_sub(rss_one.unwrap_or(0));
    assert!(!cache_full || grown <= 2 * full as u64, "{profile}: grew by {grown} bytes");
    json!({
        "bench": "gen-memory", "profile": profile, "checks": shape.checks, "worlds": worlds,
        "worlds_kept": worlds.min(full / held.max(1)), "file_bytes": first.len(),
        "held_bytes_per_world": held, "cache_cap_bytes": full, "cache_full": cache_full,
        "rss_one_world_bytes": rss_one, "rss_full_cache_bytes": rss_full,
        "vm_one_world_bytes": vm_one, "vm_full_cache_bytes": vm_full,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gen_bench_times_a_generation_part_by_part() {
    let exe = server_binary();
    let shapes = [("light", Shape::default()), ("heavy", Shape::heavy())];
    for (profile, shape) in shapes {
        let shape = Shape { checks: CHECKS, ..shape };
        for players in PLAYERS {
            println!("{}", room(&exe, profile, &shape, players).await);
        }
    }
    // Two shapes that hold different amounts per byte of their file: the heavy one, and the
    // wide one (a small file per world, so many more worlds fit the cap).
    println!("{}", memory(&exe, "heavy", &Shape { checks: CHECKS, ..Shape::heavy() }).await);
    println!("{}", memory(&exe, "wide", &Shape::wide()).await);
}
