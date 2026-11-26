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
//! The store is the in-memory one and no HTTP is parsed. Only CI's numbers count (the `bench`
//! workflow); a debug build runs a small room, to show the bench itself works.

use super::*;
use crate::sandbox::run_job;
use data_fill::job::Fill;
use data_fill::prepared::Prepared;
use data_fill::seed::{Player, encode_on, generate_on};
use data_fill::synth::{Shape, world};
use std::collections::BTreeMap;

const REPS: usize = if cfg!(debug_assertions) { 3 } else { 21 };
const CHECKS: usize = if cfg!(debug_assertions) { 100 } else { 1000 };
const PLAYERS: [usize; 2] = [1, 10];

/// The server binary cargo built beside this test binary (`target/<profile>/`).
fn server_binary() -> PathBuf {
    let test = std::env::current_exe().unwrap();
    let bin = test.parent().and_then(Path::parent).unwrap().join("constellation-server");
    assert!(bin.is_file(), "build the server binary of this profile first: {}", bin.display());
    bin
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

async fn room(exe: &Path, profile: &str, shape: &Shape, players: usize) -> Value {
    let root = temp(&format!("gen-bench-{profile}-{players}"));
    let limits = Limits::DESIGN;
    let w = hosting_on(&root, limits, JobRunner::Child(exe.to_owned())).await;
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

    // The job's input as a generation builds it.
    let files = player_files(&w, &yamls).await.unwrap();
    let prepared = prepared_world(&w, &manifest_id).await.unwrap();
    let head = fill_head("bench", &files, (7, limits.bundle_max_bytes)).unwrap();
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

    let (request_ms, phase_ms) =
        requests(&w, &json!({"manifest": manifest_id, "yamls": yamls})).await;
    let pipe_in_ms = fed_ms - start_ms;
    let rest_ms = job_ms - start_ms - pipe_in_ms - load_ms - fill_ms - encode_ms;
    let fine = |ms: f64| (ms * 1e3).round() / 1e3;
    json!({
        "bench": "gen", "runner": "child", "profile": profile, "players": players,
        "checks": shape.checks, "reps": REPS, "prepared_bytes": prepared.len(),
        "request_bytes": head.len(), "seed_bytes": seed.len(),
        "start_ms": start_ms, "pipe_in_ms": fine(pipe_in_ms), "load_ms": load_ms,
        "fill_ms": fill_ms, "encode_ms": encode_ms, "job_ms": job_ms, "rest_ms": fine(rest_ms),
        "request_ms": request_ms, "phase_ms": phase_ms,
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
}
