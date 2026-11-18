//! Uploads of `.apworld` and player `.yaml` files, and seed generation from a chosen set of
//! them, over HTTP. Admin only: every route needs `Authorization: Bearer <token>`.
//!
//! Storage is plain files under the data folder:
//! - `apworlds/<sha256>.apworld` and `yamls/<sha256>.yaml`, content-addressed, each with a
//!   `<sha256>.json` holding the upload's name and what the sidecar's inspect found;
//! - `worlds/<id>/` per generation: `apworlds/<module>.apworld` and `yamls/` (the chosen
//!   files), `status.json`, and `bundle.json` on success. The server writes `bundle.json` from
//!   the sidecar's stdout; no folder here is ever writable inside the sandbox.
//!
//! The flow follows Archipelago's web host (`WebHostLib/upload.py` checks uploads before it
//! keeps them; `WebHostLib/generate.py` generates one queued seed at a time from the stored
//! player files). Every call to the sidecar goes through [`crate::sandbox`].

use crate::sandbox::{AP, Failure, JOB, Mount, Sandbox};
use crate::{hex, load_bundle};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use config::{Generation, Limits};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;

/// The API's shared state.
struct Worlds {
    dir: PathBuf,
    /// SHA-256 of the admin token (compared in constant time, see [`authorized`]).
    token: [u8; 32],
    sandbox: Sandbox,
    limits: Limits,
    /// One generation at a time.
    one_gen: Semaphore,
    /// Makes temporary folder names and world ids unique within the process.
    seq: AtomicU64,
}

/// The two kinds of upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Apworld,
    Yaml,
}

impl Kind {
    const fn folder(self) -> &'static str {
        match self {
            Self::Apworld => "apworlds",
            Self::Yaml => "yamls",
        }
    }
    const fn ext(self) -> &'static str {
        match self {
            Self::Apworld => "apworld",
            Self::Yaml => "yaml",
        }
    }
    const fn cap(self, l: &Limits) -> usize {
        match self {
            Self::Apworld => l.apworld_max_bytes,
            Self::Yaml => l.yaml_max_bytes,
        }
    }
}

type Reply = Result<Response, (StatusCode, String)>;

fn internal(e: impl core::fmt::Display) -> (StatusCode, String) {
    tracing::error!(error = %e, "worlds api");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error (see the server log)".to_owned())
}

fn json_reply(status: StatusCode, v: &Value) -> Response {
    let mut r = (status, v.to_string()).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

/// The routes, rooted at `/api`. Makes the data folders and marks jobs a previous process left
/// unfinished as failed.
///
/// # Errors
/// When the data folder cannot be made or read.
pub fn router(
    dir: &Path,
    token: &str,
    paths: Generation,
    limits: Limits,
) -> Result<Router, String> {
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    // Uploads a previous process was inspecting when it stopped.
    if dir.join("tmp").exists() {
        std::fs::remove_dir_all(dir.join("tmp")).map_err(io)?;
    }
    for sub in ["apworlds", "yamls", "worlds", "tmp"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(io)?;
    }
    recover(&dir.join("worlds"))?;
    let state = Arc::new(Worlds {
        dir: dir.to_owned(),
        token: Sha256::digest(token.as_bytes()).into(),
        sandbox: Sandbox::new(paths, limits, dir)?,
        limits,
        one_gen: Semaphore::new(1),
        seq: AtomicU64::new(0),
    });
    let body_cap = limits.apworld_max_bytes.max(limits.yaml_max_bytes);
    Ok(Router::new()
        .route("/api/apworlds", get(list_apworlds).put(put_apworld))
        .route("/api/yamls", get(list_yamls).put(put_yaml))
        .route("/api/worlds", post(create_world))
        .route("/api/worlds/{id}", get(world_status))
        .route_layer(axum::middleware::from_fn_with_state(Arc::clone(&state), auth))
        .layer(DefaultBodyLimit::max(body_cap))
        .with_state(state))
}

async fn auth(State(w): State<Arc<Worlds>>, req: Request, next: Next) -> Response {
    if authorized(req.headers(), &w.token) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "admin token required").into_response()
    }
}

/// `Authorization: Bearer <token>` whose SHA-256 equals `want`. Both sides are hashed first, so
/// the comparison takes the same time for every token, whatever its length or content.
fn authorized(headers: &HeaderMap, want: &[u8; 32]) -> bool {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    else {
        return false;
    };
    let got: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    got.iter().zip(want).fold(0_u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// Ids are the lowercase hex of a SHA-256: checked before any id reaches a path.
fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The content address of an upload.
fn content_id(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The `name` query parameter, percent-decoded, checked to be a plain file name. It is only
/// shown back (storage uses the content address).
fn upload_name(query: Option<&str>) -> Result<String, String> {
    let raw = query
        .unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("name="))
        .ok_or("the name query parameter is required")?;
    let name = percent_decode(raw).ok_or("the name is not valid percent-encoded UTF-8")?;
    if name.is_empty() || name.len() > 255 || name == "." || name == ".." {
        return Err("the name must be 1 to 255 bytes".to_owned());
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err("the name must be a plain file name".to_owned());
    }
    Ok(name)
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.bytes();
    while let Some(b) = it.next() {
        out.push(match b {
            b'%' => {
                let hi = char::from(it.next()?).to_digit(16)?;
                let lo = char::from(it.next()?).to_digit(16)?;
                u8::try_from(hi * 16 + lo).ok()?
            }
            b'+' => b' ',
            b => b,
        });
    }
    String::from_utf8(out).ok()
}

/// Write `bytes` to `path` through a temporary file and a rename, so a reader never sees half.
/// The temporary name is unique, so two writers of one path never share it.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

async fn put_apworld(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery, body: Bytes) -> Reply {
    upload(&w, Kind::Apworld, q.as_deref(), &body).await
}

async fn put_yaml(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery, body: Bytes) -> Reply {
    upload(&w, Kind::Yaml, q.as_deref(), &body).await
}

/// Check, inspect in the sandbox, then store an upload. The same content again returns the
/// stored record.
async fn upload(w: &Worlds, kind: Kind, query: Option<&str>, body: &[u8]) -> Reply {
    let name = upload_name(query).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let cap = kind.cap(&w.limits);
    if body.len() > cap {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, format!("larger than {cap} bytes")));
    }
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty upload".to_owned()));
    }
    let id = content_id(body);
    let store = w.dir.join(kind.folder());
    let meta_path = store.join(format!("{id}.json"));
    if let Some(meta) = read_json(&meta_path) {
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
        .map_err(|e| match e {
            Failure::Refused(why) => (StatusCode::UNPROCESSABLE_ENTITY, why),
            Failure::Broken(why) => internal(why),
        })?;
    let found: Value = serde_json::from_slice(&found)
        .map_err(|e| internal(format!("sidecar printed bad JSON: {e}")))?;
    let mut meta = match (kind, found) {
        (Kind::Apworld, Value::Object(o)) => {
            if !o.get("module").and_then(Value::as_str).is_some_and(valid_module) {
                return Err((StatusCode::UNPROCESSABLE_ENTITY, "bad module name".to_owned()));
            }
            Value::Object(o)
        }
        (Kind::Yaml, players @ Value::Array(_)) => json!({"players": players}),
        (_, other) => return Err(internal(format!("unexpected inspect result {other}"))),
    };
    // The server's own fields last, so the sidecar can never set them.
    for (k, v) in [("id", json!(id)), ("name", json!(name)), ("bytes", json!(body.len()))] {
        meta[k] = v;
    }
    let store = w.dir.join(kind.folder());
    std::fs::rename(tmp.join(&file), store.join(format!("{id}.{}", kind.ext())))
        .map_err(internal)?;
    write_atomic(&store.join(format!("{id}.json")), meta.to_string().as_bytes())
        .map_err(internal)?;
    Ok(meta)
}

/// What Archipelago imports as `worlds.<module>`; also the file name in `custom_worlds`, so it
/// must never hold a path separator (the sidecar checks the same).
fn valid_module(m: &str) -> bool {
    let mut c = m.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

async fn list_apworlds(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Apworld)
}

async fn list_yamls(State(w): State<Arc<Worlds>>) -> Reply {
    list(&w, Kind::Yaml)
}

fn list(w: &Worlds, kind: Kind) -> Reply {
    let mut all: Vec<Value> = std::fs::read_dir(w.dir.join(kind.folder()))
        .map_err(internal)?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read_json(&e.path()))
        .collect();
    let key = |v: &Value| (v["name"].as_str().unwrap_or("").to_owned(), v["id"].to_string());
    all.sort_by_key(key);
    Ok(json_reply(StatusCode::OK, &Value::Array(all)))
}

/// The ids in `body[field]`: each valid, stored, and listed once.
fn chosen(w: &Worlds, body: &Value, field: &str, kind: Kind) -> Result<Vec<Value>, String> {
    let ids = body.get(field).and_then(Value::as_array).ok_or(format!("{field} must be a list"))?;
    let mut metas = Vec::with_capacity(ids.len());
    for id in ids {
        let id = id.as_str().filter(|i| valid_id(i)).ok_or(format!("{field}: bad id {id}"))?;
        if metas.iter().any(|m: &Value| m["id"] == id) {
            return Err(format!("{field}: {id} is listed twice"));
        }
        let meta = read_json(&w.dir.join(kind.folder()).join(format!("{id}.json")))
            .ok_or(format!("{field}: no upload {id}"))?;
        metas.push(meta);
    }
    Ok(metas)
}

/// Lay out a world folder: the chosen files (apworlds named by module, as Archipelago loads
/// them).
fn prepare(w: &Worlds, world: &Path, apworlds: &[Value], yamls: &[Value]) -> Result<(), String> {
    let e = |e: std::io::Error| e.to_string();
    for sub in ["apworlds", "yamls"] {
        std::fs::create_dir_all(world.join(sub)).map_err(e)?;
    }
    for (kind, metas) in [(Kind::Apworld, apworlds), (Kind::Yaml, yamls)] {
        for m in metas {
            let id = m["id"].as_str().unwrap_or("");
            let stem = if kind == Kind::Apworld { m["module"].as_str().unwrap_or("") } else { id };
            let from = w.dir.join(kind.folder()).join(format!("{id}.{}", kind.ext()));
            let to = world.join(kind.folder()).join(format!("{stem}.{}", kind.ext()));
            if std::fs::hard_link(&from, &to).is_err() {
                std::fs::copy(&from, &to).map_err(e)?;
            }
        }
    }
    Ok(())
}

async fn create_world(State(w): State<Arc<Worlds>>, body: Bytes) -> Reply {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let req: Value = serde_json::from_slice(&body).map_err(|e| bad(format!("bad JSON: {e}")))?;
    let apworlds = chosen(&w, &req, "apworlds", Kind::Apworld).map_err(bad)?;
    let yamls = chosen(&w, &req, "yamls", Kind::Yaml).map_err(bad)?;
    if yamls.is_empty() {
        return Err(bad("choose at least one yaml".to_owned()));
    }
    let mut modules: Vec<&str> = apworlds.iter().filter_map(|m| m["module"].as_str()).collect();
    modules.sort_unstable();
    if modules.windows(2).any(|p| p.first() == p.get(1)) {
        return Err(bad("two apworlds have the same module name".to_owned()));
    }
    let ids = |ms: &[Value]| ms.iter().map(|m| m["id"].clone()).collect::<Vec<_>>();
    let (apworld_ids, yaml_ids) = (ids(&apworlds), ids(&yamls));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = w.seq.fetch_add(1, Ordering::Relaxed);
    let seed = format!("{apworld_ids:?}{yaml_ids:?}{nanos}-{}-{n}", std::process::id());
    let id = content_id(seed.as_bytes());
    let world = w.dir.join("worlds").join(&id);
    prepare(&w, &world, &apworlds, &yamls).map_err(internal)?;
    let status = json!({"id": id, "status": "queued", "apworlds": apworld_ids, "yamls": yaml_ids});
    write_status(&world, &status).map_err(internal)?;
    tracing::info!(world = %id, "generation queued");
    tokio::spawn(generate(Arc::clone(&w), world, status));
    Ok(json_reply(StatusCode::ACCEPTED, &json!({"id": id, "status": "queued"})))
}

fn write_status(world: &Path, status: &Value) -> std::io::Result<()> {
    write_atomic(&world.join("status.json"), status.to_string().as_bytes())
}

/// What `ap_gen.py generate` prints. The bundle stays raw text (no tree of values in memory);
/// `load_bundle` parses it into its typed form.
#[derive(Debug, serde::Deserialize)]
struct GenOut {
    report: Value,
    bundle: Box<serde_json::value::RawValue>,
}

impl GenOut {
    fn parse(stdout: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(stdout)
            .map_err(|e| format!("the sidecar's result is malformed: {e}"))
    }
}

/// Run one queued generation (one at a time) and record how it ended.
async fn generate(w: Arc<Worlds>, world: PathBuf, mut status: Value) {
    let Ok(_permit) = w.one_gen.acquire().await else { return };
    let id = status["id"].as_str().unwrap_or("").to_owned();
    status["status"] = json!("running");
    if let Err(e) = write_status(&world, &status) {
        tracing::error!(error = %e, "status write failed");
    }
    tracing::info!(world = %id, "generation running");
    let (custom, yamls, out) =
        (format!("{AP}/custom_worlds"), format!("{JOB}/yamls"), format!("{JOB}/out"));
    let mounts = [
        Mount::Ro(world.join("apworlds"), custom.clone()),
        Mount::Ro(world.join("yamls"), yamls.clone()),
        Mount::Scratch(out.clone()),
    ];
    let args = ["generate", "--ap", AP, "--apworlds", &custom, "--yamls", &yamls, "--out", &out];
    let timeout = Duration::from_millis(w.limits.gen_timeout_ms);
    let result = match w.sandbox.run(true, &mounts, &args, timeout).await {
        Ok(stdout) => {
            let (bundle, limits) = (world.join("bundle.json"), w.limits);
            tokio::task::spawn_blocking(move || {
                let out = GenOut::parse(&stdout)?;
                write_atomic(&bundle, out.bundle.get().as_bytes()).map_err(|e| e.to_string())?;
                load_bundle(&bundle, &limits).map(|loaded| (bundle, loaded, out.report))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
        Err(e) => Err(e.to_string()),
    };
    let done = result.is_ok();
    match result {
        Ok((bundle, loaded, report)) => {
            status["status"] = json!("done");
            status["bundle"] = json!(bundle.display().to_string());
            status["seed_name"] = json!(loaded.bundle.seed_name);
            status["slots"] = json!(loaded.bundle.slots.len());
            status["bundle_hash"] = json!(hex(&loaded.hash));
            status["generation"] = report;
        }
        Err(e) => {
            status["status"] = json!("failed");
            status["error"] = json!(e);
        }
    }
    if let Err(e) = write_status(&world, &status) {
        tracing::error!(error = %e, "status write failed");
    }
    // Logged after the status is on disk: tests and operators wait on these lines.
    if done {
        tracing::info!(world = %id, "generation done");
    } else {
        tracing::warn!(world = %id, "generation failed");
    }
}

async fn world_status(State(w): State<Arc<Worlds>>, UrlPath(id): UrlPath<String>) -> Reply {
    if !valid_id(&id) {
        return Err((StatusCode::BAD_REQUEST, "bad id".to_owned()));
    }
    let status = read_json(&w.dir.join("worlds").join(&id).join("status.json"))
        .ok_or((StatusCode::NOT_FOUND, "no such world".to_owned()))?;
    Ok(json_reply(StatusCode::OK, &status))
}

/// A job a previous process left queued or running will never finish: mark it failed.
fn recover(worlds: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(worlds).map_err(|e| format!("{}: {e}", worlds.display()))?;
    for world in entries.filter_map(Result::ok).map(|e| e.path()) {
        let Some(mut status) = read_json(&world.join("status.json")) else { continue };
        if matches!(status["status"].as_str(), Some("queued" | "running")) {
            status["status"] = json!("failed");
            status["error"] = json!("the server restarted before this generation finished");
            write_status(&world, &status).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn headers(auth: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_str(auth).unwrap());
        h
    }

    #[test]
    fn only_the_exact_bearer_token_is_authorized() {
        let want: [u8; 32] = Sha256::digest(b"s3cret").into();
        assert!(authorized(&headers("Bearer s3cret"), &want));
        for bad in [
            "Bearer s3cre",
            "Bearer s3cret ",
            "Bearer S3cret",
            "bearer s3cret",
            "s3cret",
            "Bearer ",
            "Basic s3cret",
        ] {
            assert!(!authorized(&headers(bad), &want), "{bad:?}");
        }
        assert!(!authorized(&HeaderMap::new(), &want));
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

    #[test]
    fn concurrent_writers_of_one_path_never_collide() {
        let dir = temp("atomic");
        let path = dir.join("status.json");
        let writers: Vec<_> = (0..16)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    (0..50).try_for_each(|_| write_atomic(&path, format!("{i}").as_bytes()))
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap().unwrap();
        }
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");
    }

    #[test]
    fn a_status_survives_a_restart_and_unfinished_jobs_become_failed() {
        let worlds = temp("recover");
        let mk = |id: &str, status: &str| {
            let w = worlds.join(id);
            std::fs::create_dir_all(&w).unwrap();
            write_status(&w, &json!({"id": id, "status": status, "bundle": "/b"})).unwrap();
        };
        for (id, s) in [("q", "queued"), ("r", "running"), ("d", "done"), ("f", "failed")] {
            mk(id, s);
        }
        std::fs::create_dir_all(worlds.join("empty")).unwrap();
        recover(&worlds).unwrap();
        let read = |id: &str| read_json(&worlds.join(id).join("status.json")).unwrap();
        for id in ["q", "r"] {
            assert_eq!(read(id)["status"], "failed", "{id}");
            assert!(read(id)["error"].as_str().unwrap().contains("restarted"));
        }
        assert_eq!(read("d"), json!({"id": "d", "status": "done", "bundle": "/b"}));
        assert_eq!(read("f")["status"], "failed");
        assert!(!worlds.join("q/status.tmp").exists(), "the write is atomic");
    }
}
