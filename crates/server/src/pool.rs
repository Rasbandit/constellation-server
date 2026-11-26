//! Fill workers that stay up: a few child processes ([`Limits::fill_pool_workers`]; none by
//! default, then every fill is a process of its own) that each take one fill after another, so
//! a generation pays neither a process start nor, for a world its worker filled lately, the
//! load of the prepared world (#200). Generation still never runs in the serving process: a
//! worker is this executable in job mode, started through `job_argv` exactly as a one-job
//! child is, so under the same caps (address space, CPU seconds, open files, no file writes,
//! the process brake) and with an empty environment.
//!
//! What a job keeps of the one-process path:
//! - **Wall clock.** A job past `fill_timeout_ms` ends its worker (killed and reaped) and fails
//!   with the same message; the next job gets a fresh worker.
//! - **CPU.** The CPU cap is the worker's for its whole life, so no job can use more than a
//!   one-job child could. A worker is replaced before the cap could end a job that did nothing
//!   wrong (`has_cpu_left`).
//! - **Memory.** The address-space cap is the worker's too, so a job gets it less what the
//!   worker holds: its kept worlds (at most `fill_worker_cache_bytes`, each counted by what it
//!   holds) and what earlier fills left allocated. A worker whose address space after a job is
//!   over `fill_worker_recycle_percent` of the cap is replaced, as is every worker after
//!   `fill_worker_jobs` jobs.
//! - **A job never fails for what earlier jobs left.** When a worker that had already taken a
//!   job stops or breaks the protocol during a later one (out of memory after other jobs'
//!   leftovers, killed between jobs), that job runs once more on a fresh worker, where it has
//!   what a one-job child has. What fails on a fresh worker fails for real. A job that kills
//!   its worker by itself may so run twice, each run under the same caps.
//! - **Any failure ends the worker.** A refusal, a broken pipe, a worker that stopped or
//!   answered out of turn: the worker is killed and the pool starts another.
//! - **Backpressure.** A job waits for a free worker; what may wait is bounded before it gets
//!   here (`gen_concurrency`, `gen_queue_max`), and config refuses a pool with fewer workers
//!   than `gen_concurrency`.
//!
//! What a worker holds between two jobs, and nothing else: its loaded prepared worlds
//! (`serve`'s cache), each under the hash of the file it was loaded from, checked by the
//! worker itself before it keeps one. A world is immutable and depends only on an uploaded
//! manifest and the engine; a fill reads it and never writes to it (the fill engine has a test
//! of that). A job's request (the players, their options, the seed) and its seed are dropped
//! when the job answers.
//!
//! What that still tells one account about another: a fill on a kept world answers sooner
//! than one whose world must be sent and loaded, so whoever can time their own generation can
//! tell whether a world was filled on that worker lately. Today every signed-in account may
//! list and use every manifest, so this shows that some account generated a world all of them
//! can see; it shows nothing of who, with which players or options. The day a manifest can be
//! private to an account, its worlds must be kept per account or not at all (see #200). The
//! server reports no timings of a generation to its requester.
//!
//! The pipe: one message is a kind (1 byte), its body's length (4 bytes, little endian) and
//! the body. The server sends `Kind::Job`, a `data_fill::job::frame` of the fill request and
//! the world's key. The worker answers `Kind::Done` (the seed file), `Kind::Refused` (why,
//! as text) or, once, `Kind::Miss`: it does not hold that world, and the server sends it as
//! `Kind::World`. Each side treats the other's bytes as untrusted: a length over the reader's
//! cap is refused before a byte of the body is read, a kind out of turn ends the worker, and a
//! world that does not hash to its key is never loaded.

use crate::cache::{Cache, Held};
use crate::sandbox::{Failure, Keep, REFUSED_STATUS, drain, job_argv, spawn};
use config::Limits;
use data_fill::job;
use data_fill::prepared::Prepared;
use log::BlobKey;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Semaphore;

/// How the job name of a pool worker starts (`fill-job worker=<caps>`, see [`worker_job`]).
const WORKER: &str = "worker=";

/// What a worker is started with besides the caps of every job child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    /// Most bytes its kept worlds may hold (`fill_worker_cache_bytes`).
    cache_bytes: usize,
    /// The longest message it reads: more than its address-space cap could never be held.
    message_max: usize,
}

/// The job name that makes a job child a pool worker with these limits: its command line is
/// the only way in, since it gets no environment.
fn worker_job(l: &Limits) -> String {
    let message_max = usize::try_from(l.fill_memory_bytes).unwrap_or(usize::MAX);
    format!("{WORKER}{},{message_max}", l.fill_worker_cache_bytes)
}

/// The caps in a job name [`worker_job`] made; `None` when `job` is no worker's.
pub(crate) fn caps_of(job: &str) -> Option<Result<Caps, String>> {
    let caps = job.strip_prefix(WORKER)?;
    let parsed = caps.split_once(',').and_then(|(cache, message)| {
        Some(Caps { cache_bytes: cache.parse().ok()?, message_max: message.parse().ok()? })
    });
    Some(parsed.ok_or_else(|| format!("bad worker caps {caps:?}")))
}

/// The command line of a worker: a job child's ([`job_argv`]: the same program and caps).
fn worker_argv(exe: &std::path::Path, l: &Limits) -> Vec<OsString> {
    job_argv(exe, &worker_job(l), l)
}

/// What a message on a worker's pipe is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Server to worker: a fill request and the key of its world.
    Job = 1,
    /// Server to worker, after a [`Kind::Miss`]: the prepared world's file.
    World = 2,
    /// Worker to server: the seed file.
    Done = 3,
    /// Worker to server: why the job was refused, as text.
    Refused = 4,
    /// Worker to server: it does not hold the job's world.
    Miss = 5,
}

/// A message's first bytes: its kind and its body's length.
fn header(kind: Kind, len: usize) -> Result<[u8; 5], String> {
    let len = u32::try_from(len).map_err(|_| "a pool message is too long")?.to_le_bytes();
    Ok([kind as u8, len[0], len[1], len[2], len[3]])
}

/// The kind and body length a header announces.
fn announced(header: [u8; 5]) -> Result<(Kind, usize), String> {
    let [kind, len @ ..] = header;
    let kind = [Kind::Job, Kind::World, Kind::Done, Kind::Refused, Kind::Miss]
        .into_iter()
        .find(|k| *k as u8 == kind)
        .ok_or_else(|| format!("unknown pool message kind {kind}"))?;
    Ok((kind, u32::from_le_bytes(len) as usize))
}

/// A loaded world, counted against the cache's cap by what it holds in memory.
#[derive(Debug)]
struct Loaded(Prepared);

impl Held for Loaded {
    fn held_bytes(&self) -> usize {
        self.0.held_bytes()
    }
}

/// The worker's side: take fills from `from` and answer on `to` until `from` closes, keeping
/// loaded worlds between them within `caps`.
///
/// # Errors
/// A pipe that fails, or a message that breaks the protocol: the worker stops, and the
/// process's error output says why.
pub(crate) fn serve(from: impl Read, to: impl Write, caps: Caps) -> Result<(), String> {
    serve_keeping(from, to, &mut Cache::new(caps.cache_bytes), caps.message_max)
}

/// [`serve`] on the caller's cache: everything a worker holds between two jobs is in `worlds`.
fn serve_keeping(
    mut from: impl Read,
    mut to: impl Write,
    worlds: &mut Cache<Loaded>,
    message_max: usize,
) -> Result<(), String> {
    while let Some((kind, body)) = read(&mut from, message_max)? {
        if kind != Kind::Job {
            return Err(format!("a fill worker takes a job, not {kind:?}"));
        }
        let (head, key) = job::unframe(&body)?;
        let key: BlobKey = key.try_into().map_err(|_| "a job names its world by a 32-byte key")?;
        // The request first, as a job child reads it: a bad one is refused before the world
        // is asked for.
        let seed = match job::request(head) {
            Ok(request) => match world(worlds, key, (&mut from, message_max), &mut to)? {
                Ok(loaded) => job::fill_on(&loaded.0, &request).map(|(seed, _)| seed),
                Err(unloadable) => Err(unloadable),
            },
            Err(bad) => Err(bad),
        };
        match seed {
            Ok(seed) => write(&mut to, Kind::Done, &seed)?,
            Err(why) => write(&mut to, Kind::Refused, why.as_bytes())?,
        }
    }
    Ok(())
}

/// The loaded world under `key`: the kept one, or the file asked of the server, loaded and
/// kept. The inner error is a file that does not load (the job's refusal, as
/// `data_fill::job::fill` words it); the outer one ends the worker.
fn world(
    worlds: &mut Cache<Loaded>,
    key: BlobKey,
    (from, message_max): (&mut impl Read, usize),
    to: &mut impl Write,
) -> Result<Result<Arc<Loaded>, String>, String> {
    if let Some(kept) = worlds.get(&key) {
        return Ok(Ok(kept));
    }
    write(to, Kind::Miss, &[])?;
    let Some((Kind::World, file)) = read(from, message_max)? else {
        return Err("a fill worker asked for a world and got none".to_owned());
    };
    // Never kept under a key it does not hash to: a later job naming that key would fill on it.
    if log::blob_key(&file) != key {
        return Err("a world's bytes do not hash to its key".to_owned());
    }
    Ok(match Prepared::open(&file) {
        Ok(world) => {
            let loaded = Arc::new(Loaded(world));
            worlds.put(key, Arc::clone(&loaded));
            Ok(loaded)
        }
        Err(e) => Err(format!("{}{e}", job::UNLOADABLE)),
    })
}

/// The next message, its body at most `max` bytes, or `None` when the pipe closed between
/// two messages.
fn read(from: &mut impl Read, max: usize) -> Result<Option<(Kind, Vec<u8>)>, String> {
    let io = |e: std::io::Error| format!("pool pipe: {e}");
    let mut head = [0_u8; 5];
    let (first, rest) = head.split_at_mut(1);
    match from.read_exact(first) {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other.map_err(io)?,
    }
    from.read_exact(rest).map_err(io)?;
    let (kind, len) = announced(head)?;
    if len > max {
        return Err(format!("a pool message announces more than {max} bytes"));
    }
    // Read as it comes, so a length that lies allocates nothing up front.
    let mut body = Vec::new();
    from.take(len as u64).read_to_end(&mut body).map_err(io)?;
    if body.len() != len {
        return Err("a pool message is cut short".to_owned());
    }
    Ok(Some((kind, body)))
}

fn write(to: &mut impl Write, kind: Kind, body: &[u8]) -> Result<(), String> {
    let io = |e: std::io::Error| format!("pool pipe: {e}");
    to.write_all(&header(kind, body.len())?).map_err(io)?;
    to.write_all(body).map_err(io)?;
    to.flush().map_err(io)
}

async fn send<W: AsyncWrite + Unpin>(to: &mut W, kind: Kind, body: &[u8]) -> Result<(), String> {
    let io = |e: std::io::Error| format!("its pipe: {e}");
    to.write_all(&header(kind, body.len())?).await.map_err(io)?;
    to.write_all(body).await.map_err(io)?;
    to.flush().await.map_err(io)
}

/// The worker's next message, its body at most `max` bytes.
async fn recv<R: AsyncRead + Unpin>(from: &mut R, max: usize) -> Result<(Kind, Vec<u8>), String> {
    let io = |e: std::io::Error| format!("its pipe: {e}");
    let mut head = [0_u8; 5];
    from.read_exact(&mut head).await.map_err(io)?;
    let (kind, len) = announced(head)?;
    if len > max {
        return Err(format!("it announced more than {max} bytes"));
    }
    let mut body = Vec::new();
    from.take(len as u64).read_to_end(&mut body).await.map_err(io)?;
    if body.len() != len {
        return Err("its answer is cut short".to_owned());
    }
    Ok((kind, body))
}

/// One fill on a worker's pipes: the request `head` on the world `file`, named by `key`. The
/// inner result is the job's own answer (the seed, or why it was refused); the outer error is
/// a pipe or an answer that broke the protocol, after which the worker cannot be used.
async fn ask<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    (to, from): (&mut W, &mut R),
    head: &[u8],
    (key, file): (&BlobKey, &[u8]),
    l: &Limits,
) -> Result<Result<Vec<u8>, String>, String> {
    send(to, Kind::Job, &job::frame(head, key)?).await?;
    let mut sent = false;
    loop {
        match recv(from, l.gen_stdout_max_bytes).await? {
            (Kind::Done, seed) => return Ok(Ok(seed)),
            (Kind::Refused, why) => {
                let tail = why.len().saturating_sub(l.gen_error_max_bytes);
                let why = String::from_utf8_lossy(why.get(tail..).unwrap_or_default());
                return Ok(Err(why.into_owned()));
            }
            (Kind::Miss, _) if !sent => {
                send(to, Kind::World, file).await?;
                sent = true;
            }
            (kind, _) => return Err(format!("it answered {kind:?} out of turn")),
        }
    }
}

/// Whether a worker whose jobs may have used `cpu_ms` of CPU time can run one more whole job
/// on `threads` threads inside the CPU cap of its process (`fill_cpu_secs`, counted over its
/// whole life), with `config::FILL_WORKER_CPU_MARGIN_MS` in hand for its start and what it
/// does between jobs. A worker without that much left is replaced, so the cap only ever ends
/// a job that overran by itself.
fn has_cpu_left(cpu_ms: u128, threads: usize, l: &Limits) -> bool {
    let job = u128::from(l.fill_timeout_ms) * threads.max(1) as u128;
    let worst = cpu_ms + job + u128::from(config::FILL_WORKER_CPU_MARGIN_MS);
    worst < u128::from(l.fill_cpu_secs) * 1000
}

/// Bytes of `field` (`VmSize:`, `VmRSS:`, `VmPeak:`) in process `pid`'s `/proc` status;
/// `None` when the process is gone.
pub(crate) fn status_bytes(pid: u32, field: &str) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb = status.lines().find_map(|line| line.strip_prefix(field))?;
    kb.trim().trim_end_matches("kB").trim().parse::<u64>().ok().map(|kb| kb * 1024)
}

/// One fill for the pool: the request `head` (`data_fill::job::Fill` as JSON) on the prepared
/// world `file`, which hashes to `key`.
#[derive(Debug, Clone, Copy)]
pub struct Job<'a> {
    /// The fill request.
    pub head: &'a [u8],
    /// The threads that request asks its fill for: what bounds the job's CPU time.
    pub threads: usize,
    /// The hash of `file`: the name a worker keeps the loaded world under.
    pub key: &'a BlobKey,
    /// The prepared world's file, sent to a worker that does not hold it.
    pub file: &'a [u8],
}

/// How a job on a worker ended without a seed.
#[derive(Debug)]
enum Ended {
    /// The job's own answer, or its overrun: another worker would end the same.
    Job(Failure),
    /// The worker stopped or broke the protocol.
    Worker(String),
}

/// One worker process and its pipes.
#[derive(Debug)]
struct Worker {
    child: Child,
    to: ChildStdin,
    from: ChildStdout,
    /// The end of its error output, read as it comes (so it never blocks on a full pipe).
    errors: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, u64)>>,
    jobs: usize,
    /// Most CPU time its jobs can have used, ms: each job's wall time times its threads (an
    /// idle worker uses none).
    cpu_ms: u128,
}

impl Worker {
    fn start(exe: &std::path::Path, l: &Limits) -> Result<Self, Failure> {
        let mut child = spawn(&worker_argv(exe, l), true)?;
        let (Some(to), Some(from), Some(err)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Failure::Broken("no pipes to the fill worker".into()));
        };
        let errors = tokio::spawn(drain(err, l.gen_error_max_bytes, Keep::Tail));
        Ok(Self { child, to, from, errors, jobs: 0, cpu_ms: 0 })
    }

    /// Whether it is still running and may take one more job of `threads` threads.
    fn fit(&mut self, threads: usize, l: &Limits) -> bool {
        matches!(self.child.try_wait(), Ok(None))
            && self.jobs < l.fill_worker_jobs
            && has_cpu_left(self.cpu_ms, threads, l)
    }

    /// Whether its address space leaves the next job enough of the cap
    /// (`fill_worker_recycle_percent`). A worker that cannot be read is not kept.
    fn roomy(&self, l: &Limits) -> bool {
        let most = u128::from(l.fill_memory_bytes) * u128::from(l.fill_worker_recycle_percent);
        let size = self.child.id().and_then(|pid| status_bytes(pid, "VmSize:"));
        size.is_some_and(|size| u128::from(size) * 100 <= most)
    }

    /// Kill it and wait until it is gone.
    async fn stop(mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(e) = self.child.kill().await {
            tracing::warn!(error = %e, "could not kill a fill worker");
        }
    }

    /// End a worker whose pipes broke and say how it exited. The end of its error output goes
    /// to the server's log only: it is the worker's, not this job's, so no client is shown it.
    async fn ended(mut self, wait: Duration) -> String {
        // It may still run (an answer out of turn); one that stopped by itself keeps its status.
        if let Err(e) = self.child.start_kill() {
            tracing::debug!(error = %e, "the fill worker was already gone");
        }
        let status = self.child.wait().await.map_or_else(|e| e.to_string(), |s| s.to_string());
        if let Ok(Ok(Ok((tail, _)))) = tokio::time::timeout(wait, self.errors).await {
            let said = String::from_utf8_lossy(&tail);
            tracing::warn!(status, said = said.trim(), "a fill worker stopped");
        }
        status
    }
}

/// The fill workers of one server process.
#[derive(Debug)]
pub struct Pool {
    exe: PathBuf,
    limits: Limits,
    /// One permit per worker: a job holds one while it runs, the others wait in line.
    free: Semaphore,
    /// Workers between jobs, the last used on top (its worlds are the likeliest to be asked).
    idle: Mutex<Vec<Worker>>,
}

impl Pool {
    /// A pool of at most `limits.fill_pool_workers` workers, each `exe` in job mode; `None`
    /// when that is zero (no pool: the one place that reads it so). No process is started
    /// until a job needs one.
    #[must_use]
    pub fn new(exe: PathBuf, limits: Limits) -> Option<Self> {
        let free = Semaphore::new(limits.fill_pool_workers);
        (limits.fill_pool_workers > 0).then(|| Self { exe, limits, free, idle: Mutex::default() })
    }

    /// Fill `job` on a worker: an idle one, or a new one; when all are busy, once one is free.
    /// A job whose worker had taken jobs before and stops or breaks during this one runs once
    /// more on a new worker. Returns the seed file.
    ///
    /// # Errors
    /// [`Failure::Refused`] with the job's own reason, worded as a job child's refusal is;
    /// [`Failure::Broken`] when the job ran past `fill_timeout_ms` (its worker is killed), or
    /// a worker new for this job stopped or answered against the protocol. No worker that
    /// failed a job is used again.
    pub async fn fill(&self, job: Job<'_>) -> Result<Vec<u8>, Failure> {
        let closed = |_| Failure::Broken("the fill pool is closed".into());
        let _turn = self.free.acquire().await.map_err(closed)?;
        let worker = self.take(job.threads).await?;
        let reused = worker.jobs > 0;
        let ended = match self.on(worker, job).await {
            Err(Ended::Worker(why)) if reused => {
                // Not this job's doing, as far as anyone can tell: it gets a worker of its own.
                tracing::warn!(why, "a reused fill worker broke; its job runs again on a new one");
                self.on(Worker::start(&self.exe, &self.limits)?, job).await
            }
            ended => ended,
        };
        ended.map_err(|ended| match ended {
            Ended::Job(failure) => failure,
            Ended::Worker(why) => Failure::Broken(format!("the fill worker stopped ({why})")),
        })
    }

    /// Run `job` on `worker`, and keep the worker for another job or end it.
    async fn on(&self, mut worker: Worker, job: Job<'_>) -> Result<Vec<u8>, Ended> {
        let l = &self.limits;
        let timeout = Duration::from_millis(l.fill_timeout_ms);
        let started = Instant::now();
        let pipes = (&mut worker.to, &mut worker.from);
        let asked = ask(pipes, job.head, (job.key, job.file), l);
        let asked = tokio::time::timeout(timeout, asked).await;
        worker.jobs += 1;
        worker.cpu_ms += started.elapsed().as_millis() * job.threads.max(1) as u128;
        match asked {
            Ok(Ok(Ok(seed))) => {
                if worker.fit(job.threads, l) && worker.roomy(l) {
                    self.idle().push(worker);
                } else {
                    worker.stop().await;
                }
                Ok(seed)
            }
            Ok(Ok(Err(why))) => {
                worker.stop().await;
                Err(Ended::Job(Failure::Refused(format!("{} ({REFUSED_STATUS})", why.trim()))))
            }
            Ok(Err(why)) => {
                let status = worker.ended(timeout).await;
                Err(Ended::Worker(format!("{why}; {status}")))
            }
            Err(_) => {
                worker.stop().await;
                Err(Ended::Job(Failure::Broken(format!(
                    "timed out after {} s and was killed",
                    timeout.as_secs()
                ))))
            }
        }
    }

    /// The process ids of the workers between jobs (for the tests and the bench).
    #[cfg(feature = "pool-tests")]
    #[must_use]
    pub fn idle_pids(&self) -> Vec<u32> {
        self.idle().iter().filter_map(|w| w.child.id()).collect()
    }

    fn idle(&self) -> std::sync::MutexGuard<'_, Vec<Worker>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An idle worker fit for one more job of `threads` threads, else a new one. One that
    /// stopped while idle, or has no CPU time left for a whole job, is ended here.
    async fn take(&self, threads: usize) -> Result<Worker, Failure> {
        loop {
            let next = self.idle().pop();
            let Some(mut worker) = next else { return Worker::start(&self.exe, &self.limits) };
            if worker.fit(threads, &self.limits) {
                return Ok(worker);
            }
            worker.stop().await;
        }
    }
}

#[cfg(test)]
mod tests;
