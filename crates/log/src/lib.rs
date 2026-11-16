//! The log seam (doc 05 7.2, doc 02 D4): every read and write of a room's log goes through
//! [`LogStore`]. `MemLogStore` is the in-memory implementation for tests and the simulator;
//! Postgres comes later behind the same trait.
//!
//! Deviations from the doc sketch, both deliberate: no outbox rows yet, and `read_cursors`
//! exists because rehydration needs the stored applied cursors (D-02) and snapshots hold only
//! what the caller encodes.

mod mem;

use proto::{Epoch, Event, ItemSeq, RoomId, Seq, SlotNo};

pub use mem::MemLogStore;

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
}
