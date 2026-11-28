//! Who is calling: accounts, logins and the seam handlers use to ask.
//!
//! Handlers never see credentials. They take an [`Identity`] some [`Authenticator`] made from
//! the request; the gate in `worlds` answers 401 before a handler runs without one. The one authenticator today is
//! [`PasswordAuth`] (username and password, argon2id hashes and server-side sessions in the
//! host's store, a random id in an `HttpOnly` cookie). A hosted provider (Clerk, Auth0) is
//! another `Authenticator` that verifies the provider's signed token instead; nothing that asks
//! "who is this?" changes. Ownership of rooms is separate: it is keyed by `Identity::subject`.
//!
//! Sign-up is open (`Limits::signup_open` closes it) and rate-limited per client address. There
//! is no site-wide admin: what a person may do to a room is its role on it (`room_admins`).

use crate::store::Db;
use crate::{hex, worlds::client_ip};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use config::Limits;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// The cookie holding the login's random id.
const COOKIE: &str = "constellation_session";

/// A person, as whichever provider knows them. `subject` is provider-neutral and stable (rooms
/// are owned by it); `name` is for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub subject: String,
    pub name: String,
}

impl<S: Send + Sync> FromRequestParts<S> for Identity {
    type Rejection = StatusCode;

    /// The gate puts the identity in the request; a handler reached without one is a wiring
    /// bug, answered as unauthorized.
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, StatusCode> {
        parts.extensions.get::<Self>().cloned().ok_or(StatusCode::UNAUTHORIZED)
    }
}

/// The subject of the account called `name`: what a room records as its owner.
pub(crate) fn subject_of(name: &str) -> String {
    format!("pw:{name}")
}

/// The account name inside an owner subject (the subject itself when it is not one of ours).
pub(crate) fn name_of(subject: &str) -> &str {
    subject.strip_prefix("pw:").unwrap_or(subject)
}

/// `name` as an account is stored (lowercase), or None when no account can have that name, so
/// it never reaches the store.
pub(crate) fn clean_name(name: &str) -> Option<String> {
    let name = normalize(name);
    (!name.is_empty() && name_chars_ok(&name)).then_some(name)
}

/// Turns a request into an [`Identity`]; the seam a hosted provider implements.
pub(crate) trait Authenticator: Send + Sync {
    /// The caller, or None when the request carries no valid login.
    fn identify<'a>(&'a self, headers: &'a HeaderMap) -> BoxFuture<'a, Option<Identity>>;
}

/// Account names are lowercase ASCII letters, digits, `_` and `-`, so two spellings of one name
/// are one account and no look-alike character exists.
fn normalize(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Whether `name` (already normalized) holds only what an account name may. Login uses it too,
/// so a name no account can have never reaches the store (Postgres refuses a NUL byte in text,
/// which would answer 500 and skip the failure count).
fn name_chars_ok(name: &str) -> bool {
    name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// Check a new account's name and password against the limits; the name as stored (lowercase).
/// The one validator sign-up uses. Messages describe the caller's own input only.
pub(crate) fn check_account(
    name: &str,
    password: &str,
    l: &Limits,
) -> Result<String, &'static str> {
    let name = normalize(name);
    let chars = name.chars().count();
    if chars < l.account_name_min_chars || name.len() > l.account_name_max_bytes {
        return Err("the name is too short or too long");
    }
    if !name_chars_ok(&name) {
        return Err("the name may hold only letters, digits, _ and -");
    }
    if password.chars().count() < l.password_min_chars {
        return Err("the password is too short");
    }
    if password.len() > l.password_max_bytes {
        return Err("the password is too long");
    }
    Ok(name)
}

/// A random 32 bytes.
fn random_id() -> Result<[u8; 32], String> {
    let mut id = [0_u8; 32];
    getrandom::fill(&mut id).map_err(|e| format!("no random source: {e}"))?;
    Ok(id)
}

/// argon2id with the crate's defaults (the OWASP minimum), a fresh salt, as a PHC string.
fn hash_password(password: &str) -> Result<String, String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(&random_id()?[..16]).map_err(|e| e.to_string())?;
    let hash = argon2::Argon2::default().hash_password(password.as_bytes(), &salt);
    hash.map(|h| h.to_string()).map_err(|e| e.to_string())
}

fn verify_password(hash: &str, password: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(hash)
        .is_ok_and(|h| argon2::Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// A hash nobody has the password of, checked when the name is unknown so that an unknown name
/// and a wrong password take the same time.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("no account has this password").unwrap_or_default())
}

/// The key a session is stored under: the hash of the cookie value, so the store never holds a
/// usable login.
fn session_key(cookie_value: &str) -> [u8; 32] {
    Sha256::digest(cookie_value.as_bytes()).into()
}

/// The session id in the request's cookies: 64 lowercase hex digits, or none.
fn cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v)
        .filter(|v| v.len() == 64 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// `Set-Cookie` for a login of `max_age` seconds, or one that ends it (0). `Secure` when the
/// request came over https (here or at the proxy in front); a client forging that header only
/// makes its own cookie stricter.
fn set_cookie(value: &str, max_age: u64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let c = format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}");
    HeaderValue::from_str(&c).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get(HeaderName::from_static("x-forwarded-proto"))
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"https"))
}

/// Username and password accounts in the host's store.
pub(crate) struct PasswordAuth {
    db: Db,
    limits: Limits,
    proxy_header: Option<HeaderName>,
    /// Failed logins per client address (`login_fail_burst`).
    login_fails: edge_compat::RateTable<std::net::IpAddr>,
    /// Sign-ups per client address (`signup_burst`).
    signups: edge_compat::RateTable<std::net::IpAddr>,
    /// Passwords being hashed or checked right now (`password_hash_parallel`): each takes
    /// ~19 MiB and a core, and the per-address counts only bite after the work is done.
    hashing: Arc<tokio::sync::Semaphore>,
}

impl PasswordAuth {
    pub(crate) fn new(db: Db, limits: Limits, proxy_header: Option<HeaderName>) -> Self {
        let keys = limits.rate_table_max_keys;
        Self {
            db,
            limits,
            proxy_header,
            hashing: Arc::new(tokio::sync::Semaphore::new(limits.password_hash_parallel)),
            login_fails: edge_compat::RateTable::new(
                limits.login_fail_burst,
                limits.login_fail_refill_milli_per_sec,
                keys,
            ),
            signups: edge_compat::RateTable::new(
                limits.signup_burst,
                limits.signup_refill_milli_per_sec,
                keys,
            ),
        }
    }

    async fn identity(&self, headers: &HeaderMap) -> Option<Identity> {
        let key = session_key(cookie_value(headers)?);
        let name = self.db.session_user(&key).await.ok()??;
        Some(Identity { subject: subject_of(&name), name })
    }

    /// Start a login for `name`: the cookie to set.
    async fn start_session(&self, name: &str, secure: bool) -> Result<HeaderValue, String> {
        let value = hex(&random_id()?);
        let ttl = self.limits.session_ttl_secs;
        let key = session_key(&value);
        let put = self.db.session_put(&key, name, Duration::from_secs(ttl));
        put.await.map_err(|e| format!("{e:?}"))?;
        Ok(set_cookie(&value, ttl, secure))
    }
}

impl Authenticator for PasswordAuth {
    fn identify<'a>(&'a self, headers: &'a HeaderMap) -> BoxFuture<'a, Option<Identity>> {
        Box::pin(self.identity(headers))
    }
}

/// What a sign-up or login request holds. No `Debug`: a password must never reach a log.
#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

fn too_many(refill_milli_per_sec: u32) -> Response {
    let wait = 1000_u64.div_ceil(u64::from(refill_milli_per_sec).max(1));
    let mut res = StatusCode::TOO_MANY_REQUESTS.into_response();
    res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait));
    res
}

fn failed(why: impl core::fmt::Display) -> Response {
    tracing::error!(%why, "accounts: store or random source failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "the server could not do that").into_response()
}

/// Run argon2 off the async runtime's I/O threads, at most `password_hash_parallel` at a time;
/// past that the caller gets 503 at once instead of queueing more memory-hard work.
async fn blocking<T: Send + 'static>(
    a: &PasswordAuth,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Response> {
    let Ok(permit) = Arc::clone(&a.hashing).try_acquire_owned() else {
        let mut res = StatusCode::SERVICE_UNAVAILABLE.into_response();
        res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(1));
        return Err(res);
    };
    // The permit moves into the job: it frees when the hash ends, not when the request does (a
    // client that disconnects mid-hash must not free its slot while argon2 still runs).
    tokio::task::spawn_blocking(move || {
        let done = f();
        drop(permit);
        done
    })
    .await
    .map_err(failed)
}

/// The account's name as the page shows it (never the subject's provider prefix, never a hash).
fn me_body(name: &str) -> String {
    serde_json::json!({ "name": name }).to_string()
}

fn json_with_cookie(status: StatusCode, name: &str, cookie: HeaderValue) -> Response {
    let mut res =
        (status, [(header::CONTENT_TYPE, "application/json")], me_body(name)).into_response();
    res.headers_mut().insert(header::SET_COOKIE, cookie);
    res
}

/// Read the JSON credentials of a request, capped to what a valid pair can take.
async fn credentials(a: &PasswordAuth, body: Body) -> Result<Credentials, StatusCode> {
    let cap = a.limits.password_max_bytes + a.limits.account_name_max_bytes + 256;
    let bytes = to_bytes(body, cap).await.map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_REQUEST)
}

/// `POST /api/signup`: make an account and sign it in. Refused (403) when sign-up is closed,
/// 429 past the per-address allowance (every attempt counts), 400 for a name or password the
/// rules refuse, 409 for a name that is taken (the same words whoever holds it).
async fn signup(State(a): State<Arc<PasswordAuth>>, req: Request) -> Response {
    if !a.limits.signup_open {
        return (StatusCode::FORBIDDEN, "sign-up is closed on this server").into_response();
    }
    let ip = client_ip(&req, a.proxy_header.as_ref());
    if !a.signups.take(ip, Instant::now()) {
        return too_many(a.limits.signup_refill_milli_per_sec);
    }
    let secure = is_https(req.headers());
    let c = match credentials(&a, req.into_body()).await {
        Ok(c) => c,
        Err(status) => return status.into_response(),
    };
    let name = match check_account(&c.username, &c.password, &a.limits) {
        Ok(n) => n,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let hash = match blocking(&a, move || hash_password(&c.password)).await {
        Ok(Ok(hash)) => hash,
        Ok(Err(e)) => return failed(e),
        Err(res) => return res,
    };
    match a.db.user_create(&name, &hash).await {
        Ok(true) => {}
        Ok(false) => return (StatusCode::CONFLICT, "that name is not available").into_response(),
        Err(e) => return failed(format!("{e:?}")),
    }
    match a.start_session(&name, secure).await {
        Ok(cookie) => json_with_cookie(StatusCode::CREATED, &name, cookie),
        Err(e) => failed(e),
    }
}

/// `POST /api/login`: a right pair starts a session (the cookie). A wrong pair, or an unknown
/// name, is the same bare 401 after the same work, and counts against the client address;
/// past `login_fail_burst` of them the address gets 429 before any password is checked.
async fn login(State(a): State<Arc<PasswordAuth>>, req: Request) -> Response {
    let ip = client_ip(&req, a.proxy_header.as_ref());
    if !a.login_fails.ready(ip, Instant::now()) {
        return too_many(a.limits.login_fail_refill_milli_per_sec);
    }
    let secure = is_https(req.headers());
    let c = match credentials(&a, req.into_body()).await {
        Ok(c) => c,
        Err(status) => return status.into_response(),
    };
    let name = normalize(&c.username);
    let stored = if name_chars_ok(&name) {
        match a.db.user_hash(&name).await {
            Ok(h) => h,
            Err(e) => return failed(format!("{e:?}")),
        }
    } else {
        None
    };
    let known = stored.is_some();
    let hash = stored.unwrap_or_else(|| dummy_hash().to_owned());
    let right = match blocking(&a, move || verify_password(&hash, &c.password)).await {
        Ok(right) => right,
        Err(res) => return res,
    };
    if !(known && right) {
        a.login_fails.charge(ip, 1, Instant::now());
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match a.start_session(&name, secure).await {
        Ok(cookie) => json_with_cookie(StatusCode::OK, &name, cookie),
        Err(e) => failed(e),
    }
}

/// `POST /api/logout`: end the session and clear the cookie. Always 204.
async fn logout(State(a): State<Arc<PasswordAuth>>, req: Request) -> Response {
    if let Some(v) = cookie_value(req.headers())
        && let Err(e) = a.db.session_delete(&session_key(v)).await
    {
        return failed(format!("{e:?}"));
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, set_cookie("", 0, is_https(req.headers())));
    res
}

/// `GET /api/me`: the signed-in account's name, or 401. Open: it is how a page asks.
async fn me(State(a): State<Arc<PasswordAuth>>, req: Request) -> Response {
    match a.identity(req.headers()).await {
        Some(i) => ([(header::CONTENT_TYPE, "application/json")], me_body(&i.name)).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// The open account routes (no token): sign-up, login, logout and who-am-I.
pub(crate) fn routes(auth: Arc<PasswordAuth>) -> Router {
    Router::new()
        .route("/api/signup", post(signup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .with_state(auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::MemLogStore;
    use tower::ServiceExt;

    fn auth(limits: Limits) -> Arc<PasswordAuth> {
        Arc::new(PasswordAuth::new(Db::Mem(Arc::new(MemLogStore::new())), limits, None))
    }

    async fn post_json(app: &Router, path: &str, body: &str) -> Response {
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        app.clone().oneshot(req).await.unwrap()
    }

    fn pair(user: &str, pw: &str) -> String {
        serde_json::json!({"username": user, "password": pw}).to_string()
    }

    /// The `name=value` part of a response's cookie, to send back.
    fn cookie_of(r: &Response) -> String {
        let c = r.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
        c.split(';').next().unwrap().to_owned()
    }

    async fn status_with_cookie(app: &Router, path: &str, cookie: &str) -> StatusCode {
        let req = Request::builder()
            .uri(path)
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    const PW: &str = "a long enough password";

    #[test]
    fn a_name_is_lowercase_letters_digits_underscore_and_dash_within_the_limits() {
        let l = Limits::DESIGN;
        assert_eq!(check_account("Ann_B-2", PW, &l).as_deref(), Ok("ann_b-2"));
        for bad in [
            "",
            "ab",
            "a b",
            "ann!",
            "ann\u{e9}",
            "../x",
            &"a".repeat(l.account_name_max_bytes + 1),
        ] {
            assert!(check_account(bad, PW, &l).is_err(), "{bad:?}");
        }
        assert!(check_account(&"a".repeat(l.account_name_max_bytes), PW, &l).is_ok());
        assert!(check_account(&"a".repeat(l.account_name_min_chars), PW, &l).is_ok());
    }

    #[test]
    fn a_password_must_reach_the_configured_minimum_and_stay_under_the_cap() {
        let l = Limits::DESIGN;
        assert!(check_account("ann", &"x".repeat(l.password_min_chars - 1), &l).is_err());
        assert!(check_account("ann", &"x".repeat(l.password_min_chars), &l).is_ok());
        assert!(check_account("ann", &"x".repeat(l.password_max_bytes), &l).is_ok());
        assert!(check_account("ann", &"x".repeat(l.password_max_bytes + 1), &l).is_err());
    }

    #[tokio::test]
    async fn signing_up_makes_an_account_and_signs_it_in_with_a_hardened_cookie() {
        let app = routes(auth(Limits::DESIGN));
        let r = post_json(&app, "/api/signup", &pair("Ann", PW)).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let set = r.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_owned();
        for part in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=1209600"] {
            assert!(set.contains(part), "{part} in {set}");
        }
        assert!(!set.contains("Secure"), "plain http: {set}");
        assert_eq!(status_with_cookie(&app, "/api/me", &cookie_of(&r)).await, StatusCode::OK);
        // The same pair logs in again, whatever the name's case.
        assert_eq!(post_json(&app, "/api/login", &pair("ANN", PW)).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_cookie_is_secure_behind_an_https_proxy() {
        let app = routes(auth(Limits::DESIGN));
        let req = Request::builder()
            .method("POST")
            .uri("/api/signup")
            .header("x-forwarded-proto", "https")
            .body(Body::from(pair("ann", PW)))
            .unwrap();
        let r = app.oneshot(req).await.unwrap();
        let set = r.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_owned();
        assert!(set.contains("; Secure"), "{set}");
    }

    #[tokio::test]
    async fn a_taken_name_in_any_case_is_refused_without_saying_whose_it_is() {
        let app = routes(auth(Limits::DESIGN));
        post_json(&app, "/api/signup", &pair("ann", PW)).await;
        let r = post_json(&app, "/api/signup", &pair("ANN", "another long password")).await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        let body =
            String::from_utf8(to_bytes(r.into_body(), 1024).await.unwrap().to_vec()).unwrap();
        assert_eq!(body, "that name is not available");
        // The first account's password still works; the second's does not.
        assert_eq!(post_json(&app, "/api/login", &pair("ann", PW)).await.status(), StatusCode::OK);
        let other = post_json(&app, "/api/login", &pair("ann", "another long password")).await;
        assert_eq!(other.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bad_names_short_passwords_and_bad_bodies_are_refused_and_make_nothing() {
        let app = routes(auth(Limits { signup_burst: 100, ..Limits::DESIGN }));
        for body in [
            pair("a", PW),
            pair("has space", PW),
            pair("ann", "short"),
            "{}".to_owned(),
            "nope".to_owned(),
        ] {
            let r = post_json(&app, "/api/signup", &body).await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        let huge = pair("ann", &"x".repeat(100_000));
        assert_eq!(
            post_json(&app, "/api/signup", &huge).await.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            post_json(&app, "/api/login", &pair("ann", PW)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_closed_server_refuses_sign_up_and_still_logs_existing_accounts_in() {
        let db = Db::Mem(Arc::new(MemLogStore::new()));
        let open = routes(Arc::new(PasswordAuth::new(db.clone(), Limits::DESIGN, None)));
        post_json(&open, "/api/signup", &pair("ann", PW)).await;
        let limits = Limits { signup_open: false, ..Limits::DESIGN };
        let closed = routes(Arc::new(PasswordAuth::new(db, limits, None)));
        let r = post_json(&closed, "/api/signup", &pair("bo", PW)).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            post_json(&closed, "/api/login", &pair("ann", PW)).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            post_json(&closed, "/api/login", &pair("bo", PW)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn one_address_may_sign_up_only_so_often() {
        let limits = Limits { signup_burst: 2, signup_refill_milli_per_sec: 1, ..Limits::DESIGN };
        let app = routes(auth(limits));
        for n in 0..2 {
            let r = post_json(&app, "/api/signup", &pair(&format!("user{n}"), PW)).await;
            assert_eq!(r.status(), StatusCode::CREATED);
        }
        let r = post_json(&app, "/api/signup", &pair("user2", PW)).await;
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(r.headers().contains_key(header::RETRY_AFTER));
        // Only sign-up spends that allowance; a login is another table.
        assert_eq!(
            post_json(&app, "/api/login", &pair("user0", PW)).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_wrong_password_and_an_unknown_name_look_the_same() {
        let app = routes(auth(Limits::DESIGN));
        post_json(&app, "/api/signup", &pair("ann", PW)).await;
        let wrong = post_json(&app, "/api/login", &pair("ann", "not the password!!")).await;
        let unknown = post_json(&app, "/api/login", &pair("nobody", PW)).await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(wrong.headers().len(), unknown.headers().len());
        assert!(!wrong.headers().contains_key(header::SET_COOKIE));
    }

    #[test]
    fn an_unknown_name_is_checked_against_a_real_hash_so_it_costs_the_same() {
        assert!(dummy_hash().starts_with("$argon2id$"), "{}", dummy_hash());
        assert!(!verify_password(dummy_hash(), PW));
    }

    /// Fails without the semaphore: every attempt would start its own argon2 (a flood from one
    /// address runs unbounded work before the failure count ever bites).
    #[tokio::test]
    async fn hashing_is_capped_so_a_flood_gets_503_not_unbounded_work() {
        let a = auth(Limits { password_hash_parallel: 1, ..Limits::DESIGN });
        let app = routes(Arc::clone(&a));
        let _busy = a.hashing.try_acquire().unwrap();
        for path in ["/api/login", "/api/signup"] {
            let r = post_json(&app, path, &pair("ann", PW)).await;
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
            assert!(r.headers().contains_key(header::RETRY_AFTER));
        }
    }

    /// Fails while the permit belongs to the request future: a client that disconnects mid-hash
    /// frees its slot while argon2 still runs, so send-and-disconnect exceeds the cap.
    #[tokio::test]
    async fn a_client_that_disconnects_mid_hash_keeps_its_slot_until_the_hash_ends() {
        let a = auth(Limits { password_hash_parallel: 1, ..Limits::DESIGN });
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel::<()>();
        let a2 = Arc::clone(&a);
        let request = tokio::spawn(async move {
            blocking(&a2, move || {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
            })
            .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap()).await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(a.hashing.available_permits(), 0, "the hash still runs, so its slot is taken");
        let r = post_json(&routes(Arc::clone(&a)), "/api/login", &pair("ann", PW)).await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        finish_tx.send(()).unwrap();
        drop(a.hashing.acquire().await.unwrap());
    }

    #[test]
    fn a_name_with_a_nul_or_any_other_stray_byte_never_reaches_the_store() {
        assert!(name_chars_ok("ann_b-2"));
        for bad in ["a\0b", "a b", "ann\u{e9}", "A"] {
            assert!(!name_chars_ok(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn failed_logins_are_limited_per_address_and_a_good_one_is_never_counted() {
        let limits =
            Limits { login_fail_burst: 2, login_fail_refill_milli_per_sec: 1, ..Limits::DESIGN };
        let app = routes(auth(limits));
        post_json(&app, "/api/signup", &pair("ann", PW)).await;
        for _ in 0..5 {
            assert_eq!(
                post_json(&app, "/api/login", &pair("ann", PW)).await.status(),
                StatusCode::OK
            );
        }
        for _ in 0..2 {
            let r = post_json(&app, "/api/login", &pair("ann", "wrong wrong wrong")).await;
            assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        }
        let r = post_json(&app, "/api/login", &pair("ann", "wrong wrong wrong")).await;
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        // Locked out: not even the right password is checked, so guessing past the limit gains nothing.
        assert_eq!(
            post_json(&app, "/api/login", &pair("ann", PW)).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn logging_out_ends_the_session_and_an_expired_one_is_dead() {
        let app = routes(auth(Limits::DESIGN));
        let r = post_json(&app, "/api/signup", &pair("ann", PW)).await;
        let cookie = cookie_of(&r);
        let req = Request::builder()
            .method("POST")
            .uri("/api/logout")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap();
        let out = app.clone().oneshot(req).await.unwrap();
        assert_eq!(out.status(), StatusCode::NO_CONTENT);
        assert!(
            out.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().contains("Max-Age=0")
        );
        assert_eq!(status_with_cookie(&app, "/api/me", &cookie).await, StatusCode::UNAUTHORIZED);

        // A session that outlived its time to live is not a login.
        let zero = Limits { session_ttl_secs: 0, ..Limits::DESIGN };
        let app = routes(auth(zero));
        let r = post_json(&app, "/api/signup", &pair("ann", PW)).await;
        assert_eq!(
            status_with_cookie(&app, "/api/me", &cookie_of(&r)).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_made_up_or_malformed_cookie_is_not_a_login() {
        let app = routes(auth(Limits::DESIGN));
        for cookie in [
            format!("{COOKIE}={}", "a".repeat(64)),
            format!("{COOKIE}=short"),
            format!("{COOKIE}='; DROP TABLE sessions"),
            "other=1".to_owned(),
        ] {
            assert_eq!(
                status_with_cookie(&app, "/api/me", &cookie).await,
                StatusCode::UNAUTHORIZED,
                "{cookie}"
            );
        }
    }

    #[tokio::test]
    async fn a_password_and_its_hash_never_appear_in_an_answer() {
        let app = routes(auth(Limits::DESIGN));
        let r = post_json(&app, "/api/signup", &pair("ann", PW)).await;
        let text = format!("{:?}", r.headers());
        let body =
            String::from_utf8(to_bytes(r.into_body(), 1024).await.unwrap().to_vec()).unwrap();
        assert!(!body.contains(PW) && !body.contains("argon2") && !text.contains(PW));
    }
}
