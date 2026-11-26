//! Fill workers that stay up: a few child processes ([`Limits::fill_pool_workers`]) that each
//! take one fill after another, so a generation pays neither a process start nor, for a world
//! its worker filled lately, the load of the prepared world (#200). Generation still never
//! runs in the serving process: a worker is this executable in job mode, started through
//! `job_argv` exactly as a one-job child is, so under the same caps (address space, CPU
//! seconds, open files, no file writes, the process brake) and with an empty environment.
//!
//! What a job keeps of the one-process path:
//! - **Wall clock.** A job past `fill_timeout_ms` ends its worker (killed and reaped) and fails
//!   with the same message; the next job gets a fresh worker.
//! - **CPU.** The CPU cap is the worker's for its whole life, so no job can use more than a
//!   one-job child could. A worker is replaced before the cap could end a job that did nothing
//!   wrong (`has_cpu_left`).
//! - **Memory.** The address-space cap is the worker's too: a job gets it less what the worker
//!   holds (its kept worlds, about `config::PREPARED_CACHE_BYTES` at most, and what
//!   earlier fills left fragmented, bounded by replacing the worker every `fill_worker_jobs`).
//!   A job past it ends the worker and fails alone.
//! - **Any failure ends the worker.** A refusal, a broken pipe, a worker that stopped or
//!   answered out of turn: the worker is killed, only its job fails, the pool starts another.
//! - **Backpressure.** A job waits for a free worker; what may wait is bounded before it gets
//!   here (`gen_concurrency`, `gen_queue_max`).
//!
//! What a worker holds between two jobs, and nothing else: its loaded prepared worlds
//! (`serve`'s cache), each under the hash of the file it was loaded from, checked by the
//! worker itself before it keeps one. A world is immutable and depends only on an uploaded
//! manifest and the engine; a fill reads it and never writes to it. A job's request (the
//! players, their options, the seed) and its seed are dropped when the job answers. So a job
//! can learn from an earlier one at most that a world was filled lately (by how fast its own
//! fill answers), which the manifest listing shows anyway.
//!
//! The pipe: one message is a kind (1 byte), its body's length (4 bytes, little endian) and
//! the body. The server sends `Kind::Job`, a `data_fill::job::frame` of the fill request and
//! the world's key. The worker answers `Kind::Done` (the seed file), `Kind::Refused` (why,
//! as text) or, once, `Kind::Miss`: it does not hold that world, and the server sends it as
//! `Kind::World`. Each side treats the other's bytes as untrusted: lengths are capped, a kind
//! out of turn ends the worker, and a world that does not hash to its key is never loaded.

use crate::cache::{Cache, Held};
use crate::sandbox::{Failure, Keep, drain, job_argv, spawn};
use config::Limits;
use data_fill::job;
use data_fill::prepared::Prepared;
use log::BlobKey;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Semaphore;

/// The job name that makes a job child a pool worker (`fill-job worker`).
pub const WORKER: &str = "worker";

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

/// A loaded world, counted against the cache's cap by the file it was loaded from: an
/// estimate of what it holds in memory (`config::PREPARED_HELD_PER_FILE_BYTE`).
#[derive(Debug)]
struct Loaded {
    world: Prepared,
    file_bytes: usize,
}

impl Held for Loaded {
    fn held_bytes(&self) -> usize {
        self.file_bytes.saturating_mul(config::PREPARED_HELD_PER_FILE_BYTE)
    }
}

/// The worker's side: take fills from `from` and answer on `to` until `from` closes, keeping
/// at most `cache_bytes` of loaded worlds between them.
///
/// # Errors
/// A pipe that fails, or a message that breaks the protocol: the worker stops, and the
/// process's error output says why.
pub(crate) fn serve(from: impl Read, to: impl Write, cache_bytes: usize) -> Result<(), String> {
    serve_keeping(from, to, &mut Cache::new(cache_bytes))
}

/// [`serve`] on the caller's cache: everything a worker holds between two jobs is in `worlds`.
fn serve_keeping(
    mut from: impl Read,
    mut to: impl Write,
    worlds: &mut Cache<Loaded>,
) -> Result<(), String> {
    while let Some((kind, body)) = read(&mut from)? {
        if kind != Kind::Job {
            return Err(format!("a fill worker takes a job, not {kind:?}"));
        }
        let (head, key) = job::unframe(&body)?;
        let key: BlobKey = key.try_into().map_err(|_| "a job names its world by a 32-byte key")?;
        let seed = match world(worlds, key, &mut from, &mut to)? {
            Ok(loaded) => job::fill_on(&loaded.world, head).map(|(seed, _)| seed),
            Err(unloadable) => Err(unloadable),
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
    from: &mut impl Read,
    to: &mut impl Write,
) -> Result<Result<Arc<Loaded>, String>, String> {
    if let Some(kept) = worlds.get(&key) {
        return Ok(Ok(kept));
    }
    write(to, Kind::Miss, &[])?;
    let Some((Kind::World, file)) = read(from)? else {
        return Err("a fill worker asked for a world and got none".to_owned());
    };
    // Never kept under a key it does not hash to: a later job naming that key would fill on it.
    if log::blob_key(&file) != key {
        return Err("a world's bytes do not hash to its key".to_owned());
    }
    Ok(match Prepared::open(&file) {
        Ok(world) => {
            let loaded = Arc::new(Loaded { world, file_bytes: file.len() });
            worlds.put(key, Arc::clone(&loaded));
            Ok(loaded)
        }
        Err(e) => Err(format!("{}{e}", job::UNLOADABLE)),
    })
}

/// The next message, or `None` when the pipe closed between two messages.
fn read(from: &mut impl Read) -> Result<Option<(Kind, Vec<u8>)>, String> {
    let io = |e: std::io::Error| format!("pool pipe: {e}");
    let mut head = [0_u8; 5];
    let (first, rest) = head.split_at_mut(1);
    match from.read_exact(first) {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other.map_err(io)?,
    }
    from.read_exact(rest).map_err(io)?;
    let (kind, len) = announced(head)?;
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

/// Whether a worker that spent `busy` on jobs can run one more whole job inside the CPU cap of
/// its process (`fill_cpu_secs`, counted over its whole life). A fill uses at most
/// `config::GENERATION_WORKERS` threads (the server writes that into every request), so its
/// CPU time is at most its wall time times that, and an idle worker uses none. A worker
/// without that much left is replaced, so the cap only ever ends a job that overran by itself.
fn has_cpu_left(busy: Duration, l: &Limits) -> bool {
    let threads = config::GENERATION_WORKERS as u128;
    let worst = (busy.as_millis() + u128::from(l.fill_timeout_ms)) * threads;
    worst < u128::from(l.fill_cpu_secs) * 1000
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
    busy: Duration,
}

impl Worker {
    fn start(exe: &std::path::Path, l: &Limits) -> Result<Self, Failure> {
        let mut child = spawn(&job_argv(exe, WORKER, l), true)?;
        let (Some(to), Some(from), Some(err)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Failure::Broken("no pipes to the fill worker".into()));
        };
        let errors = tokio::spawn(drain(err, l.gen_error_max_bytes, Keep::Tail));
        Ok(Self { child, to, from, errors, jobs: 0, busy: Duration::ZERO })
    }

    /// Whether it is still running and may take one more job.
    fn fit(&mut self, l: &Limits) -> bool {
        matches!(self.child.try_wait(), Ok(None))
            && self.jobs < l.fill_worker_jobs
            && has_cpu_left(self.busy, l)
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

    /// End a worker whose pipes broke: how it exited and the end of its error output.
    async fn ended(mut self, wait: Duration) -> String {
        // It may still run (an answer out of turn); one that stopped by itself keeps its status.
        if let Err(e) = self.child.start_kill() {
            tracing::debug!(error = %e, "the fill worker was already gone");
        }
        let status = self.child.wait().await.map_or_else(|e| e.to_string(), |s| s.to_string());
        let said = match tokio::time::timeout(wait, self.errors).await {
            Ok(Ok(Ok((tail, _)))) => String::from_utf8_lossy(&tail).trim().to_owned(),
            _ => String::new(),
        };
        format!("{status}; {said}")
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
    /// A pool of at most `limits.fill_pool_workers` workers (at least one), each `exe` in job
    /// mode. No process is started until a job needs one.
    #[must_use]
    pub fn new(exe: PathBuf, limits: Limits) -> Self {
        let free = Semaphore::new(limits.fill_pool_workers.max(1));
        Self { exe, limits, free, idle: Mutex::default() }
    }

    /// Fill the request `head` (`data_fill::job::Fill` as JSON) on the prepared world `file`,
    /// which hashes to `key`, on a worker: an idle one, or a new one; when all are busy, once
    /// one is free. Returns the seed file.
    ///
    /// # Errors
    /// [`Failure::Refused`] with the job's own reason; [`Failure::Broken`] when the job ran
    /// past `fill_timeout_ms` (its worker is killed), or its worker stopped or answered
    /// against the protocol. Either way only this job fails and its worker is not used again.
    pub async fn fill(&self, head: &[u8], key: &BlobKey, file: &[u8]) -> Result<Vec<u8>, Failure> {
        let closed = |_| Failure::Broken("the fill pool is closed".into());
        let _turn = self.free.acquire().await.map_err(closed)?;
        let mut worker = self.take().await?;
        let timeout = Duration::from_millis(self.limits.fill_timeout_ms);
        let started = Instant::now();
        let pipes = (&mut worker.to, &mut worker.from);
        let asked =
            tokio::time::timeout(timeout, ask(pipes, head, (key, file), &self.limits)).await;
        worker.jobs += 1;
        worker.busy += started.elapsed();
        match asked {
            Ok(Ok(Ok(seed))) => {
                if worker.fit(&self.limits) {
                    self.idle().push(worker);
                } else {
                    worker.stop().await;
                }
                Ok(seed)
            }
            Ok(Ok(Err(why))) => {
                worker.stop().await;
                Err(Failure::Refused(why))
            }
            Ok(Err(why)) => {
                let how = worker.ended(timeout).await;
                Err(Failure::Broken(format!("the fill worker stopped ({why}; {how})")))
            }
            Err(_) => {
                worker.stop().await;
                Err(Failure::Broken(format!(
                    "timed out after {} s and was killed",
                    timeout.as_secs()
                )))
            }
        }
    }

    /// The process ids of the workers between jobs.
    #[must_use]
    pub fn idle_pids(&self) -> Vec<u32> {
        self.idle().iter().filter_map(|w| w.child.id()).collect()
    }

    fn idle(&self) -> std::sync::MutexGuard<'_, Vec<Worker>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An idle worker fit for one more job, else a new one. One that stopped while idle, or
    /// has no CPU time left for a whole job, is ended here and never given a job.
    async fn take(&self) -> Result<Worker, Failure> {
        loop {
            let next = self.idle().pop();
            let Some(mut worker) = next else { return Worker::start(&self.exe, &self.limits) };
            if worker.fit(&self.limits) {
                return Ok(worker);
            }
            worker.stop().await;
        }
    }
}

#[cfg(test)]
mod tests;
