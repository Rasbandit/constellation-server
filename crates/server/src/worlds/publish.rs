//! A finished generation's seed as a room: its seed name claimed, the seed stored in the host,
//! and the room listed and recorded under the account that asked for it.

use super::*;

impl Worlds {
    /// Reserve `seed` for a world about to enter the rooms folder. The host lists one room per
    /// seed name, so a second world with a seed it already hosts would silently share the first
    /// one's room: refused instead.
    fn claim_seed(&self, seed: &str) -> Result<(), String> {
        let mut published = self.published.lock().unwrap_or_else(PoisonError::into_inner);
        let listed = self.rooms.as_ref().is_some_and(|(folder, _)| folder.key_of(seed).is_some());
        if listed || !published.insert(seed.to_owned()) {
            return Err(format!(
                "seed {seed:?} is already hosted as a room on this server; generate again for a \
                 new seed"
            ));
        }
        Ok(())
    }
}

/// Store a generated seed in the host's store and list it as a room (with `--rooms`), after its
/// seed name is claimed. The world's generator chose the name: a hosted one is refused, listed
/// or not.
pub(super) async fn publish_loaded(w: &Worlds, loaded: Loaded) -> Result<(), String> {
    let list = w.rooms.is_some();
    w.claim_seed(loaded.bundle.seed_name())?;
    let importer = w.importer.as_ref().ok_or("no host to store the seed in")?;
    importer.import(loaded, list).await?;
    Ok(())
}

/// The listed room of a finished world (`--rooms` only): its rooms folder, key and join address
/// (`{host}:{port}/{key}`). Found by the seed name in the world's status; the host lists each
/// seed once.
pub(super) fn hosted<'a>(
    w: &'a Worlds,
    status: &Value,
) -> Option<(&'a RoomsFolder, RoomKey, String)> {
    let (folder, public) = w.rooms.as_ref()?;
    let key = folder.key_of(status["seed_name"].as_str()?)?;
    let address = key.address(public);
    Some((folder, key, address))
}

/// Record the finished world's listed room under the account that asked for it, so `GET /api/rooms` finds it. A failure is logged: the
/// world is made, only its listing under that account is missing.
pub(super) async fn claim_room(w: &Worlds, status: &Value) {
    let (Some((_, key, _)), Some(seed)) = (hosted(w, status), status["seed_name"].as_str()) else {
        return;
    };
    let info = log::OwnedRoom {
        world_id: status["id"].as_str().map(str::to_owned),
        seed_name: seed.to_owned(),
        room_key: key.as_str().to_owned(),
        owner: None,
        age_secs: 0,
    };
    let owner = status["owner"].as_str();
    if let Err(e) = w.store.room_claim(crate::room_id(seed), owner, &info).await {
        tracing::error!(error = ?e, world = ?status["id"], "recording the room's owner failed");
    }
}
