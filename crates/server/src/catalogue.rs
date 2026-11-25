//! The game catalogue's routes (#188, design in #173): games, their owners and their versions.
//!
//! Any signed-in account may make a game and is then its first owner. The catalogue is open to
//! every signed-in account to read; only an owner changes a game: adds or removes owners, posts
//! a version, withdraws one. A version names an `.apworld` already uploaded through
//! `PUT /api/apworlds` (which applies the upload limits and inspects it) and starts as
//! `checking`; the checks that make it live are #189.

use crate::accounts::{Identity, clean_name};
use crate::worlds::{Worlds, json_reply, percent_decode, raw_param};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post, put};
use log::{Game, GameVersion, NewVersion, OwnerRemoved, StoreError, VersionAdded, VersionState};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

type Reply = Result<Response, (StatusCode, String)>;

pub(crate) fn routes() -> Router<Arc<Worlds>> {
    Router::new()
        .route("/api/games", get(list_games).post(create_game))
        .route("/api/games/{game}", get(read_game))
        .route("/api/games/{game}/owners/{name}", put(add_owner).delete(remove_owner))
        .route("/api/games/{game}/versions", post(add_version))
        .route("/api/games/{game}/versions/{version}/withdraw", post(withdraw))
}

fn failed(e: &StoreError) -> (StatusCode, String) {
    tracing::error!(error = ?e, "catalogue");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error (see the server log)".to_owned())
}

fn bad(why: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, why.into())
}

fn no_game() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "no such game".to_owned())
}

fn not_owner() -> (StatusCode, String) {
    (StatusCode::FORBIDDEN, "only an owner of the game may do that".to_owned())
}

/// `raw` trimmed, 1 to `max` bytes, with no control characters (`multiline` allows line
/// breaks and tabs). A name or version is in a URL path, so it has no `/` and is not `.` or
/// `..` either.
fn text(raw: &str, field: &str, max: usize, multiline: bool) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() || t.len() > max {
        return Err(format!("{field} must be 1 to {max} bytes"));
    }
    let allowed = |c: char| multiline && matches!(c, '\n' | '\r' | '\t');
    if t.chars().any(|c| c.is_control() && !allowed(c)) {
        return Err(format!("{field} has a control character"));
    }
    if !multiline && (t.contains('/') || t == "." || t == "..") {
        return Err(format!("{field} may not contain / or be . or .."));
    }
    Ok(t.to_owned())
}

fn game_json(g: &Game) -> Value {
    json!({"name": g.name, "description": g.description, "created_by": g.created_by})
}

fn version_json(v: &GameVersion) -> Value {
    json!({
        "version": v.version, "note": v.note, "apworld": crate::hex(&v.apworld),
        "generator": v.generator, "state": v.state.as_str(), "error": v.error,
    })
}

/// The game and whether `who` owns it, or 404.
async fn owned_by(
    w: &Worlds,
    who: &Identity,
    name: &str,
) -> Result<(Game, bool), (StatusCode, String)> {
    let db = w.db();
    let game = db.game_get(name).await.map_err(|e| failed(&e))?.ok_or_else(no_game)?;
    let owners = db.game_owners(name).await.map_err(|e| failed(&e))?;
    let owner = owners.contains(&who.name);
    Ok((game, owner))
}

/// What a game route answers: the game, its owners, its versions (newest upload first) and
/// whether the caller owns it.
async fn page(w: &Worlds, who: &Identity, game: &Game, status: StatusCode) -> Reply {
    let db = w.db();
    let owners = db.game_owners(&game.name).await.map_err(|e| failed(&e))?;
    let versions = db.versions(&game.name).await.map_err(|e| failed(&e))?;
    let mut body = game_json(game);
    body["owner"] = json!(owners.contains(&who.name));
    body["owners"] = json!(owners);
    body["versions"] = versions.iter().map(version_json).collect();
    Ok(json_reply(status, &body))
}

/// `GET /api/games?limit=&after=`: a page of games ascending by name ignoring case,
/// `{items, next}`; `next` is the `after` of the following page, null on the last. `limit` is
/// at most `list_page_max`, which is also the default.
async fn list_games(State(w): State<Arc<Worlds>>, RawQuery(q): RawQuery) -> Reply {
    let q = q.unwrap_or_default();
    let param = |key: &str| raw_param(&q, key).and_then(percent_decode);
    let max = w.limits().list_page_max;
    let limit = param("limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(max).clamp(1, max);
    let after = param("after");
    // One more than asked says whether another page follows.
    let mut games = w.db().game_list(after.as_deref(), limit + 1).await.map_err(|e| failed(&e))?;
    let next = (games.len() > limit).then(|| {
        games.truncate(limit);
        games.last().map(|g| g.name.clone())
    });
    let items: Vec<Value> = games.iter().map(game_json).collect();
    Ok(json_reply(StatusCode::OK, &json!({"items": items, "next": next.flatten()})))
}

#[derive(Deserialize)]
struct NewGame {
    name: String,
    #[serde(default)]
    description: String,
}

/// `POST /api/games` with `{"name": ..., "description": ...}`: the caller makes the game and is
/// its first owner (201, the game's page). A name taken ignoring case is 409.
async fn create_game(State(w): State<Arc<Worlds>>, who: Identity, body: Bytes) -> Reply {
    let new: NewGame = serde_json::from_slice(&body)
        .map_err(|_| bad("expected {\"name\": text, \"description\": text}"))?;
    let l = w.limits();
    let name = text(&new.name, "name", l.catalogue_name_max_bytes, false).map_err(bad)?;
    let description = if new.description.trim().is_empty() {
        String::new()
    } else {
        text(&new.description, "description", l.catalogue_description_max_bytes, true)
            .map_err(bad)?
    };
    let game = Game { name, description, created_by: who.name.clone() };
    // The game and its first owner (the caller) go in together.
    if !w.db().game_create(&game).await.map_err(|e| failed(&e))? {
        return Err((StatusCode::CONFLICT, "a game of that name exists".to_owned()));
    }
    tracing::info!(game = game.name, by = who.name, "catalogue game made");
    page(&w, &who, &game, StatusCode::CREATED).await
}

/// `GET /api/games/{game}`: the game's page, or 404.
async fn read_game(State(w): State<Arc<Worlds>>, who: Identity, Path(name): Path<String>) -> Reply {
    let game = w.db().game_get(&name).await.map_err(|e| failed(&e))?.ok_or_else(no_game)?;
    page(&w, &who, &game, StatusCode::OK).await
}

/// `PUT /api/games/{game}/owners/{name}`: an owner makes an existing account an owner too; a
/// repeat changes nothing. Not an owner: 403; no such account: 404.
async fn add_owner(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path((game, name)): Path<(String, String)>,
) -> Reply {
    let (game, owner) = owned_by(&w, &who, &game).await?;
    if !owner {
        return Err(not_owner());
    }
    let no_user = || (StatusCode::NOT_FOUND, "no such user".to_owned());
    let name = clean_name(&name).ok_or_else(no_user)?;
    if !w.db().game_owner_add(&game.name, &name, &who.name).await.map_err(|e| failed(&e))? {
        return Err(no_user());
    }
    page(&w, &who, &game, StatusCode::OK).await
}

/// `DELETE /api/games/{game}/owners/{name}`: an owner removes an owner, itself included, but a
/// game keeps at least one (409). Not an owner: 403; `name` is none: 404.
async fn remove_owner(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path((game, name)): Path<(String, String)>,
) -> Reply {
    let (game, owner) = owned_by(&w, &who, &game).await?;
    if !owner {
        return Err(not_owner());
    }
    let gone = || (StatusCode::NOT_FOUND, "that account is not an owner".to_owned());
    let name = clean_name(&name).ok_or_else(gone)?;
    match w.db().game_owner_remove(&game.name, &name).await.map_err(|e| failed(&e))? {
        OwnerRemoved::Removed => page(&w, &who, &game, StatusCode::OK).await,
        OwnerRemoved::NotOwner => Err(gone()),
        OwnerRemoved::LastOwner => {
            Err((StatusCode::CONFLICT, "a game keeps at least one owner".to_owned()))
        }
    }
}

#[derive(Deserialize)]
struct PostedVersion {
    version: String,
    #[serde(default)]
    note: String,
    apworld: String,
}

/// `POST /api/games/{game}/versions` with `{"version": ..., "note": ..., "apworld": <id>}`: an
/// owner adds a version of an `.apworld` already uploaded, as `checking` (201, the game's
/// page). A version number the game has is 409; an id with no `.apworld` upload is 400.
async fn add_version(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path(game): Path<String>,
    body: Bytes,
) -> Reply {
    let (game, owner) = owned_by(&w, &who, &game).await?;
    if !owner {
        return Err(not_owner());
    }
    let posted: PostedVersion = serde_json::from_slice(&body)
        .map_err(|_| bad("expected {\"version\": text, \"note\": text, \"apworld\": upload id}"))?;
    let l = w.limits();
    let version =
        text(&posted.version, "version", l.catalogue_version_max_bytes, false).map_err(bad)?;
    let note = if posted.note.trim().is_empty() {
        String::new()
    } else {
        text(&posted.note, "note", l.catalogue_note_max_bytes, true).map_err(bad)?
    };
    let apworld = crate::manifests::key_of(&posted.apworld)
        .ok_or_else(|| bad("apworld must be an upload id (64 hex digits)"))?;
    // Not checked yet: the check records the generator it ran with (see #189).
    let new =
        NewVersion { game: game.name.clone(), version, note, apworld, generator: String::new() };
    match w.db().version_add(&new).await.map_err(|e| failed(&e))? {
        VersionAdded::Added => {}
        VersionAdded::NoGame => return Err(no_game()),
        VersionAdded::NoUpload => return Err(bad("no .apworld upload has that id")),
        VersionAdded::Taken => {
            return Err((StatusCode::CONFLICT, "the game already has that version".to_owned()));
        }
    }
    tracing::info!(
        game = game.name,
        version = new.version,
        by = who.name,
        "catalogue version added"
    );
    page(&w, &who, &game, StatusCode::CREATED).await
}

/// `POST /api/games/{game}/versions/{version}/withdraw`: an owner withdraws a version, for
/// good. New rooms stop using it; rooms made with it keep it. Again: no change. Not an
/// owner: 403; no such version: 404.
async fn withdraw(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path((game, version)): Path<(String, String)>,
) -> Reply {
    let (game, owner) = owned_by(&w, &who, &game).await?;
    if !owner {
        return Err(not_owner());
    }
    let db = w.db();
    let versions = db.versions(&game.name).await.map_err(|e| failed(&e))?;
    let found = versions.iter().find(|v| v.version == version);
    let found = found.ok_or_else(|| (StatusCode::NOT_FOUND, "no such version".to_owned()))?;
    if found.state != VersionState::Withdrawn {
        let set =
            db.version_set(&game.name, &version, VersionState::Withdrawn, &found.generator, None);
        // False only when it was withdrawn in between, which is the same outcome.
        set.await.map_err(|e| failed(&e))?;
        tracing::info!(game = game.name, version, by = who.name, "catalogue version withdrawn");
    }
    page(&w, &who, &game, StatusCode::OK).await
}
