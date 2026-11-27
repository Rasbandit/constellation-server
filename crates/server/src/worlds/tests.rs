use super::*;

/// Where a generation's time goes, on a release build (CI's `bench` job).
#[cfg(feature = "gen-bench")]
mod gen_bench;
/// Whole generations through real fill processes, with and without the pool.
#[cfg(feature = "pool-tests")]
mod pool_path;

#[test]
fn only_64_lowercase_hex_digits_are_an_id() {
    let good = "a".repeat(64);
    assert!(valid_id(&good));
    assert!(valid_id(&content_id(b"")));
    for bad in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        format!("{}/", "a".repeat(63)),
        format!("..{}", "a".repeat(62)),
        "g".repeat(64),
    ] {
        assert!(!valid_id(&bad), "{bad:?}");
    }
}

/// Expected value: the published SHA-256 of "abc" (FIPS 180-2 appendix B.1).
#[test]
fn the_content_address_is_the_hex_sha256_of_the_bytes() {
    assert_eq!(
        content_id(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(content_id(b"abc"), content_id(b"abx"));
}

#[test]
fn an_ipv6_client_is_counted_by_its_64() {
    let req = |ip: &str| {
        let mut r = Request::new(axum::body::Body::empty());
        r.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 1)));
        r
    };
    assert_eq!(
        client_ip(&req("2001:db8:1:2::1"), None),
        client_ip(&req("2001:db8:1:2:ffff::9"), None)
    );
    assert_ne!(client_ip(&req("2001:db8:1:2::1"), None), client_ip(&req("2001:db8:1:3::1"), None));
    assert_eq!(client_ip(&req("10.0.0.7"), None), "10.0.0.7".parse::<IpAddr>().unwrap());
}

#[test]
fn an_internal_error_is_a_500_that_hides_its_detail() {
    let (status, text) = internal("/srv/data/secret.json: permission denied");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!text.is_empty() && !text.contains("secret"), "{text}");
}

#[test]
fn an_unavailable_room_is_a_503_that_asks_to_try_again() {
    let (status, text) = unavailable("room task stopped");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(text, "the room is not available right now; try again");
}

/// Shows the data folder only: never the sandbox or the limits.
#[test]
fn the_state_shows_its_folder_only() {
    let root = temp("debug");
    let w = worlds(&root, Limits::DESIGN);
    let shown = format!("{w:?}");
    assert_eq!(shown, format!("Worlds {{ dir: {:?}, .. }}", root.join("data")));
}

/// The API's state on a fake checkout under `root`, data in `root/data`.
fn worlds(root: &Path, limits: Limits) -> Worlds {
    let paths = crate::sandbox::tests::layout(root);
    let mut w = open(&root.join("data"), paths, limits).unwrap();
    // The unit tests are not the server's executable: the jobs run in this process.
    w.jobs = JobRunner::InProcess;
    w.pool = None;
    w.accounts = Some(Arc::new(PasswordAuth::new(w.store.clone(), limits, None)));
    w
}

/// Store `body` in `w`'s store as an earlier upload did, with record `meta` plus its id;
/// returns the record.
async fn stored(w: &Worlds, kind: Kind, body: &[u8], mut meta: Value) -> Value {
    let id = content_id(body);
    meta["id"] = json!(id);
    let key = manifests::key_of(&id).unwrap();
    let game = game_folder(kind, &meta);
    let row = UploadRecord { kind: kind.store(), id: key, game, meta: meta.to_string() };
    w.store.upload_put(&row, body).await.unwrap();
    meta
}

/// Write `body` under `data/<folder>` as an older server kept an upload, with record
/// `meta` plus its id; returns the record.
fn old_file(root: &Path, folder: &str, ext: &str, body: &[u8], mut meta: Value) -> Value {
    let id = content_id(body);
    meta["id"] = json!(id);
    let dir = root.join("data").join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{id}.{ext}")), body).unwrap();
    std::fs::write(dir.join(format!("{id}.json")), meta.to_string()).unwrap();
    meta
}

async fn json_of(r: Response) -> Value {
    assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
    serde_json::from_slice(&axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap()).unwrap()
}

/// An upload already stored is answered from its record, so this never reaches the
/// sandbox: what is left to see is the size check in front of it.
#[tokio::test]
async fn an_upload_of_exactly_its_cap_is_taken_and_one_byte_more_is_refused() {
    let root = temp("cap");
    let limits = Limits { apworld_max_bytes: 8, yaml_max_bytes: 4, ..Limits::DESIGN };
    let w = worlds(&root, limits);
    for (kind, folder, cap) in [(Kind::Apworld, "apworlds", 8), (Kind::Yaml, "yamls", 4)] {
        let body = vec![b'x'; cap];
        let meta = stored(&w, kind, &body, json!({"name": "a"})).await;
        let r = upload(&w, kind, Some("name=a"), &body).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{folder}");
        assert_eq!(json_of(r).await, meta);
        let over = upload(&w, kind, Some("name=a"), &vec![b'x'; cap + 1]).await.unwrap_err();
        assert_eq!(over.0, StatusCode::PAYLOAD_TOO_LARGE, "{folder}");
    }
}

#[tokio::test]
async fn a_list_holds_each_stored_record_once() {
    let root = temp("list");
    let w = worlds(&root, Limits::DESIGN);
    let meta = stored(&w, Kind::Yaml, b"name: a", json!({"name": "a.yaml"})).await;
    let r = list(&w, Kind::Yaml).await.unwrap();
    assert_eq!(json_of(r).await, json!([meta]));
    assert_eq!(json_of(list(&w, Kind::Apworld).await.unwrap()).await, json!([]));
}

/// The reason a choice of yamls is refused (always a 400).
async fn refused(w: &Worlds, ids: Value) -> String {
    let e = chosen(w, &json!({ "yamls": ids }), "yamls", Kind::Yaml).await.unwrap_err();
    assert_eq!(e.0, StatusCode::BAD_REQUEST, "{}", e.1);
    e.1
}

#[tokio::test]
async fn chosen_ids_must_each_be_stored_and_listed_once() {
    let root = temp("chosen");
    let w = worlds(&root, Limits::DESIGN);
    let a = stored(&w, Kind::Yaml, b"a", json!({"name": "a"})).await;
    let b = stored(&w, Kind::Yaml, b"b", json!({"name": "b"})).await;
    let both = json!({ "yamls": [a["id"], b["id"]] });
    assert_eq!(chosen(&w, &both, "yamls", Kind::Yaml).await.unwrap(), [a.clone(), b]);
    assert!(refused(&w, json!([a["id"], a["id"]])).await.contains("listed twice"));
    assert!(refused(&w, json!([content_id(b"c")])).await.contains("no upload"));
    assert!(refused(&w, json!(["../x"])).await.contains("bad id"));
    assert!(refused(&w, json!("x")).await.contains("must be a list"));
    // The same bytes stored as the other kind are not a choice of this one.
    stored(&w, Kind::Apworld, b"zip", json!({})).await;
    assert!(refused(&w, json!([content_id(b"zip")])).await.contains("no upload"));
    let none = chosen(&w, &json!({}), "apworlds", Kind::Apworld).await.unwrap_err().1;
    assert!(none.contains("apworlds must be a list"), "{none}");
}

/// The job folder gets the chosen files from the store; nothing is kept beside it.
#[tokio::test]
async fn a_world_folder_holds_the_chosen_files_apworlds_named_by_module() {
    let root = temp("prepare");
    let w = worlds(&root, Limits::DESIGN);
    let apworld = stored(&w, Kind::Apworld, b"zip", json!({"module": "clique"})).await;
    let yaml = stored(&w, Kind::Yaml, b"name: a", json!({})).await;
    let world = root.join("data/worlds/w");
    prepare(&w, &world, &[apworld], std::slice::from_ref(&yaml)).await.unwrap();
    for old in ["apworlds", "yamls"] {
        assert!(!root.join("data").join(old).exists(), "no {old} folder in the data folder");
    }
    let names = |sub: &str| -> Vec<String> {
        let entries = std::fs::read_dir(world.join(sub)).unwrap().filter_map(Result::ok);
        entries.map(|e| e.file_name().to_string_lossy().into_owned()).collect()
    };
    assert_eq!(names("apworlds"), ["clique.apworld"]);
    assert_eq!(std::fs::read(world.join("apworlds/clique.apworld")).unwrap(), b"zip");
    let id = yaml["id"].as_str().unwrap();
    assert_eq!(names("yamls"), [format!("{id}.yaml")]);
    assert_eq!(std::fs::read(world.join(format!("yamls/{id}.yaml"))).unwrap(), b"name: a");
}

#[tokio::test]
async fn two_apworlds_with_one_module_name_are_refused_before_anything_runs() {
    let root = temp("same-module");
    let w = Arc::new(worlds(&root, Limits::DESIGN));
    let one = stored(&w, Kind::Apworld, b"1", json!({"module": "clique"})).await;
    let two = stored(&w, Kind::Apworld, b"2", json!({"module": "clique"})).await;
    let yaml = stored(&w, Kind::Yaml, b"y", json!({})).await;
    let req = json!({"apworlds": [one["id"], two["id"]], "yamls": [yaml["id"]]});
    let e = create_world(State(w), me(), Bytes::from(req.to_string())).await.unwrap_err();
    assert_eq!(e, (StatusCode::BAD_REQUEST, "two apworlds have the same module name".into()));
    let worlds_dir = std::fs::read_dir(root.join("data/worlds")).unwrap();
    assert_eq!(worlds_dir.count(), 0, "no world folder was made");
}

#[tokio::test]
async fn a_world_status_needs_a_valid_id_and_an_existing_world() {
    let root = temp("status");
    let w = worlds(&root, Limits::DESIGN);
    let bad = read_status(&w, "../../token").await.unwrap_err();
    assert_eq!(bad.0, StatusCode::BAD_REQUEST);
    let none = read_status(&w, &content_id(b"none")).await.unwrap_err();
    assert_eq!(none.0, StatusCode::NOT_FOUND);
}

#[test]
fn upload_names_are_decoded_and_must_be_plain_file_names() {
    assert_eq!(upload_name(Some("name=Majoras%20Mask.yaml")).unwrap(), "Majoras Mask.yaml");
    assert_eq!(upload_name(Some("x=1&name=a+b.yaml")).unwrap(), "a b.yaml");
    assert_eq!(upload_name(Some("name=%C3%A9.yaml")).unwrap(), "é.yaml");
    for bad in [None, Some(""), Some("name="), Some("name=a%2Fb"), Some("name=..")] {
        assert!(upload_name(bad).is_err(), "{bad:?}");
    }
    for bad in ["name=%2", "name=%zz", "name=%FF", "name=a%00b", "name=a%5Cb"] {
        assert!(upload_name(Some(bad)).is_err(), "{bad:?}");
    }
    assert!(upload_name(Some(&format!("name={}", "a".repeat(256)))).is_err());
    assert!(upload_name(Some(&format!("name={}", "a".repeat(255)))).is_ok());
}

#[test]
fn module_names_are_python_identifiers() {
    for good in ["clique", "mm_2ship", "_x", "A1"] {
        assert!(valid_module(good), "{good}");
    }
    for bad in ["", "1a", "a-b", "a/b", "..", "a.b", "é"] {
        assert!(!valid_module(bad), "{bad}");
    }
}

#[test]
fn the_generator_version_is_read_from_the_checkouts_utils() {
    let dir = temp("generator-version");
    assert_eq!(generator_version(&dir), None, "no Utils.py");
    for (text, want) in [
        ("import x\n__version__ = \"0.6.8\"\n", Some("0.6.8")),
        ("__version__='0.7.0'  # comment\n", Some("0.7.0")),
        ("__version__ = \"\"\n", None),
        ("__version__ = version_of(x)\n", None),
        ("my__version__ = \"1\"\n", None),
    ] {
        std::fs::write(dir.join("Utils.py"), text).unwrap();
        assert_eq!(generator_version(&dir).as_deref(), want, "{text}");
    }
}

fn me() -> Identity {
    Identity { subject: "pw:me".into(), name: "me".into() }
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("worlds-{name}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&d));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_generation_result_must_be_an_object_with_a_report_and_a_bundle() {
    let ok = GenOut::parse(br#"{"report": {"seconds": 1}, "bundle": {"a": [1, 2]}}"#).unwrap();
    assert_eq!((ok.report, ok.bundle.get()), (json!({"seconds": 1}), r#"{"a": [1, 2]}"#));
    for bad in [&b"[]"[..], b"\"x\"", b"{}", br#"{"report": 1}"#, b"", b"{nope"] {
        let e = GenOut::parse(bad).unwrap_err();
        assert!(e.contains("malformed"), "{e}");
    }
}

/// What `run` logs, at debug and above, on this test's thread.
async fn logged(run: impl Future<Output = ()>) -> String {
    #[derive(Clone, Default)]
    struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let out = buf.clone();
    let logs = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || out.clone())
        .finish();
    {
        let _logs = tracing::subscriber::set_default(logs);
        run.await;
    }
    String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
}

/// A job folder's log line names each file with its size, in order, and links without
/// following them; past its cap it says how many more.
#[test]
fn a_job_folders_contents_are_named_for_its_log_line() {
    let dir = temp("contents");
    let most = Limits::DESIGN.job_log_files_max;
    assert_eq!(folder_contents(&dir, most), "nothing");
    std::fs::create_dir_all(dir.join("yamls")).unwrap();
    std::fs::write(dir.join("yamls/b.yaml"), b"bb").unwrap();
    std::fs::write(dir.join("a.apworld"), b"a").unwrap();
    std::os::unix::fs::symlink("/etc", dir.join("link")).unwrap();
    let held = folder_contents(&dir, most);
    assert_eq!(held, "a.apworld (1 bytes), link (not a file), yamls/b.yaml (2 bytes)");
    for i in 0..most {
        std::fs::write(dir.join(format!("f{i:02}")), b"").unwrap();
    }
    let held = folder_contents(&dir, most);
    assert!(held.ends_with(", and 3 more"), "{held}");
}

/// A failed generation's log line holds its error, as its status does, so a log alone says
/// why it failed.
#[tokio::test]
async fn a_failed_generations_log_line_holds_its_error() {
    let root = temp("failed-log");
    let w = worlds(&root, Limits::DESIGN);
    let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let meta = json!({"name": "nameless.yaml", "players": [{"game": "Fixture Game"}]});
    let nameless = stored(&w, Kind::Yaml, br#"{"game": "Fixture Game"}"#, meta).await;
    let id = "e".repeat(64);
    let status = json!({
        "id": id, "status": "queued", "engine": ENGINE_DATA_ONLY,
        "manifest": manifest_id, "yamls": [nameless["id"]],
    });
    let log = logged(run_generation(&w, &root, status, w.admit().unwrap())).await;
    let line = log.lines().find(|l| l.contains("generation failed")).expect("{log}");
    assert!(line.contains(&id) && line.contains("the player file has no name"), "{line}");
    let stored = read_status(&w, &id).await.unwrap();
    assert!(stored["error"].as_str().unwrap().contains("no name"), "{stored}");
}

/// The in-memory store under `w`, to age a job as if its process stopped.
fn mem_store(w: &Worlds) -> Arc<log::MemLogStore> {
    match &w.store {
        store::Db::Mem(m) => Arc::clone(m),
        store::Db::Pg(_) => unreachable!("these tests run in memory"),
    }
}

/// Statuses an older server kept as files move into the store; the ones it left queued or
/// running are failed, as its process stopped. The job folders a stopped process left go
/// too: its copies of the inputs, then each folder left empty (#119).
#[tokio::test]
async fn old_statuses_move_into_the_store_and_unfinished_ones_fail() {
    let root = temp("recover");
    let worlds_dir = root.join("data/worlds");
    let id = |c: char| c.to_string().repeat(64);
    for (c, state) in [('a', "queued"), ('b', "running"), ('c', "done"), ('d', "failed")] {
        let dir = worlds_dir.join(id(c));
        std::fs::create_dir_all(dir.join("apworlds")).unwrap();
        let status = json!({"id": id(c), "status": state, "bundle": "/b"});
        std::fs::write(dir.join("status.json"), status.to_string()).unwrap();
    }
    std::fs::write(worlds_dir.join(id('b')).join("apworlds/m.apworld"), b"zip").unwrap();
    let w = worlds(&root, Limits::DESIGN);
    let log = logged(w.import_jobs(true)).await;
    // Each left folder is said in the log before it goes: which world, how far, what it held.
    let left = log.lines().find(|l| l.contains(&id('b'))).expect("a line for the world");
    assert!(left.contains("a stopped server left this job folder"), "{left}");
    assert!(left.contains("running") && left.contains("m.apworld (3 bytes)"), "{left}");
    for c in ['a', 'b'] {
        let status = read_status(&w, &id(c)).await.unwrap();
        assert_eq!(status["status"], "failed", "{c}");
        assert!(status["error"].as_str().unwrap().contains("restarted"));
    }
    let done = read_status(&w, &id('c')).await.unwrap();
    assert_eq!(done, json!({"id": id('c'), "status": "done", "bundle": "/b"}));
    assert_eq!(read_status(&w, &id('d')).await.unwrap()["status"], "failed");
    let left = std::fs::read_dir(&worlds_dir).unwrap().count();
    assert_eq!(left, 0, "every status moved and every job folder cleared");
}

/// A job waiting in a long queue is kept alive by its heartbeat, so the sweep never fails
/// it; once the heartbeat stops, the job is lost and failed (#119).
#[tokio::test(start_paused = true)]
async fn a_queued_jobs_heartbeat_keeps_the_sweep_off_it() {
    let root = temp("heartbeat");
    let w = Arc::new(worlds(&root, Limits::DESIGN));
    let id = "f".repeat(64);
    w.save_status(&json!({"id": id, "status": "queued"})).await.unwrap();
    let (store, lost) = (mem_store(&w), Duration::from_secs(Limits::DESIGN.gen_lost_secs));
    // Unmarked for the lost time, as if it had waited that long in the queue.
    store.age_job_for_tests(&id, lost);
    let beat = tokio::spawn(heartbeat(Arc::clone(&w), id.clone()));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(Limits::DESIGN.gen_heartbeat_secs)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    w.fail_stale().await;
    assert_eq!(read_status(&w, &id).await.unwrap()["status"], "queued", "marked alive");
    beat.abort();
    store.age_job_for_tests(&id, lost);
    w.fail_stale().await;
    assert_eq!(read_status(&w, &id).await.unwrap()["status"], "failed", "no heartbeat now");
}

/// A queued or running job unmarked for `gen_stale_secs` reads as stalled, and as running
/// again once marked; unmarked for `gen_lost_secs` the sweep fails it. One still marked
/// alive, and a finished one, are left alone (#119).
#[tokio::test]
async fn the_sweep_fails_only_lost_jobs_and_a_stalled_one_comes_back() {
    let root = temp("sweep");
    let w = worlds(&root, Limits::DESIGN);
    let id = |c: char| c.to_string().repeat(64);
    for (c, state) in [('a', "running"), ('b', "running"), ('c', "done"), ('d', "queued")] {
        w.save_status(&json!({"id": id(c), "status": state})).await.unwrap();
    }
    let stale = Duration::from_secs(Limits::DESIGN.gen_stale_secs);
    let lost = Duration::from_secs(Limits::DESIGN.gen_lost_secs);
    let store = mem_store(&w);
    store.age_job_for_tests(&id('a'), lost);
    store.age_job_for_tests(&id('c'), lost);
    store.age_job_for_tests(&id('d'), stale);
    w.fail_stale().await;
    let a = read_status(&w, &id('a')).await.unwrap();
    assert_eq!(a["status"], "failed");
    assert!(a["error"].as_str().unwrap().contains("stopped answering"), "{a}");
    assert_eq!(read_status(&w, &id('b')).await.unwrap()["status"], "running", "still alive");
    assert_eq!(read_status(&w, &id('c')).await.unwrap()["status"], "done");
    assert_eq!(read_status(&w, &id('d')).await.unwrap()["status"], "stalled", "not lost yet");
    w.store.job_touch(&id('d')).await.unwrap();
    assert_eq!(read_status(&w, &id('d')).await.unwrap()["status"], "queued", "came back");
}

/// A job folder this host's stopped process left fails its job at once, without waiting to
/// be lost; a job without a folder here is the sweep's (#119).
#[tokio::test]
async fn a_job_folder_left_by_a_restart_fails_its_job_at_once() {
    let root = temp("left-folder");
    let w = worlds(&root, Limits::DESIGN);
    let id = |c: char| c.to_string().repeat(64);
    for c in ['a', 'b'] {
        w.save_status(&json!({"id": id(c), "status": "running"})).await.unwrap();
    }
    let dir = root.join("data/worlds").join(id('a'));
    std::fs::create_dir_all(dir.join("yamls")).unwrap();
    std::fs::write(dir.join("yamls/p.yaml"), b"p").unwrap();
    let log = logged(w.import_jobs(true)).await;
    let a = read_status(&w, &id('a')).await.unwrap();
    assert_eq!(a["status"], "failed");
    assert!(a["error"].as_str().unwrap().contains("restarted"), "{a}");
    assert!(log.contains("state=\"running\"") || log.contains("state=running"), "{log}");
    assert!(!dir.exists(), "its copies cleared");
    assert_eq!(read_status(&w, &id('b')).await.unwrap()["status"], "running");
}

/// A manifest world has no job folder of legacy inputs, yet a restart fails its job at
/// once, as it does a legacy one: every job makes its folder when it is queued (#119).
#[tokio::test]
async fn a_manifest_job_left_by_a_restart_fails_at_once() {
    let root = temp("left-manifest-job");
    let w = Arc::new(worlds(&root, Limits::DESIGN));
    let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let insts = fixture_instances();
    let yaml = player_yaml(&w, "Ann", "Fixture Game", &insts[0]).await;
    let req = json!({"manifest": manifest_id, "yamls": [yaml["id"]]}).to_string();
    // The one generation permit is taken, so the job waits queued, as one a restart cuts.
    let _held = w.one_gen.acquire().await.unwrap();
    let queued = create_world(State(Arc::clone(&w)), me(), Bytes::from(req)).await.unwrap();
    let id = json_of(queued).await["id"].as_str().unwrap().to_owned();
    // The restarted server: the same store and data folder, a new process.
    let mut next = worlds(&root, Limits::DESIGN);
    next.store = w.store.clone();
    next.import_jobs(true).await;
    let status = read_status(&next, &id).await.unwrap();
    assert_eq!(status["status"], "failed", "{status}");
    assert!(status["error"].as_str().unwrap().contains("restarted"), "{status}");
    assert!(!root.join("data/worlds").join(&id).exists(), "its folder cleared");
}

/// The last status write is tried again while the store is down, so a finished job does
/// not stay `running` for the sweep to fail (#119).
#[tokio::test(start_paused = true)]
async fn a_failed_final_status_write_is_tried_again() {
    let root = temp("final-write");
    let w = Arc::new(worlds(&root, Limits::DESIGN));
    let id = "e".repeat(64);
    let queued = json!({"id": id, "status": "queued", "engine": ENGINE_DATA_ONLY});
    w.save_status(&queued).await.unwrap();
    // The "running" write fails, and so do two tries at the last one.
    mem_store(&w).fail_job_puts_for_tests(3);
    let slot = w.admit().unwrap();
    run_generation(&w, &root, queued, slot).await;
    let status = read_status(&w, &id).await.unwrap();
    assert_eq!(status["status"], "failed", "the job's real end is stored: {status}");
    // A store that stays down is given up on, said as an error and not hidden.
    mem_store(&w).fail_job_puts_for_tests(usize::MAX);
    assert!(w.save_final(&json!({"id": id, "status": "done"})).await.is_err());
}

/// A status file an older server kept never replaces a row the store already has (#119).
#[tokio::test]
async fn an_old_status_file_never_replaces_a_newer_row() {
    let root = temp("old-status-newer-row");
    let w = worlds(&root, Limits::DESIGN);
    let id = "a".repeat(64);
    let row = json!({"id": id, "status": "done", "slots": 2});
    w.save_status(&row).await.unwrap();
    let dir = root.join("data/worlds").join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("status.json"), json!({"id": id, "status": "running"}).to_string())
        .unwrap();
    w.import_jobs(true).await;
    assert_eq!(read_status(&w, &id).await.unwrap(), row);
    assert!(!dir.exists(), "the file is dropped all the same");
}

/// A stored upload whose bytes were torn is not answered as stored, so the same upload
/// again stores it, which heals it, as a manifest is.
#[tokio::test]
async fn a_torn_upload_is_not_answered_as_stored_and_a_put_heals_it() {
    let root = temp("upload-heal");
    let w = worlds(&root, Limits::DESIGN);
    let meta = stored(&w, Kind::Apworld, b"zip", json!({"module": "m"})).await;
    let id = meta["id"].as_str().unwrap().to_owned();
    assert!(w.intact_upload(Kind::Apworld, &id).await.unwrap().is_some());
    let key = manifests::key_of(&id).unwrap();
    mem_store(&w).tear_upload_for_tests(log::UploadKind::Apworld, &key);
    assert_eq!(w.intact_upload(Kind::Apworld, &id).await.unwrap(), None, "torn");
    stored(&w, Kind::Apworld, b"zip", json!({"module": "m"})).await;
    assert!(w.intact_upload(Kind::Apworld, &id).await.unwrap().is_some(), "healed");
    assert_eq!(w.upload_file(Kind::Apworld, &id).await.unwrap(), b"zip");
}

#[test]
fn a_game_name_becomes_a_safe_short_folder_name() {
    assert_eq!(game_slug("../../x"), "x");
    assert_eq!(game_slug("Majora's Mask"), "majora-s-mask");
    assert_eq!(game_slug(".."), UNKNOWN_GAME);
    assert_eq!(game_slug(""), UNKNOWN_GAME);
    assert_eq!(game_slug("a/b\\c.d"), "a-b-c-d");
    assert_eq!(game_slug(&"x".repeat(500)).len(), MAX_SLUG);
    for game in ["../../etc", "/abs", "a\0b", "é", "..\\.."] {
        let s = game_slug(game);
        assert!(!s.is_empty() && s.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-')));
        assert!(!s.starts_with('-'), "{s}");
    }
}

/// A record of `kind` naming `games`, shaped as that kind's inspect result.
fn record(kind: Kind, games: &[&str]) -> Value {
    match kind {
        Kind::Apworld => json!({"game": games.first()}),
        Kind::Yaml => {
            json!({"players": games.iter().map(|g| json!({"game": g})).collect::<Vec<_>>()})
        }
    }
}

/// Every kind in the table: its game folder is safe, and it is the same rule for all.
#[test]
fn every_kind_stores_under_a_safe_game_folder_and_several_games_go_under_mixed() {
    for (i, kind) in [Kind::Apworld, Kind::Yaml].into_iter().enumerate() {
        assert_eq!(kind as usize, i, "Kind must index KINDS");
        assert_eq!(game_folder(kind, &record(kind, &["Clique"])), "clique");
        assert_eq!(game_folder(kind, &record(kind, &[])), UNKNOWN_GAME);
        assert_eq!(game_folder(kind, &record(kind, &["../../x"])), "x");
        for hostile in ["..", "/abs", "a/../b", "", "\\..\\x", "a\0b"] {
            let folder = game_folder(kind, &record(kind, &[hostile]));
            let path = Path::new("/data").join(kind.folder()).join(&folder);
            assert!(path.starts_with(format!("/data/{}", kind.folder())), "{hostile}: {path:?}");
            assert_eq!(path.components().count(), 4, "{hostile}: {path:?}");
        }
    }
    assert_eq!(KINDS.len(), 2);
    let yaml = |games: &[&str]| game_folder(Kind::Yaml, &record(Kind::Yaml, games));
    assert_eq!(yaml(&["Clique", "Clique"]), "clique");
    assert_eq!(yaml(&["Clique", "Other"]), MIXED);
}

/// Uploads an older server kept as files, by game and flat, move into the store: found by
/// id, listed, and their files readable. In memory the files stay, to move again on the next
/// start (#119).
#[tokio::test]
async fn old_upload_files_by_game_or_flat_move_into_the_store() {
    let root = temp("by-game");
    let flat = old_file(&root, "yamls", "yaml", b"flat", json!({"name": "flat"}));
    let grouped = old_file(&root, "yamls/clique", "yaml", b"g1", json!({"name": "g1"}));
    let same_game = old_file(&root, "yamls/clique", "yaml", b"g2", json!({"name": "g2"}));
    let world = old_file(&root, "apworlds/clique", "apworld", b"zip", json!({"module": "m"}));
    let w = worlds(&root, Limits::DESIGN);
    w.import_files(false).await;
    let ids = json!({ "yamls": [flat["id"], grouped["id"], same_game["id"]] });
    let got = chosen(&w, &ids, "yamls", Kind::Yaml).await.unwrap();
    assert_eq!(got, [flat.clone(), grouped.clone(), same_game.clone()]);
    let listed = json_of(list(&w, Kind::Yaml).await.unwrap()).await;
    let mut want = vec![flat, grouped.clone(), same_game];
    want.sort_by_key(|v| v["name"].as_str().unwrap().to_owned());
    assert_eq!(listed, Value::Array(want));
    assert_eq!(w.upload_file(Kind::Apworld, world["id"].as_str().unwrap()).await.unwrap(), b"zip");
    assert!(
        root.join("data/yamls/clique")
            .join(format!("{}.yaml", grouped["id"].as_str().unwrap()))
            .is_file()
    );
}

/// With a durable store each moved file is removed, then the folders left empty; a file
/// whose bytes are not its id's stays where it was, and nothing of it is stored.
#[tokio::test]
async fn a_moved_old_file_is_removed_and_a_damaged_one_stays() {
    let root = temp("move");
    let good = old_file(&root, "yamls/clique", "yaml", b"good", json!({"name": "good"}));
    let bad = old_file(&root, "apworlds", "apworld", b"bad", json!({"module": "m"}));
    let bad_id = bad["id"].as_str().unwrap();
    std::fs::write(root.join(format!("data/apworlds/{bad_id}.apworld")), b"torn").unwrap();
    let w = worlds(&root, Limits::DESIGN);
    w.import_files(true).await;
    assert!(w.upload_meta(Kind::Yaml, good["id"].as_str().unwrap()).await.unwrap().is_some());
    assert!(!root.join("data/yamls").exists(), "moved, and the emptied folders removed");
    assert!(w.upload_meta(Kind::Apworld, bad_id).await.unwrap().is_none());
    assert!(root.join(format!("data/apworlds/{bad_id}.json")).is_file(), "kept to look at");
}

/// A record whose file went (a remove stopped halfway) is removed on the next start once
/// its upload is stored; one whose upload is not stored stays, with a warning.
#[tokio::test]
async fn a_record_left_without_its_file_is_finished_on_the_next_start() {
    let root = temp("half-moved");
    let done = old_file(&root, "apworlds", "apworld", b"done", json!({"module": "d"}));
    let w = worlds(&root, Limits::DESIGN);
    w.import_files(true).await;
    // As if the remove had stopped after the file: each record back without its file.
    let lost = old_file(&root, "apworlds", "apworld", b"lost", json!({"module": "l"}));
    old_file(&root, "apworlds", "apworld", b"done", json!({"module": "d"}));
    let path = |m: &Value, ext: &str| {
        root.join(format!("data/apworlds/{}.{ext}", m["id"].as_str().unwrap()))
    };
    for m in [&done, &lost] {
        std::fs::remove_file(path(m, "apworld")).unwrap();
    }
    w.import_files(true).await;
    assert!(!path(&done, "json").exists(), "stored, so only the record was left to go");
    assert!(path(&lost, "json").is_file(), "its upload is not stored: kept to look at");
}

/// The test world with a game name and version, as an uploaded manifest.
fn manifest(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let doc: Value =
        serde_json::from_str(include_str!("../../../data-fill/tests/fixtures/room_world.json"))
            .unwrap();
    let mut m = doc["manifest"].clone();
    m["game"] = json!("Fixture Game");
    m["version"] = json!("1.2");
    edit(&mut m);
    m.to_string().into_bytes()
}

/// A manifest is checked by the engine in a child (here in process) and stored in the
/// database by content hash, with the listing columns beside it.
#[tokio::test]
async fn a_manifest_is_checked_stored_in_the_database_and_listed_in_pages() {
    let root = temp("manifest");
    let w = worlds(&root, Limits { list_page_max: 1, ..Limits::DESIGN });
    let body = manifest(|_| {});
    let r = upload_manifest(&w, Some("name=world.json"), &body).await.unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let meta = json_of(r).await;
    assert_eq!(meta["id"], content_id(&body));
    assert_eq!((&meta["game"], &meta["version"]), (&json!("Fixture Game"), &json!("1.2")));
    assert_eq!(
        (&meta["name"], &meta["items"], &meta["locations"]),
        (&json!("world.json"), &json!(4), &json!(6))
    );
    assert!(!root.join("data/manifests").exists(), "no file: it is in the store");
    let again = upload_manifest(&w, Some("name=other.json"), &body).await.unwrap();
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(json_of(again).await, meta, "the stored record, not a second copy");
    let other = manifest(|m| m["version"] = json!("2"));
    upload_manifest(&w, Some("name=v2.json"), &other).await.unwrap();
    let w = Arc::new(w);
    let first = json_of(
        list_manifests(State(Arc::clone(&w)), RawQuery(Some("limit=50".into()))).await.unwrap(),
    )
    .await;
    assert_eq!(first["items"].as_array().map(Vec::len), Some(1), "capped at list_page_max");
    let cursor = first["next"].as_str().unwrap();
    let second = json_of(
        list_manifests(State(Arc::clone(&w)), RawQuery(Some(format!("after={cursor}"))))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(second["items"].as_array().map(Vec::len), Some(1));
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    let bad = list_manifests(State(w), RawQuery(Some("after=nope".into()))).await.unwrap_err();
    assert_eq!(bad.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_manifest_that_is_not_json_has_no_game_or_does_not_compile_is_refused() {
    let root = temp("manifest-bad");
    let w = worlds(&root, Limits::DESIGN);
    let bad: [(&str, Vec<u8>); 7] = [
        ("game with a NUL", manifest(|m| m["game"] = json!("a\u{0}b"))),
        ("game past the cap", manifest(|m| m["game"] = json!("x".repeat(129)))),
        ("not json", b"{".to_vec()),
        ("no game", manifest(|m| drop(m.as_object_mut().unwrap().remove("game")))),
        ("empty game", manifest(|m| m["game"] = json!(" "))),
        ("no version", manifest(|m| drop(m.as_object_mut().unwrap().remove("version")))),
        ("no regions", manifest(|m| drop(m.as_object_mut().unwrap().remove("regions")))),
    ];
    for (why, body) in bad {
        let e = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap_err();
        assert_eq!(e.0, StatusCode::UNPROCESSABLE_ENTITY, "{why}: {}", e.1);
    }
    let none = w.manifests.list(None, None, None).await.unwrap();
    assert!(none.0.is_empty(), "nothing kept");
}

/// A stored manifest whose bytes were torn is not answered as stored: the same upload again
/// checks and stores it, which heals it.
#[tokio::test]
async fn uploading_a_torn_manifest_again_heals_it() {
    let root = temp("manifest-heal");
    let store = Arc::new(log::MemLogStore::new());
    let mut w = worlds(&root, Limits::DESIGN);
    w.manifests = Manifests::new(store::Db::Mem(Arc::clone(&store)), &Limits::DESIGN);
    let body = manifest(|_| {});
    let first = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let id = content_id(&body);
    store.tear_manifest_for_tests(&hex_key(&id));
    assert!(w.manifests.bytes(&id).await.is_err(), "the torn row is refused on read");
    let again = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    assert_eq!(again.status(), StatusCode::CREATED, "stored again, not answered as stored");
    assert_eq!(w.manifests.bytes(&id).await.unwrap().as_deref(), Some(&body));
    let third = upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    assert_eq!(third.status(), StatusCode::OK);
}

fn hex_key(id: &str) -> [u8; 32] {
    manifests::key_of(id).unwrap()
}

#[tokio::test]
async fn a_manifest_over_its_cap_is_refused_and_the_cap_itself_is_taken() {
    let root = temp("manifest-cap");
    let w = worlds(&root, Limits { manifest_max_bytes: 8, ..Limits::DESIGN });
    let stored = w
        .manifests
        .put("m.json", b"12345678", &json!({"game": "G", "version": "1"}))
        .await
        .unwrap();
    let r = upload_manifest(&w, Some("name=m.json"), b"12345678").await.unwrap();
    assert_eq!((r.status(), json_of(r).await), (StatusCode::OK, stored));
    let e = upload_manifest(&w, Some("name=m.json"), b"123456789").await.unwrap_err();
    assert_eq!(e.0, StatusCode::PAYLOAD_TOO_LARGE);
}

/// Checks are child processes: past `manifest_check_concurrency` the answer is "busy,
/// retry later", never an unbounded spawn.
#[tokio::test]
async fn manifest_checks_past_their_concurrency_are_refused_as_busy() {
    let root = temp("manifest-busy");
    let w = worlds(&root, Limits { manifest_check_concurrency: 1, ..Limits::DESIGN });
    let held = w.checks.try_acquire().unwrap();
    let r = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(r.headers().contains_key(header::RETRY_AFTER));
    drop(held);
    let r = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
}

/// One client address may write only `admin_write_burst` times in a burst; reads are free.
#[tokio::test]
async fn an_address_that_uploads_past_its_burst_gets_429_and_reads_still_work() {
    use tower::ServiceExt;
    let root = temp("write-rate");
    let limits =
        Limits { admin_write_burst: 2, admin_write_refill_milli_per_sec: 1, ..Limits::DESIGN };
    let app = routes(Arc::new(worlds(&root, limits)));
    let cookie = signed_up(&app, "ann").await;
    let call = |method: &str, path: &str| {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, &cookie)
            .body(axum::body::Body::from("x"))
            .unwrap();
        app.clone().oneshot(req)
    };
    for _ in 0..2 {
        let r = call("PUT", "/api/manifests?name=m.json").await.unwrap();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY, "not JSON, but allowed in");
    }
    let r = call("PUT", "/api/manifests?name=m.json").await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers().contains_key(header::RETRY_AFTER));
    assert_eq!(call("GET", "/api/manifests").await.unwrap().status(), StatusCode::OK);
}

/// A player file as the sidecar's inspect records it: one player with a game.
async fn player_yaml(w: &Worlds, name: &str, game: &str, doc: &Value) -> Value {
    let mut doc = doc.clone();
    doc["name"] = json!(name);
    doc["game"] = json!(game);
    let meta = json!({"name": name, "players": [{"name": name, "game": game}]});
    stored(w, Kind::Yaml, doc.to_string().as_bytes(), meta).await
}

fn fixture_instances() -> Vec<Value> {
    let doc: Value =
        serde_json::from_str(include_str!("../../../data-fill/tests/fixtures/room_world.json"))
            .unwrap();
    doc["instances"].as_array().unwrap().clone()
}

/// The API's state with a host listing rooms under `root/rooms`, as `--rooms` does.
async fn hosting(root: &Path) -> Arc<Worlds> {
    hosting_with(root, Limits::DESIGN).await
}

async fn hosting_with(root: &Path, limits: Limits) -> Arc<Worlds> {
    hosting_on(root, limits, JobRunner::InProcess, None).await
}

/// [`hosting_with`], its data-only jobs run by `jobs` and its fills by `pool` when given.
async fn hosting_on(
    root: &Path,
    limits: Limits,
    jobs: JobRunner,
    pool: Option<Pool>,
) -> Arc<Worlds> {
    let rooms = root.join("rooms");
    std::fs::create_dir_all(&rooms).unwrap();
    let (_, mut host) = crate::app(&crate::Source::Rooms(rooms), None, None, limits).await.unwrap();
    let (waker, folder, db) = (host.waker(), host.folder().unwrap(), host.db());
    tokio::spawn(async move { host.supervise().await });
    let mut w = worlds(root, limits);
    w.jobs = jobs;
    w.pool = pool;
    w.importer = Some(waker);
    w.rooms = Some((folder, "host:1".to_owned()));
    w.store = db.clone();
    w.accounts = Some(Arc::new(PasswordAuth::new(db.clone(), limits, None)));
    w.manifests = Manifests::new(db, &limits);
    Arc::new(w)
}

/// A world `who` asked for, generated by the engine and hosted as a listed room; its id.
async fn finished_world(w: &Arc<Worlds>, who: &Identity) -> String {
    let up = upload_manifest(w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let insts = fixture_instances();
    let yaml = player_yaml(w, "Ann", "Fixture Game", &insts[0]).await["id"].clone();
    let req = json!({"manifest": manifest_id, "yamls": [yaml]});
    let queued = create_world(State(Arc::clone(w)), who.clone(), Bytes::from(req.to_string()));
    let id = json_of(queued.await.unwrap()).await["id"].as_str().unwrap().to_owned();
    for _ in 0..600 {
        let status = read_status(w, &id).await.unwrap();
        if status["status"] == "done" && hosted(w, &status).is_some() {
            return id;
        }
        assert!(status["status"] != "failed", "{status}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the world never finished and listed");
}

/// A generation asks for one thread: the product's default (a caller of the fill job may
/// still ask for more). Fails with the old four.
#[test]
fn a_generation_asks_the_fill_for_one_worker() {
    let head = fill_head("s", &[], (1, 64)).unwrap();
    let req: Value = serde_json::from_slice(&head).unwrap();
    assert_eq!(req["workers"], 1);
    assert_eq!(config::GENERATION_WORKERS, 1);
}

/// The wiring: an uploaded manifest and two player files, a queued world, the engine's fill
/// in a child (here in process), the seed stored in the host and its room listed and loaded.
/// Fails without `generate_data_only` (the world fails: it would run the legacy path).
#[tokio::test]
async fn a_manifest_world_is_generated_by_the_engine_and_hosted_as_a_room() {
    let root = temp("data-only");
    let w = hosting(&root).await;

    let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let insts = fixture_instances();
    let mut yamls = Vec::new();
    for (k, n) in ["Ann", "Bo"].iter().enumerate() {
        yamls.push(player_yaml(&w, n, "Fixture Game", &insts[k]).await["id"].clone());
    }
    let req = json!({"manifest": manifest_id, "yamls": yamls});
    let queued =
        create_world(State(Arc::clone(&w)), me(), Bytes::from(req.to_string())).await.unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let id = json_of(queued).await["id"].as_str().unwrap().to_owned();
    let mut status = read_status(&w, &id).await.unwrap();
    assert_eq!(status["engine"], ENGINE_DATA_ONLY);
    for _ in 0..600 {
        status = read_status(&w, &id).await.unwrap();
        if status["status"] != "queued" && status["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status["status"], "done", "{status}");
    assert_eq!(status["slots"], 2);
    assert_eq!(status["generation"]["engine"], ENGINE_DATA_ONLY);
    let (folder, key, address) = hosted(&w, &status).expect("the room is listed");
    assert_eq!(address, key.address("host:1"));
    let (room, _visit) = folder.enter(key).await.unwrap();
    let summary = room.summary().await.unwrap();
    assert_eq!(summary["slots"].as_array().map(Vec::len), Some(2), "{summary}");
    assert_eq!(w.queued.load(Ordering::Acquire), 0, "the job's slot is released");
    // A manifest world takes no legacy world files.
    let both = json!({"manifest": manifest_id, "yamls": yamls, "apworlds": ["x"]});
    let e = create_world(State(w), me(), Bytes::from(both.to_string())).await.unwrap_err();
    assert_eq!(e.0, StatusCode::BAD_REQUEST);
}

/// The upload keeps what its check compiled, and the generation fills from that: one
/// `prepare` for the world, then only `fill`s. Fails without the stored prepared world.
#[tokio::test]
async fn an_upload_stores_the_prepared_world_and_generations_fill_from_it() {
    let root = temp("prepared");
    let w = hosting(&root).await;
    finished_world(&w, &me()).await;
    let other = accounts::Identity { subject: "pw:bo".into(), name: "bo".into() };
    finished_world(&w, &other).await;
    assert_eq!(*w.ran.lock().unwrap(), ["prepare", "fill", "fill"], "compiled once");
    let file = w.manifests.prepared(&content_id(&manifest(|_| {}))).await.unwrap().unwrap();
    let world = data_fill::prepared::Prepared::open(&file).unwrap();
    assert_eq!(world.game(), "Fixture Game");
}

/// No pool unless config asks for one: by default a generation's fill is a job in a
/// process of its own, and `fill_pool_workers` turns the pool on.
#[tokio::test]
async fn fills_go_to_a_pool_only_when_config_asks_for_workers() {
    let root = temp("pool-choice");
    let paths = || crate::sandbox::tests::layout(&root);
    assert_eq!(Limits::DESIGN.fill_pool_workers, 0);
    assert!(open(&root.join("data"), paths(), Limits::DESIGN).unwrap().pool.is_none());
    let on = Limits { fill_pool_workers: 1, ..Limits::DESIGN };
    assert!(open(&root.join("data"), paths(), on).unwrap().pool.is_some());
}

/// The pool is told the threads the request itself asks for: its CPU bound for a worker
/// rests on that number.
#[test]
fn the_pool_is_told_the_threads_the_fill_request_asks_for() {
    let head = fill_head("s", &[], (1, 64)).unwrap();
    let req: Value = serde_json::from_slice(&head).unwrap();
    assert_eq!(req["workers"], fill_threads());
}

/// A finished generation's status tells its requester no timings (how long the fill job
/// took would say whether its world was already loaded in a worker, that is, whether
/// someone filled it lately). The server keeps the phase timings for its own log.
#[tokio::test]
async fn a_generations_timings_go_to_the_log_and_not_into_its_status() {
    let root = temp("laps");
    let w = hosting(&root).await;
    let id = finished_world(&w, &me()).await;
    let status = read_status(&w, &id).await.unwrap();
    let told: Vec<&str> =
        status["generation"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(told, ["engine", "players", "rng_seed"], "{status}");
    assert!(!status.to_string().contains("\"ms\""), "{status}");
    let laps = w.laps.lock().unwrap().clone();
    let phases: Vec<&str> = laps.keys().map(String::as_str).collect();
    assert_eq!(phases, ["job", "open", "publish", "request", "store"]);
    assert!(laps.values().all(|v| v.as_f64().is_some_and(|ms| ms >= 0.0)), "{laps:?}");
}

/// The seed a generation stores is, byte for byte, the seed the path before stored for the
/// same inputs: the manifest compiled and filled, the bundle written as JSON and imported.
/// So are its hash and its package. Fails if the fill's own seed file differs anywhere.
#[tokio::test]
async fn the_stored_seed_is_the_one_the_json_import_path_stored() {
    let root = temp("prepared-bytes");
    let w = hosting(&root).await;
    let id = finished_world(&w, &me()).await;
    let status = read_status(&w, &id).await.unwrap();
    let (name, rng) = (status["seed_name"].as_str().unwrap(), &status["generation"]["rng_seed"]);
    let store::Db::Mem(mem) = &w.store else { panic!("the test host is in memory") };
    let stored = log::LogStore::seed_get(&**mem, crate::room_id(name)).await.unwrap().unwrap();

    let world: Value = serde_json::from_slice(&manifest(|_| {})).unwrap();
    let ann = json!({"name": "Ann", "instance": fixture_instances()[0]});
    let players = [serde_json::from_value(ann).unwrap()];
    let bundle =
        data_fill::seed::generate(&world, name, &players, rng.as_u64().unwrap(), 1).unwrap();
    let old = parse_bundle(&serde_json::to_vec(&bundle).unwrap(), &Limits::DESIGN).unwrap();
    assert_eq!(stored, old.bundle.bytes());
    assert_eq!(status["bundle_hash"], crate::hex(&old.hash), "{status}");
    for package in &old.packages {
        let key = log::blob_key(package);
        assert_eq!(w.store.blob_get(&key).await.unwrap().as_ref(), Some(package));
    }
    // A seed cut short is refused (its body hash no longer matches).
    let cut = stored[..stored.len() - 1].to_vec();
    assert!(crate::parse_seed(cut, old.packages.clone(), &Limits::DESIGN).is_err());
}

/// A world prepared by another build is stale: the generation sees no prepared world for
/// its own build, prepares the stored manifest again and stores that. So a fix to the
/// compiler reaches every stored world with the deploy that carries it.
#[tokio::test]
async fn a_world_prepared_by_another_build_is_prepared_again() {
    let root = temp("prepared-stale");
    let mut w = hosting(&root).await;
    let body = manifest(|_| {});
    upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    // A new build is deployed: it is no longer the one the world was stored under.
    Arc::get_mut(&mut w).unwrap().manifests.engine = manifests::engine_key("9.9.9+another");
    let id = content_id(&body);
    assert!(w.manifests.prepared(&id).await.unwrap().is_none(), "stale: not this build's");
    finished_world(&w, &me()).await;
    assert_eq!(*w.ran.lock().unwrap(), ["prepare", "prepare", "fill"]);
    assert!(w.manifests.prepared(&id).await.unwrap().is_some(), "stored for this build");
}

/// A stored world the fill cannot load (here bytes that are no prepared world, stored
/// whole under their own hash) does not fail its manifest's generations for good: the
/// generation prepares the manifest again, stores that and fills. Fails without the retry
/// (the world ends as failed).
#[tokio::test]
async fn a_stored_world_the_fill_cannot_load_is_prepared_again() {
    let root = temp("prepared-unloadable");
    let w = hosting(&root).await;
    let body = manifest(|_| {});
    upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    let id = content_id(&body);
    w.manifests.put_prepared(&id, b"not a prepared world".to_vec()).await.unwrap();
    finished_world(&w, &me()).await;
    assert_eq!(*w.ran.lock().unwrap(), ["prepare", "fill", "prepare", "fill"]);
    let file = w.manifests.prepared(&id).await.unwrap().unwrap();
    assert!(data_fill::prepared::Prepared::open(&file).is_ok(), "the stored one loads now");
}

/// A prepared world whose stored bytes were torn (they no longer hash to their key) is
/// refused by the store, prepared again from the manifest, and the put heals the blob.
#[tokio::test]
async fn a_prepared_world_whose_blob_is_torn_is_prepared_again() {
    let root = temp("prepared-torn");
    let mut w = hosting(&root).await;
    let body = manifest(|_| {});
    upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    let id = content_id(&body);
    let engine = w.manifests.engine.clone();
    let key = w.store.prepared_key(&hex_key(&id), &engine).await.unwrap().unwrap();
    let store::Db::Mem(mem) = w.store.clone() else { panic!("the test host is in memory") };
    mem.tear_blob_for_tests(&key);
    // Another server instance: nothing of the world is in its memory.
    Arc::get_mut(&mut w).unwrap().manifests =
        Manifests::new(store::Db::Mem(Arc::clone(&mem)), &Limits::DESIGN);
    assert!(w.store.blob_get(&key).await.is_err(), "the store refuses the torn blob");
    assert!(w.manifests.prepared(&id).await.unwrap().is_none(), "so it reads as not prepared");
    finished_world(&w, &me()).await;
    assert_eq!(*w.ran.lock().unwrap(), ["prepare", "prepare", "fill"]);
    assert!(w.store.blob_get(&key).await.unwrap().is_some(), "healed by the put");
}

/// A prepared world whose blob is not in the store is prepared again from the manifest.
#[tokio::test]
async fn a_prepared_world_whose_blob_is_missing_is_prepared_again() {
    let root = temp("prepared-missing");
    let w = hosting(&root).await;
    let body = manifest(|_| {});
    upload_manifest(&w, Some("name=m.json"), &body).await.unwrap();
    let id = content_id(&body);
    let engine = w.manifests.engine.clone();
    w.store.prepared_put(&hex_key(&id), &engine, &[9; 32]).await.unwrap();
    assert!(w.manifests.prepared(&id).await.unwrap().is_none(), "no such blob");
    finished_world(&w, &me()).await;
    assert_eq!(*w.ran.lock().unwrap(), ["prepare", "prepare", "fill"]);
    assert!(w.manifests.prepared(&id).await.unwrap().is_some(), "stored again");
}

/// A world made for an account is recorded as its room: its list has it, another account's
/// has not, and the admin's has it with its owner. Fails without `claim_room`.
#[tokio::test]
async fn a_room_made_for_an_account_is_listed_for_that_account_only() {
    let root = temp("owned-world");
    let w = hosting(&root).await;
    let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let insts = fixture_instances();
    let yaml = player_yaml(&w, "Ann", "Fixture Game", &insts[0]).await["id"].clone();
    let req = json!({"manifest": manifest_id, "yamls": [yaml]});
    let ann = accounts::Identity { subject: "pw:ann".into(), name: "ann".into() };
    let queued = create_world(State(Arc::clone(&w)), ann, Bytes::from(req.to_string())).await;
    let id = json_of(queued.unwrap()).await["id"].as_str().unwrap().to_owned();
    let mut status = read_status(&w, &id).await.unwrap();
    assert_eq!(status["owner"], "pw:ann");
    for _ in 0..600 {
        status = read_status(&w, &id).await.unwrap();
        if status["status"] != "queued" && status["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status["status"], "done", "{status}");
    let mine = w.store.rooms_owned(Some("pw:ann")).await.unwrap();
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0].world_id.as_deref(), Some(id.as_str()));
    assert_eq!(mine[0].owner.as_deref(), Some("pw:ann"));
    assert!(w.store.rooms_owned(Some("pw:bo")).await.unwrap().is_empty());
    assert_eq!(w.store.rooms_owned(None).await.unwrap().len(), 1);
}

/// Sign up on `app` and return the session cookie to send back.
async fn signed_up(app: &Router, name: &str) -> String {
    use tower::ServiceExt;
    let body = json!({"username": name, "password": "a long enough password"});
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/signup")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let c = r.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
    c.split(';').next().unwrap().to_owned()
}

async fn get_as(app: &Router, path: &str, auth: Option<(&str, &str)>) -> Response {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().uri(path);
    if let Some((name, value)) = auth {
        req = req.header(name, value);
    }
    app.clone().oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap()
}

fn room(n: u8) -> log::OwnedRoom {
    log::OwnedRoom {
        world_id: Some(format!("{n:064x}")),
        seed_name: format!("seed{n}"),
        room_key: format!("key{n}"),
        owner: None,
        age_secs: 0,
    }
}

async fn call(app: &Router, method: &str, path: &str, cookie: &str, body: &str) -> Response {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_owned()))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

/// Accounts ann, bob and cy on a router, with ann's room 1 and bob's room 2, and room 3
/// made before accounts (no owner).
async fn three_accounts() -> (Arc<Worlds>, Router, [String; 3]) {
    let w = Arc::new(worlds(&temp("room-roles"), Limits { signup_burst: 10, ..Limits::DESIGN }));
    let app = routes(Arc::clone(&w));
    let c =
        [signed_up(&app, "ann").await, signed_up(&app, "bob").await, signed_up(&app, "cyd").await];
    let db = &w.store;
    db.room_claim(proto::RoomId(1), Some("pw:ann"), &room(1)).await.unwrap();
    db.room_claim(proto::RoomId(2), Some("pw:bob"), &room(2)).await.unwrap();
    db.room_claim(proto::RoomId(3), None, &room(3)).await.unwrap();
    (w, app, c)
}

async fn names_of(r: Response) -> Vec<String> {
    assert_eq!(r.status(), StatusCode::OK);
    let v = json_of(r).await;
    v.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap().to_owned()).collect()
}

/// Each account lists its own rooms with its role; a room with no owner is listed to nobody;
/// a signed-out caller gets 401. Fails if the list ignores the caller.
#[tokio::test]
async fn an_account_lists_only_the_rooms_it_owns_or_administers_with_its_role() {
    let (_w, app, [ann, bob, cy]) = three_accounts().await;
    let as_ann = call(&app, "GET", "/api/rooms", &ann, "").await;
    let v = json_of(as_ann).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!((v[0]["name"].as_str(), v[0]["role"].as_str()), (Some("seed1"), Some("owner")));
    assert_eq!(v[0]["owner"], "ann", "the name, not the subject");
    assert_eq!(names_of(call(&app, "GET", "/api/rooms", &bob, "").await).await, ["seed2"]);
    assert!(names_of(call(&app, "GET", "/api/rooms", &cy, "").await).await.is_empty());
    let r = call(&app, "PUT", "/api/rooms/key2/admins/ann", &bob, "").await;
    assert_eq!(r.status(), StatusCode::OK);
    let v = json_of(call(&app, "GET", "/api/rooms", &ann, "").await).await;
    let roles: Vec<_> = v.as_array().unwrap().iter().map(|x| x["role"].as_str().unwrap()).collect();
    assert_eq!(roles.len(), 2);
    assert!(roles.contains(&"admin") && roles.contains(&"owner"));
    assert_eq!(get_as(&app, "/api/rooms", None).await.status(), StatusCode::UNAUTHORIZED);
}

fn account(name: &str) -> Identity {
    Identity { subject: format!("pw:{name}"), name: name.to_owned() }
}

/// A hosted world, its room's key, and the router, with accounts ann, bob and cyd signed up.
async fn hosted_for_ann(
    name: &str,
    limits: Limits,
) -> (Arc<Worlds>, Router, [String; 3], String, String) {
    let limits = Limits { signup_burst: 10, ..limits };
    let w = hosting_with(&temp(name), limits).await;
    let app = routes(Arc::clone(&w));
    let c =
        [signed_up(&app, "ann").await, signed_up(&app, "bob").await, signed_up(&app, "cyd").await];
    let id = finished_world(&w, &account("ann")).await;
    let status = json_of(call(&app, "GET", &format!("/api/worlds/{id}"), &c[0], "").await).await;
    let key = status["room_key"].as_str().unwrap().to_owned();
    (w, app, c, id, key)
}

/// A finished world's status, bundle, room and event routes answer its requester and the
/// admins of its room; another account gets 404, like for an unknown id; a room admin never
/// sees the owner's account id. Each route fails without `read_status_for`.
#[tokio::test]
async fn another_account_cannot_read_a_world_it_did_not_ask_for() {
    let (_w, app, [ann, bob, _], id, key) = hosted_for_ann("read-routes", Limits::DESIGN).await;
    let paths: Vec<String> = ["", "/bundle", "/room", "/room/events"]
        .iter()
        .map(|t| format!("/api/worlds/{id}{t}"))
        .collect();
    for path in &paths {
        assert_eq!(call(&app, "GET", path, &bob, "").await.status(), 404, "bob on {path}");
        assert_eq!(call(&app, "GET", path, &ann, "").await.status(), 200, "ann on {path}");
    }
    let r = call(&app, "PUT", &format!("/api/rooms/{key}/admins/bob"), &ann, "").await;
    assert_eq!(r.status(), StatusCode::OK);
    for path in &paths {
        assert_eq!(call(&app, "GET", path, &bob, "").await.status(), 200, "admin bob on {path}");
    }
    assert_eq!(json_of_call(&app, &paths[0], &ann).await["owner"], "pw:ann");
    assert!(
        json_of_call(&app, &paths[0], &bob).await.get("owner").is_none(),
        "an admin does not get the owner's id"
    );
}

async fn json_of_call(app: &Router, path: &str, cookie: &str) -> Value {
    json_of(call(app, "GET", path, cookie, "").await).await
}

/// Reads `r`'s body until the server ends it; fails after 5 s.
async fn ends(r: Response) {
    use futures_util::StreamExt;
    let mut body = r.into_body().into_data_stream();
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while body.next().await.is_some() {}
    });
    end.await.expect("the stream was ended by the server");
}

/// An open room stream ends soon after its viewer is removed as a room admin, and after its
/// account is deleted. Fails without `access_lost` (the stream stays open).
#[tokio::test]
async fn an_open_room_stream_ends_when_the_viewer_loses_access() {
    let limits = Limits { view_access_recheck_ms: 20, ..Limits::DESIGN };
    let (w, app, [ann, bob, cyd], id, key) = hosted_for_ann("stream-access", limits).await;
    for n in ["bob", "cyd"] {
        let r = call(&app, "PUT", &format!("/api/rooms/{key}/admins/{n}"), &ann, "").await;
        assert_eq!(r.status(), StatusCode::OK);
    }
    let events = format!("/api/worlds/{id}/room/events");
    let (bobs, cyds) =
        (call(&app, "GET", &events, &bob, "").await, call(&app, "GET", &events, &cyd, "").await);
    assert_eq!((bobs.status(), cyds.status()), (StatusCode::OK, StatusCode::OK));
    let r = call(&app, "DELETE", &format!("/api/rooms/{key}/admins/bob"), &ann, "").await;
    assert_eq!(r.status(), StatusCode::OK);
    ends(bobs).await;
    w.store.user_delete("cyd").await.unwrap();
    ends(cyds).await;
    let owners = call(&app, "GET", &events, &ann, "").await;
    assert_eq!(owners.status(), StatusCode::OK, "the owner keeps hers");
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// Open to everyone: no session needed.
    Public,
    /// Uploads: shared by every account until per-account trust exists (see #127).
    Global,
    /// Makes something owned by the caller; touches nobody else's data.
    Own,
    /// Reads or changes one room or world: only those with a role on it.
    Scoped,
    /// Answers 404 whatever it is asked.
    CatchAll,
    /// The game catalogue: every account reads it, only a game's owners change it.
    Catalogue,
}

/// Every `/api` route of the router and what it may show another account. A route the
/// router has and this table lacks fails `no_route_escapes_the_cross_account_walk`.
const ROUTES: &[(&str, &[&str], Class)] = &[
    ("/api/auth", &["GET"], Class::Public),
    ("/api/signup", &["POST"], Class::Public),
    ("/api/login", &["POST"], Class::Public),
    ("/api/logout", &["POST"], Class::Public),
    ("/api/me", &["GET"], Class::Public),
    ("/api/apworlds", &["GET", "PUT"], Class::Global),
    ("/api/yamls", &["GET", "PUT"], Class::Global),
    ("/api/manifests", &["GET", "PUT"], Class::Global),
    ("/api/rooms", &["GET"], Class::Own),
    ("/api/worlds", &["POST"], Class::Own),
    ("/api/worlds/{id}", &["GET"], Class::Scoped),
    ("/api/worlds/{id}/room", &["GET"], Class::Scoped),
    ("/api/worlds/{id}/bundle", &["GET"], Class::Scoped),
    ("/api/worlds/{id}/room/events", &["GET"], Class::Scoped),
    ("/api/rooms/{key}/admins", &["GET", "POST"], Class::Scoped),
    ("/api/rooms/{key}/admins/{name}", &["PUT", "DELETE"], Class::Scoped),
    ("/api/games", &["GET", "POST"], Class::Catalogue),
    ("/api/games/{game}", &["GET"], Class::Catalogue),
    ("/api/games/{game}/owners/{name}", &["PUT", "DELETE"], Class::Catalogue),
    ("/api/games/{game}/versions", &["POST"], Class::Catalogue),
    ("/api/games/{game}/versions/{version}/withdraw", &["POST"], Class::Catalogue),
    ("/api", &["ANY"], Class::CatchAll),
    ("/api/", &["ANY"], Class::CatchAll),
    ("/api/{*rest}", &["ANY"], Class::CatchAll),
];

/// Every `.rs` file under `dir`, with its non-test code: the tests module cut off, or left
/// out when it is a file of its own (`tests.rs`).
fn source_files(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs")
            && path.file_name().is_some_and(|n| n != "tests.rs")
        {
            let src = std::fs::read_to_string(&path).unwrap();
            let code = src.split("\n#[cfg(test)]\nmod tests").next().unwrap().to_owned();
            out.push((path, code));
        }
    }
}

/// The HTTP methods named in `seg` (the text of one `.route(..)` call after its path):
/// each `get(`, `post(`, ... is one; `any(` is every method (`ANY` in the table). Any other
/// way to pick methods (`on(`, `trace(`, `connect(`, `*_service(`) cannot be read here and
/// fails, as does a route that names none.
fn methods_in(seg: &str, path: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in seg.match_indices('(') {
        let word: String = seg[..at]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let known = ["get", "post", "put", "delete", "patch", "head", "options", "any"];
        if known.contains(&word.as_str()) {
            found.push(word.to_ascii_uppercase());
        }
        let unreadable = ["on", "trace", "connect"].contains(&word.as_str())
            || word.ends_with("_service")
            || word.starts_with("on_");
        assert!(
            !unreadable,
            "{path}: `{word}(` picks methods in a way this test cannot read; classify it by hand"
        );
    }
    assert!(!found.is_empty(), "{path}: no recognised method; classify it by hand");
    found
}

/// Every `(METHOD, path)` under `/api` that any non-test code of this crate registers, read
/// from all of `crates/server/src` so a route in a new module cannot be missed. A `.nest(`,
/// `.route_service(` or `.nest_service(` of an `/api` path cannot be read this way and fails.
fn registered_routes() -> Vec<(String, String)> {
    let mut files = Vec::new();
    source_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut found = Vec::new();
    for (file, code) in &files {
        for call in [".nest(", ".route_service(", ".nest_service("] {
            for part in code.split(call).skip(1) {
                let lit = part.trim_start().strip_prefix('"');
                let path = lit.unwrap_or_else(|| {
                    panic!("{}: {call} takes a path that is not a plain literal", file.display())
                });
                assert!(
                    !path.starts_with("/api"),
                    "{} registers an /api path with {call}: classify it by hand",
                    file.display()
                );
            }
        }
        for part in code.split(".route(").skip(1) {
            let lit = part.trim_start().strip_prefix('"').unwrap_or_else(|| {
                let at = part.lines().next().unwrap_or("");
                panic!("{}: a .route( path is not a plain string literal: {at}", file.display())
            });
            let (path, rest) = lit.split_once('"').unwrap();
            if !path.starts_with("/api") {
                continue;
            }
            let mut depth = 1_usize;
            let end = rest
                .char_indices()
                .find(|(_, c)| {
                    depth = match c {
                        '(' => depth + 1,
                        ')' => depth - 1,
                        _ => depth,
                    };
                    depth == 0
                })
                .map_or(rest.len(), |(i, _)| i);
            for m in methods_in(&rest[..end], path) {
                found.push((m, path.to_owned()));
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

#[test]
fn no_route_escapes_the_cross_account_walk() {
    let mut table: Vec<(String, String)> = ROUTES
        .iter()
        .flat_map(|(p, methods, _)| methods.iter().map(move |m| ((*m).to_owned(), (*p).to_owned())))
        .collect();
    table.sort();
    assert_eq!(
        registered_routes(),
        table,
        "classify every (method, path) in ROUTES, and drop gone ones"
    );
}

/// User B, with every route of the router and A's ids and keys, never gets A's data and
/// changes nothing of A's: scoped routes are 404, lists show only B's own, and A still has
/// everything afterwards. Fails if any scoped route skips the role check.
#[tokio::test]
async fn another_account_walking_every_route_never_reaches_the_first_ones_data() {
    let (w, app, [ann, bob, _], id, key) = hosted_for_ann("walk", Limits::DESIGN).await;
    let a_rooms = call(&app, "GET", "/api/rooms", &ann, "").await;
    let a_rooms = json_of(a_rooms).await;
    let a_admins = admins_of(&app, &key, &ann).await;
    let a_world = json_of_call(&app, &format!("/api/worlds/{id}"), &ann).await;
    let a_game = ann_game(&w, &app, &ann).await;
    for (template, methods, class) in ROUTES.iter().filter(|r| r.2 != Class::Public) {
        // `{name}` is the owner, and B himself (B tries to make himself an admin).
        for name in ["ann", "bob"] {
            let path = template
                .replace("{id}", &id)
                .replace("{key}", &key)
                .replace("{name}", name)
                .replace("{game}", "annsgame")
                .replace("{version}", "1");
            for method in *methods {
                let method = if *method == "ANY" { "GET" } else { method };
                let r = call(&app, method, &path, &bob, "{\"username\":\"bob\"}").await;
                let status = r.status();
                let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
                let body = String::from_utf8_lossy(&bytes).into_owned();
                let what = format!("{method} {path} ({class:?}): {status} {body}");
                match class {
                    Class::Scoped | Class::CatchAll => {
                        assert_eq!(status, StatusCode::NOT_FOUND, "{what}");
                    }
                    Class::Own => {
                        assert!(status != StatusCode::OK || !body.contains(&key), "{what}");
                        assert!(!body.contains(&id), "{what}");
                    }
                    // Uploads are global until #127. Open routes are skipped above (B's
                    // own logout would end B's session).
                    // B reads ann's game, and changes nothing of it.
                    Class::Catalogue if method != "GET" => {
                        assert!(!status.is_success(), "{what}");
                    }
                    Class::Catalogue | Class::Global | Class::Public => {}
                }
                assert!(!body.contains("pw:ann"), "A's account id leaked: {what}");
            }
        }
    }
    // B's own list has none of A's rooms, and nothing B did changed A's.
    let b_rooms = json_of_call(&app, "/api/rooms", &bob).await;
    assert_eq!(b_rooms, json!([]));
    assert_eq!(json_of_call(&app, "/api/rooms", &ann).await, a_rooms);
    assert_eq!(admins_of(&app, &key, &ann).await, a_admins);
    assert_eq!(json_of_call(&app, &format!("/api/worlds/{id}"), &ann).await, a_world);
    assert_eq!(json_of_call(&app, "/api/games/annsgame", &ann).await, a_game);
}

/// A router whose catalogue checks run `fake` in place of the sandbox, with generator
/// `gen-1` and `seeds` test seeds; ann is signed up and owns the game `ww`.
async fn checking(
    name: &str,
    seeds: usize,
    fake: crate::checks::FakeRunner,
) -> (Arc<Worlds>, Router, String) {
    checking_with(name, seeds, fake, Some("gen-1")).await
}

async fn checking_with(
    name: &str,
    seeds: usize,
    fake: crate::checks::FakeRunner,
    generator: Option<&str>,
) -> (Arc<Worlds>, Router, String) {
    let limits = Limits { signup_burst: 10, catalogue_check_seeds: seeds, ..Limits::DESIGN };
    let mut w = worlds(&temp(name), limits);
    w.generator = generator.map(str::to_owned);
    w.catalogue_checks.fake = Some(fake);
    let w = Arc::new(w);
    let app = routes(Arc::clone(&w));
    let ann = signed_up(&app, "ann").await;
    let r = call(&app, "POST", "/api/games", &ann, r#"{"name":"ww"}"#).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    (w, app, ann)
}

/// Post version `v` of `ww` from a stored `.apworld` of `body`.
async fn post_version(w: &Worlds, app: &Router, ann: &str, v: &str, body: &[u8]) {
    let up = stored(w, Kind::Apworld, body, json!({"name": "ww.apworld"})).await;
    let req = json!({"version": v, "apworld": up["id"]}).to_string();
    let r = call(app, "POST", "/api/games/ww/versions", ann, &req).await;
    assert_eq!(r.status(), StatusCode::CREATED);
}

/// Version `v` of `ww` once its check has ended (or after five seconds).
async fn checked(w: &Worlds, v: &str) -> log::GameVersion {
    for _ in 0..500 {
        let all = w.store.versions("ww").await.unwrap();
        let found = all.into_iter().find(|x| x.version == v).unwrap();
        if found.state != log::VersionState::Checking {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("version {v} is still checking");
}

/// A stand-in sandbox: the module is `ww`, the scan flags one line, and a test seed of an
/// `.apworld` whose bytes start with `bad` fails. Every step it ran is in the list.
fn fake_sandbox() -> (crate::checks::FakeRunner, Arc<std::sync::Mutex<Vec<String>>>) {
    use crate::checks::Step;
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&ran);
    let fake: crate::checks::FakeRunner = Arc::new(move |step: &Step, apworld: &[u8]| {
        seen.lock().unwrap().push(match step {
            Step::Inspect => "inspect".to_owned(),
            Step::Scan => "scan".to_owned(),
            Step::Seed { module, .. } => format!("seed {module}"),
        });
        match step {
            Step::Inspect if apworld.starts_with(b"not a zip") => {
                Err(Failure::Refused("not a zip file".to_owned()))
            }
            Step::Inspect => Ok(br#"{"module":"ww","game":"Wind Waker"}"#.to_vec()),
            Step::Scan => {
                Ok(br#"[{"file":"ww/__init__.py","line":1,"reason":"connects to the network"}]"#
                    .to_vec())
            }
            Step::Seed { .. } if apworld.starts_with(b"bad") => {
                Err(Failure::Refused("KeyError: 'Master Sword'".to_owned()))
            }
            Step::Seed { .. } => Ok(br#"{"report":{"slots":1}}"#.to_vec()),
        }
    });
    (fake, ran)
}

/// A version that passes every check goes live, checked with the server's generator and
/// its scan saved; one whose test seed fails is failed with which check and the error, and
/// the game's live version stays live. The scan runs before the seeds.
#[tokio::test]
async fn a_version_goes_live_only_when_every_check_passes() {
    let (fake, ran) = fake_sandbox();
    let (w, app, ann) = checking("checks-pass", 3, fake).await;
    post_version(&w, &app, &ann, "1", b"good world").await;
    let one = checked(&w, "1").await;
    assert_eq!(
        (one.state, one.generator.as_str(), one.error),
        (log::VersionState::Live, "gen-1", None)
    );
    assert_eq!(*ran.lock().unwrap(), ["inspect", "scan", "seed ww", "seed ww", "seed ww"]);
    let page = json_of_call(&app, "/api/games/ww", &ann).await;
    assert_eq!(page["versions"][0]["scan"][0]["reason"], json!("connects to the network"));

    ran.lock().unwrap().clear();
    post_version(&w, &app, &ann, "2", b"bad world").await;
    let two = checked(&w, "2").await;
    assert_eq!(two.state, log::VersionState::Failed);
    let error = two.error.unwrap();
    assert!(error.starts_with("check 3 (test seed 1 of 3, seed "), "{error}");
    assert!(error.ends_with("KeyError: 'Master Sword'"), "{error}");
    assert!(two.scan.is_some(), "the scan ran before the seeds");
    assert_eq!(*ran.lock().unwrap(), ["inspect", "scan", "seed ww"], "stops at the first");
    let live = w.store.version_latest_live("ww", "gen-1").await.unwrap();
    assert_eq!(live.map(|v| v.version).as_deref(), Some("1"), "the previous one stays live");
}

/// Check 1 refuses an `.apworld` over today's cap, check 2 one the sandbox refuses, and a
/// server that cannot tell its generator version fails every version (none goes live with
/// an unknown one). None of them gets to the test seeds.
#[tokio::test]
async fn size_inspection_and_an_unknown_generator_fail_before_any_seed() {
    let (fake, ran) = fake_sandbox();
    let (w, app, ann) = checking("checks-early", 3, Arc::clone(&fake)).await;
    post_version(&w, &app, &ann, "1", b"not a zip").await;
    let one = checked(&w, "1").await;
    assert_eq!(one.error.as_deref(), Some("check 2 (inspect): not a zip file"));

    let big = vec![b'x'; Limits::DESIGN.apworld_max_bytes + 1];
    post_version(&w, &app, &ann, "2", &big).await;
    let two = checked(&w, "2").await;
    let cap = Limits::DESIGN.apworld_max_bytes;
    let want = format!("check 1 (size): the .apworld is larger than {cap} bytes");
    assert_eq!(two.error.as_deref(), Some(want.as_str()));
    assert!(!ran.lock().unwrap().iter().any(|s| s.starts_with("seed")));

    let (w, app, ann) = checking_with("checks-no-gen", 3, fake, None).await;
    post_version(&w, &app, &ann, "1", b"good world").await;
    let one = checked(&w, "1").await;
    assert_eq!(one.state, log::VersionState::Failed);
    assert!(one.error.unwrap().contains("generator version is unknown"));
    assert!(!ran.lock().unwrap().iter().any(|s| s.starts_with("seed")));
}

/// A version a stopped process left checking is checked again at the next start; one that
/// is withdrawn meanwhile is not.
#[tokio::test]
async fn a_restart_runs_the_checks_a_stopped_process_left() {
    let (fake, ran) = fake_sandbox();
    let (w, _app, _ann) = checking("checks-resume", 2, fake).await;
    let up = stored(&w, Kind::Apworld, b"good world", json!({"name": "ww.apworld"})).await;
    for v in ["1", "2"] {
        // As a process that stopped before its checks ended left them.
        let new = log::NewVersion {
            game: "ww".to_owned(),
            version: v.to_owned(),
            note: String::new(),
            apworld: manifests::key_of(up["id"].as_str().unwrap()).unwrap(),
            generator: String::new(),
        };
        assert_eq!(w.store.version_add(&new).await.unwrap(), log::VersionAdded::Added);
    }
    let withdrawn = log::VersionState::Withdrawn;
    assert!(w.store.version_set("ww", "2", withdrawn, "", None).await.unwrap());
    crate::checks::resume(Arc::clone(&w)).await;
    assert_eq!(checked(&w, "1").await.state, log::VersionState::Live);
    assert_eq!(checked(&w, "2").await.state, withdrawn);
    assert_eq!(ran.lock().unwrap().len(), 4, "only version 1: inspect, scan, two seeds");
}

/// A test seed waits for a generation slot like a player's world: while the only slot is
/// taken, the version's inspection and scan run but no seed does.
#[tokio::test]
async fn a_test_seed_takes_its_turn_for_a_generation_slot() {
    let (fake, ran) = fake_sandbox();
    let (w, app, ann) = checking("checks-turn", 1, fake).await;
    let player = w.generation_turn().await.unwrap();
    post_version(&w, &app, &ann, "1", b"good world").await;
    for _ in 0..500 {
        if ran.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(*ran.lock().unwrap(), ["inspect", "scan"], "no seed while the slot is held");
    drop(player);
    assert_eq!(checked(&w, "1").await.state, log::VersionState::Live);
}

/// Ann's game `annsgame` with version 1 from a stored `.apworld`, once its check ended;
/// its page.
async fn ann_game(w: &Worlds, app: &Router, ann: &str) -> Value {
    let up = stored(w, Kind::Apworld, b"ann's world", json!({"name": "a.apworld"})).await;
    let r = call(app, "POST", "/api/games", ann, r#"{"name":"annsgame"}"#).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let v = json!({"version": "1", "apworld": up["id"]}).to_string();
    let r = call(app, "POST", "/api/games/annsgame/versions", ann, &v).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    // Its check ends at once here (no generator), so the page stays as it is from now on.
    for _ in 0..500 {
        let all = w.store.versions("annsgame").await.unwrap();
        if all.iter().all(|v| v.state != log::VersionState::Checking) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    json_of_call(app, "/api/games/annsgame", ann).await
}

/// The catalogue routes: make a game (its maker is its owner, a name taken ignoring case
/// is 409), list it, add and remove owners (never the last), post versions (one the game
/// has is 409, an unknown upload 400, a non-owner 403) and withdraw one, for good.
#[tokio::test]
async fn a_game_is_made_listed_owned_versioned_and_withdrawn() {
    let (w, app, [ann, bob, _]) = three_accounts().await;
    let up = stored(&w, Kind::Apworld, b"wind waker", json!({"name": "ww.apworld"})).await;
    let make = |name: &str| json!({"name": name, "description": " Sail. "}).to_string();
    let r = call(&app, "POST", "/api/games", &ann, &make(" Wind Waker ")).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let page = json_of(r).await;
    assert_eq!(
        (&page["name"], &page["description"], &page["owners"], &page["owner"]),
        (&json!("Wind Waker"), &json!("Sail."), &json!(["ann"]), &json!(true))
    );
    let taken = call(&app, "POST", "/api/games", &bob, &make("wind waker")).await;
    assert_eq!(taken.status(), StatusCode::CONFLICT);
    for bad in [
        r#"{"name":"  "}"#,
        r#"{"name":"a/b"}"#,
        r#"{"name":"a\u0000b"}"#,
        r#"{"name":".."}"#,
        "{}",
    ] {
        let r = call(&app, "POST", "/api/games", &bob, bad).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let long = "x".repeat(Limits::DESIGN.catalogue_name_max_bytes + 1);
    let r = call(&app, "POST", "/api/games", &bob, &make(&long)).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let r = call(&app, "POST", "/api/games", &bob, &make("Alpha")).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    // A page at a time, by name ignoring case.
    let first = json_of_call(&app, "/api/games?limit=1", &bob).await;
    assert_eq!((&first["items"][0]["name"], &first["next"]), (&json!("Alpha"), &json!("Alpha")));
    let second = json_of_call(&app, "/api/games?limit=1&after=Alpha", &bob).await;
    assert_eq!(
        (&second["items"][0]["name"], &second["next"]),
        (&json!("Wind Waker"), &Value::Null)
    );

    // Versions: only an owner posts one; bob reads the page but is no owner.
    let v = |n: &str, id: &Value| json!({"version": n, "note": "first", "apworld": id});
    let path = "/api/games/wind%20waker/versions";
    let r = call(&app, "POST", path, &bob, &v("1", &up["id"]).to_string()).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = call(&app, "POST", path, &ann, &v("1", &up["id"]).to_string()).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let page = json_of(r).await;
    assert_eq!(page["versions"][0]["state"], json!("checking"));
    assert_eq!(page["versions"][0]["apworld"], up["id"]);
    let again = call(&app, "POST", path, &ann, &v("1", &up["id"]).to_string()).await;
    assert_eq!(again.status(), StatusCode::CONFLICT, "a version number the game has");
    let unknown = json!(content_id(b"never uploaded"));
    let r = call(&app, "POST", path, &ann, &v("2", &unknown).to_string()).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "no such upload");
    let r = call(&app, "POST", path, &ann, &v("2", &json!("zz")).to_string()).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "not an id");
    let r =
        call(&app, "POST", "/api/games/nope/versions", &ann, &v("1", &up["id"]).to_string()).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let seen = json_of_call(&app, "/api/games/Wind%20Waker", &bob).await;
    assert_eq!((&seen["owner"], &seen["versions"][0]["version"]), (&json!(false), &json!("1")));

    // Owners: ann adds bob; neither may remove the last one.
    let owners = "/api/games/wind%20waker/owners";
    let r = call(&app, "PUT", &format!("{owners}/nobody"), &ann, "").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let r = call(&app, "PUT", &format!("{owners}/bob"), &ann, "").await;
    assert_eq!(json_of(r).await["owners"], json!(["ann", "bob"]));
    let r = call(&app, "DELETE", &format!("{owners}/ann"), &bob, "").await;
    assert_eq!(json_of(r).await["owners"], json!(["bob"]));
    let r = call(&app, "DELETE", &format!("{owners}/bob"), &bob, "").await;
    assert_eq!(r.status(), StatusCode::CONFLICT, "the last owner stays");
    let r = call(&app, "PUT", &format!("{owners}/ann"), &ann, "").await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "ann is no owner any more");

    // Withdraw: for good, and again changes nothing.
    let withdraw = "/api/games/wind%20waker/versions/1/withdraw";
    assert_eq!(call(&app, "POST", withdraw, &ann, "").await.status(), StatusCode::FORBIDDEN);
    let r = call(&app, "POST", withdraw, &bob, "").await;
    assert_eq!(json_of(r).await["versions"][0]["state"], json!("withdrawn"));
    let r = call(&app, "POST", withdraw, &bob, "").await;
    assert_eq!(json_of(r).await["versions"][0]["state"], json!("withdrawn"));
    let gone = "/api/games/wind%20waker/versions/9/withdraw";
    assert_eq!(call(&app, "POST", gone, &bob, "").await.status(), StatusCode::NOT_FOUND);
    let ok = w.store.version_set("wind waker", "1", log::VersionState::Live, "g", None);
    assert!(!ok.await.unwrap(), "a withdrawal is final");
}

/// A lookup that errors, a session that is gone and one that hangs all end a room stream;
/// only a clean yes keeps it. Fails open without `still_allowed` and `until_lost`.
#[tokio::test]
async fn a_room_stream_fails_closed_on_an_error_a_gone_session_or_a_hang() {
    let w = Arc::new(worlds(&temp("fail-closed"), Limits { signup_burst: 10, ..Limits::DESIGN }));
    let app = routes(Arc::clone(&w));
    let ann = signed_up(&app, "ann").await;
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_str(&ann).unwrap());
    let (good, bad) = (content_id(b"good"), content_id(b"bad"));
    w.save_status(&json!({"id": good, "status": "queued", "owner": "pw:ann"})).await.unwrap();
    w.store.job_put(&bad, JobState::Queued, "{not json").await.unwrap();
    assert!(still_allowed(&w, &headers, &good).await);
    assert!(!still_allowed(&w, &headers, &bad).await, "a store or row error is a no");
    assert!(!still_allowed(&w, &HeaderMap::new(), &good).await, "no session is a no");
    let every = Duration::from_millis(20);
    let five = Duration::from_secs(5);
    tokio::time::timeout(five, until_lost(every, || std::future::pending::<bool>()))
        .await
        .expect("a check that hangs ends the stream");
    let keeps =
        tokio::time::timeout(Duration::from_millis(200), until_lost(every, || async { true }));
    assert!(keeps.await.is_err(), "a clean yes keeps it open");
}

async fn admins_of(app: &Router, key: &str, cookie: &str) -> Value {
    json_of(call(app, "GET", &format!("/api/rooms/{key}/admins"), cookie, "").await).await
}

/// The owner and an admin both grant admin; a repeat changes nothing; an unknown name is
/// 404; the owner needs no grant. Fails without the grant routes.
#[tokio::test]
async fn an_owner_and_an_admin_can_grant_admin_and_a_repeat_changes_nothing() {
    let (_w, app, [ann, bob, _cy]) = three_accounts().await;
    assert_eq!(call(&app, "PUT", "/api/rooms/key1/admins/bob", &ann, "").await.status(), 200);
    assert_eq!(call(&app, "PUT", "/api/rooms/key1/admins/bob", &ann, "").await.status(), 200);
    assert_eq!(admins_of(&app, "key1", &ann).await["admins"], json!(["bob"]));
    // Bob, now an admin, adds cy by the POST form.
    let r = call(&app, "POST", "/api/rooms/key1/admins", &bob, "{\"username\":\"CYD\"}").await;
    assert_eq!(r.status(), StatusCode::OK);
    let v = admins_of(&app, "key1", &bob).await;
    assert_eq!(
        (v["admins"].clone(), v["owner"].as_str(), v["role"].as_str()),
        (json!(["bob", "cyd"]), Some("ann"), Some("admin"))
    );
    assert_eq!(call(&app, "PUT", "/api/rooms/key1/admins/ghost", &ann, "").await.status(), 404);
    assert_eq!(call(&app, "PUT", "/api/rooms/key1/admins/ann", &bob, "").await.status(), 200);
    assert_eq!(
        admins_of(&app, "key1", &ann).await["admins"],
        json!(["bob", "cyd"]),
        "no owner row"
    );
}

/// A caller with no role gets 404 for the room, the same as for a room that does not exist,
/// on every admins route. Fails if a non-member can tell the room is there.
#[tokio::test]
async fn a_non_member_gets_404_for_a_room_and_cannot_tell_it_exists() {
    let (_w, app, [_ann, _bob, cy]) = three_accounts().await;
    for (method, tail) in [("GET", ""), ("POST", ""), ("PUT", "/bob"), ("DELETE", "/bob")] {
        let body = "{\"username\":\"bob\"}";
        let real = call(&app, method, &format!("/api/rooms/key1/admins{tail}"), &cy, body).await;
        let none = call(&app, method, &format!("/api/rooms/nokey/admins{tail}"), &cy, body).await;
        assert_eq!((real.status(), none.status()), (StatusCode::NOT_FOUND, StatusCode::NOT_FOUND));
        let bytes = |r: Response| axum::body::to_bytes(r.into_body(), usize::MAX);
        assert_eq!(bytes(real).await.unwrap(), bytes(none).await.unwrap());
    }
    // The owner-less room belongs to nobody.
    let r = call(&app, "GET", "/api/rooms/key3/admins", &cy, "").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

/// The owner removes any admin; an admin removes only itself (403 for another), and leaving
/// takes its access (404 after); the owner cannot be removed (409); a non-admin is 404.
#[tokio::test]
async fn only_the_owner_removes_other_admins_and_the_owner_stays() {
    let (_w, app, [ann, bob, cy]) = three_accounts().await;
    for n in ["bob", "cyd"] {
        assert_eq!(
            call(&app, "PUT", &format!("/api/rooms/key1/admins/{n}"), &ann, "").await.status(),
            200
        );
    }
    let del = |who: &str| format!("/api/rooms/key1/admins/{who}");
    assert_eq!(
        call(&app, "DELETE", &del("cyd"), &bob, "").await.status(),
        403,
        "admin removes admin"
    );
    assert_eq!(call(&app, "DELETE", &del("ann"), &bob, "").await.status(), 409, "owner by admin");
    assert_eq!(call(&app, "DELETE", &del("ann"), &ann, "").await.status(), 409, "owner by owner");
    assert_eq!(call(&app, "DELETE", &del("bob"), &bob, "").await.status(), 204, "an admin leaves");
    assert_eq!(call(&app, "GET", "/api/rooms/key1/admins", &bob, "").await.status(), 404);
    assert_eq!(call(&app, "DELETE", &del("bob"), &ann, "").await.status(), 404, "already gone");
    let r = call(&app, "DELETE", &del("cyd"), &ann, "").await;
    assert_eq!(r.status(), StatusCode::OK, "the owner removes an admin");
    assert_eq!(admins_of(&app, "key1", &ann).await["admins"], json!([]));
    assert_eq!(call(&app, "GET", "/api/rooms/key1/admins", &cy, "").await.status(), 404);
}

/// The API is the boundary: a player file for another game is refused before anything runs.
#[tokio::test]
async fn a_player_file_for_another_game_than_the_manifests_is_refused() {
    let root = temp("game-mismatch");
    let w = Arc::new(worlds(&root, Limits::DESIGN));
    let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let insts = fixture_instances();
    let ok = player_yaml(&w, "Ann", "Fixture Game", &insts[0]).await;
    let other = player_yaml(&w, "Bo", "Other Game", &insts[1]).await;
    let req = json!({"manifest": manifest_id, "yamls": [ok["id"], other["id"]]});
    let e = create_world(State(w), me(), Bytes::from(req.to_string())).await.unwrap_err();
    assert_eq!(e.0, StatusCode::BAD_REQUEST);
    assert!(e.1.contains("Bo") && e.1.contains("Fixture Game"), "{}", e.1);
    let made = std::fs::read_dir(root.join("data/worlds")).unwrap().count();
    assert_eq!(made, 0, "no world folder was made");
}

/// Past `gen_queue_max` queued or running generations a request is refused as busy; the
/// slot comes back when a job ends.
#[tokio::test]
async fn generations_past_the_queue_limit_are_refused_as_busy() {
    let root = temp("queue-full");
    let w = Arc::new(worlds(&root, Limits { gen_queue_max: 1, ..Limits::DESIGN }));
    let up = upload_manifest(&w, Some("name=m.json"), &manifest(|_| {})).await.unwrap();
    let manifest_id = json_of(up).await["id"].clone();
    let insts = fixture_instances();
    let yaml = player_yaml(&w, "Ann", "Fixture Game", &insts[0]).await;
    let req = json!({"manifest": manifest_id, "yamls": [yaml["id"]]}).to_string();
    // The one generation permit is taken, so the first job waits queued.
    let held = w.one_gen.acquire().await.unwrap();
    let first = create_world(State(Arc::clone(&w)), me(), Bytes::from(req.clone())).await.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let second = create_world(State(Arc::clone(&w)), me(), Bytes::from(req.clone())).await.unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(second.headers().contains_key(header::RETRY_AFTER));
    drop(held);
    for _ in 0..200 {
        if w.queued.load(Ordering::Acquire) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let third = create_world(State(w), me(), Bytes::from(req)).await.unwrap();
    assert_eq!(third.status(), StatusCode::ACCEPTED, "the finished job gave its slot back");
}

#[cfg(feature = "pg-tests")]
mod pg {
    use super::*;

    async fn store() -> store::Db {
        let url = config::load_secrets().unwrap().database_url.expect(
            "pg-tests needs CONSTELLATION_DATABASE_URL (docs/TESTING.md rule 5: no silent skip)",
        );
        let db = log::testing::fresh_schema(&url).await.unwrap();
        let pg = log::PgLogStore::connect(&db, &Limits::DESIGN).await.unwrap();
        store::Db::Pg(Arc::new(pg))
    }

    /// On Postgres an old upload file is moved in and removed, a new upload is stored, and
    /// both are there for the next process: nothing of either is left in the data folder
    /// (#119).
    #[tokio::test]
    async fn uploads_live_in_postgres_and_old_files_move_in_once() {
        let root = temp("pg-uploads");
        let old = old_file(&root, "yamls/clique", "yaml", b"old", json!({"name": "old"}));
        let db = store().await;
        let mut w = worlds(&root, Limits::DESIGN);
        w.store = db.clone();
        w.import_files(w.store.is_durable()).await;
        assert!(!root.join("data/yamls").exists(), "moved in and removed");
        let new = stored(&w, Kind::Apworld, b"zip", json!({"module": "m"})).await;
        drop(w);

        let mut w = worlds(&root, Limits::DESIGN);
        w.store = db;
        let ids = json!({ "yamls": [old["id"]] });
        assert_eq!(
            chosen(&w, &ids, "yamls", Kind::Yaml).await.unwrap(),
            std::slice::from_ref(&old)
        );
        assert_eq!(json_of(list(&w, Kind::Yaml).await.unwrap()).await, json!([old]));
        let file = w.upload_file(Kind::Apworld, new["id"].as_str().unwrap()).await.unwrap();
        assert_eq!(file, b"zip");
        // The same file uploaded again is answered from its record, without the sandbox.
        let again = upload(&w, Kind::Apworld, Some("name=x.apworld"), b"zip").await.unwrap();
        assert_eq!((again.status(), json_of(again).await), (StatusCode::OK, new));
    }

    /// A status written by one process is read by the next on the same database, and a
    /// job another process left running reads as stalled, then is failed by this one's
    /// sweep once it is lost, while a job still marked alive is neither (#119).
    #[tokio::test]
    async fn statuses_live_in_postgres_and_a_stopped_servers_job_is_failed() {
        let root = temp("pg-jobs");
        let db = store().await;
        let id = |c: char| c.to_string().repeat(64);
        let mut first = worlds(&root, Limits::DESIGN);
        first.store = db.clone();
        first.save_status(&json!({"id": id('a'), "status": "done", "slots": 2})).await.unwrap();
        first.save_status(&json!({"id": id('b'), "status": "running"})).await.unwrap();
        drop(first);

        let mut w = worlds(&root, Limits::DESIGN);
        w.store = db.clone();
        let done = read_status(&w, &id('a')).await.unwrap();
        assert_eq!(done, json!({"id": id('a'), "status": "done", "slots": 2}));
        w.fail_stale().await;
        let alive = read_status(&w, &id('b')).await.unwrap();
        assert_eq!(alive["status"], "running", "marked alive just now");
        // Past the stale time it reads as stalled and is kept; past the lost time it fails.
        let mut stalled = worlds(&root, Limits { gen_stale_secs: 0, ..Limits::DESIGN });
        stalled.store = db.clone();
        stalled.fail_stale().await;
        let kept = read_status(&stalled, &id('b')).await.unwrap();
        assert_eq!(kept["status"], "stalled", "{kept}");
        let lost = Limits { gen_stale_secs: 0, gen_lost_secs: 0, ..Limits::DESIGN };
        let mut later = worlds(&root, lost);
        later.store = db;
        later.fail_stale().await;
        let failed = read_status(&later, &id('b')).await.unwrap();
        assert_eq!(failed["status"], "failed", "{failed}");
    }
}
