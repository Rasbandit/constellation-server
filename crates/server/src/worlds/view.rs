//! Reading a world: its status, its seed and its room, for the accounts that may see it.

use super::*;

pub(super) fn checked_id(id: &str) -> Result<(), (StatusCode, String)> {
    if valid_id(id) { Ok(()) } else { Err((StatusCode::BAD_REQUEST, "bad id".to_owned())) }
}

/// Job `id`'s status. A queued or running job not marked alive for `gen_stale_secs` reads as
/// `stalled`: its server stopped, or lost the store and may come back (then it reads as
/// before). Worked out here, never stored; past `gen_lost_secs` the sweep fails it.
pub(super) async fn read_status(w: &Worlds, id: &str) -> Result<Value, (StatusCode, String)> {
    checked_id(id)?;
    let found = w.store.job_status(id).await.map_err(store_error)?;
    let (text, ago) = found.ok_or((StatusCode::NOT_FOUND, "no such world".to_owned()))?;
    let mut status: Value = serde_json::from_str(&text)
        .map_err(|e| internal(format!("world {id}: bad status: {e}")))?;
    let unfinished = matches!(status["status"].as_str(), Some("queued" | "running"));
    if unfinished && ago >= Duration::from_secs(w.limits.gen_stale_secs) {
        status["status"] = json!("stalled");
    }
    Ok(status)
}

/// [`read_status`] for a caller: the world's requester, or anyone with a role on its listed room
/// (`room_admins::role_on`), may see it; everyone else gets the same 404 as for no such world.
pub(super) async fn read_status_for(
    w: &Worlds,
    who: &Identity,
    id: &str,
) -> Result<Value, (StatusCode, String)> {
    let status = read_status(w, id).await?;
    let mut allowed = status["owner"].as_str() == Some(who.subject.as_str());
    if let (false, Some((_, key, _))) = (allowed, hosted(w, &status)) {
        let role = crate::room_admins::role_on(w.db(), who, key.as_str()).await;
        allowed = role.map_err(|e| internal(format!("{e:?}")))?.is_some();
    }
    if allowed { Ok(status) } else { Err((StatusCode::NOT_FOUND, "no such world".to_owned())) }
}

/// The status, plus `room_key` and `address` once the host lists the world's room.
pub(super) async fn world_status(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    UrlPath(id): UrlPath<String>,
) -> Reply {
    let mut status = read_status_for(&w, &who, &id).await?;
    // The requester's account id is theirs to see; a room admin does not get the owner's.
    if status["owner"].as_str() != Some(who.subject.as_str()) {
        status.as_object_mut().map(|s| s.remove("owner"));
    }
    if let Some((_, key, address)) = hosted(&w, &status) {
        status["room_key"] = json!(key.as_str());
        status["address"] = json!(address);
    }
    Ok(json_reply(StatusCode::OK, &status))
}

pub(super) fn no_room() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "this world does not run as a room on this server".to_owned())
}

/// The room is listed but could not answer now (it failed to load, or stopped): try again.
pub(super) fn unavailable(why: impl core::fmt::Display) -> (StatusCode, String) {
    tracing::warn!(%why, "room view unavailable");
    (StatusCode::SERVICE_UNAVAILABLE, "the room is not available right now; try again".to_owned())
}

/// The world's room now ([`edge_compat::CompatRoom::summary`]), with its key and address. A
/// cold room is loaded to answer, as a connection would load it.
/// A finished world's seed, decoded from the store to its v1 JSON: the human view of what the
/// server keeps as binary (`docs/seed-bundle-v2.md`).
pub(super) async fn world_bundle(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    UrlPath(id): UrlPath<String>,
) -> Reply {
    let status = read_status_for(&w, &who, &id).await?;
    let seed =
        status["seed_name"].as_str().ok_or((StatusCode::NOT_FOUND, "not done".to_owned()))?;
    let hash = status["bundle_hash"].as_str().unwrap_or_default().to_owned();
    let importer = w.importer.as_ref().ok_or_else(no_room)?;
    let v1 = importer.export(crate::room_id(seed), hash).await.map_err(unavailable)?;
    Ok(json_text(StatusCode::OK, v1))
}

pub(super) async fn room_summary(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    UrlPath(id): UrlPath<String>,
) -> Reply {
    let status = read_status_for(&w, &who, &id).await?;
    let (folder, key, address) = hosted(&w, &status).ok_or_else(no_room)?;
    let (room, _visit) = folder.enter(key.clone()).await.map_err(unavailable)?;
    let mut summary = room.summary().await.map_err(|e| unavailable(format!("{e:?}")))?;
    summary["room_key"] = json!(key.as_str());
    summary["address"] = json!(address);
    Ok(json_reply(StatusCode::OK, &summary))
}

/// The world's room events as server-sent events, resuming after `Last-Event-ID`
/// ([`crate::room_view::events`]). The stream counts as a visit, so the room stays in memory
/// while someone watches it; at most `view_streams` are open at once.
pub(super) async fn room_events(
    State(w): State<Arc<Worlds>>,
    who: Identity,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Reply {
    let status = read_status_for(&w, &who, &id).await?;
    let (folder, key, _) = hosted(&w, &status).ok_or_else(no_room)?;
    let permit = Arc::clone(&w.view_streams).try_acquire_owned().map_err(|_| {
        (StatusCode::SERVICE_UNAVAILABLE, "too many room views are open; try again".to_owned())
    })?;
    let (room, visit) = folder.enter(key).await.map_err(unavailable)?;
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok()?.parse().ok())
        .map_or(proto::Seq(0), proto::Seq);
    let held = crate::room_view::Held { _visit: visit, _permit: permit };
    let lost = access_lost(Arc::clone(&w), headers, id);
    crate::room_view::events(room, held, after, w.limits.view_history_events, lost)
        .await
        .map_err(|e| unavailable(format!("{e:?}")))
}

/// Whether the request's account can still read world `id`, through the same
/// [`read_status_for`] as the open. Fails closed: no session, no account, a store error or a
/// bad row all read as "no".
pub(super) async fn still_allowed(w: &Worlds, headers: &HeaderMap, id: &str) -> bool {
    let Some(a) = &w.accounts else { return false };
    let Some(who) = a.identify(headers).await else { return false };
    read_status_for(w, &who, id).await.is_ok()
}

/// Completes the first time `check` says no, answers later than `every`, or does not answer in
/// `every`: a hung store ends the stream as a refusal does.
pub(super) async fn until_lost<F, Fut>(every: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    loop {
        tokio::time::sleep(every).await;
        if !matches!(tokio::time::timeout(every, check()).await, Ok(true)) {
            return;
        }
    }
}

/// Completes when the request's account can no longer read world `id` (see [`still_allowed`]),
/// checked every `view_access_recheck_ms`. Any failure to check ends the stream.
pub(super) async fn access_lost(w: Arc<Worlds>, headers: HeaderMap, id: String) {
    let every = Duration::from_millis(w.limits.view_access_recheck_ms.max(1));
    until_lost(every, || still_allowed(&w, &headers, &id)).await;
}
