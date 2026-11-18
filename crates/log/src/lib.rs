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

use proto::{Epoch, Event, ItemSeq, RoomId, Seq, SlotNo};

pub use mem::MemLogStore;
#[cfg(feature = "pg-tests")]
pub use pg::testing;
#[cfg(feature = "postgres")]
pub use pg::{Listed, OpenError, Opened, PgLogStore};

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

    /// Store a room's seed. Write once: a room that has a seed keeps it.
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
/// later an object store with a local cache sit behind. Read through [`blob_get`], the one
/// place a blob is checked against its key.
pub trait BlobStore: Send + Sync + 'static {
    /// Store `bytes` under `key` (their SHA-256). Idempotent: a stored key keeps its bytes.
    fn put(
        &self,
        key: &BlobKey,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The bytes stored under `key`, as stored; use [`blob_get`], which checks them.
    fn get_unchecked(
        &self,
        key: &BlobKey,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send;
}

/// The SHA-256 of `bytes`: the key [`BlobStore::put`] takes.
#[must_use]
pub fn blob_key(bytes: &[u8]) -> BlobKey {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// The blob stored under `key`, checked: bytes that do not hash to `key` (a tampered or torn
/// row) are [`StoreError::Corrupt`], never returned.
///
/// # Errors
/// When the store fails or the stored bytes do not match the key.
pub async fn blob_get<B: BlobStore>(b: &B, key: &BlobKey) -> Result<Option<Vec<u8>>, StoreError> {
    match b.get_unchecked(key).await? {
        Some(bytes) if blob_key(&bytes) != *key => {
            Err(StoreError::Corrupt("a stored blob does not match its key".to_owned()))
        }
        found => Ok(found),
    }
}

#[cfg(test)]
mod contract;
