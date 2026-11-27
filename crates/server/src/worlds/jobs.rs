//! A generation as a job: its place in the queue, its status in the store, its heartbeat and
//! the sweep that fails lost ones, and the run itself.

use super::*;

/// One generation queued or running, counted against `gen_queue_max` until it is dropped.
pub(super) struct JobSlot(Arc<AtomicUsize>);

impl Drop for JobSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Worlds {
    /// Count a generation against `gen_queue_max`, or `None` when that many are already queued
    /// or running.
    pub(super) fn admit(&self) -> Option<JobSlot> {
        let before = self.queued.fetch_add(1, Ordering::AcqRel);
        let slot = JobSlot(Arc::clone(&self.queued));
        (before < self.limits.gen_queue_max).then_some(slot)
    }

    /// Write a status to the store; its `id` and `status` fields say which job and where it is.
    ///
    /// # Errors
    /// A status without them, or the store failing.
    pub(super) async fn save_status(&self, status: &Value) -> Result<(), String> {
        let (id, state, text) = job_row(status)?;
        self.store.job_put(id, state, &text).await.map_err(|e| store_failed(&e))
    }

    /// [`Self::save_status`] for a job's last status, tried again with a growing wait while
    /// the store is down: a finished job left `running` would be failed later by the sweep.
    pub(super) async fn save_final(&self, status: &Value) -> Result<(), String> {
        let mut wait = Duration::from_millis(config::GEN_FINAL_WRITE_WAIT_MS);
        for attempt in 1..config::GEN_FINAL_WRITE_TRIES {
            let Err(e) = self.save_status(status).await else { return Ok(()) };
            tracing::warn!(attempt, error = %e, "final status write failed; trying again");
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
        self.save_status(status).await
    }

    /// Fail every queued or running job not marked alive for `gen_lost_secs`: the server
    /// running it is gone. A job its server marks alive meanwhile is left alone.
    pub(super) async fn fail_stale(&self) {
        let older = Duration::from_secs(self.limits.gen_lost_secs);
        let error = "the server running this generation stopped answering";
        match self.store.job_fail_all_stale(older, error).await {
            Ok(ids) => ids.iter().for_each(|id| {
                tracing::warn!(world = %id, "generation failed: its server stopped");
            }),
            Err(e) => tracing::warn!(error = ?e, "could not fail stopped generations"),
        }
    }
}

/// Whether `status` is of a job that has not ended: queued or running.
pub(super) fn unfinished(status: &Value) -> bool {
    matches!(status["status"].as_str(), Some("queued" | "running"))
}

/// A status's job id, state and text, as the store keeps them.
pub(super) fn job_row(status: &Value) -> Result<(&str, JobState, String), String> {
    let id = status["id"].as_str().filter(|i| valid_id(i)).ok_or("a status with a bad id")?;
    let state = status["status"].as_str().and_then(JobState::parse);
    Ok((id, state.ok_or("a status with a bad state")?, status.to_string()))
}

/// What a job folder holds, for its log line: each file under it with its size, in name order,
/// at most `most` named. Links are named, never followed.
pub(super) fn folder_contents(dir: &Path, most: usize) -> String {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.filter_map(Result::ok) {
            let (path, kind) = (e.path(), e.file_type());
            let name = path.strip_prefix(dir).unwrap_or(&path).display().to_string();
            match kind {
                Ok(k) if k.is_dir() => stack.push(path),
                Ok(k) if k.is_file() => {
                    let bytes = e.metadata().map_or(0, |m| m.len());
                    found.push(format!("{name} ({bytes} bytes)"));
                }
                _ => found.push(format!("{name} (not a file)")),
            }
        }
    }
    found.sort();
    let more = found.len().saturating_sub(most);
    found.truncate(most);
    match (found.is_empty(), more) {
        (true, _) => "nothing".to_owned(),
        (false, 0) => found.join(", "),
        (false, n) => format!("{}, and {n} more", found.join(", ")),
    }
}

/// Fail lost generations every `gen_heartbeat_secs` (the first time at once), until the API is
/// gone.
pub(super) async fn sweep(w: std::sync::Weak<Worlds>) {
    let every = Duration::from_secs(w.upgrade().map_or(1, |w| w.limits.gen_heartbeat_secs).max(1));
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(w) = w.upgrade() else { return };
        w.fail_stale().await;
    }
}

/// A task aborted when this is dropped: a job's heartbeat stops with its job, even when the
/// job panics, so a job that died is never kept alive.
pub(super) struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Mark job `id` alive every `gen_heartbeat_secs`, until aborted when the job ends.
pub(super) async fn heartbeat(w: Arc<Worlds>, id: String) {
    let every = Duration::from_secs(w.limits.gen_heartbeat_secs.max(1));
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = w.store.job_touch(&id).await {
            tracing::warn!(world = %id, error = ?e, "could not mark a generation alive");
        }
    }
}

/// Note who asked for the world, so its room is recorded as theirs once it is listed
/// ([`claim_room`]).
pub(super) fn with_owner(status: &mut Value, owner: Option<&str>) {
    if let Some(o) = owner {
        status["owner"] = json!(o);
    }
}

pub(super) fn ids(metas: &[Value]) -> Vec<Value> {
    metas.iter().map(|m| m["id"].clone()).collect()
}

/// A world id no other world has: a hash of the chosen ids, the time and a counter.
pub(super) fn new_world_id(w: &Worlds, chosen: &[&Vec<Value>]) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = w.seq.fetch_add(1, Ordering::Relaxed);
    content_id(format!("{chosen:?}{nanos}-{}-{n}", std::process::id()).as_bytes())
}

/// Make the job's folder, write the queued status and start the job: the one place every kind
/// of world is queued. The folder (a legacy job's copies of its inputs are in it) is made for
/// every job, so a restart finds it and fails the job at once; it is removed when the job ends.
pub(super) async fn queue(w: &Arc<Worlds>, status: Value, slot: JobSlot) -> Reply {
    let id = status["id"].clone();
    let world = w.dir.join("worlds").join(id.as_str().unwrap_or_default());
    let made = tokio::fs::create_dir_all(&world).await.map_err(|e| e.to_string());
    let saved = match made {
        Ok(()) => w.save_status(&status).await,
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        drop(tokio::fs::remove_dir_all(&world).await);
        return Err(internal(e));
    }
    tracing::info!(world = %id, "generation queued");
    tokio::spawn(generate(Arc::clone(w), world, status, slot));
    Ok(json_reply(StatusCode::ACCEPTED, &json!({"id": id, "status": "queued"})))
}

/// Run one queued generation (one at a time), marked alive while it waits and runs, and
/// record how it ended.
pub(super) async fn generate(w: Arc<Worlds>, world: PathBuf, status: Value, slot: JobSlot) {
    let id = status["id"].as_str().unwrap_or("").to_owned();
    let beat = AbortOnDrop(tokio::spawn(heartbeat(Arc::clone(&w), id.clone())));
    run_generation(&w, &world, status, slot).await;
    drop(beat);
    // Only copies of uploads (the store keeps them) and nothing the generator made, but its
    // log line says what it held, should a job ever need looking into.
    let most = w.limits.job_log_files_max;
    let removed = tokio::task::spawn_blocking(move || {
        let held = folder_contents(&world, most);
        (std::fs::remove_dir_all(&world), held)
    });
    match removed.await {
        Ok((Ok(()), held)) => tracing::info!(world = %id, held, "job folder removed"),
        Ok((Err(e), held)) => {
            tracing::warn!(world = %id, held, error = %e, "job folder not cleaned up");
        }
        Err(e) => tracing::warn!(world = %id, error = %e, "job folder not cleaned up"),
    }
}

pub(super) async fn run_generation(w: &Worlds, world: &Path, mut status: Value, _slot: JobSlot) {
    let Ok(_permit) = w.one_gen.acquire().await else { return };
    let id = status["id"].as_str().unwrap_or("").to_owned();
    status["status"] = json!("running");
    if let Err(e) = w.save_status(&status).await {
        tracing::error!(error = %e, "status write failed");
    }
    tracing::info!(world = %id, "generation running");
    let result = if status["engine"] == ENGINE_DATA_ONLY {
        generate_data_only(w, &status).await
    } else {
        generate_sandboxed(w, world).await
    };
    let done = result.is_ok();
    match result {
        Ok((loaded, report)) => {
            status["status"] = json!("done");
            status["seed_name"] = json!(loaded.bundle.seed_name());
            status["slots"] = json!(loaded.bundle.slots().len());
            status["bundle_hash"] = json!(hex(&loaded.hash));
            status["generation"] = report;
            claim_room(w, &status).await;
        }
        Err(e) => {
            status["status"] = json!("failed");
            status["error"] = json!(e);
        }
    }
    if let Err(e) = w.save_final(&status).await {
        tracing::error!(error = %e, "status write failed");
    }
    // Logged after the status is stored: tests and operators wait on these lines. A failure's
    // line holds its error (the sidecar's output within `gen_error_max_bytes`), as its status
    // does.
    if done {
        tracing::info!(world = %id, "generation done");
    } else {
        let error = status["error"].as_str().unwrap_or("");
        tracing::warn!(world = %id, error, "generation failed");
    }
}
