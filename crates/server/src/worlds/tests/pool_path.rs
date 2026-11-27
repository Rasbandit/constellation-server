//! Whole generations through real fill processes: the request, the queue, the fill (in a
//! pooled worker, or without a pool in a job process of its own), the stored seed. The
//! server binary of this build's profile is the job process; `cargo test -p server --features
//! pool-tests` builds it with these tests (CI's `check` job does). Run alone after a change
//! to the fill, build the binary first, or a stale one answers.

use super::*;

/// The server binary cargo built for this test binary's profile: in the nearest folder above
/// it (`target/<profile>/`; how deep the test binary sits under it depends on the cargo).
pub(super) fn server_binary() -> PathBuf {
    let test = std::env::current_exe().unwrap();
    let bin = test.ancestors().map(|dir| dir.join("constellation-server")).find(|b| b.is_file());
    bin.unwrap_or_else(|| panic!("build the server binary of this profile first: {test:?}"))
}

/// A hosting server whose data-only jobs are real processes, with the pool `limits` asks for.
async fn real(name: &str, limits: Limits) -> Arc<Worlds> {
    let exe = server_binary();
    let pool = Pool::new(exe.clone(), limits);
    hosting_on(&temp(name), limits, JobRunner::Child(exe), pool).await
}

const ON: Limits = Limits { fill_pool_workers: 1, ..Limits::DESIGN };

/// The fixture manifest as version `version`: each version is another world.
fn versioned(version: &str) -> Vec<u8> {
    manifest(|m| m["version"] = json!(version))
}

/// Ask for a world of `names` on the uploaded `manifest` and wait until it ends: its status.
async fn generation(w: &Arc<Worlds>, manifest: &[u8], names: &[&str]) -> Value {
    let up = upload_manifest(w, Some("name=m.json"), manifest).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let instances = fixture_instances();
    let mut yamls = Vec::new();
    for (name, instance) in names.iter().zip(&instances) {
        yamls.push(player_yaml(w, name, "Fixture Game", instance).await["id"].clone());
    }
    let req = json!({"manifest": manifest_id, "yamls": yamls});
    let queued = create_world(State(Arc::clone(w)), me(), Bytes::from(req.to_string()));
    let id = json_of(queued.await.unwrap()).await["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = read_status(w, &id).await.unwrap();
        if matches!(status["status"].as_str(), Some("done" | "failed")) {
            return status;
        }
        assert!(Instant::now() < deadline, "the generation never ended: {status}");
        tokio::task::yield_now().await;
    }
}

/// The seed the server stored for a finished generation, and the seed the reference path
/// (the manifest compiled and filled in this process, the bundle imported) makes for the
/// same world, players, seed name and random stream.
async fn stored_and_reference(
    w: &Worlds,
    status: &Value,
    manifest: &[u8],
    names: &[&str],
) -> (Vec<u8>, Vec<u8>) {
    assert_eq!(status["status"], "done", "{status}");
    let (name, rng) = (status["seed_name"].as_str().unwrap(), &status["generation"]["rng_seed"]);
    let store::Db::Mem(mem) = &w.store else { panic!("the test host is in memory") };
    let stored = log::LogStore::seed_get(&**mem, crate::room_id(name)).await.unwrap().unwrap();
    let world: Value = serde_json::from_slice(manifest).unwrap();
    let players: Vec<data_fill::seed::Player> = names
        .iter()
        .zip(fixture_instances())
        .map(|(n, instance)| serde_json::from_value(json!({"name": n, "instance": instance})))
        .collect::<Result<_, _>>()
        .unwrap();
    let bundle =
        data_fill::seed::generate(&world, name, &players, rng.as_u64().unwrap(), 1).unwrap();
    let reference = parse_bundle(&serde_json::to_vec(&bundle).unwrap(), &Limits::DESIGN).unwrap();
    (stored, reference.bundle.bytes().to_vec())
}

fn kill(pid: u32) {
    let sent = std::process::Command::new("kill").args(["-KILL", &pid.to_string()]).status();
    assert!(sent.unwrap().success(), "kill -KILL {pid}");
}

/// The same generations with the pool and without it store the seeds the reference path
/// makes: two worlds in turn, one and two players, on a worker whose cache holds one of the
/// two worlds (every generation drops a world and loads it again) and which is replaced every
/// three jobs (so generations run on both sides of a recycle).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generations_store_the_same_seeds_with_the_pool_and_without_it() {
    let worlds = [versioned("1.2"), versioned("1.3")];
    let one_world = worlds
        .iter()
        .map(|m| {
            let (file, _) =
                data_fill::prepared::build(&serde_json::from_slice(m).unwrap()).unwrap();
            data_fill::prepared::Prepared::open(&file).unwrap().held_bytes()
        })
        .max()
        .unwrap();
    let pooled = Limits { fill_worker_cache_bytes: one_world, fill_worker_jobs: 3, ..ON };
    for (name, limits) in [("path-own", Limits::DESIGN), ("path-pool", pooled)] {
        let w = real(name, limits).await;
        assert_eq!(w.pool.is_some(), limits.fill_pool_workers > 0);
        let mut workers = Vec::new();
        for names in [&["Ann"][..], &["Ann", "Bo"], &["Cy"], &["Dee", "Eli"]] {
            for manifest in &worlds {
                let status = generation(&w, manifest, names).await;
                let (stored, reference) = stored_and_reference(&w, &status, manifest, names).await;
                assert!(stored == reference, "{name}: {names:?} differs from the reference");
                workers.extend(w.pool.iter().flat_map(Pool::idle_pids));
            }
        }
        workers.dedup();
        let expected = if w.pool.is_some() { 3 } else { 0 };
        assert_eq!(workers.len(), expected, "{name}: eight jobs at three a worker: {workers:?}");
    }
}

/// A generation whose worker was killed since the generation before it is filled all the
/// same, on a new worker, and stores the right seed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_generation_whose_worker_was_killed_in_between_is_filled_on_a_new_one() {
    let w = real("path-killed", ON).await;
    let (manifest, names) = (versioned("1.2"), ["Ann"]);
    assert_eq!(generation(&w, &manifest, &names).await["status"], "done");
    let pool = w.pool.as_ref().unwrap();
    let first = pool.idle_pids();
    kill(first[0]);
    let status = generation(&w, &manifest, &names).await;
    let (stored, reference) = stored_and_reference(&w, &status, &manifest, &names).await;
    assert!(stored == reference);
    assert!(pool.idle_pids().len() == 1 && pool.idle_pids() != first);
}

/// A stored world the pooled worker cannot load is prepared again and filled, as without the
/// pool: the worker's refusal is worded so that the server sees it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stored_world_a_pooled_worker_cannot_load_is_prepared_again() {
    let w = real("path-unloadable", ON).await;
    let body = versioned("1.2");
    upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    let id = content_id(&body);
    w.manifests.put_prepared(&id, b"not a prepared world".to_vec()).await.unwrap();
    let status = generation(&w, &body, &["Ann"]).await;
    let (stored, reference) = stored_and_reference(&w, &status, &body, &["Ann"]).await;
    assert!(stored == reference);
    let file = w.manifests.prepared(&id).await.unwrap().unwrap();
    assert!(data_fill::prepared::Prepared::open(&file).is_ok(), "the stored one loads now");
}

/// A generation past its time fails with the pool as without it, and the server goes on:
/// nothing is left of the worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pooled_generation_past_its_time_fails_as_one_in_its_own_process_does() {
    for (name, limits) in [("path-late-own", Limits::DESIGN), ("path-late-pool", ON)] {
        let w = real(name, Limits { fill_timeout_ms: 1, ..limits }).await;
        let status = generation(&w, &versioned("1.2"), &["Ann"]).await;
        assert_eq!(status["status"], "failed", "{name}: {status}");
        assert_eq!(status["error"], "timed out after 0 s and was killed", "{name}");
        assert!(w.pool.iter().all(|pool| pool.idle_pids().is_empty()), "{name}");
    }
}
