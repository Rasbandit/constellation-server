//! What a signed-in account may do to a room: its role on it, the one rule every room route
//! asks, and the routes that list rooms and manage a room's admins.
//!
//! Roles are per room: the owner (`rooms.owner`, never removable) and room admins (rows of
//! `room_admins`). Both may add any existing account as an admin; the owner may remove any
//! admin and an admin only itself. There is no site-wide admin. A room the caller has no role
//! on answers 404, never 403, so room keys do not leak.

use crate::accounts::{Identity, clean_name, name_of};
use crate::store::Db;
use crate::worlds::Worlds;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use log::{OwnedRoom, StoreError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Owner,
    Admin,
}

impl Role {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
        }
    }
}

type Reply = Result<Response, (StatusCode, String)>;

/// The one answer to "what is `who` on room `key`": the room and the role, or None (no such
/// room, or no role on it: the caller cannot tell which).
pub(crate) async fn role_on(
    db: &Db,
    who: &Identity,
    key: &str,
) -> Result<Option<(OwnedRoom, Role)>, StoreError> {
    let Some(room) = db.room_by_key(key).await? else { return Ok(None) };
    if room.owner.as_deref() == Some(who.subject.as_str()) {
        return Ok(Some((room, Role::Owner)));
    }
    let admin = db.room_admins(key).await?.iter().any(|a| *a == who.name);
    Ok(admin.then_some((room, Role::Admin)))
}

/// Every room `who` owns or administers, newest first, each with its role.
pub(crate) async fn rooms_for(
    db: &Db,
    who: &Identity,
) -> Result<Vec<(OwnedRoom, Role)>, StoreError> {
    let owned = db.rooms_owned(Some(&who.subject)).await?;
    let mut all: Vec<_> = owned.into_iter().map(|r| (r, Role::Owner)).collect();
    for r in db.rooms_administered(&who.name).await? {
        if !all.iter().any(|(o, _)| o.room_key == r.room_key) {
            all.push((r, Role::Admin));
        }
    }
    all.sort_by_key(|(r, _)| r.age_secs);
    Ok(all)
}

pub(crate) fn routes() -> Router<Arc<Worlds>> {
    Router::new()
        .route("/api/rooms", get(list_rooms))
        .route("/api/rooms/{key}/admins", get(list_admins).post(add_admin))
        .route("/api/rooms/{key}/admins/{name}", put(put_admin).delete(remove_admin))
}

fn failed(e: &StoreError) -> (StatusCode, String) {
    tracing::error!(error = ?e, "room admins");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error (see the server log)".to_owned())
}

fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "no such room".to_owned())
}

fn json(status: StatusCode, v: &Value) -> Response {
    let mut r = (status, v.to_string()).into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/json"));
    r
}

/// `GET /api/rooms`: the rooms the caller owns or administers, each with the caller's `role`.
/// A room made before accounts has no owner and is listed to nobody.
async fn list_rooms(State(w): State<Arc<Worlds>>, who: Identity) -> Reply {
    let rooms = rooms_for(w.db(), &who).await.map_err(|e| failed(&e))?;
    let rooms: Vec<Value> = rooms
        .into_iter()
        .map(|(r, role)| {
            json!({
                "id": r.world_id, "name": r.seed_name, "room_key": r.room_key,
                "owner": r.owner.as_deref().map(name_of), "age_secs": r.age_secs,
                "role": role.as_str(),
            })
        })
        .collect();
    Ok(json(StatusCode::OK, &Value::Array(rooms)))
}

/// The room's owner, its admins and the caller's role: what every admins route answers.
async fn listing(db: &Db, room: &OwnedRoom, role: Role) -> Reply {
    let admins = db.room_admins(&room.room_key).await.map_err(|e| failed(&e))?;
    let owner = room.owner.as_deref().map(name_of);
    Ok(json(StatusCode::OK, &json!({"owner": owner, "admins": admins, "role": role.as_str()})))
}

async fn role_or_404(
    w: &Worlds,
    who: &Identity,
    key: &str,
) -> Result<(OwnedRoom, Role), (StatusCode, String)> {
    role_on(w.db(), who, key).await.map_err(|e| failed(&e))?.ok_or_else(not_found)
}

async fn list_admins(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path(key): Path<String>,
) -> Reply {
    let (room, role) = role_or_404(&w, &who, &key).await?;
    listing(w.db(), &room, role).await
}

#[derive(Deserialize)]
struct NewAdmin {
    username: String,
}

/// `POST /api/rooms/{key}/admins` with `{"username": ...}`.
async fn add_admin(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path(key): Path<String>,
    body: Bytes,
) -> Reply {
    let new: NewAdmin = serde_json::from_slice(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "expected {\"username\": name}".to_owned()))?;
    grant(&w, &who, &key, &new.username).await
}

/// `PUT /api/rooms/{key}/admins/{name}`: the same, by name.
async fn put_admin(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path((key, name)): Path<(String, String)>,
) -> Reply {
    grant(&w, &who, &key, &name).await
}

/// Owner or admin makes an existing account an admin; a repeat changes nothing, and the owner
/// already has every right (nothing is stored for it).
async fn grant(w: &Worlds, who: &Identity, key: &str, name: &str) -> Reply {
    let (room, role) = role_or_404(w, who, key).await?;
    let no_user = || (StatusCode::NOT_FOUND, "no such user".to_owned());
    let name = clean_name(name).ok_or_else(no_user)?;
    let is_owner = room.owner.as_deref().map(name_of) == Some(name.as_str());
    if !is_owner
        && !w.db().room_admin_add(key, &name, Some(&who.name)).await.map_err(|e| failed(&e))?
    {
        return Err(no_user());
    }
    listing(w.db(), &room, role).await
}

/// `DELETE /api/rooms/{key}/admins/{name}`: the owner removes any admin; an admin only itself
/// (403); the owner is never removable (409); someone who is no admin is 404.
async fn remove_admin(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    Path((key, name)): Path<(String, String)>,
) -> Reply {
    let (room, role) = role_or_404(&w, &who, &key).await?;
    let gone = || (StatusCode::NOT_FOUND, "that account is not an admin".to_owned());
    let name = clean_name(&name).ok_or_else(gone)?;
    if room.owner.as_deref().map(name_of) == Some(name.as_str()) {
        return Err((StatusCode::CONFLICT, "the owner cannot be removed".to_owned()));
    }
    if role == Role::Admin && name != who.name {
        return Err((StatusCode::FORBIDDEN, "an admin may only remove itself".to_owned()));
    }
    if !w.db().room_admin_remove(&key, &name).await.map_err(|e| failed(&e))? {
        return Err(gone());
    }
    // An admin who left no longer has a role: answer with what it can still see (nothing).
    match role_on(w.db(), &who, &key).await.map_err(|e| failed(&e))? {
        Some((room, role)) => listing(w.db(), &room, role).await,
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}
