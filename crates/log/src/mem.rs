//! In-memory `LogStore`. One mutex makes the fence check and the write one atomic step.
use crate::{
    AppendBatch, AppendError, AppendReceipt, BlobKey, CursorUpdate, LogStore, Snapshot, StoreError,
};
use proto::{Epoch, Event, ItemSeq, RoomId, Seq, SlotNo};
use std::collections::BTreeMap;
use std::future::ready;
use std::sync::{Mutex, PoisonError};

#[derive(Debug, Default)]
struct Room {
    owner: Epoch,
    /// `events[i].seq == i + 1`.
    events: Vec<Event>,
    cursors: BTreeMap<SlotNo, ItemSeq>,
    snapshot: Option<Snapshot>,
}

impl Room {
    fn head(&self) -> Seq {
        Seq(u64::try_from(self.events.len()).unwrap_or(u64::MAX))
    }

    fn fence(&self, epoch: Epoch) -> Result<(), AppendError> {
        if epoch == self.owner { Ok(()) } else { Err(AppendError::Fenced { current: self.owner }) }
    }
}

/// Test and simulator store. Rooms start at owner epoch 0; [`MemLogStore::claim`] plays the
/// directory's `room_owner` row.
#[derive(Debug, Default)]
pub struct MemLogStore {
    rooms: Mutex<BTreeMap<RoomId, Room>>,
    /// Blobs (game packages) and room seeds: write once, like the tables.
    packages: Mutex<BTreeMap<BlobKey, Vec<u8>>>,
    seeds: Mutex<BTreeMap<RoomId, Vec<u8>>>,
}

/// Insert unless present, under the lock; what is stored after.
fn put_once<K: Ord>(m: &Mutex<BTreeMap<K, Vec<u8>>>, k: K, v: &[u8]) -> Vec<u8> {
    m.lock().unwrap_or_else(PoisonError::into_inner).entry(k).or_insert_with(|| v.to_vec()).clone()
}

/// A write-once seed: the same bytes again are fine, others are refused.
pub(crate) fn same_or_refused(stored: &[u8], seed: &[u8]) -> Result<(), StoreError> {
    if stored == seed {
        Ok(())
    } else {
        Err(StoreError::Corrupt("the room already has a different seed".to_owned()))
    }
}

fn get<K: Ord>(m: &Mutex<BTreeMap<K, Vec<u8>>>, k: &K) -> Option<Vec<u8>> {
    m.lock().unwrap_or_else(PoisonError::into_inner).get(k).cloned()
}

impl MemLogStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `f` on the room under the store lock, so a fence check and its write are one step.
    fn with<T>(&self, room: RoomId, f: impl FnOnce(&mut Room) -> T) -> T {
        // A panicking test thread must not hide the data from the assertions that follow.
        let mut g = self.rooms.lock().unwrap_or_else(PoisonError::into_inner);
        f(g.entry(room).or_default())
    }

    /// Drop a finished room's seed (the database keeps its rows; memory need not).
    pub fn forget_seed(&self, room: RoomId) {
        self.seeds.lock().unwrap_or_else(PoisonError::into_inner).remove(&room);
    }

    /// Replace a blob's bytes with no check: for tests of the read check (a tampered row).
    pub fn tamper_blob(&self, key: BlobKey, bytes: &[u8]) {
        self.packages.lock().unwrap_or_else(PoisonError::into_inner).insert(key, bytes.to_vec());
    }

    /// How many distinct blobs (game packages) are stored: for tests and metrics.
    #[must_use]
    pub fn blobs(&self) -> usize {
        self.packages.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Bump the room's owner epoch and return it (the directory's claim). Every older epoch is
    /// fenced from this call's return on.
    pub fn claim(&self, room: RoomId) -> Epoch {
        self.with(room, |r| {
            r.owner = Epoch(r.owner.0.saturating_add(1));
            r.owner
        })
    }
}

impl Room {
    fn append(&mut self, batch: &AppendBatch<'_>) -> Result<AppendReceipt, AppendError> {
        self.fence(batch.epoch)?;
        let mut expected = self.head().0;
        for e in batch.events {
            expected = expected
                .checked_add(1)
                .ok_or_else(|| AppendError::Corrupt("seq overflow".into()))?;
            if e.seq.0 != expected {
                return Err(AppendError::Corrupt(format!(
                    "expected seq {expected}, got {}",
                    e.seq.0
                )));
            }
        }
        self.events.extend_from_slice(batch.events);
        for c in batch.cursors {
            let v = self.cursors.entry(c.slot).or_default();
            *v = (*v).max(c.applied);
        }
        Ok(AppendReceipt { head: self.head() })
    }

    fn put_snapshot(&mut self, snap: Snapshot, epoch: Epoch) -> Result<(), AppendError> {
        self.fence(epoch)?;
        if snap.seq > self.head() {
            return Err(AppendError::Corrupt("snapshot ahead of the log head".into()));
        }
        if self.snapshot.as_ref().is_some_and(|old| snap.seq < old.seq) {
            return Err(AppendError::Corrupt("snapshot older than the stored one".into()));
        }
        self.snapshot = Some(snap);
        Ok(())
    }
}

// Nothing here awaits: every method is a lock, a copy and a ready future.
impl LogStore for MemLogStore {
    fn append(
        &self,
        batch: AppendBatch<'_>,
    ) -> impl Future<Output = Result<AppendReceipt, AppendError>> + Send {
        ready(self.with(batch.room, |r| r.append(&batch)))
    }

    fn read_tail(
        &self,
        room: RoomId,
        after: Seq,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Event>, StoreError>> + Send {
        let skip = usize::try_from(after.0).unwrap_or(usize::MAX);
        ready(Ok(self.with(room, |r| r.events.iter().skip(skip).take(limit).cloned().collect())))
    }

    fn read_range(
        &self,
        room: RoomId,
        from: Seq,
        to: Seq,
    ) -> impl Future<Output = Result<Vec<Event>, StoreError>> + Send {
        let skip = usize::try_from(from.0.saturating_sub(1)).unwrap_or(usize::MAX);
        ready(Ok(self.with(room, |r| {
            r.events.iter().skip(skip).take_while(|e| e.seq <= to).cloned().collect()
        })))
    }

    fn read_cursors(
        &self,
        room: RoomId,
    ) -> impl Future<Output = Result<Vec<CursorUpdate>, StoreError>> + Send {
        ready(Ok(self.with(room, |r| {
            r.cursors.iter().map(|(&slot, &applied)| CursorUpdate { slot, applied }).collect()
        })))
    }

    fn snapshot_get(
        &self,
        room: RoomId,
    ) -> impl Future<Output = Result<Option<Snapshot>, StoreError>> + Send {
        ready(Ok(self.with(room, |r| r.snapshot.clone())))
    }

    fn snapshot_put(
        &self,
        room: RoomId,
        snap: Snapshot,
        epoch: Epoch,
    ) -> impl Future<Output = Result<(), AppendError>> + Send {
        ready(self.with(room, |r| r.put_snapshot(snap, epoch)))
    }

    fn seed_put(
        &self,
        room: RoomId,
        seed: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        ready(same_or_refused(&put_once(&self.seeds, room, seed), seed))
    }

    fn seed_get(
        &self,
        room: RoomId,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        ready(Ok(get(&self.seeds, &room)))
    }
}

/// The in-memory blob store (tests and the no-database server).
impl crate::raw::RawBlobs for MemLogStore {
    fn put_raw(
        &self,
        key: &BlobKey,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        // Write once; a row whose bytes are not its key's (torn) is healed, as on Postgres.
        let mut packages = self.packages.lock().unwrap_or_else(PoisonError::into_inner);
        let row = packages.entry(*key).or_insert_with(|| bytes.to_vec());
        if crate::blob_key(row) != *key {
            bytes.clone_into(row);
        }
        ready(Ok(()))
    }

    fn get_raw(
        &self,
        key: &BlobKey,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        ready(Ok(get(&self.packages, key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlobStore, blob_key};

    /// The read check, and a put healing the tampered row (the `PgLogStore` side is in the
    /// server's restart tests).
    #[tokio::test]
    async fn a_tampered_blob_is_refused_on_read_and_healed_by_a_put() {
        let s = MemLogStore::new();
        let key = blob_key(b"package");
        s.put(&key, b"package").await.unwrap();
        s.tamper_blob(key, b"other bytes");
        assert!(matches!(s.get(&key).await, Err(StoreError::Corrupt(_))));
        s.put(&key, b"package").await.unwrap();
        assert_eq!(s.get(&key).await.unwrap().as_deref(), Some(&b"package"[..]));
    }

    /// `blobs` counts distinct keys: a second put of the same bytes adds none.
    #[tokio::test]
    async fn blobs_counts_each_distinct_blob_once() {
        let s = MemLogStore::new();
        assert_eq!(s.blobs(), 0);
        for b in [&b"one"[..], b"two", b"one"] {
            s.put(&blob_key(b), b).await.unwrap();
        }
        assert_eq!(s.blobs(), 2);
    }

    /// `forget_seed` drops only that room's seed, and the room can then take another one.
    #[tokio::test]
    async fn forget_seed_drops_only_that_rooms_seed() {
        let s = MemLogStore::new();
        let (a, b) = (RoomId(1), RoomId(2));
        s.seed_put(a, b"seed a").await.unwrap();
        s.seed_put(b, b"seed b").await.unwrap();
        s.forget_seed(a);
        assert_eq!(s.seed_get(a).await.unwrap(), None);
        assert_eq!(s.seed_get(b).await.unwrap().as_deref(), Some(&b"seed b"[..]));
        s.seed_put(a, b"new seed").await.unwrap();
        assert_eq!(s.seed_get(a).await.unwrap().as_deref(), Some(&b"new seed"[..]));
    }
}
