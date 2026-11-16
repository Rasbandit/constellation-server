//! In-memory `LogStore`. One mutex makes the fence check and the write one atomic step.
use crate::{
    AppendBatch, AppendError, AppendReceipt, CursorUpdate, LogStore, Snapshot, StoreError,
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
        ready(Ok(self.with(room, |r| r.events.iter().skip(skip).take(limit).copied().collect())))
    }

    fn read_range(
        &self,
        room: RoomId,
        from: Seq,
        to: Seq,
    ) -> impl Future<Output = Result<Vec<Event>, StoreError>> + Send {
        let skip = usize::try_from(from.0.saturating_sub(1)).unwrap_or(usize::MAX);
        ready(Ok(self.with(room, |r| {
            r.events.iter().skip(skip).take_while(|e| e.seq <= to).copied().collect()
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
}

#[cfg(test)]
mod tests;
