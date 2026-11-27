//! What a start finds in the data folder: uploads and statuses an older server kept as files
//! move into the store, and a job folder a stopped process left is cleared and its job failed.

use super::*;

impl Worlds {
    /// Move the uploads an older server kept as files (`apworlds/` and `yamls/`, in game
    /// folders or flat) into the store, each checked against its id. With `remove` (a durable
    /// store) each file and its record go once stored, and then every folder left empty; an
    /// in-memory store keeps them, to read again on the next start. A file that does not move
    /// stays, with a warning, and is tried again on the next start.
    pub(super) async fn import_files(&self, remove: bool) {
        let (mut moved, mut kept) = (0_usize, 0_usize);
        for kind in [Kind::Apworld, Kind::Yaml] {
            let root = self.dir.join(kind.folder());
            let Some(Some((dirs, records))) = blocking(move || upload_records(&root)).await else {
                continue;
            };
            for record in records {
                match self.import_file(kind, &record, remove).await {
                    Ok(()) => moved += 1,
                    Err(why) => {
                        kept += 1;
                        let file = record.display();
                        tracing::warn!(%file, "old upload not moved into the store: {why}");
                    }
                }
            }
            if remove {
                // Game folders first, then the kind's; only an empty folder is removed.
                blocking(move || dirs.iter().rev().for_each(|d| drop(std::fs::remove_dir(d))))
                    .await;
            }
        }
        if moved + kept > 0 {
            tracing::info!(moved, kept, removed = remove, "old upload files moved into the store");
        }
    }

    /// Write a status an older server kept as a file, only when the store has none for the job.
    ///
    /// # Errors
    /// A status without an id and state, or the store failing.
    pub(super) async fn save_old_status(&self, status: &Value) -> Result<(), String> {
        let (id, state, text) = job_row(status)?;
        let put = self.store.job_put_if_absent(id, state, &text).await;
        put.map(drop).map_err(|e| store_failed(&e))
    }

    /// Fail job `id` at once, while it is queued or running: its job folder here outlived this
    /// host's process, so the job died with it (each instance has its own data folder). How
    /// far the job had got, for the log.
    pub(super) async fn fail_left(&self, id: &str) -> Option<String> {
        let (text, _) = self.store.job_status(id).await.ok()??;
        let mut status: Value = serde_json::from_str(&text).ok()?;
        let was = status["status"].as_str().map(str::to_owned);
        fail_restarted(&mut status);
        match self.store.job_fail_stale(id, &status.to_string(), Duration::ZERO).await {
            Ok(true) => tracing::warn!(world = %id, "generation failed: its server restarted"),
            Ok(false) => {}
            Err(e) => tracing::warn!(world = %id, error = ?e, "could not fail a left generation"),
        }
        was
    }

    /// Move the statuses an older server kept as `worlds/<id>/status.json` into the store, and
    /// clear what a stopped process left in this host's job folders. A status that was queued or
    /// running is failed: the process running it was this host's, and it stopped. With
    /// `remove` (a durable store) each moved `status.json` goes, then every job folder left
    /// empty; an in-memory store keeps them, to read again on the next start.
    pub(super) async fn import_jobs(&self, remove: bool) {
        let root = self.dir.join("worlds");
        let Some(Some(worlds)) = blocking(move || job_folders(&root)).await else { return };
        let most = self.limits.job_log_files_max;
        let mut moved = 0_usize;
        for world in worlds {
            let file = world.join("status.json");
            let dir = world.clone();
            let scan = blocking(move || {
                let found = read_json(&dir.join("status.json"));
                let inputs = [Kind::Apworld.folder(), Kind::Yaml.folder()].map(|sub| dir.join(sub));
                let held = inputs.iter().any(|d| d.exists()) || found.is_none();
                (found, inputs, held.then(|| folder_contents(&dir, most)))
            });
            let Some((found, inputs, held)) = scan.await else { continue };
            // A job folder with its copies of the inputs, or with no status file (every job
            // makes its folder when it starts): left by a process that stopped mid-job. Said in
            // the log (which world, how far it got, what it held) before the copies go.
            if let Some(held) = held {
                let name = world.file_name().map(|n| n.to_string_lossy().into_owned());
                let name = name.filter(|n| valid_id(n)).unwrap_or_default();
                let stored = if found.is_none() { self.fail_left(&name).await } else { None };
                let state = found.as_ref().and_then(|s| s["status"].as_str());
                tracing::warn!(
                    world = name,
                    state = state.or(stored.as_deref()).unwrap_or("unknown"),
                    held,
                    "a stopped server left this job folder; its copies of the inputs are removed"
                );
                blocking(move || inputs.iter().for_each(|d| drop(std::fs::remove_dir_all(d))))
                    .await;
            }
            if let Some(mut status) = found {
                if unfinished(&status) {
                    fail_restarted(&mut status);
                }
                // Never over a row the store already has: a file is older than any row.
                match self.save_old_status(&status).await {
                    Ok(()) => {
                        moved += 1;
                        if remove && let Err(e) = remove_files(vec![file.clone()]).await {
                            tracing::warn!(error = %e, "old status left");
                        }
                    }
                    Err(why) => tracing::warn!(
                        file = %file.display(),
                        "old status not moved into the store: {why}"
                    ),
                }
            }
            blocking(move || drop(std::fs::remove_dir(&world))).await;
        }
        if moved > 0 {
            tracing::info!(moved, removed = remove, "old generation statuses moved into the store");
        }
    }

    /// Move one upload an older server kept: `record` is its `<id>.json`, beside its file.
    pub(super) async fn import_file(
        &self,
        kind: Kind,
        record: &Path,
        remove: bool,
    ) -> Result<(), String> {
        let id = record.file_stem().and_then(|s| s.to_str()).filter(|s| valid_id(s));
        let id = id.ok_or("not an upload record")?;
        let file = record.with_extension(kind.ext());
        let meta = read_json(record).ok_or("its record is not readable JSON")?;
        let key = manifests::key_of(id).ok_or("bad id")?;
        // Read and hashed off the runtime's threads: a file may be up to its cap.
        let (path, want) = (file.clone(), id.to_owned());
        let read = tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(&path)?;
            Ok::<_, std::io::Error>((content_id(&bytes) == want, bytes))
        });
        let read = read.await.map_err(|e| format!("the read stopped: {e}"))?;
        let bytes = match read {
            Ok((true, bytes)) => bytes,
            Ok((false, _)) => return Err("its file does not hash to its id".to_owned()),
            // The file went but its record stayed (a remove stopped halfway): the upload is
            // stored, so only the record is left to go.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && remove => {
                if self.intact_upload(kind, id).await?.is_none() {
                    return Err(format!("{}: {e}", file.display()));
                }
                return remove_files(vec![record.to_owned()]).await;
            }
            Err(e) => return Err(format!("{}: {e}", file.display())),
        };
        self.store_upload(kind, key, &meta, &bytes).await.map_err(|e| store_failed(&e))?;
        if remove {
            // The file first: a record without its file is finished on the next start (above).
            remove_files(vec![file, record.to_owned()]).await?;
        }
        Ok(())
    }
}

/// Mark `status` failed: its job was this host's, and the process running it stopped.
fn fail_restarted(status: &mut Value) {
    status["status"] = json!(JobState::Failed.as_str());
    status["error"] = json!("the server restarted before this generation finished");
}

/// Run blocking file work off the runtime's threads; `None` when it panicked (said in the log).
pub(super) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| tracing::error!(error = %e, "file work stopped"))
        .ok()
}

/// Remove `paths` in order, stopping at the first that will not go.
pub(super) async fn remove_files(paths: Vec<PathBuf>) -> Result<(), String> {
    let gone = blocking(move || {
        paths
            .iter()
            .try_for_each(|p| std::fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display())))
    });
    gone.await.unwrap_or_else(|| Err("the removal stopped".to_owned()))
}

/// The folders under an upload kind's `root` (the root and its game folders), and every upload
/// record (`*.json`) in them; none when there is no root.
pub(super) fn upload_records(root: &Path) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
    let games = std::fs::read_dir(root).ok()?;
    let mut dirs = vec![root.to_owned()];
    dirs.extend(games.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()));
    let files = dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten();
    let files = files.filter_map(Result::ok).map(|e| e.path());
    let records = files.filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    Some((dirs, records))
}

/// Every job folder under `root`; none when there is no root.
pub(super) fn job_folders(root: &Path) -> Option<Vec<PathBuf>> {
    let entries = std::fs::read_dir(root).ok()?;
    Some(entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect())
}

/// Write `bytes` to `path` through a temporary file and a rename, so a reader never sees half.
/// The temporary name is unique, so two writers of one path never share it.
pub(super) fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
