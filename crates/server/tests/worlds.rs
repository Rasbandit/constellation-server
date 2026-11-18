//! The upload and generation API end to end: the real server binary with `--data-dir`, the real
//! bubblewrap sandbox, and a fake sidecar (`fake_ap_gen.py`) in place of Archipelago. Needs
//! `bwrap` and `python3` on the machine: `cargo test -p server --features sandbox-tests --test
//! worlds` (`just test-sandbox`). Without them it fails, it never skips.
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the server package"
)]

use e2e::Server;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BIN: &str = env!("CARGO_BIN_EXE_constellation-server");
const TOKEN: &str = "test-admin-token-test-admin-token";

/// A fresh folder with the data dir, a fake checkout and the fake sidecar's tools folder.
struct Setup {
    root: PathBuf,
}

impl Setup {
    fn new(name: &str) -> Self {
        let bwrap = std::process::Command::new("bwrap").arg("--version").output();
        assert!(
            bwrap.is_ok_and(|o| o.status.success()),
            "the sandbox-tests feature needs bwrap (bubblewrap) on PATH"
        );
        let root = std::env::temp_dir().join(format!("worlds-it-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&root));
        let tools = root.join("tools/fake");
        for d in ["tools/fake", "ap/custom_worlds", "ap/worlds/generic", "ap/worlds/alttp"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["ap/worlds/__init__.py", "ap/worlds/AutoWorld.py"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::copy(here.join("tests/fake_ap_gen.py"), tools.join("ap_gen.py")).unwrap();
        std::fs::copy(e2e::fixture_path(), tools.join("bundle.json")).unwrap();
        // What the fake sidecar must not see: the token (in the server's environment) and
        // host folders.
        let hidden: Vec<String> = ["data", "ap", "tools", ""]
            .iter()
            .map(|p| root.join(p).display().to_string())
            .collect();
        let probe = json!({"value": TOKEN, "hidden_paths": hidden});
        std::fs::write(tools.join("probe.json"), probe.to_string()).unwrap();
        Self { root }
    }

    fn env(&self) -> Vec<(String, String)> {
        let p = |s: &str| self.root.join(s).display().to_string();
        vec![
            ("CONSTELLATION_ADMIN_TOKEN".into(), TOKEN.into()),
            ("CONSTELLATION_AP_DIR".into(), p("ap")),
            ("CONSTELLATION_AP_PYTHON".into(), "/usr/bin/python3".into()),
            ("CONSTELLATION_AP_GEN_SCRIPT".into(), p("tools/fake/ap_gen.py")),
        ]
    }

    fn start(&self) -> Server {
        self.start_with(&[])
    }

    fn start_with(&self, more: &[(&str, &str)]) -> Server {
        let data = self.root.join("data").display().to_string();
        let mut env = self.env();
        env.extend(more.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        Server::start(Path::new(BIN), &["--data-dir", &data], &env).unwrap()
    }
}

/// The admin API's own address, from its log line.
fn admin_addr(server: &Server) -> String {
    let line = server.wait_for_log("admin api").unwrap();
    let rest = line.split_once("addr=").unwrap().1;
    rest.split_whitespace().next().unwrap().to_owned()
}

/// One HTTP/1.1 request to the admin API on its own connection; returns the status and the body
/// as JSON (or as a JSON string when it is not JSON).
async fn http(
    server: &Server,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &[u8],
) -> (u16, Value) {
    request(&admin_addr(server), method, path, token, body).await
}

async fn request(
    addr: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &[u8],
) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or_else(|_| json!(body)))
}

#[tokio::test]
async fn upload_list_generate_poll_and_done() {
    let setup = Setup::new("flow");
    let server = setup.start();
    let t = Some(TOKEN);

    let (st, world) =
        http(&server, "PUT", "/api/apworlds?name=My%20World.apworld", t, b"my_world").await;
    assert_eq!(st, 201, "{world}");
    assert_eq!(
        (world["name"].as_str(), world["module"].as_str()),
        (Some("My World.apworld"), Some("my_world"))
    );
    let world_id = world["id"].as_str().unwrap().to_owned();
    // SHA-256 of "my_world", from `printf my_world | sha256sum`.
    assert_eq!(world_id, "8643fb2b4ab1fed653c5bb2e863b847805713116aafb2338f14ad99203edbcfd");
    let (st, again) =
        http(&server, "PUT", "/api/apworlds?name=other.apworld", t, b"my_world").await;
    assert_eq!((st, &again), (200, &world), "a re-upload returns the stored record");

    let (st, alice) =
        http(&server, "PUT", "/api/yamls?name=Majoras%20Mask.yaml", t, b"Alice").await;
    assert_eq!(st, 201, "{alice}");
    assert_eq!(alice["players"], json!([{"name": "Alice", "game": "Fake Game"}]));
    let (st, bob) = http(&server, "PUT", "/api/yamls?name=bob.yaml", t, b"Bob").await;
    assert_eq!(st, 201, "{bob}");

    let (st, list) = http(&server, "GET", "/api/yamls", t, b"").await;
    assert_eq!(st, 200);
    let names: Vec<&str> =
        list.as_array().unwrap().iter().map(|y| y["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Majoras Mask.yaml", "bob.yaml"]);
    let (_, list) = http(&server, "GET", "/api/apworlds", t, b"").await;
    assert_eq!(list, json!([world]));

    let req = json!({"apworlds": [world_id], "yamls": [alice["id"], bob["id"]]}).to_string();
    let (st, job) = http(&server, "POST", "/api/worlds", t, req.as_bytes()).await;
    assert_eq!(st, 202, "{job}");
    assert_eq!(job["status"], "queued");
    let id = job["id"].as_str().unwrap().to_owned();

    server.wait_for_log("generation done").unwrap();
    let (st, done) = http(&server, "GET", &format!("/api/worlds/{id}"), t, b"").await;
    assert_eq!(st, 200);
    assert_eq!(done["status"], "done", "{done}");
    let fixture = e2e::fixture().unwrap();
    assert_eq!(done["seed_name"], fixture.seed_name);
    assert_eq!(done["slots"], fixture.slots.len());
    // The server wrote the bundle from the sidecar's stdout; the symlink and FIFO the sidecar
    // left in its scratch folder never reached the host.
    let bundle = PathBuf::from(done["bundle"].as_str().unwrap());
    assert!(std::fs::symlink_metadata(&bundle).unwrap().is_file());
    let read = |p: &Path| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap();
    assert_eq!(read(&bundle), read(&e2e::fixture_path()));
    let world_dir = bundle.parent().unwrap();
    let mut files: Vec<String> = std::fs::read_dir(world_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["apworlds", "bundle.json", "status.json", "yamls"]);
    // What the sandbox showed the sidecar, and what it let it do.
    let g = &done["generation"];
    assert_eq!(g["custom_worlds"], json!(["my_world.apworld"]), "only the chosen world, by module");
    assert_eq!(g["core_worlds"], json!(["AutoWorld.py", "__init__.py", "generic"]));
    assert_eq!(g["yamls"], 2);
    assert_eq!(g["interfaces"], json!(["lo"]), "a network namespace of its own");
    assert_eq!((&g["env_has_canary"], &g["pid1_env_has_canary"]), (&json!(false), &json!(false)));
    assert_eq!(g["visible"], json!([]), "host folders are invisible");
    for w in ["write_root", "write_dev", "write_ap", "write_tools", "write_tmp_over_cap"] {
        assert_eq!(g[w], "refused", "{w}");
    }
    assert_ne!(g["nested_userns_exit"], 0, "no user namespaces inside");
    assert!(g["forks_of_100"].as_u64().unwrap() < 64, "{}", g["forks_of_100"]);
    assert!(g["memfds_of_300"].as_u64().unwrap() < 256, "{}", g["memfds_of_300"]);
    // The sidecar's spoofed `id` and `name` never replaced the server's.
    assert_eq!(world["id"].as_str(), Some(world_id.as_str()));
    drop(server.stop());

    // The status is on disk: a new server process still reports it.
    let server = setup.start();
    let (st, after) = http(&server, "GET", &format!("/api/worlds/{id}"), t, b"").await;
    assert_eq!((st, after), (200, done));
}

#[tokio::test]
async fn with_a_rooms_folder_a_finished_world_becomes_a_room_reached_by_its_key() {
    let setup = Setup::new("to-room");
    let rooms = setup.root.join("rooms");
    std::fs::create_dir_all(&rooms).unwrap();
    // A second room, so a connection must name its key to reach the generated one.
    let mut other: Value =
        serde_json::from_str(&std::fs::read_to_string(e2e::fixture_path()).unwrap()).unwrap();
    other["seed_name"] = json!("other-room");
    std::fs::write(rooms.join("other.json"), other.to_string()).unwrap();
    let data = setup.root.join("data").display().to_string();
    let mut env = setup.env();
    env.push(("CONSTELLATION_ROOMS_SCAN_MS".into(), "50".into()));
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let server = Server::start_rooms(Path::new(BIN), &rooms, &["--data-dir", &data], &env).unwrap();

    let id = generate_one(&server, b"Alice").await;
    server.wait_for_log("generation done").unwrap();
    let published = rooms.join(format!("{id}.json"));
    let read = |p: &Path| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap();
    assert_eq!(read(&published), read(&e2e::fixture_path()), "the bundle is in the rooms folder");
    let left: Vec<_> = std::fs::read_dir(&rooms).unwrap().filter_map(Result::ok).collect();
    assert_eq!(left.len(), 2, "no temporary file is left in the rooms folder");

    let seed = e2e::fixture().unwrap().seed_name;
    let ready = server.wait_for_log(&format!("seed=\"{seed}\"")).unwrap();
    let key = ready.split("key=").nth(1).unwrap().split_whitespace().next().unwrap().to_owned();
    let st = status(&server, &id).await;
    assert_eq!(st["status"], "done", "{st}");
    assert_eq!(st["room_key"], key.as_str(), "{st}");
    let port = server.addr.port();
    assert_eq!(st["address"], format!("{key}.localhost:{port}"), "{st}");

    let url = format!("ws://{}", st["address"].as_str().unwrap());
    let (_, connected, _) = e2e::join_via(server.addr, &url, "Alice").await.unwrap();
    assert_eq!(connected["cmd"], "Connected", "{connected}");
}

#[tokio::test]
async fn without_a_rooms_folder_a_world_status_has_no_room() {
    let setup = Setup::new("no-room");
    let server = setup.start();
    let id = generate_one(&server, b"Alice").await;
    server.wait_for_log("generation done").unwrap();
    let st = status(&server, &id).await;
    assert_eq!(st["status"], "done", "{st}");
    assert!(st.get("room_key").is_none() && st.get("address").is_none(), "{st}");
}

#[tokio::test]
async fn a_generation_past_its_time_limit_is_killed_and_reported_failed() {
    let setup = Setup::new("timeout");
    let server = setup.start_with(&[("CONSTELLATION_GEN_TIMEOUT_MS", "1000")]);
    let t = Some(TOKEN);
    let (_, y) = http(&server, "PUT", "/api/yamls?name=slow.yaml", t, b"SLOW").await;
    let req = json!({"apworlds": [], "yamls": [y["id"]]}).to_string();
    let (_, job) = http(&server, "POST", "/api/worlds", t, req.as_bytes()).await;
    server.wait_for_log("generation failed").unwrap();
    let (_, st) =
        http(&server, "GET", &format!("/api/worlds/{}", job["id"].as_str().unwrap()), t, b"").await;
    assert_eq!(st["status"], "failed", "{st}");
    assert!(st["error"].as_str().unwrap().contains("timed out"), "{st}");
    // The sandbox is gone: no process still names the sidecar's folder (only bwrap's command
    // line does). Its inner processes end when its first one does, so this waits on their exit,
    // with a deadline.
    let marker = setup.root.join("tools/fake").display().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !processes_naming(&marker).is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(processes_naming(&marker), Vec::<u32>::new());
}

/// Ids of live processes whose command line contains `needle`.
fn processes_naming(needle: &str) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            String::from_utf8_lossy(&cmd).contains(needle)
        })
        .collect()
}

async fn generate_one(server: &Server, yaml: &[u8]) -> String {
    let t = Some(TOKEN);
    let (st, y) = http(server, "PUT", "/api/yamls?name=y.yaml", t, yaml).await;
    assert!(st == 201 || st == 200, "{y}");
    let req = json!({"apworlds": [], "yamls": [y["id"]]}).to_string();
    let (st, job) = http(server, "POST", "/api/worlds", t, req.as_bytes()).await;
    assert_eq!(st, 202, "{job}");
    job["id"].as_str().unwrap().to_owned()
}

async fn status(server: &Server, id: &str) -> Value {
    http(server, "GET", &format!("/api/worlds/{id}"), Some(TOKEN), b"").await.1
}

#[tokio::test]
async fn a_flood_of_output_is_cut_to_its_caps_not_held() {
    let setup = Setup::new("flood");
    let server = setup.start();
    let err = generate_one(&server, b"FLOOD_ERR").await;
    server.wait_for_log("generation failed").unwrap();
    let st = status(&server, &err).await;
    let e = st["error"].as_str().unwrap();
    assert!(e.len() <= 16 * 1024 + 64 && e.contains("THE-END"), "{} bytes", e.len());
    let out = generate_one(&server, b"FLOOD_OUT").await;
    let line = format!("generation failed world={out}");
    server.wait_for_log(&line).unwrap();
    let st = status(&server, &out).await;
    assert!(st["error"].as_str().unwrap().contains("printed more than"), "{st}");
}

#[tokio::test]
async fn a_result_that_is_not_an_object_fails_the_job() {
    let setup = Setup::new("badresult");
    let server = setup.start();
    let id = generate_one(&server, b"BAD_RESULT").await;
    server.wait_for_log(&format!("generation failed world={id}")).unwrap();
    let st = status(&server, &id).await;
    assert_eq!(st["status"], "failed", "{st}");
    assert!(st["error"].as_str().unwrap().contains("malformed"), "{st}");
}

/// memfd pages count against no address space limit; the sandbox's own cgroup must stop them.
#[tokio::test]
async fn memory_past_the_cgroup_cap_kills_the_job_not_the_server() {
    let setup = Setup::new("memfd");
    let cap = (200 * 1024 * 1024).to_string();
    let server = setup.start_with(&[("CONSTELLATION_GEN_CGROUP_MEMORY_BYTES", &cap)]);
    let id = generate_one(&server, b"MEMFD").await;
    server.wait_for_log(&format!("generation failed world={id}")).unwrap();
    let st = status(&server, &id).await;
    assert_eq!(st["status"], "failed", "{st}");
    let room = server.url().trim_start_matches("ws://").to_owned();
    assert_eq!(request(&room, "GET", "/healthz", None, b"").await.0, 200, "the server lives");
}

#[tokio::test]
async fn generations_run_one_at_a_time() {
    let setup = Setup::new("queue");
    let server = setup.start();
    let first = generate_one(&server, b"WAIT").await;
    server.wait_for_log(&format!("generation running world={first}")).unwrap();
    let second = generate_one(&server, b"Bob").await;
    assert_eq!(status(&server, &second).await["status"], "queued");
    assert_eq!(status(&server, &first).await["status"], "running");
    std::fs::write(setup.root.join("tools/fake/release"), "").unwrap();
    server.wait_for_log(&format!("generation done world={second}")).unwrap();
    assert_eq!(status(&server, &first).await["status"], "done");
    assert_eq!(status(&server, &second).await["status"], "done");
}

#[tokio::test]
async fn an_upload_over_its_cap_is_refused_with_413() {
    let setup = Setup::new("cap");
    let server = setup.start();
    let t = Some(TOKEN);
    let big = vec![b'a'; 1024 * 1024 + 1];
    assert_eq!(http(&server, "PUT", "/api/yamls?name=big.yaml", t, &big).await.0, 413);
    assert_eq!(http(&server, "PUT", "/api/yamls?name=ok.yaml", t, &big[1..]).await.0, 201);
}

#[tokio::test]
async fn every_route_needs_the_admin_token() {
    let setup = Setup::new("auth");
    let server = setup.start();
    for (method, path) in [
        ("GET", "/api/apworlds"),
        ("PUT", "/api/yamls?name=a.yaml"),
        ("POST", "/api/worlds"),
        ("GET", "/api/worlds/00"),
    ] {
        for token in [None, Some("wrong"), Some("test-admin-token-test-admin-tokenx")] {
            let (st, _) = http(&server, method, path, token, b"x").await;
            assert_eq!(st, 401, "{method} {path} {token:?}");
        }
    }
    let room = server.url().trim_start_matches("ws://").to_owned();
    let (st, _) = request(&room, "GET", "/healthz", None, b"").await;
    assert_eq!(st, 200, "the room's own routes are unchanged");
    let (st, _) = request(&room, "GET", "/api/apworlds", Some(TOKEN), b"").await;
    assert_eq!(st, 404, "the admin API is never on the room's port");
    assert!(admin_addr(&server).starts_with("127.0.0.1:"), "loopback by default");
}

#[tokio::test]
async fn bad_input_is_refused_before_it_touches_a_path() {
    let setup = Setup::new("bad");
    let server = setup.start();
    let t = Some(TOKEN);
    for path in
        ["/api/worlds/..%2F..%2Fetc", "/api/worlds/ABC", &format!("/api/worlds/{}", "a".repeat(63))]
    {
        assert_eq!(http(&server, "GET", path, t, b"").await.0, 400, "{path}");
    }
    assert_eq!(
        http(&server, "GET", &format!("/api/worlds/{}", "a".repeat(64)), t, b"").await.0,
        404
    );
    assert_eq!(http(&server, "PUT", "/api/yamls?name=a%2Fb.yaml", t, b"A").await.0, 400);
    assert_eq!(http(&server, "PUT", "/api/yamls", t, b"A").await.0, 400);
    assert_eq!(http(&server, "PUT", "/api/yamls?name=e.yaml", t, b"").await.0, 400);
    let (st, why) = http(&server, "PUT", "/api/apworlds?name=x.apworld", t, b"BAD").await;
    assert_eq!(st, 422);
    assert!(why.as_str().unwrap().contains("not a zip"), "{why}");
    let (st, y) = http(&server, "PUT", "/api/yamls?name=a.yaml", t, b"Al").await;
    assert_eq!(st, 201);
    for body in [
        json!({"apworlds": [], "yamls": []}),
        json!({"apworlds": [], "yamls": ["../x"]}),
        json!({"apworlds": [], "yamls": ["a".repeat(64)]}),
        json!({"apworlds": [], "yamls": [y["id"], y["id"]]}),
        json!({"yamls": [y["id"]]}),
    ] {
        let (st, e) = http(&server, "POST", "/api/worlds", t, body.to_string().as_bytes()).await;
        assert_eq!(st, 400, "{body} {e}");
    }
    let (st, _) = http(&server, "POST", "/api/worlds", t, b"{nope").await;
    assert_eq!(st, 400);
}

#[test]
fn the_server_refuses_a_data_dir_without_the_admin_token() {
    let setup = Setup::new("notoken");
    let data = setup.root.join("data").display().to_string();
    let env: Vec<(String, String)> = setup.env().into_iter().skip(1).collect();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let out = e2e::command(Path::new(BIN), &["--data-dir", &data], &env)
        .env_remove("CONSTELLATION_ADMIN_TOKEN")
        .env_remove("CONSTELLATION_ADMIN_TOKEN_FILE")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(log.contains("CONSTELLATION_ADMIN_TOKEN"), "{log}");
    assert!(!log.contains("listening"), "{log}");
}
