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
    manifests: Mutex<BTreeMap<BlobKey, (crate::ManifestRecord, Vec<u8>)>>,
    uploads: Mutex<BTreeMap<(crate::UploadKind, BlobKey), Upload>>,
    jobs: Mutex<BTreeMap<String, Job>>,
    users: Mutex<BTreeMap<String, String>>,
    /// Login (by token hash) to its account and its end.
    sessions: Mutex<BTreeMap<[u8; 32], (String, std::time::Instant)>>,
    /// Hosted rooms: the record and when it was made.
    owned: Mutex<BTreeMap<RoomId, (crate::OwnedRoom, std::time::Instant)>>,
    /// How many of the next `job_put`s fail, as a store that is down for a while does.
    job_put_failures: Mutex<usize>,
}

/// An upload's record and file.
type Upload = (crate::UploadRecord, Vec<u8>);

/// A job's state, status and when it was last marked alive.
type Job = (crate::JobState, String, std::time::Instant);

/// Queued or running, and not marked alive for `older_than`.
fn stale(job: &Job, older_than: core::time::Duration) -> bool {
    use crate::JobState::{Queued, Running};
    matches!(job.0, Queued | Running) && job.2.elapsed() >= older_than
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

impl MemLogStore {
    /// Damage a stored manifest's bytes, as a torn write would, so a test can see it healed.
    pub fn tear_manifest_for_tests(&self, id: &BlobKey) {
        let mut m = self.manifests.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, bytes)) = m.get_mut(id) {
            bytes.push(0);
        }
    }
}

impl crate::ManifestStore for MemLogStore {
    fn manifest_put(
        &self,
        record: &crate::ManifestRecord,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let ok = crate::blob_key(bytes) == record.id;
        if ok {
            let mut m = self.manifests.lock().unwrap_or_else(PoisonError::into_inner);
            let row = m.entry(record.id).or_insert_with(|| (record.clone(), bytes.to_vec()));
            // A torn row is healed, as on Postgres.
            if crate::is_torn(&row.1, &record.id) {
                bytes.clone_into(&mut row.1);
            }
        }
        ready(if ok {
            Ok(())
        } else {
            Err(StoreError::Corrupt("a manifest put under an id it does not hash to".into()))
        })
    }

    fn manifest_record(
        &self,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<crate::ManifestRecord>, StoreError>> + Send {
        let m = self.manifests.lock().unwrap_or_else(PoisonError::into_inner);
        ready(Ok(m.get(id).map(|(r, _)| r.clone())))
    }

    fn manifest_bytes(
        &self,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        let m = self.manifests.lock().unwrap_or_else(PoisonError::into_inner);
        ready(match m.get(id) {
            Some((_, b)) if crate::blob_key(b) != *id => {
                Err(StoreError::Corrupt("a stored manifest does not match its id".into()))
            }
            found => Ok(found.map(|(_, b)| b.clone())),
        })
    }

    fn manifest_list(
        &self,
        game: Option<&str>,
        after: Option<&BlobKey>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<crate::ManifestRecord>, StoreError>> + Send {
        use std::ops::Bound::{Excluded, Unbounded};
        let m = self.manifests.lock().unwrap_or_else(PoisonError::into_inner);
        let from = after.map_or(Unbounded, |a| Excluded(*a));
        let page = m
            .range((from, Unbounded))
            .map(|(_, (r, _))| r)
            .filter(|r| game.is_none_or(|g| r.game == g))
            .take(limit)
            .cloned()
            .collect();
        ready(Ok(page))
    }
}

impl MemLogStore {
    /// Damage a stored upload's bytes, as a torn write would, so a test can see the read
    /// check and the heal.
    pub fn tear_upload_for_tests(&self, kind: crate::UploadKind, id: &BlobKey) {
        let mut m = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, bytes)) = m.get_mut(&(kind, *id)) {
            bytes.push(0);
        }
    }
}

impl crate::UploadStore for MemLogStore {
    fn upload_put(
        &self,
        record: &crate::UploadRecord,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let ok = crate::blob_key(bytes) == record.id;
        if ok {
            let mut m = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
            let row = m
                .entry((record.kind, record.id))
                .or_insert_with(|| (record.clone(), bytes.to_vec()));
            // A torn row is healed, as on Postgres.
            if crate::is_torn(&row.1, &record.id) {
                bytes.clone_into(&mut row.1);
            }
        }
        ready(if ok {
            Ok(())
        } else {
            Err(StoreError::Corrupt("an upload put under an id it does not hash to".into()))
        })
    }

    fn upload_meta(
        &self,
        kind: crate::UploadKind,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send {
        let m = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        ready(Ok(m.get(&(kind, *id)).map(|(r, _)| r.meta.clone())))
    }

    fn upload_bytes(
        &self,
        kind: crate::UploadKind,
        id: &BlobKey,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        let m = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        ready(crate::checked_upload(m.get(&(kind, *id)).map(|(_, b)| b.clone()), id))
    }

    fn upload_intact(
        &self,
        kind: crate::UploadKind,
        id: &BlobKey,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send {
        let m = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        ready(Ok(m.get(&(kind, *id)).is_some_and(|(_, b)| !crate::is_torn(b, id))))
    }

    fn upload_list(
        &self,
        kind: crate::UploadKind,
    ) -> impl Future<Output = Result<Vec<String>, StoreError>> + Send {
        let m = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        let all = m.iter().filter(|((k, _), _)| *k == kind).map(|(_, (r, _))| r.meta.clone());
        ready(Ok(all.collect()))
    }
}

impl MemLogStore {
    /// Fail the next `n` `job_put`s with [`StoreError::Unavailable`].
    pub fn fail_job_puts_for_tests(&self, n: usize) {
        *self.job_put_failures.lock().unwrap_or_else(PoisonError::into_inner) = n;
    }

    /// Mark job `id` as last alive `by` ago, as if its process stopped then.
    pub fn age_job_for_tests(&self, id: &str, by: core::time::Duration) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(job) = jobs.get_mut(id) {
            job.2 = job.2.checked_sub(by).unwrap_or(job.2);
        }
    }
}

impl crate::JobStore for MemLogStore {
    fn job_put(
        &self,
        id: &str,
        state: crate::JobState,
        status: &str,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let mut failures = self.job_put_failures.lock().unwrap_or_else(PoisonError::into_inner);
        let down = failures.checked_sub(1).inspect(|n| *failures = *n).is_some();
        drop(failures);
        if down {
            return ready(Err(StoreError::Unavailable));
        }
        let job = (state, status.to_owned(), std::time::Instant::now());
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).insert(id.to_owned(), job);
        ready(Ok(()))
    }

    fn job_put_if_absent(
        &self,
        id: &str,
        state: crate::JobState,
        status: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let vacant = !jobs.contains_key(id);
        if vacant {
            jobs.insert(id.to_owned(), (state, status.to_owned(), std::time::Instant::now()));
        }
        ready(Ok(vacant))
    }

    fn job_status(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<(String, core::time::Duration)>, StoreError>> + Send
    {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        ready(Ok(jobs.get(id).map(|j| (j.1.clone(), j.2.elapsed()))))
    }

    fn job_touch(&self, id: &str) -> impl Future<Output = Result<(), StoreError>> + Send {
        use crate::JobState::{Queued, Running};
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(job) = jobs.get_mut(id).filter(|j| matches!(j.0, Queued | Running)) {
            job.2 = std::time::Instant::now();
        }
        ready(Ok(()))
    }

    fn job_fail_all_stale(
        &self,
        older_than: core::time::Duration,
        error: &str,
    ) -> impl Future<Output = Result<Vec<String>, StoreError>> + Send {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let mut failed = Vec::new();
        for (id, job) in jobs.iter_mut().filter(|(_, j)| stale(j, older_than)) {
            let status = crate::failed_status(&job.1, error);
            *job = (crate::JobState::Failed, status, std::time::Instant::now());
            failed.push(id.clone());
        }
        ready(Ok(failed))
    }

    fn job_fail_stale(
        &self,
        id: &str,
        status: &str,
        older_than: core::time::Duration,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let failed = match jobs.get_mut(id).filter(|j| stale(j, older_than)) {
            Some(job) => {
                *job = (crate::JobState::Failed, status.to_owned(), std::time::Instant::now());
                true
            }
            None => false,
        };
        ready(Ok(failed))
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

impl crate::AccountStore for MemLogStore {
    fn user_create(
        &self,
        name: &str,
        hash: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send {
        let mut users = self.users.lock().unwrap_or_else(PoisonError::into_inner);
        let vacant = !users.contains_key(name);
        if vacant {
            users.insert(name.to_owned(), hash.to_owned());
        }
        ready(Ok(vacant))
    }

    fn user_hash(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send {
        ready(Ok(self.users.lock().unwrap_or_else(PoisonError::into_inner).get(name).cloned()))
    }

    fn session_put(
        &self,
        token_hash: &[u8; 32],
        name: &str,
        ttl: core::time::Duration,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let end = std::time::Instant::now() + ttl;
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let now = std::time::Instant::now();
        sessions.retain(|_, (_, end)| *end > now);
        sessions.insert(*token_hash, (name.to_owned(), end));
        ready(Ok(()))
    }

    fn session_user(
        &self,
        token_hash: &[u8; 32],
    ) -> impl Future<Output = Result<Option<String>, StoreError>> + Send {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let live = sessions.get(token_hash).filter(|(_, end)| *end > std::time::Instant::now());
        ready(Ok(live.map(|(name, _)| name.clone())))
    }

    fn session_delete(
        &self,
        token_hash: &[u8; 32],
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(token_hash);
        ready(Ok(()))
    }

    fn room_claim(
        &self,
        room: RoomId,
        owner: &str,
        room_info: &crate::OwnedRoom,
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        let mut record = room_info.clone();
        record.owner = Some(owner.to_owned());
        let mut owned = self.owned.lock().unwrap_or_else(PoisonError::into_inner);
        owned.entry(room).or_insert((record, std::time::Instant::now()));
        ready(Ok(()))
    }

    fn rooms_owned(
        &self,
        owner: Option<&str>,
    ) -> impl Future<Output = Result<Vec<crate::OwnedRoom>, StoreError>> + Send {
        let owned = self.owned.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rooms: Vec<_> = owned
            .values()
            .filter(|(r, _)| owner.is_none_or(|o| r.owner.as_deref() == Some(o)))
            .map(|(r, made)| crate::OwnedRoom { age_secs: made.elapsed().as_secs(), ..r.clone() })
            .collect();
        rooms.sort_by_key(|r| r.age_secs);
        ready(Ok(rooms))
    }
}
