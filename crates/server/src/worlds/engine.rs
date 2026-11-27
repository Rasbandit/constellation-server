//! The data-only path: manifests the data-fill engine checks and prepares, and the worlds it
//! fills from them, in a capped child process or on a pooled worker.

use super::*;

/// `status.engine` of a world the data-fill engine generates (a legacy world has none).
pub(super) const ENGINE_DATA_ONLY: &str = "data-only";

/// How the data-only jobs (a fill, a manifest check) run: in a capped child process, always but
/// in the unit tests of this module, whose executable is not the server's.
#[derive(Debug, Clone)]
pub(super) enum JobRunner {
    /// This executable's path, found once at startup (see `sandbox::exe_path`).
    Child(PathBuf),
    #[cfg(test)]
    InProcess,
}

impl Worlds {
    /// Fill the request `head` on the prepared world: on a pooled worker that stays up and may
    /// hold the world loaded, or without a pool as a `fill` job in a process of its own. The
    /// same seed either way, under the same caps and the same `fill_timeout_ms` kill.
    pub(super) async fn fill(
        &self,
        head: &[u8],
        world: &PreparedWorld,
    ) -> Result<Vec<u8>, Failure> {
        if let Some(pool) = &self.pool {
            let job = crate::pool::Job {
                head,
                threads: fill_threads(),
                key: &world.key,
                file: &world.file,
            };
            return pool.fill(job).await;
        }
        let input = data_fill::job::frame(head, &world.file).map_err(Failure::Broken)?;
        self.run_job("fill", &input, Duration::from_millis(self.limits.fill_timeout_ms)).await
    }

    /// Run a data-only job (`prepare` or `fill`) on `input`; see [`JobRunner`].
    pub(super) async fn run_job(
        &self,
        job: &str,
        input: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, Failure> {
        match &self.jobs {
            JobRunner::Child(exe) => run_job(exe, job, input, &self.limits, timeout).await,
            #[cfg(test)]
            JobRunner::InProcess => {
                let (job, input) = (job.to_owned(), input.to_vec());
                self.ran.lock().unwrap_or_else(PoisonError::into_inner).push(job.clone());
                tokio::task::spawn_blocking(move || {
                    data_fill::job::run(&job, &input).map_err(Failure::Refused)
                })
                .await
                .unwrap_or_else(|e| Err(Failure::Broken(e.to_string())))
            }
        }
    }
}

pub(super) async fn put_manifest(
    State(w): State<Arc<Worlds>>,
    RawQuery(q): RawQuery,
    body: Bytes,
) -> Reply {
    upload_manifest(&w, q.as_deref(), &body).await
}

/// Check `manifest` in a capped child process (the same engine that will fill it) and keep what
/// the check compiled: the found record and the prepared world's file (`data_fill::prepared`).
pub(super) async fn prepare_manifest(
    w: &Worlds,
    manifest: &[u8],
) -> Result<(Value, Vec<u8>), Failure> {
    let timeout = Duration::from_millis(w.limits.inspect_timeout_ms);
    let out = w.run_job("prepare", manifest, timeout).await?;
    let (found, file) = data_fill::job::unframe(&out).map_err(Failure::Broken)?;
    let found = serde_json::from_slice(found)
        .map_err(|e| Failure::Broken(format!("the manifest check printed bad JSON: {e}")))?;
    Ok((found, file.to_vec()))
}

/// Manifest `id`'s prepared world: the stored one, or, when that is stale or lost (prepared by
/// another build or layout, or its blob is missing or torn), prepared again from the stored
/// manifest and stored.
pub(super) async fn prepared_world(w: &Worlds, id: &str) -> Result<PreparedWorld, String> {
    match w.manifests.prepared(id).await.map_err(|e| format!("{e:?}"))? {
        Some(stored) => Ok(stored),
        None => prepare_again(w, id).await,
    }
}

/// Prepare manifest `id`'s world from the stored manifest and store it in place of what was
/// stored for this engine.
pub(super) async fn prepare_again(w: &Worlds, id: &str) -> Result<PreparedWorld, String> {
    let manifest = w.manifests.bytes(id).await.map_err(|e| format!("{e:?}"))?;
    let manifest = manifest.ok_or("the manifest is gone")?;
    tracing::info!(id, engine = w.manifests.engine, "preparing a world again");
    let (_, prepared) = prepare_manifest(w, &manifest).await.map_err(|e| e.to_string())?;
    w.manifests.put_prepared(id, prepared).await.map_err(|e| format!("{e:?}"))
}

/// Check a manifest in a capped child process (the same engine that will fill it), then store it
/// and its prepared world in the database. The same content again returns the stored record
/// without a check.
pub(super) async fn upload_manifest(w: &Worlds, query: Option<&str>, body: &[u8]) -> Reply {
    let name = upload_name(query).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let cap = w.limits.manifest_max_bytes;
    if body.len() > cap {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, format!("larger than {cap} bytes")));
    }
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty upload".to_owned()));
    }
    let id = content_id(body);
    if let Some(stored) = w.manifests.record(&id).await.map_err(store_error)? {
        // A stored manifest whose bytes no longer hash to its id (a torn row) is not served as
        // stored: it falls through to the check and is written again, which heals it.
        match w.manifests.bytes(&id).await {
            Ok(Some(_)) => return Ok(json_reply(StatusCode::OK, &stored)),
            Ok(None) | Err(log::StoreError::Corrupt(_)) => {
                tracing::warn!(id, "stored manifest is missing or torn; storing it again");
            }
            Err(e) => return Err(store_error(e)),
        }
    }
    let Ok(_check) = w.checks.try_acquire() else {
        return Ok(busy(w, "too many manifests are being checked"));
    };
    let (found, prepared) = prepare_manifest(w, body).await.map_err(|e| match e {
        Failure::Refused(why) => (StatusCode::UNPROCESSABLE_ENTITY, why),
        Failure::Broken(why) => internal(why),
    })?;
    let stored = w.manifests.put(&name, body, &found).await.map_err(store_error)?;
    // What the check compiled is kept: a generation fills from it and compiles nothing. The
    // manifest is stored by now, and a generation prepares a world that has no stored form, so
    // failing to keep it is not the upload failing.
    if let Err(e) = w.manifests.put_prepared(&id, prepared).await {
        tracing::warn!(id, error = ?e, "the prepared world was not stored; a generation will");
    }
    tracing::info!(id, name, "manifest stored");
    Ok(json_reply(StatusCode::CREATED, &stored))
}

/// `GET /api/manifests?limit=&after=&game=`: a page of records ascending by id, `{items, next}`;
/// `next` is the `after` of the following page, null on the last.
pub(super) async fn list_manifests(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery) -> Reply {
    let q = q.unwrap_or_default();
    let param = |key: &str| raw_param(&q, key).and_then(percent_decode);
    let limit = param("limit").and_then(|v| v.parse().ok());
    let bad = |e: log::StoreError| match e {
        log::StoreError::Corrupt(why) => (StatusCode::BAD_REQUEST, why),
        e @ log::StoreError::Unavailable => internal(format!("{e:?}")),
    };
    let (items, next) = w
        .manifests
        .list(param("game").as_deref(), param("after").as_deref(), limit)
        .await
        .map_err(bad)?;
    Ok(json_reply(StatusCode::OK, &json!({"items": items, "next": next})))
}

/// `POST /api/worlds` with `{"manifest": <id>, "yamls": [<id>...]}`: a data-only world, filled by
/// the engine in a capped child process (no world code runs). The player files are JSON
/// documents with a `name`, a `game` (the manifest's) and the world's options, pool and start
/// items (`data_fill::Instance`).
pub(super) async fn create_data_only_world(
    w: &Arc<Worlds>,
    req: &Value,
    owner: Option<&str>,
    slot: JobSlot,
) -> Reply {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    if req.get("apworlds").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
        return Err(bad("a manifest world takes no legacy world files".to_owned()));
    }
    let manifest_id = req["manifest"].as_str().filter(|i| valid_id(i));
    let manifest_id = manifest_id.ok_or_else(|| bad("manifest: bad id".to_owned()))?;
    let manifest = w.manifests.record(manifest_id).await.map_err(store_error)?;
    let manifest = manifest.ok_or_else(|| bad(format!("manifest: no upload {manifest_id}")))?;
    let yamls = chosen(w, req, "yamls", Kind::Yaml).await?;
    if yamls.is_empty() {
        return Err(bad("choose at least one player file".to_owned()));
    }
    // A player file says which game it is for; the room plays the manifest's one.
    let game = manifest["game"].as_str().unwrap_or("");
    for y in &yamls {
        let games = (Kind::Yaml.spec().games)(y);
        if games.is_empty() || games.iter().any(|g| g.trim() != game) {
            let name = y["name"].as_str().unwrap_or("a player file");
            return Err(bad(format!("{name}: its game is not {game:?}, the manifest's")));
        }
    }
    let yaml_ids = ids(&yamls);
    let id = new_world_id(w, &[&vec![json!(manifest_id)], &yaml_ids]);
    let mut status = json!({
        "id": id, "status": "queued", "engine": ENGINE_DATA_ONLY,
        "manifest": manifest_id, "yamls": yaml_ids,
    });
    with_owner(&mut status, owner);
    queue(w, status, slot).await
}

/// A fill request's player: the slot's `name` and the player file (the world's instance for
/// this player, `data_fill::Instance`; unknown keys such as `game` are ignored by it).
#[derive(serde::Serialize)]
pub(super) struct PlayerReq {
    pub(super) name: String,
    pub(super) instance: Value,
}

/// The players' files as fill request players. Real YAML player files are not read for manifest
/// worlds yet (see #114).
pub(super) fn player_requests(files: &[(String, Vec<u8>)]) -> Result<Vec<PlayerReq>, String> {
    files
        .iter()
        .map(|(label, raw)| {
            let doc: Value = serde_json::from_slice(raw)
                .map_err(|e| format!("{label}: a manifest world reads JSON player files: {e}"))?;
            let name =
                doc["name"].as_str().ok_or(format!("{label}: the player file has no name"))?;
            Ok(PlayerReq { name: name.to_owned(), instance: doc.clone() })
        })
        .collect()
}

/// Threads a generation asks its fill for: written into the request ([`fill_head`]) and told
/// to the pool, which bounds a worker's CPU time by it. The one place both read.
pub(super) const fn fill_threads() -> usize {
    config::GENERATION_WORKERS
}

/// The head of a generation's fill request (`data_fill::job::Fill`, JSON), which the prepared
/// world follows: on `config::GENERATION_WORKERS` threads, for a seed of at most
/// `seed_max_bytes`.
pub(super) fn fill_head(
    seed_name: &str,
    files: &[(String, Vec<u8>)],
    (rng_seed, seed_max_bytes): (u64, u64),
) -> Result<Vec<u8>, String> {
    let head = data_fill::job::Fill {
        seed_name: seed_name.to_owned(),
        players: player_requests(files)?,
        rng_seed,
        workers: fill_threads(),
        algorithm: data_fill::room::Algorithm::Exact,
        seed_max_bytes,
    };
    serde_json::to_vec(&head).map_err(|e| e.to_string())
}

/// Each chosen player file from the store, with the name it was uploaded under (for errors).
pub(super) async fn player_files(
    w: &Worlds,
    yamls: &[String],
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut files = Vec::with_capacity(yamls.len());
    for id in yamls {
        let meta = w.upload_meta(Kind::Yaml, id).await?.ok_or(format!("no upload {id}"))?;
        let label = meta["name"].as_str().map_or_else(|| id.clone(), str::to_owned);
        files.push((label, w.upload_file(Kind::Yaml, id).await?));
    }
    Ok(files)
}

/// A manifest world: the engine fills it in a child process under the fill caps (address space,
/// CPU time, wall clock; a child past its time is killed), under the same `gen_concurrency`
/// permit, status file and publish path as a legacy world.
pub(super) async fn generate_data_only(
    w: &Worlds,
    status: &Value,
) -> Result<(Loaded, Value), String> {
    let mut laps = Laps::start();
    let id = status["id"].as_str().unwrap_or("");
    let manifest = status["manifest"].as_str().unwrap_or("");
    let mut prepared = prepared_world(w, manifest).await?;
    let yamls: Vec<String> = status["yamls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|y| y.as_str().map(str::to_owned))
        .collect();
    // The seed is the world id's own hash: the same world id gives the same seed.
    let rng_seed =
        u64::from_le_bytes(Sha256::digest(id.as_bytes())[..8].try_into().unwrap_or([0; 8]));
    let seed_name = format!("data-only-{}", id.get(..12).unwrap_or(id));
    let files = player_files(w, &yamls).await?;
    laps.lap("store");
    let players = yamls.len();
    // Building the request is blocking work.
    let limits = w.limits;
    let head = tokio::task::spawn_blocking(move || {
        fill_head(&seed_name, &files, (rng_seed, limits.bundle_max_bytes))
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))?;
    laps.lap("request");
    // The job answers with the seed file itself; it names the game's package of the prepared
    // world.
    let mut first = true;
    let seed = loop {
        match w.fill(&head, &prepared).await {
            // A stored world this engine cannot load (it hashes to its key, so the store gave
            // it back) would fail every generation of its manifest: prepare it again, once.
            Err(Failure::Refused(why)) if first && why.contains(data_fill::job::UNLOADABLE) => {
                tracing::warn!(manifest, why, "the stored prepared world does not load");
                prepared = prepare_again(w, manifest).await?;
                first = false;
            }
            done => break done.map_err(|e| e.to_string())?,
        }
    };
    laps.lap("job");
    let loaded = tokio::task::spawn_blocking(move || {
        let game = data_fill::prepared::seed_game(&prepared)?.to_vec();
        crate::parse_seed(seed, vec![game], &limits)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))?;
    laps.lap("open");
    publish_loaded(w, loaded.clone()).await?;
    laps.lap("publish");
    // For the operator only. A requester is told no timings: how long the job took says whether
    // its world was already loaded in a pooled worker, that is, whether someone filled it lately.
    let ms = Value::Object(laps.ms.clone()).to_string();
    tracing::info!(world = %id, ms, "data-only generation timed");
    #[cfg(test)]
    {
        *w.laps.lock().unwrap_or_else(PoisonError::into_inner) = laps.ms;
    }
    let report = json!({"engine": ENGINE_DATA_ONLY, "rng_seed": rng_seed, "players": players});
    Ok((loaded, report))
}

/// Where a generation's wall time went: the milliseconds of each phase, for the server's log.
pub(super) struct Laps {
    pub(super) at: Instant,
    pub(super) ms: serde_json::Map<String, Value>,
}

impl Laps {
    pub(super) fn start() -> Self {
        Self { at: Instant::now(), ms: serde_json::Map::new() }
    }

    /// The phase since the last lap (or the start) ends now.
    pub(super) fn lap(&mut self, phase: &str) {
        let now = Instant::now();
        // To a thousandth of a millisecond.
        let ms = ((now - self.at).as_secs_f64() * 1e6).round() / 1e3;
        self.ms.insert(phase.to_owned(), json!(ms));
        self.at = now;
    }
}
