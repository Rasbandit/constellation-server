//! The host's store as the admin API uses it: manifests, uploaded world and player files, and
//! generation statuses, over Postgres or memory. The one place that chooses between the two.

use log::{
    AccountStore, BlobKey, BlobStore, CatalogueStore, Game, GameVersion, JobState, JobStore,
    ManifestRecord, ManifestStore, MemLogStore, NewVersion, OwnedRoom, OwnerRemoved, PgLogStore,
    StoreError, UploadKind, UploadRecord, UploadStore, VersionAdded, VersionState,
};
use std::sync::Arc;
use std::time::Duration;

/// The host's store.
#[derive(Debug, Clone)]
pub(crate) enum Db {
    Pg(Arc<PgLogStore>),
    Mem(Arc<MemLogStore>),
}

/// Run `$call` on whichever store `$db` is.
macro_rules! on {
    ($db:expr, $s:ident => $call:expr) => {
        match $db {
            Db::Pg($s) => $call,
            Db::Mem($s) => $call,
        }
    };
}

impl Db {
    /// Postgres: what is stored here outlives the process.
    pub(crate) const fn is_durable(&self) -> bool {
        matches!(self, Self::Pg(_))
    }

    pub(crate) async fn upload_put(&self, r: &UploadRecord, b: &[u8]) -> Result<(), StoreError> {
        on!(self, s => s.upload_put(r, b).await)
    }

    pub(crate) async fn upload_meta(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<Option<String>, StoreError> {
        on!(self, s => s.upload_meta(kind, id).await)
    }

    pub(crate) async fn upload_bytes(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        on!(self, s => s.upload_bytes(kind, id).await)
    }

    pub(crate) async fn upload_intact(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.upload_intact(kind, id).await)
    }

    pub(crate) async fn upload_list(&self, kind: UploadKind) -> Result<Vec<String>, StoreError> {
        on!(self, s => s.upload_list(kind).await)
    }

    pub(crate) async fn job_put(
        &self,
        id: &str,
        state: JobState,
        status: &str,
    ) -> Result<(), StoreError> {
        on!(self, s => s.job_put(id, state, status).await)
    }

    pub(crate) async fn job_put_if_absent(
        &self,
        id: &str,
        state: JobState,
        status: &str,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.job_put_if_absent(id, state, status).await)
    }

    pub(crate) async fn job_status(
        &self,
        id: &str,
    ) -> Result<Option<(String, Duration)>, StoreError> {
        on!(self, s => s.job_status(id).await)
    }

    pub(crate) async fn job_touch(&self, id: &str) -> Result<(), StoreError> {
        on!(self, s => s.job_touch(id).await)
    }

    pub(crate) async fn job_fail_all_stale(
        &self,
        older: Duration,
        error: &str,
    ) -> Result<Vec<String>, StoreError> {
        on!(self, s => s.job_fail_all_stale(older, error).await)
    }

    pub(crate) async fn job_fail_stale(
        &self,
        id: &str,
        status: &str,
        older: Duration,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.job_fail_stale(id, status, older).await)
    }

    pub(crate) async fn manifest_put(
        &self,
        r: &ManifestRecord,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        on!(self, s => s.manifest_put(r, bytes).await)
    }

    pub(crate) async fn manifest_record(
        &self,
        id: &BlobKey,
    ) -> Result<Option<ManifestRecord>, StoreError> {
        on!(self, s => s.manifest_record(id).await)
    }

    pub(crate) async fn manifest_bytes(&self, id: &BlobKey) -> Result<Option<Vec<u8>>, StoreError> {
        on!(self, s => s.manifest_bytes(id).await)
    }

    pub(crate) async fn manifest_list(
        &self,
        game: Option<&str>,
        after: Option<&BlobKey>,
        limit: usize,
    ) -> Result<Vec<ManifestRecord>, StoreError> {
        on!(self, s => s.manifest_list(game, after, limit).await)
    }

    pub(crate) async fn prepared_put(
        &self,
        id: &BlobKey,
        layout: u32,
        package: &BlobKey,
    ) -> Result<(), StoreError> {
        on!(self, s => s.prepared_put(id, layout, package).await)
    }

    pub(crate) async fn prepared_key(
        &self,
        id: &BlobKey,
        layout: u32,
    ) -> Result<Option<BlobKey>, StoreError> {
        on!(self, s => s.prepared_key(id, layout).await)
    }

    /// Store a blob under its content hash (`log::BlobStore`).
    pub(crate) async fn blob_put(&self, key: &BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        on!(self, s => s.put(key, bytes).await)
    }

    /// A blob, checked against its key.
    pub(crate) async fn blob_get(&self, key: &BlobKey) -> Result<Option<Vec<u8>>, StoreError> {
        on!(self, s => s.get(key).await)
    }

    pub(crate) async fn user_create(&self, name: &str, hash: &str) -> Result<bool, StoreError> {
        on!(self, s => s.user_create(name, hash).await)
    }

    pub(crate) async fn user_hash(&self, name: &str) -> Result<Option<String>, StoreError> {
        on!(self, s => s.user_hash(name).await)
    }

    pub(crate) async fn session_put(
        &self,
        token_hash: &[u8; 32],
        name: &str,
        ttl: Duration,
    ) -> Result<(), StoreError> {
        on!(self, s => s.session_put(token_hash, name, ttl).await)
    }

    pub(crate) async fn session_user(
        &self,
        token_hash: &[u8; 32],
    ) -> Result<Option<String>, StoreError> {
        on!(self, s => s.session_user(token_hash).await)
    }

    pub(crate) async fn session_delete(&self, token_hash: &[u8; 32]) -> Result<(), StoreError> {
        on!(self, s => s.session_delete(token_hash).await)
    }

    pub(crate) async fn room_claim(
        &self,
        room: proto::RoomId,
        owner: Option<&str>,
        info: &OwnedRoom,
    ) -> Result<(), StoreError> {
        on!(self, s => s.room_claim(room, owner, info).await)
    }

    pub(crate) async fn rooms_owned(
        &self,
        owner: Option<&str>,
    ) -> Result<Vec<OwnedRoom>, StoreError> {
        on!(self, s => s.rooms_owned(owner).await)
    }

    #[cfg(test)]
    pub(crate) async fn user_delete(&self, name: &str) -> Result<(), StoreError> {
        on!(self, s => s.user_delete(name).await)
    }

    pub(crate) async fn room_by_key(&self, key: &str) -> Result<Option<OwnedRoom>, StoreError> {
        on!(self, s => s.room_by_key(key).await)
    }

    pub(crate) async fn rooms_administered(
        &self,
        username: &str,
    ) -> Result<Vec<OwnedRoom>, StoreError> {
        on!(self, s => s.rooms_administered(username).await)
    }

    pub(crate) async fn room_admins(&self, key: &str) -> Result<Vec<String>, StoreError> {
        on!(self, s => s.room_admins(key).await)
    }

    pub(crate) async fn room_admin_add(
        &self,
        key: &str,
        username: &str,
        granted_by: &str,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.room_admin_add(key, username, granted_by).await)
    }

    pub(crate) async fn room_admin_remove(
        &self,
        key: &str,
        username: &str,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.room_admin_remove(key, username).await)
    }

    pub(crate) async fn game_create(&self, game: &Game) -> Result<bool, StoreError> {
        on!(self, s => s.game_create(game).await)
    }

    pub(crate) async fn game_get(&self, name: &str) -> Result<Option<Game>, StoreError> {
        on!(self, s => s.game_get(name).await)
    }

    pub(crate) async fn game_list(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Game>, StoreError> {
        on!(self, s => s.game_list(after, limit).await)
    }

    pub(crate) async fn game_owner_add(
        &self,
        game: &str,
        username: &str,
        granted_by: &str,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.game_owner_add(game, username, granted_by).await)
    }

    pub(crate) async fn game_owner_remove(
        &self,
        game: &str,
        username: &str,
    ) -> Result<OwnerRemoved, StoreError> {
        on!(self, s => s.game_owner_remove(game, username).await)
    }

    pub(crate) async fn game_owners(&self, game: &str) -> Result<Vec<String>, StoreError> {
        on!(self, s => s.game_owners(game).await)
    }

    pub(crate) async fn version_add(&self, v: &NewVersion) -> Result<VersionAdded, StoreError> {
        on!(self, s => s.version_add(v).await)
    }

    pub(crate) async fn version_set(
        &self,
        game: &str,
        version: &str,
        state: VersionState,
        generator: &str,
        error: Option<&str>,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.version_set(game, version, state, generator, error).await)
    }

    pub(crate) async fn versions(&self, game: &str) -> Result<Vec<GameVersion>, StoreError> {
        on!(self, s => s.versions(game).await)
    }
}
