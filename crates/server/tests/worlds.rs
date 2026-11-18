//! The upload and generation API end to end: the real server binary with `--data-dir`, the real
//! bubblewrap sandbox, and a fake sidecar (`fake_ap_gen.py`) in place of Archipelago. Needs
//! `bwrap` and `python3` on the machine.
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the server package"
)]

use e2e::Server;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BIN: &str = env!("CARGO_BIN_EXE_constellation-server");
const TOKEN: &str = "test-admin-token";

/// A fresh folder with the data dir, a fake checkout and the fake sidecar's tools folder.
struct Setup {
    root: PathBuf,
}

impl Setup {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("worlds-it-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&root));
        let tools = root.join("tools/fake");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(root.join("ap/custom_worlds")).unwrap();
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::copy(here.join("tests/fake_ap_gen.py"), tools.join("ap_gen.py")).unwrap();
        std::fs::copy(e2e::fixture_path(), tools.join("bundle.json")).unwrap();
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

/// One HTTP/1.1 request on its own connection; returns the status and the body as JSON (or as a
/// JSON string when it is not JSON).
async fn http(
    server: &Server,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &[u8],
) -> (u16, Value) {
    let addr = server.url().trim_start_matches("ws://").to_owned();
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
    let bundle = PathBuf::from(done["bundle"].as_str().unwrap());
    assert_eq!(std::fs::read(&bundle).unwrap(), std::fs::read(e2e::fixture_path()).unwrap());
    // What the sandbox showed the sidecar, and what it let it do.
    let g = &done["generation"];
    assert_eq!(g["custom_worlds"], json!(["my_world.apworld"]), "only the chosen world, by module");
    assert_eq!(g["yamls"], 2);
    assert_eq!(g["network"], "blocked");
    assert_eq!(g["write_tools"], "refused");
    assert_eq!(g["write_checkout"], "allowed", "a throwaway overlay");
    assert!(!setup.root.join("ap/written-by-job").exists(), "the overlay never reaches the host");
    drop(server.stop());

    // The status is on disk: a new server process still reports it.
    let server = setup.start();
    let (st, after) = http(&server, "GET", &format!("/api/worlds/{id}"), t, b"").await;
    assert_eq!((st, after), (200, done));
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
        for token in [None, Some("wrong"), Some("test-admin-tokenx")] {
            let (st, _) = http(&server, method, path, token, b"x").await;
            assert_eq!(st, 401, "{method} {path} {token:?}");
        }
    }
    let (st, _) = http(&server, "GET", "/healthz", None, b"").await;
    assert_eq!(st, 200, "the room's own routes are unchanged");
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
