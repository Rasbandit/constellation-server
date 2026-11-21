//! The log seam (doc 05 7.2, doc 02 D4): every read and write of a room's log goes through
//! [`LogStore`]. `MemLogStore` is the in-memory implementation for tests and the simulator;
//! `PgLogStore` (feature `postgres`, D-98) is the durable one. One contract suite
//! (`src/contract.rs`) runs against both, so they cannot drift.
//!
//! Deviations from the doc sketch, both deliberate: no outbox rows yet, and `read_cursors`
//! exists because rehydration needs the stored applied cursors (D-02) and snapshots hold only
//! what the caller encodes.

mod mem;
#[cfg(feature = "postgres")]
mod pg;

use core::time::Duration;
use proto::{Epoch, Event, ItemSeq, RoomId, Seq, SlotNo};

pub use mem::MemLogStore;
#[cfg(feature = "pg-tests")]
pub use pg::testing;
#[cfg(feature = "postgres")]
pub use pg::{Listed, OpenError, Opened, PgLogStore, World};

/// Opaque snapshot of room state as of room `seq` `seq` (the caller owns the encoding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The log position the snapshot includes.
    pub seq: Seq,
    /// Encoded state.
    pub data: Vec<u8>,
}

/// A batched applied-cursor write, committed with the events (a cursor update, not a log event).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorUpdate {
    /// The slot.
    pub slot: SlotNo,
    /// Its applied `item_seq`. The store keeps the maximum ever written.
    pub applied: ItemSeq,
}

/// One atomic append.
#[derive(Debug, Clone, Copy)]
pub struct AppendBatch<'a> {
    /// The room.
    pub room: RoomId,
    /// Fence: the append fails unless this is the room's current owner epoch.
    pub epoch: Epoch,
    /// Events, contiguous, the first being `head + 1`.
    pub events: &'a [Event],
    /// Cursor writes in the same transaction.
    pub cursors: &'a [CursorUpdate],
}

/// What a committed append reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendReceipt {
    /// The log head after the append.
    pub head: Seq,
}

/// Why an append (or snapshot write) failed. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendError {
    /// Another owner holds the room; this writer must stop and drop its state.
    Fenced {
        /// The directory's current epoch.
        current: Epoch,
    },
    /// The store could not be reached; retry with backoff.
    Unavailable,
    /// The batch is not a valid continuation of the log.
    Corrupt(String),
}

/// Why a read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The store could not be reached; retry with backoff.
    Unavailable,
    /// Stored data is inconsistent.
    Corrupt(String),
}

/// The storage seam. Native async fn in traits, generics at the composition root.
pub trait LogStore: Send + Sync + 'static {
    /// Append events and cursors atomically if `batch.epoch` is current. An old epoch never commits.
    fn append(
        &self,
        batch: AppendBatch<'_>,
    ) -> impl Future<Output = Result<AppendReceipt, AppendError>> + Send;

    /// Up to `limit` events with `seq > after`, in order.
    fn read_tail(
        &self,
        room: RoomId,
        after: Seq,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Event>, StoreError>> + Send;

    /// Events with `from <= seq <= to`, in order.
    fn read_range(
        &self,
        room: RoomId,
        from: Seq,
        to: Seq,
    ) -> impl Future<Output = Result<Vec<Event>, StoreError>> + Send;

    /// The stored applied cursors, one per slot that has any.
    fn read_cursors(
        &self,
        room: RoomId,
    ) -> impl Future<Output = Result<Vec<CursorUpdate>, StoreError>> + Send;

    /// The latest snapshot, if any.
    fn snapshot_get(
        &self,
        room: RoomId,
    ) -> impl Future<Output = Result<Option<Snapshot>, StoreError>> + Send;

    /// Store a snapshot, fenced like an append. The snapshot may not be ahead of the log head.
    fn snapshot_put(
        &self,
        room: RoomId,
        snap: Snapshot,
        epoch: Epoch,
    ) -> impl Future<Output = Result<(), AppendError>> + Send;

    /// Store a room's seed. Write once: storing the seed a room already has is a no-op, and a
    /// different one is refused ([`StoreError::Corrupt`]), never silently dropped.
    fn seed_put(
        &self,
        room: RoomId,
        seed: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// A room's seed, if one is stored.
    fn seed_get(
        &self,
        room: RoomId,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send;
}

/// A blob's content address: the SHA-256 of its bytes.
pub type BlobKey = [u8; 32];

/// Content-addressed immutable blobs (game packages): the seam a Postgres table, memory, or
/// later an object store with a local cache sit behind. Both directions check the key: [`put`]
/// refuses bytes that do not hash to it, [`get`] refuses stored bytes that do not (a tampered
/// or torn row). Implementations live in this crate (the raw side is not public).
///
/// [`put`]: BlobStore::put
/// [`get`]: BlobStore::get
pub trait BlobStore: raw::RawBlobs + Send + Sync + 'static {
    /// Store `bytes` under `key`, their SHA-256 ([`blob_key`]). Idempotent.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] when `key` is not the hash of `bytes`; the store failing.
    fn put(
        &self,
        key: &BlobKey,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let put = (blob_key(bytes) == *key).then(|| self.put_raw(key, bytes));
        async move {
            match put {
                Some(put) => put.await,
                None => {
                    Err(StoreError::Corrupt("a blob put under a key it does not hash to".into()))
                }
            }
        }
    }

    /// The blob stored under `key`, checked against it.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] when the stored bytes do not hash to `key`; the store failing.
    fn get(
        &self,
        key: &BlobKey,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        let raw = self.get_raw(key);
        let key = *key;
        async move {
            match raw.await? {
                Some(bytes) if blob_key(&bytes) != key => {
                    Err(StoreError::Corrupt("a stored blob does not match its key".to_owned()))
                }
                found => Ok(found),
            }
        }
    }
}

impl<T: raw::RawBlobs + Send + Sync + 'static> BlobStore for T {}

/// The unchecked side of a blob store: nameable only inside this crate, so every read and
/// write outside it goes through [`BlobStore`]'s checks.
pub(crate) mod raw {
    use super::{BlobKey, StoreError};

    /// Raw blob IO, unchecked.
    pub trait RawBlobs {
        /// Store the bytes, write once (a stored key keeps its bytes unless they were torn).
        fn put_raw(
            &self,
            key: &BlobKey,
            bytes: &[u8],
        ) -> impl Future<Output = Result<(), StoreError>> + Send;
        /// The bytes as stored.
        fn get_raw(
            &self,
            key: &BlobKey,
        ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send;
    }
}

/// What a manifest listing shows, kept beside the manifest so a listing never reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestRecord {
    /// The content address: the SHA-256 of the manifest.
    pub id: BlobKey,
    /// The uploaded file's name (shown only).
    pub name: String,
    /// The world's game name.
    pub game: String,
    /// The world's version.
    pub version: String,
    /// Manifest size in bytes.
    pub bytes: u64,
    /// Items in the world.
    pub items: u32,
    /// Locations in the world.
    pub locations: u32,
}

/// Uploaded world manifests, deduplicated by content address, with the listing columns beside
/// them. Memory and Postgres implement it; a listing is keyset-paged by id.
pub trait ManifestStore: Send + Sync + 'static {
    /// Store a manifest under `record.id` (its SHA-256). Idempotent: a manifest already stored
    /// keeps its record.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] when `record.id` is not the hash of `bytes`; the store failing.
    fn manifest_put(
        &self,
        record: &ManifestRecord,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// A manifest's record, without its bytes.
    ///
    /// # Errors
    /// The store failing.
    fn manifest_record(
        &self,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<ManifestRecord>, StoreError>> + Send;

    /// A manifest's bytes, checked against its id.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] when the stored bytes do not hash to `id`; the store failing.
    fn manifest_bytes(
        &self,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send;

    /// Up to `limit` records ascending by id, after `after`, of `game` when given.
    ///
    /// # Errors
    /// The store failing.
    fn manifest_list(
        &self,
        game: Option<&str>,
        after: Option<&BlobKey>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ManifestRecord>, StoreError>> + Send;
}

/// The kinds of upload a legacy generation reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UploadKind {
    /// A world file (`.apworld`).
    Apworld,
    /// A player file (`.yaml`).
    Yaml,
}

impl UploadKind {
    /// The kind's name in the store (`world_uploads.kind`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apworld => "apworld",
            Self::Yaml => "yaml",
        }
    }
}

/// An upload's record: what the upload API answers with, kept beside the file so a listing
/// never reads a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRecord {
    /// Which kind of upload.
    pub kind: UploadKind,
    /// The content address: the SHA-256 of the file.
    pub id: BlobKey,
    /// The game it is for, as one folder-style name (shown and grouped only).
    pub game: String,
    /// The record as JSON text, opaque to the store.
    pub meta: String,
}

/// Uploaded world and player files, deduplicated by kind and content address, with their
/// records beside them. Memory and Postgres implement it.
pub trait UploadStore: Send + Sync + 'static {
    /// Store a file under `record.id` (its SHA-256). Idempotent: a file already stored keeps
    /// its record.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] when `record.id` is not the hash of `bytes`; the store failing.
    fn upload_put(
        &self,
        record: &UploadRecord,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// An upload's record (JSON text), without its file.
    ///
    /// # Errors
    /// The store failing.
    fn upload_meta(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send;

    /// An upload's file, checked against its id.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] when the stored bytes do not hash to `id`; the store failing.
    fn upload_bytes(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send;

    /// Every record of `kind` (JSON text), ascending by id.
    ///
    /// # Errors
    /// The store failing.
    fn upload_list(
        &self,
        kind: UploadKind,
    ) -> impl Future<Output = Result<Vec<String>, StoreError>> + Send;

    /// Whether the upload is stored whole: present, and its file hashes to `id`. A missing or
    /// torn upload is `false`, not an error. Decided where the file is, so its bytes need not
    /// travel.
    ///
    /// # Errors
    /// The store failing.
    fn upload_intact(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;
}

/// Where a generation is (`world_jobs.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Waiting for a generation permit.
    Queued,
    /// Generating.
    Running,
    /// Finished with a world.
    Done,
    /// Finished without one.
    Failed,
}

impl JobState {
    /// The state's name in the store, as the status API shows it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// The state named `s`, if it is one.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Queued, Self::Running, Self::Done, Self::Failed]
            .into_iter()
            .find(|j| j.as_str() == s)
    }
}

#[cfg(test)]
#[test]
fn every_job_state_parses_from_its_name_and_nothing_else_does() {
    for j in [JobState::Queued, JobState::Running, JobState::Done, JobState::Failed] {
        assert_eq!(JobState::parse(j.as_str()), Some(j));
    }
    assert_eq!(JobState::parse("lost"), None);
}

/// World generations' statuses, shared by every server instance. Queued and running jobs are
/// marked alive by the process running them; one that stops being marked is stale.
pub trait JobStore: Send + Sync + 'static {
    /// Write job `id`'s status (JSON text, opaque to the store) and state, marking it alive.
    /// It replaces whatever is stored, a sweep's `failed` too: a server that lost the store
    /// for a while and then finished its job writes how the job really ended.
    ///
    /// # Errors
    /// The store failing.
    fn job_put(
        &self,
        id: &str,
        state: JobState,
        status: &str,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Write job `id`'s status only when the store has none for it; whether it was written.
    /// For a status an older server kept as a file, which must never overwrite a newer row.
    ///
    /// # Errors
    /// The store failing.
    fn job_put_if_absent(
        &self,
        id: &str,
        state: JobState,
        status: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Job `id`'s status, and how long ago it was last marked alive (or written).
    ///
    /// # Errors
    /// The store failing.
    fn job_status(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<(String, Duration)>, StoreError>> + Send;

    /// Mark job `id` alive, while it is queued or running.
    ///
    /// # Errors
    /// The store failing.
    fn job_touch(&self, id: &str) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Fail, in one step, every queued or running job not marked alive for `older_than`: the
    /// job's status (a JSON object) gets `"status": "failed"` and `"error": error`, the rest of
    /// it is kept. The failed ids, ascending.
    ///
    /// # Errors
    /// The store failing.
    fn job_fail_all_stale(
        &self,
        older_than: Duration,
        error: &str,
    ) -> impl Future<Output = Result<Vec<String>, StoreError>> + Send;

    /// Fail job `id` with `status`, only while it is still queued or running and not marked
    /// alive for `older_than` (its process may have marked it since it was listed); whether
    /// it was failed.
    ///
    /// # Errors
    /// The store failing.
    fn job_fail_stale(
        &self,
        id: &str,
        status: &str,
        older_than: Duration,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;
}

/// A hosted room as the account listing shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedRoom {
    /// The generation the web UI opens the room by (None for a room made before accounts).
    pub world_id: Option<String>,
    /// The room's seed name.
    pub seed_name: String,
    /// The room's public key.
    pub room_key: String,
    /// Who made it: a provider-neutral subject (None: no owner, only the admin lists it).
    pub owner: Option<String>,
    /// Seconds since the room was made.
    pub age_secs: u64,
}

/// Accounts, their logins and room ownership. Nothing here ever holds a password: `hash` is an
/// argon2id PHC string, and a session is found by the SHA-256 of its cookie value.
pub trait AccountStore: Send + Sync + 'static {
    /// Add account `name` (already normalized: lowercase) with password `hash`; false when the
    /// name is taken.
    ///
    /// # Errors
    /// The store failing.
    fn user_create(
        &self,
        name: &str,
        hash: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Account `name`'s password hash.
    ///
    /// # Errors
    /// The store failing.
    fn user_hash(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send;

    /// Start a login for `name`, found by `token_hash`, dead after `ttl`.
    ///
    /// # Errors
    /// The store failing.
    fn session_put(
        &self,
        token_hash: &[u8; 32],
        name: &str,
        ttl: Duration,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The account of a live (not expired) login.
    ///
    /// # Errors
    /// The store failing.
    fn session_user(
        &self,
        token_hash: &[u8; 32],
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send;

    /// End a login.
    ///
    /// # Errors
    /// The store failing.
    fn session_delete(
        &self,
        token_hash: &[u8; 32],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Record `room` (just listed) as made by `owner` (None: no account), and
    /// the world it came from. Write once: a room already recorded keeps its owner. Only
    /// `room_info.world_id` is read in Postgres, which has the rest in the room's row.
    ///
    /// # Errors
    /// The store failing.
    fn room_claim(
        &self,
        room: RoomId,
        owner: Option<&str>,
        room_info: &OwnedRoom,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The rooms `owner` made, newest first; with None, every listed room (the admin's view).
    ///
    /// # Errors
    /// The store failing.
    fn rooms_owned(
        &self,
        owner: Option<&str>,
    ) -> impl Future<Output = Result<Vec<OwnedRoom>, StoreError>> + Send;

    /// The listed room with public key `key`.
    ///
    /// # Errors
    /// The store failing.
    fn room_by_key(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<OwnedRoom>, StoreError>> + Send;

    /// The rooms account `username` is an admin of (not the ones it owns), newest first.
    ///
    /// # Errors
    /// The store failing.
    fn rooms_administered(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<Vec<OwnedRoom>, StoreError>> + Send;

    /// The accounts that are admins of room `key`, by name.
    ///
    /// # Errors
    /// The store failing.
    fn room_admins(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Vec<String>, StoreError>> + Send;

    /// Make account `username` an admin of room `key`, granted by account `granted_by`.
    /// Already an admin: kept as it was, still true. False when there is no
    /// such account or room.
    ///
    /// # Errors
    /// The store failing.
    fn room_admin_add(
        &self,
        key: &str,
        username: &str,
        granted_by: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// End `username`'s adminship of room `key`; false when it had none.
    ///
    /// # Errors
    /// The store failing.
    fn room_admin_remove(
        &self,
        key: &str,
        username: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Delete account `name`, with its logins and room adminships (its rooms stay, owned by a
    /// name no one can sign in as). No route does this yet; it is the store's rule.
    ///
    /// # Errors
    /// The store failing.
    fn user_delete(&self, name: &str) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// Whether an upload's `bytes` no longer hash to its `id` (a torn row).
#[must_use]
pub(crate) fn is_torn(bytes: &[u8], id: &BlobKey) -> bool {
    blob_key(bytes) != *id
}

/// What a read of upload `id` gives: its bytes, or [`StoreError::Corrupt`] when they are torn.
pub(crate) fn checked_upload(
    found: Option<Vec<u8>>,
    id: &BlobKey,
) -> Result<Option<Vec<u8>>, StoreError> {
    match found {
        Some(b) if is_torn(&b, id) => {
            Err(StoreError::Corrupt("a stored upload does not match its id".into()))
        }
        found => Ok(found),
    }
}

fn has_nul(v: &serde_json::Value) -> bool {
    use serde_json::Value::{Array, Object, String};
    match v {
        String(s) => s.contains('\0'),
        Array(a) => a.iter().any(has_nul),
        Object(m) => m.iter().any(|(k, v)| k.contains('\0') || has_nul(v)),
        _ => false,
    }
}

/// `status` (a job's JSON text) as failed with `error`; text that is not a JSON object is
/// replaced by one holding only those two fields.
pub(crate) fn failed_status(status: &str, error: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(status).unwrap_or_default();
    // Postgres's jsonb refuses a NUL, so such text is replaced there; the same here.
    if !v.is_object() || has_nul(&v) {
        v = serde_json::json!({});
    }
    v["status"] = serde_json::json!(JobState::Failed.as_str());
    v["error"] = serde_json::json!(error);
    v.to_string()
}

/// The SHA-256 of `bytes`: the key [`BlobStore::put`] takes.
#[must_use]
pub fn blob_key(bytes: &[u8]) -> BlobKey {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

#[cfg(test)]
mod contract;
