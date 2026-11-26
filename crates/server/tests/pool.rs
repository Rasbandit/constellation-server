//! The fill pool through real worker processes (the server binary in job mode, under `prlimit`
//! as the server starts it): seeds byte for byte those of a job in a process of its own, and
//! what a worker that is killed, runs out of memory, overruns, is used up or refuses does to
//! its job and the pool. Feature `pool-tests`; CI's `check` job runs it on every PR.
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the server package"
)]

mod support {
    pub mod fills;
}

use config::Limits;
use data_fill::job;
use data_fill::prepared::Prepared;
use data_fill::room::{Algorithm, Dials};
use data_fill::synth::Shape;
use log::BlobKey;
use server::pool::{Job, Pool};
use server::{Failure, run_job};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::fills::{alone, head, prepared, small};

const BIN: &str = env!("CARGO_BIN_EXE_constellation-server");

/// The design limits with a pool of one worker.
const ON: Limits = Limits { fill_pool_workers: 1, ..Limits::DESIGN };

fn pool(bin: &Path, limits: Limits) -> Pool {
    Pool::new(bin.to_owned(), limits).expect("a pool of at least one worker")
}

fn job<'a>(head: &'a [u8], key: &'a BlobKey, file: &'a [u8]) -> Job<'a> {
    Job { head, threads: config::GENERATION_WORKERS, key, file }
}

/// What a `fill` job in a process of its own answers: the path the pool stands in for.
async fn own_process(head: &[u8], file: &[u8]) -> Result<Vec<u8>, Failure> {
    let l = Limits::DESIGN;
    let input = job::frame(head, file).unwrap();
    run_job(Path::new(BIN), "fill", &input, &l, Duration::from_millis(l.fill_timeout_ms)).await
}

/// A copy of the server binary under a name of this test's own, so its workers can be told
/// from every other test's in `/proc`.
fn own_binary(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pool-it-{test}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("server-copy");
    std::fs::copy(BIN, &bin).unwrap();
    bin
}

/// The processes running `bin`.
fn running(bin: &Path) -> Vec<u32> {
    let marker = bin.display().to_string();
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains(&marker))
        })
        .collect()
}

fn kill(pid: u32) {
    let sent = std::process::Command::new("kill").args(["-KILL", &pid.to_string()]).status();
    assert!(sent.unwrap().success(), "kill -KILL {pid}");
}

/// Give other tasks the thread until `done` holds: a signal the test waits on, never a sleep.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        tokio::task::yield_now().await;
    }
}

/// A job that runs long enough to be caught in the middle: ten players on a heavy world.
fn long_job() -> (BlobKey, Vec<u8>, Vec<u8>) {
    let (key, file, instance) = prepared(&Shape::heavy(), 3);
    let ten: Vec<String> = (0..10).map(|p| format!("P{p}")).collect();
    let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
    (key, file, head(&ten, &instance, 1, Algorithm::Exact))
}

/// Byte identity through real processes: on two worlds, for one and ten players, the three
/// fill modes and two seeds, the pooled seed is the seed of a `fill` job in its own process,
/// which is the seed the job's code gives in this process. The worker's cache holds one of
/// the two worlds, so every other job drops a world and loads it again, and a worker is
/// replaced every five jobs, so the jobs of one request run on both sides of a recycle.
#[tokio::test]
async fn a_pooled_seed_is_byte_for_byte_the_seed_of_a_job_in_its_own_process() {
    let worlds = [prepared(&small(), 1), prepared(&Shape { checks: 200, ..Shape::heavy() }, 2)];
    let held = |w: &(BlobKey, Vec<u8>, _)| Prepared::open(&w.1).unwrap().held_bytes();
    let one_world = held(&worlds[0]).max(held(&worlds[1]));
    let limits = Limits { fill_worker_cache_bytes: one_world, fill_worker_jobs: 5, ..ON };
    let pool = pool(Path::new(BIN), limits);
    let modes = [
        Algorithm::Exact,
        Algorithm::Level(Dials::default()),
        Algorithm::Shuffle(Dials::default()),
    ];
    let ten: Vec<String> = (0..10).map(|p| format!("P{p}")).collect();
    let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
    let (mut pids, mut filled) = (Vec::new(), 0);
    for rng in 1..=2 {
        for mode in modes {
            for names in [&["Ann"][..], &ten] {
                for (key, file, instance) in &worlds {
                    let head = head(names, instance, rng, mode);
                    let own = own_process(&head, file).await;
                    assert_eq!(pool.fill(job(&head, key, file)).await, own);
                    if let Ok(seed) = own {
                        assert_eq!(alone(&head, file).unwrap(), seed);
                        filled += 1;
                    }
                    pids.extend(pool.idle_pids());
                }
            }
        }
    }
    assert!(filled >= 20, "only {filled} of 24 jobs filled: too few to show identity");
    pids.dedup();
    assert!(pids.len() >= 4, "24 jobs at five a worker: {pids:?}");
}

/// A refusal reads the same with and without the pool: a request that is no request, and a
/// world that does not load (which the server answers by preparing it again).
#[tokio::test]
async fn a_refusal_is_worded_as_a_job_in_its_own_process_words_it() {
    let pool = pool(Path::new(BIN), ON);
    let (key, file, instance) = prepared(&small(), 1);
    let good = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let junk = b"not a prepared world".to_vec();
    let junk_key = log::blob_key(&junk);
    for (head, key, file) in [(&b"{"[..], &key, &file), (&good[..], &junk_key, &junk)] {
        let pooled = pool.fill(job(head, key, file)).await;
        let Err(Failure::Refused(why)) = &pooled else { panic!("{pooled:?}") };
        assert!(why.ends_with("(exit status: 1)"), "{why}");
        assert_eq!(pooled, own_process(head, file).await);
    }
    let unloadable = pool.fill(job(&good, &junk_key, &junk)).await.unwrap_err();
    assert!(unloadable.to_string().starts_with(job::UNLOADABLE), "{unloadable}");
}

/// A job never fails for what its worker went through before it: a worker that has taken a
/// job and is killed in the middle of the next one costs that job nothing but time. It runs
/// again on a new worker and gives the right bytes, as does the job waiting behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_whose_reused_worker_is_killed_runs_again_on_a_new_one() {
    let bin = own_binary("killed-reused");
    let pool = Arc::new(pool(&bin, ON));
    let (key, file, instance) = prepared(&small(), 1);
    let quick = head(&["Ann"], &instance, 1, Algorithm::Exact);
    assert!(pool.fill(job(&quick, &key, &file)).await.is_ok());
    let first = pool.idle_pids();
    assert_eq!(first.len(), 1);

    let (long_key, long_file, long) = long_job();
    let want = alone(&long, &long_file).unwrap();
    let caught = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move { pool.fill(job(&long, &long_key, &long_file)).await })
    };
    until("the long job takes the worker", || pool.idle_pids().is_empty()).await;
    // Behind it in line, for the one worker.
    let waiting = {
        let (pool, quick, file) = (Arc::clone(&pool), quick.clone(), file.clone());
        tokio::spawn(async move { pool.fill(job(&quick, &key, &file)).await })
    };
    kill(first[0]);
    assert_eq!(caught.await.unwrap(), Ok(want));
    assert_eq!(waiting.await.unwrap(), Ok(alone(&quick, &file).unwrap()));
    let second = pool.idle_pids();
    assert!(second.len() == 1 && second != first, "a new worker: {first:?} then {second:?}");
    assert_eq!(running(&bin), second, "and the killed one is gone");
}

/// A job that fails on a worker that is new for it fails for real, alone, and says so: a
/// worker killed in the middle of its first job. The next job gets a new worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_whose_new_worker_is_killed_fails_alone_and_the_pool_recovers() {
    let bin = own_binary("killed-new");
    let pool = Arc::new(pool(&bin, ON));
    let (long_key, long_file, long) = long_job();
    let doomed = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move { pool.fill(job(&long, &long_key, &long_file)).await })
    };
    until("the worker starts", || !running(&bin).is_empty()).await;
    kill(running(&bin)[0]);
    let failed = doomed.await.unwrap().unwrap_err();
    let Failure::Broken(why) = &failed else { panic!("{failed:?}") };
    assert!(why.contains("the fill worker stopped") && why.contains("signal: 9"), "{why}");
    assert!(running(&bin).is_empty(), "nothing ran it a second time");

    let (key, file, instance) = prepared(&small(), 1);
    let quick = head(&["Ann"], &instance, 1, Algorithm::Exact);
    assert_eq!(pool.fill(job(&quick, &key, &file)).await, Ok(alone(&quick, &file).unwrap()));
}

/// A worker killed between two jobs fails neither, whether the pool has seen it stop or not:
/// the next job is asked at once, with no wait for the dead worker to be noticed.
#[tokio::test]
async fn a_worker_killed_between_jobs_fails_no_job() {
    let bin = own_binary("killed-idle");
    let pool = pool(&bin, ON);
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let want = alone(&head, &file).unwrap();
    for _ in 0..5 {
        assert_eq!(pool.fill(job(&head, &key, &file)).await.as_ref(), Ok(&want));
        let idle = pool.idle_pids();
        kill(idle[0]);
        assert_eq!(pool.fill(job(&head, &key, &file)).await.as_ref(), Ok(&want));
        assert!(pool.idle_pids() != idle);
    }
}

/// What earlier jobs leave in a worker never fails a later one: under an address-space cap a
/// few megabytes over what one job needs, with nothing replacing the worker early, a worker
/// that keeps world after world runs out of memory in the middle of some job. Every job is
/// filled all the same, because that one ran again on a new worker; alone it would never
/// have failed.
#[tokio::test]
async fn a_job_whose_worker_ran_out_of_memory_after_others_is_filled_on_a_new_one() {
    let bin = own_binary("memory");
    let never_early =
        Limits { fill_worker_recycle_percent: 100, fill_worker_jobs: usize::MAX, ..ON };
    // Another game name is another prepared file: a world the worker has not kept.
    let world = |n: usize| {
        let (mut manifest, instance) = data_fill::synth::world(&Shape::default(), 1, 0);
        manifest["game"] = format!("Synth {n}").into();
        manifest["version"] = "1".into();
        let out = job::prepare(manifest.to_string().as_bytes()).unwrap();
        let file = job::unframe(&out).unwrap().1.to_vec();
        (log::blob_key(&file), file, instance)
    };
    // What one such job needs of address space, at its peak, in a worker of its own.
    let probe = pool(&bin, never_early);
    let (key, file, instance) = world(0);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let want = probe.fill(job(&head, &key, &file)).await.unwrap();
    let status = std::fs::read_to_string(format!("/proc/{}/status", probe.idle_pids()[0])).unwrap();
    let peak_kb = status.lines().find_map(|l| l.strip_prefix("VmPeak:")).unwrap();
    let peak = peak_kb.trim().trim_end_matches("kB").trim().parse::<u64>().unwrap() * 1024;
    drop(probe);

    let tight = Limits { fill_memory_bytes: peak + (3 << 20), ..never_early };
    let pool = pool(&bin, tight);
    assert_eq!(pool.fill(job(&head, &key, &file)).await.as_ref(), Ok(&want), "alone it fits");
    let first = pool.idle_pids();
    let mut replaced = false;
    for n in 1..200 {
        let (key, file, _) = world(n);
        let filled = pool.fill(job(&head, &key, &file)).await;
        assert!(filled.is_ok(), "job {n} failed for what jobs before it left: {filled:?}");
        if pool.idle_pids() != first {
            replaced = true;
            break;
        }
    }
    assert!(replaced, "the worker never ran out: the test shows nothing");
}

/// A job that does not fit the address-space cap even in a worker of its own fails, as a job
/// child's would, and leaves no process.
#[tokio::test]
async fn a_job_over_the_address_space_cap_fails_and_leaves_no_process() {
    let bin = own_binary("too-small");
    let pool = pool(&bin, Limits { fill_memory_bytes: 4 << 20, ..ON });
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let failed = pool.fill(job(&head, &key, &file)).await.unwrap_err();
    let Failure::Broken(why) = &failed else { panic!("{failed:?}") };
    assert!(why.contains("the fill worker stopped"), "{why}");
    assert!(pool.idle_pids().is_empty() && running(&bin).is_empty(), "{:?}", running(&bin));
}

/// A job past its time is killed with its worker, reported as a one-job child's overrun is,
/// and leaves no process behind.
#[tokio::test]
async fn a_job_past_its_time_ends_its_worker_and_leaves_no_process() {
    let bin = own_binary("overrun");
    // Too short for any process to start and answer.
    let pool = pool(&bin, Limits { fill_timeout_ms: 1, ..ON });
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let failed = pool.fill(job(&head, &key, &file)).await.unwrap_err();
    assert_eq!(failed, Failure::Broken("timed out after 0 s and was killed".into()));
    assert!(pool.idle_pids().is_empty() && running(&bin).is_empty(), "{:?}", running(&bin));
}

/// A worker is replaced by a fresh process after `fill_worker_jobs` jobs and after any job it
/// refuses, and no worker is left over.
#[tokio::test]
async fn a_worker_is_replaced_after_its_jobs_and_after_a_refusal() {
    let bin = own_binary("recycled");
    let pool = pool(&bin, Limits { fill_worker_jobs: 2, ..ON });
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let want = alone(&head, &file).unwrap();
    let fill = || pool.fill(job(&head, &key, &file));

    assert_eq!(fill().await.as_ref(), Ok(&want));
    let first = pool.idle_pids();
    assert_eq!(first.len(), 1);
    assert_eq!(fill().await.as_ref(), Ok(&want));
    assert!(pool.idle_pids().is_empty(), "two jobs: it is not kept for a third");
    assert_eq!(fill().await.as_ref(), Ok(&want));
    let second = pool.idle_pids();
    assert!(second.len() == 1 && second != first);

    // A refusal is the job's own answer, and ends the worker that gave it.
    assert!(matches!(pool.fill(job(b"{", &key, &file)).await, Err(Failure::Refused(_))));
    assert!(pool.idle_pids().is_empty() && running(&bin).is_empty(), "{:?}", running(&bin));
    assert_eq!(fill().await.as_ref(), Ok(&want));
}

/// A worker is replaced when its address space after a job is over the configured share of
/// the cap, and when its CPU cap could not cover what its jobs may have used plus one more
/// whole job: at once where a single job leaves no room, and after some jobs where they add
/// up to it.
#[tokio::test]
async fn a_worker_is_replaced_when_its_size_or_its_cpu_time_leave_the_next_job_too_little() {
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let want = alone(&head, &file).unwrap();
    // One percent of the cap is less than any process's address space; one CPU second past a
    // job's time leaves no margin.
    let at_once = [
        ("size", Limits { fill_worker_recycle_percent: 1, ..ON }),
        ("cpu", Limits { fill_cpu_secs: 61, ..ON }),
    ];
    for (which, limits) in at_once {
        let bin = own_binary(&format!("fit-{which}"));
        let pool = pool(&bin, limits);
        assert_eq!(pool.fill(job(&head, &key, &file)).await.as_ref(), Ok(&want));
        assert!(pool.idle_pids().is_empty() && running(&bin).is_empty(), "{which}");
    }
    // Room for one second of jobs: kept after the first, replaced once they add up.
    let bin = own_binary("fit-adds-up");
    let adds_up =
        Limits { fill_timeout_ms: 20_000, fill_cpu_secs: 22, fill_worker_jobs: usize::MAX, ..ON };
    let pool = pool(&bin, adds_up);
    assert_eq!(pool.fill(job(&head, &key, &file)).await.as_ref(), Ok(&want));
    let first = pool.idle_pids();
    assert_eq!(first.len(), 1, "one short job leaves time for more");
    let mut jobs = 1;
    while pool.idle_pids() == first {
        assert!(jobs < 5000, "never replaced: its jobs' time is not counted");
        assert_eq!(pool.fill(job(&head, &key, &file)).await.as_ref(), Ok(&want));
        jobs += 1;
    }
}

/// Backpressure: jobs asked at once take a worker each up to the pool's size and wait in
/// line past it, and every one is filled.
#[tokio::test]
async fn jobs_past_the_pools_size_wait_for_a_free_worker() {
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let want = alone(&head, &file).unwrap();
    for workers in [1, 2] {
        let bin = own_binary(&format!("line-{workers}"));
        let pool = pool(&bin, Limits { fill_pool_workers: workers, ..ON });
        let fill = || pool.fill(job(&head, &key, &file));
        let (a, b, c) = tokio::join!(fill(), fill(), fill());
        assert_eq!(
            (a.unwrap(), b.unwrap(), c.unwrap()),
            (want.clone(), want.clone(), want.clone())
        );
        assert_eq!(pool.idle_pids().len(), workers, "three jobs at once on {workers}");
        assert_eq!(running(&bin).len(), workers);
    }
}

/// A request that goes away in the middle of its fill (its future is dropped) takes its
/// worker with it: no process is left, and no worker with half an answer in its pipe waits
/// for the next job. A pool that is dropped takes its idle workers with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_job_and_a_dropped_pool_leave_no_process() {
    let bin = own_binary("dropped");
    let pool = Arc::new(pool(&bin, ON));
    let (long_key, long_file, long) = long_job();
    let gone = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move { pool.fill(job(&long, &long_key, &long_file)).await })
    };
    until("the worker starts", || !running(&bin).is_empty()).await;
    gone.abort();
    assert!(gone.await.unwrap_err().is_cancelled());
    until("the dropped job's worker is gone", || running(&bin).is_empty()).await;
    assert!(pool.idle_pids().is_empty());

    let (key, file, instance) = prepared(&small(), 1);
    let quick = head(&["Ann"], &instance, 1, Algorithm::Exact);
    assert_eq!(pool.fill(job(&quick, &key, &file)).await, Ok(alone(&quick, &file).unwrap()));
    assert_eq!(running(&bin).len(), 1, "an idle worker");
    drop(pool);
    until("the dropped pool's worker is gone", || running(&bin).is_empty()).await;
}
