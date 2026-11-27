//! The legacy generator path: `.apworld` and `.yaml` uploads the sidecar inspects in the
//! sandbox, and the worlds it generates from them there.

use super::*;

impl Worlds {
    /// Run one step of a catalogue check in the sandbox on `apworld` (see [`crate::checks`]),
    /// in a folder of its own under `tmp/` (cleared at startup) that goes when it ends.
    pub(crate) async fn sandboxed_check(
        &self,
        step: &crate::checks::Step,
        apworld: &[u8],
    ) -> Result<Vec<u8>, Failure> {
        use crate::checks::Step;
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join("tmp").join(format!("check-{}-{n}", std::process::id()));
        let broken = |e: std::io::Error| Failure::Broken(format!("check folder: {e}"));
        let ran = async {
            let inside = format!("{JOB}/upload.apworld");
            let (unpacked, entries) = (
                self.limits.apworld_max_unpacked_bytes.to_string(),
                self.limits.apworld_max_entries.to_string(),
            );
            let inspect = Duration::from_millis(self.limits.inspect_timeout_ms);
            match step {
                Step::Inspect | Step::Scan => {
                    tokio::fs::create_dir_all(&tmp).await.map_err(broken)?;
                    tokio::fs::write(tmp.join("upload.apworld"), apworld).await.map_err(broken)?;
                    let cmd =
                        if *step == Step::Inspect { "inspect-apworld" } else { "scan-apworld" };
                    let args =
                        [cmd, &inside, "--max-unpacked", &unpacked, "--max-entries", &entries];
                    let mounts = [Mount::Ro(tmp.clone(), JOB.to_owned())];
                    self.sandbox.run(false, &mounts, &args, inspect).await
                }
                Step::Seed { module, seed } => {
                    if !valid_module(module) {
                        return Err(Failure::Refused(format!("bad module name {module:?}")));
                    }
                    let worlds = tmp.join("apworlds");
                    tokio::fs::create_dir_all(&worlds).await.map_err(broken)?;
                    let file = worlds.join(format!("{module}.apworld"));
                    tokio::fs::write(file, apworld).await.map_err(broken)?;
                    let (custom, out) = (format!("{AP}/custom_worlds"), format!("{JOB}/out"));
                    let seed = seed.to_string();
                    let args = [
                        "check",
                        "--ap",
                        AP,
                        "--apworlds",
                        &custom,
                        "--module",
                        module,
                        "--seed",
                        &seed,
                        "--out",
                        &out,
                    ];
                    let mounts = [Mount::Ro(worlds, custom.clone()), Mount::Scratch(out.clone())];
                    let timeout = Duration::from_millis(self.limits.gen_timeout_ms);
                    self.sandbox.run(true, &mounts, &args, timeout).await
                }
            }
        };
        let result = ran.await;
        if let Err(e) = tokio::fs::remove_dir_all(&tmp).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, dir = %tmp.display(), "check folder left behind");
        }
        result
    }
}

pub(super) async fn put_apworld(
    State(w): State<Arc<Worlds>>,
    RawQuery(q): RawQuery,
    body: Bytes,
) -> Reply {
    upload(&w, Kind::Apworld, q.as_deref(), &body).await
}

pub(super) async fn put_yaml(
    State(w): State<Arc<Worlds>>,
    RawQuery(q): RawQuery,
    body: Bytes,
) -> Reply {
    upload(&w, Kind::Yaml, q.as_deref(), &body).await
}

/// Check, inspect in the sandbox, then store an upload. The same content again returns the
/// stored record.
pub(super) async fn upload(w: &Worlds, kind: Kind, query: Option<&str>, body: &[u8]) -> Reply {
    let (name, id) = checked_upload(query, body, kind.cap(&w.limits))?;
    if let Some(meta) = w.intact_upload(kind, &id).await.map_err(internal)? {
        return Ok(json_reply(StatusCode::OK, &meta));
    }
    let n = w.seq.fetch_add(1, Ordering::Relaxed);
    let tmp = w.dir.join("tmp").join(format!("{id}-{}-{n}", std::process::id()));
    let result = inspect_and_store(w, kind, &name, &id, body, &tmp).await;
    if let Err(e) = std::fs::remove_dir_all(&tmp) {
        tracing::warn!(error = %e, dir = %tmp.display(), "temporary upload folder left behind");
    }
    let meta = result?;
    tracing::info!(kind = kind.folder(), id, name, "upload stored");
    Ok(json_reply(StatusCode::CREATED, &meta))
}

async fn inspect_and_store(
    w: &Worlds,
    kind: Kind,
    name: &str,
    id: &str,
    body: &[u8],
    tmp: &Path,
) -> Result<Value, (StatusCode, String)> {
    let file = format!("upload.{}", kind.ext());
    std::fs::create_dir_all(tmp).map_err(internal)?;
    std::fs::write(tmp.join(&file), body).map_err(internal)?;
    let mut meta = inspect_in_sandbox(w, kind, &file, tmp).await?;
    // The server's own fields last, so the sidecar can never set them.
    for (k, v) in [("id", json!(id)), ("name", json!(name)), ("bytes", json!(body.len()))] {
        meta[k] = v;
    }
    let key = manifests::key_of(id).ok_or_else(|| internal("an upload id that is not hex"))?;
    w.store_upload(kind, key, &meta, body).await.map_err(store_error)?;
    Ok(meta)
}

/// What the sidecar finds in an `.apworld` or a player `.yaml` (the file is `tmp/<file>`).
async fn inspect_in_sandbox(
    w: &Worlds,
    kind: Kind,
    file: &str,
    tmp: &Path,
) -> Result<Value, (StatusCode, String)> {
    let inside = format!("{JOB}/{file}");
    let (unpacked, entries) =
        (w.limits.apworld_max_unpacked_bytes.to_string(), w.limits.apworld_max_entries.to_string());
    let args: Vec<&str> = match kind {
        Kind::Apworld => {
            vec!["inspect-apworld", &inside, "--max-unpacked", &unpacked, "--max-entries", &entries]
        }
        Kind::Yaml => vec!["inspect-yaml", &inside],
    };
    let timeout = Duration::from_millis(w.limits.inspect_timeout_ms);
    let found = w
        .sandbox
        .run(false, &[Mount::Ro(tmp.to_owned(), JOB.to_owned())], &args, timeout)
        .await
        .map_err(failure_reply)?;
    let found: Value = serde_json::from_slice(&found)
        .map_err(|e| internal(format!("sidecar printed bad JSON: {e}")))?;
    match (kind, found) {
        (Kind::Apworld, Value::Object(o)) => {
            if !o.get("module").and_then(Value::as_str).is_some_and(valid_module) {
                return Err((StatusCode::UNPROCESSABLE_ENTITY, "bad module name".to_owned()));
            }
            Ok(Value::Object(o))
        }
        (Kind::Yaml, players @ Value::Array(_)) => Ok(json!({"players": players})),
        (_, other) => Err(internal(format!("unexpected inspect result {other}"))),
    }
}

/// What Archipelago imports as `worlds.<module>`; also the file name in `custom_worlds`, so it
/// must never hold a path separator (the sidecar checks the same).
pub(crate) fn valid_module(m: &str) -> bool {
    let mut c = m.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

pub(super) async fn list_apworlds(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Apworld).await
}

pub(super) async fn list_yamls(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Yaml).await
}

pub(super) async fn list(w: &Worlds, kind: Kind) -> Reply {
    let records = w.store.upload_list(kind.store()).await.map_err(store_error)?;
    let mut all: Vec<Value> = records.iter().filter_map(|r| serde_json::from_str(r).ok()).collect();
    let key = |v: &Value| (v["name"].as_str().unwrap_or("").to_owned(), v["id"].to_string());
    all.sort_by_key(key);
    Ok(json_reply(StatusCode::OK, &Value::Array(all)))
}

/// Lay out a world folder: the chosen files, written from the store (apworlds named by
/// module, as the generator loads them).
pub(super) async fn prepare(
    w: &Worlds,
    world: &Path,
    apworlds: &[Value],
    yamls: &[Value],
) -> Result<(), String> {
    // Written off the runtime's threads: an apworld may be up to its cap. Its own wording of a
    // stopped write: see #220.
    let blocking = async |f: Box<dyn FnOnce() -> std::io::Result<()> + Send>| {
        let done = tokio::task::spawn_blocking(f).await;
        done.map_err(|e| format!("the write stopped: {e}"))?.map_err(|e| e.to_string())
    };
    let subs = ["apworlds", "yamls"].map(|sub| world.join(sub));
    blocking(Box::new(move || subs.iter().try_for_each(std::fs::create_dir_all))).await?;
    for (kind, metas) in [(Kind::Apworld, apworlds), (Kind::Yaml, yamls)] {
        for m in metas {
            let id = m["id"].as_str().unwrap_or("");
            let stem = if kind == Kind::Apworld { m["module"].as_str().unwrap_or("") } else { id };
            let file = w.upload_file(kind, id).await?;
            let to = world.join(kind.folder()).join(format!("{stem}.{}", kind.ext()));
            blocking(Box::new(move || std::fs::write(&to, file))).await?;
        }
    }
    Ok(())
}

/// The legacy branch of `POST /api/worlds`: `{"apworlds": [<id>...], "yamls": [<id>...]}`, a
/// world the sidecar generates from the chosen files, laid out in its job folder first.
pub(super) async fn create_legacy_world(
    w: &Arc<Worlds>,
    req: &Value,
    owner: Option<&str>,
    slot: JobSlot,
) -> Reply {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let apworlds = chosen(w, req, "apworlds", Kind::Apworld).await?;
    let yamls = chosen(w, req, "yamls", Kind::Yaml).await?;
    if yamls.is_empty() {
        return Err(bad("choose at least one yaml".to_owned()));
    }
    let mut modules: Vec<&str> = apworlds.iter().filter_map(|m| m["module"].as_str()).collect();
    modules.sort_unstable();
    if modules.windows(2).any(|p| p.first() == p.get(1)) {
        return Err(bad("two apworlds have the same module name".to_owned()));
    }
    let (apworld_ids, yaml_ids) = (ids(&apworlds), ids(&yamls));
    let id = new_world_id(w, &[&apworld_ids, &yaml_ids]);
    let world = w.dir.join("worlds").join(&id);
    if let Err(e) = prepare(w, &world, &apworlds, &yamls).await {
        drop(std::fs::remove_dir_all(&world));
        return Err(internal(e));
    }
    let mut status =
        json!({"id": id, "status": "queued", "apworlds": apworld_ids, "yamls": yaml_ids});
    with_owner(&mut status, owner);
    queue(w, status, slot).await
}

/// What `ap_gen.py generate` prints. The bundle stays raw text (no tree of values in memory);
/// `parse_bundle` parses it into its typed form.
#[derive(Debug, serde::Deserialize)]
pub(super) struct GenOut {
    pub(super) report: Value,
    pub(super) bundle: Box<serde_json::value::RawValue>,
}

impl GenOut {
    pub(super) fn parse(stdout: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(stdout)
            .map_err(|e| format!("the sidecar's result is malformed: {e}"))
    }
}

/// A legacy world: the sidecar generates it in the sandbox from the chosen files.
pub(super) async fn generate_sandboxed(
    w: &Worlds,
    world: &Path,
) -> Result<(Loaded, Value), String> {
    let (custom, yamls, out) =
        (format!("{AP}/custom_worlds"), format!("{JOB}/yamls"), format!("{JOB}/out"));
    let mounts = [
        Mount::Ro(world.join("apworlds"), custom.clone()),
        Mount::Ro(world.join("yamls"), yamls.clone()),
        Mount::Scratch(out.clone()),
    ];
    let args = ["generate", "--ap", AP, "--apworlds", &custom, "--yamls", &yamls, "--out", &out];
    let timeout = Duration::from_millis(w.limits.gen_timeout_ms);
    // The job folder (its copies of the inputs) goes when the job ends ([`generate`]).
    match w.sandbox.run(true, &mounts, &args, timeout).await {
        Ok(stdout) => publish(w, stdout).await,
        Err(e) => Err(e.to_string()),
    }
}

/// Import the sidecar's v1 bundle (encoded to v2 off the runtime) into the host's store: its
/// seed and packages, never a file. With `--rooms` it is also listed as a room, after its seed
/// name is claimed.
pub(super) async fn publish(w: &Worlds, stdout: Vec<u8>) -> Result<(Loaded, Value), String> {
    let limits = w.limits;
    let parsed = try_blocking(move || {
        let out = GenOut::parse(&stdout)?;
        parse_bundle(out.bundle.get().as_bytes(), &limits).map(|loaded| (loaded, out.report))
    });
    let (loaded, report) = parsed.await?;
    publish_loaded(w, loaded.clone()).await?;
    Ok((loaded, report))
}
