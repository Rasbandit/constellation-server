//! The fill pool through real worker processes (the server binary in job mode, under `prlimit`
//! as the server starts it): seeds byte for byte those of a job in a process of its own, and
//! what a worker that is killed, overruns, is used up or refuses does to its job and the pool.
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the server package"
)]

mod support {
    pub mod fills;
}

use config::Limits;
use data_fill::job;
use data_fill::room::Algorithm;
use data_fill::synth::Shape;
use server::pool::Pool;
use server::{Failure, run_job};
use std::path::{Path, PathBuf};
use std::time::Duration;
use support::fills::{alone, head, prepared, small};

const BIN: &str = env!("CARGO_BIN_EXE_constellation-server");

/// The seed of a `fill` job in a process of its own: the path the pool replaces.
async fn own_process(head: &[u8], file: &[u8]) -> Vec<u8> {
    let l = Limits::DESIGN;
    let input = job::frame(head, file).unwrap();
    let timeout = Duration::from_millis(l.fill_timeout_ms);
    run_job(Path::new(BIN), "fill", &input, &l, timeout).await.unwrap()
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

/// Byte identity through real processes: on two worlds, for one and ten players and two
/// seeds, the pooled seed (world sent, then kept) is the seed of a `fill` job in its own
/// process, which is the seed the job's code gives in this process. One worker took them all.
#[tokio::test]
async fn a_pooled_seed_is_byte_for_byte_the_seed_of_a_job_in_its_own_process() {
    let pool = Pool::new(BIN.into(), Limits::DESIGN);
    let worlds = [prepared(&small(), 1), prepared(&Shape { checks: 200, ..Shape::heavy() }, 2)];
    let ten: Vec<String> = (0..10).map(|p| format!("P{p}")).collect();
    let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
    let mut pids = Vec::new();
    for rng in 1..=2 {
        for names in [&["Ann"][..], &ten] {
            for (key, file, instance) in &worlds {
                let head = head(names, instance, rng, Algorithm::Exact);
                let own = own_process(&head, file).await;
                assert_eq!(pool.fill(&head, key, file).await.unwrap(), own);
                assert_eq!(alone(&head, file).unwrap(), own);
                pids.extend(pool.idle_pids());
            }
        }
    }
    pids.dedup();
    assert_eq!(pids.len(), 1, "one worker stayed up for all eight jobs");
}

/// A worker killed in the middle of a job fails that job alone, with an error that says so;
/// the job waiting behind it (one worker: it waits its turn) is filled by a new worker, with
/// the right bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_killed_mid_job_fails_only_that_job_and_the_pool_recovers() {
    let bin = own_binary("killed");
    let pool = std::sync::Arc::new(Pool::new(bin.clone(), Limits::DESIGN));
    let (key, file, instance) = prepared(&small(), 1);
    let quick = head(&["Ann"], &instance, 1, Algorithm::Exact);
    assert!(pool.fill(&quick, &key, &file).await.is_ok());
    let first = pool.idle_pids();
    assert_eq!(first.len(), 1);

    // A job that runs for seconds: many players on a large heavy world.
    let (big_key, big_file, big) = prepared(&Shape { checks: 2000, ..Shape::heavy() }, 3);
    let many: Vec<String> = (0..40).map(|p| format!("P{p}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let long = head(&many, &big, 1, Algorithm::Exact);
    let doomed = {
        let pool = std::sync::Arc::clone(&pool);
        tokio::spawn(async move { pool.fill(&long, &big_key, &big_file).await })
    };
    while !pool.idle_pids().is_empty() {
        tokio::task::yield_now().await;
    }
    // Behind it in line, for the one worker.
    let waiting = {
        let (pool, quick, file) = (std::sync::Arc::clone(&pool), quick.clone(), file.clone());
        tokio::spawn(async move { pool.fill(&quick, &key, &file).await })
    };
    kill(first[0]);
    let failed = doomed.await.unwrap().unwrap_err();
    let Failure::Broken(why) = &failed else { panic!("{failed:?}") };
    assert!(why.contains("the fill worker stopped") && why.contains("signal: 9"), "{why}");
    assert_eq!(waiting.await.unwrap().unwrap(), alone(&quick, &file).unwrap());
    let second = pool.idle_pids();
    assert!(second.len() == 1 && second != first, "a new worker: {first:?} then {second:?}");
    assert_eq!(running(&bin), second, "and the killed one is gone");
}

/// A job past its time is killed with its worker, reported as a one-job child's overrun is,
/// and leaves no process behind.
#[tokio::test]
async fn a_job_past_its_time_ends_its_worker_and_leaves_no_process() {
    let bin = own_binary("overrun");
    // Too short for any process to start and answer.
    let pool = Pool::new(bin.clone(), Limits { fill_timeout_ms: 1, ..Limits::DESIGN });
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let failed = pool.fill(&head, &key, &file).await.unwrap_err();
    assert_eq!(failed, Failure::Broken("timed out after 0 s and was killed".into()));
    assert!(pool.idle_pids().is_empty() && running(&bin).is_empty(), "{:?}", running(&bin));
}

/// A worker is replaced by a fresh process after `fill_worker_jobs` jobs, and after any job
/// it refuses; one that stopped while idle is replaced without failing a job.
#[tokio::test]
async fn a_worker_is_replaced_after_its_jobs_a_refusal_or_its_own_end() {
    let bin = own_binary("recycled");
    let pool = Pool::new(bin.clone(), Limits { fill_worker_jobs: 2, ..Limits::DESIGN });
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 1, Algorithm::Exact);
    let want = alone(&head, &file).unwrap();
    let fill = || pool.fill(&head, &key, &file);

    assert_eq!(fill().await.unwrap(), want);
    let first = pool.idle_pids();
    assert_eq!(first.len(), 1);
    assert_eq!(fill().await.unwrap(), want);
    assert!(pool.idle_pids().is_empty(), "two jobs: it is not kept for a third");
    assert_eq!(fill().await.unwrap(), want);
    let second = pool.idle_pids();
    assert!(second.len() == 1 && second != first);

    // A refusal is the job's own answer, and ends the worker that gave it.
    let refused = pool.fill(b"{", &key, &file).await.unwrap_err();
    assert_eq!(refused, Failure::Refused(alone(b"{", &file).unwrap_err()));
    assert!(pool.idle_pids().is_empty() && running(&bin).is_empty(), "{:?}", running(&bin));

    // A worker that stopped between jobs: the next job gets a new one and does not fail.
    assert_eq!(fill().await.unwrap(), want);
    let third = pool.idle_pids();
    kill(third[0]);
    while std::fs::read_to_string(format!("/proc/{}/stat", third[0]))
        .is_ok_and(|stat| !stat.contains(") Z "))
    {
        tokio::task::yield_now().await;
    }
    assert_eq!(fill().await.unwrap(), want);
    assert!(pool.idle_pids() != third);
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
        let pool = Pool::new(bin.clone(), Limits { fill_pool_workers: workers, ..Limits::DESIGN });
        let fill = || pool.fill(&head, &key, &file);
        let (a, b, c) = tokio::join!(fill(), fill(), fill());
        assert_eq!(
            (a.unwrap(), b.unwrap(), c.unwrap()),
            (want.clone(), want.clone(), want.clone())
        );
        assert_eq!(pool.idle_pids().len(), workers, "three jobs at once on {workers}");
        assert_eq!(running(&bin).len(), workers);
    }
}
