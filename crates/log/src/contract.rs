//! The `LogStore` contract: one suite, run against `MemLogStore` always and against
//! `PgLogStore` with `--features pg-tests` (needs `CONSTELLATION_DATABASE_URL`; without it the
//! Postgres tests fail, they never skip). Each Postgres test gets its own schema.
//!
//! The race tests (fence race, cancel safety) are gate G-DB items (doc 11 section 4.8).
#![allow(clippy::arithmetic_side_effects, reason = "test counters stay far below u64::MAX")]
use crate::{
    AppendBatch, AppendError, AppendReceipt, BlobStore, CursorUpdate, JobState, JobStore, LogStore,
    ManifestRecord, ManifestStore, MemLogStore, Snapshot, StoreError, UploadKind, UploadRecord,
    UploadStore, blob_key,
};
use proto::{Cid, Epoch, Event, EventKind, ItemSeq, RoomId, Seq, SlotNo, Timestamp};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

const R: RoomId = RoomId(1);

/// A store plus the directory's claim, which is not part of the trait.
trait Store:
    LogStore + BlobStore + ManifestStore + UploadStore + JobStore + crate::AccountStore
{
    fn bump(&self, room: RoomId) -> impl Future<Output = Epoch> + Send;
    /// Change a stored upload's file without its id: a torn row.
    fn tear_for_tests(
        &self,
        kind: UploadKind,
        id: &crate::BlobKey,
    ) -> impl Future<Output = ()> + Send;
    /// Change a stored manifest's bytes without its id: a torn row.
    fn tear_manifest_for_tests(&self, id: &crate::BlobKey) -> impl Future<Output = ()> + Send;
    /// Mark job `id` as last alive `by` ago.
    fn age_job_for_tests(&self, id: &str, by: Duration) -> impl Future<Output = ()> + Send;
}

impl Store for MemLogStore {
    fn bump(&self, room: RoomId) -> impl Future<Output = Epoch> + Send {
        std::future::ready(self.claim(room))
    }

    fn tear_for_tests(
        &self,
        kind: UploadKind,
        id: &crate::BlobKey,
    ) -> impl Future<Output = ()> + Send {
        self.tear_upload_for_tests(kind, id);
        std::future::ready(())
    }

    fn tear_manifest_for_tests(&self, id: &crate::BlobKey) -> impl Future<Output = ()> + Send {
        Self::tear_manifest_for_tests(self, id);
        std::future::ready(())
    }

    fn age_job_for_tests(&self, id: &str, by: Duration) -> impl Future<Output = ()> + Send {
        Self::age_job_for_tests(self, id, by);
        std::future::ready(())
    }
}

#[cfg(feature = "pg-tests")]
impl Store for crate::PgLogStore {
    async fn bump(&self, room: RoomId) -> Epoch {
        self.claim(room).await.unwrap()
    }

    async fn tear_for_tests(&self, kind: UploadKind, id: &crate::BlobKey) {
        self.tear_upload_for_tests(kind, id).await;
    }

    async fn tear_manifest_for_tests(&self, id: &crate::BlobKey) {
        Self::tear_manifest_for_tests(self, id).await;
    }

    async fn age_job_for_tests(&self, id: &str, by: Duration) {
        Self::age_job_for_tests(self, id, by).await;
    }
}

fn ev(seq: u64) -> Event {
    Event {
        seq: Seq(seq),
        slot: Some(SlotNo(1)),
        kind: EventKind::Checked { cid: Cid(seq), location: 0, at: Timestamp(0) },
    }
}

fn evs(seqs: impl IntoIterator<Item = u64>) -> Vec<Event> {
    seqs.into_iter().map(ev).collect()
}

fn cur(slot: u32, applied: u64) -> CursorUpdate {
    CursorUpdate { slot: SlotNo(slot), applied: ItemSeq(applied) }
}

async fn put<S: Store>(
    s: &S,
    room: RoomId,
    epoch: Epoch,
    events: &[Event],
    cursors: &[CursorUpdate],
) -> Result<AppendReceipt, AppendError> {
    s.append(AppendBatch { room, epoch, events, cursors }).await
}

async fn append<S: Store>(
    s: &S,
    epoch: Epoch,
    events: &[Event],
) -> Result<AppendReceipt, AppendError> {
    put(s, R, epoch, events, &[]).await
}

async fn log<S: Store>(s: &S, room: RoomId) -> Vec<Event> {
    s.read_tail(room, Seq(0), usize::MAX).await.unwrap()
}

fn snap(seq: u64, data: Vec<u8>) -> Snapshot {
    Snapshot { seq: Seq(seq), data }
}

async fn an_empty_room_reads_as_empty<S: Store>(s: Arc<S>) {
    assert_eq!(s.read_tail(R, Seq(0), 10).await.unwrap(), []);
    assert_eq!(s.read_range(R, Seq(1), Seq(9)).await.unwrap(), []);
    assert_eq!(s.read_cursors(R).await.unwrap(), []);
    assert_eq!(s.snapshot_get(R).await.unwrap(), None);
}

async fn appended_events_read_back_as_tail_and_range<S: Store>(s: Arc<S>) {
    let e = s.bump(R).await;
    assert_eq!(append(&*s, e, &evs(1..=3)).await.unwrap().head, Seq(3));
    assert_eq!(append(&*s, e, &[ev(4)]).await.unwrap().head, Seq(4));
    assert_eq!(s.read_tail(R, Seq(1), 2).await.unwrap(), evs(2..=3));
    assert_eq!(s.read_tail(R, Seq(4), 2).await.unwrap(), []);
    assert_eq!(s.read_tail(R, Seq(0), 0).await.unwrap(), []);
    assert_eq!(s.read_tail(R, Seq(u64::MAX), usize::MAX).await.unwrap(), []);
    assert_eq!(s.read_range(R, Seq(2), Seq(3)).await.unwrap(), evs(2..=3));
    assert_eq!(s.read_range(R, Seq(0), Seq(1)).await.unwrap(), [ev(1)]);
    assert_eq!(s.read_range(R, Seq(3), Seq(2)).await.unwrap(), []);
    assert_eq!(s.read_range(R, Seq(3), Seq(u64::MAX)).await.unwrap(), evs(3..=4));
}

async fn every_event_kind_and_extreme_ids_read_back_unchanged<S: Store>(s: Arc<S>) {
    let e = s.bump(R).await;
    let events = [
        Event {
            seq: Seq(1),
            slot: Some(SlotNo(u32::MAX)),
            kind: EventKind::Checked {
                cid: Cid(u64::MAX),
                location: i64::MIN,
                at: Timestamp(u64::MAX),
            },
        },
        Event {
            seq: Seq(2),
            slot: Some(SlotNo(0)),
            kind: EventKind::Item {
                from: SlotNo(7),
                location: -1,
                item: i64::MAX,
                item_seq: ItemSeq(u64::MAX),
            },
        },
        Event { seq: Seq(3), slot: None, kind: EventKind::Goal },
    ];
    append(&*s, e, &events).await.unwrap();
    assert_eq!(log(&*s, R).await, events);
}

async fn an_old_epoch_is_fenced_and_writes_nothing<S: Store>(s: Arc<S>) {
    let old = s.bump(R).await;
    append(&*s, old, &[ev(1)]).await.unwrap();
    let new = s.bump(R).await;
    assert_eq!(append(&*s, old, &[ev(2)]).await, Err(AppendError::Fenced { current: new }));
    assert_eq!(log(&*s, R).await, [ev(1)]);
    assert_eq!(append(&*s, new, &[ev(2)]).await.unwrap().head, Seq(2));
}

async fn a_future_or_unclaimed_epoch_is_fenced_too<S: Store>(s: Arc<S>) {
    assert_eq!(
        append(&*s, Epoch(5), &[ev(1)]).await,
        Err(AppendError::Fenced { current: Epoch(0) })
    );
    assert_eq!(
        append(&*s, Epoch(u64::MAX), &[ev(1)]).await,
        Err(AppendError::Fenced { current: Epoch(0) })
    );
    let e = s.bump(R).await;
    assert_eq!(e, Epoch(1), "the first claim is epoch 1");
    assert_eq!(append(&*s, Epoch(2), &[ev(1)]).await, Err(AppendError::Fenced { current: e }));
    assert_eq!(log(&*s, R).await, []);
}

async fn an_unclaimed_room_takes_epoch_zero_until_its_first_claim<S: Store>(s: Arc<S>) {
    assert_eq!(append(&*s, Epoch(0), &[ev(1)]).await.unwrap().head, Seq(1));
    let e = s.bump(R).await;
    assert_eq!(append(&*s, Epoch(0), &[ev(2)]).await, Err(AppendError::Fenced { current: e }));
    assert_eq!(append(&*s, e, &[ev(2)]).await.unwrap().head, Seq(2));
}

async fn a_fenced_batch_does_not_move_cursors<S: Store>(s: Arc<S>) {
    let old = s.bump(R).await;
    s.bump(R).await;
    assert!(put(&*s, R, old, &[], &[cur(1, 3)]).await.is_err());
    assert_eq!(s.read_cursors(R).await.unwrap(), []);
}

async fn a_fenced_batch_with_a_gap_is_fenced_not_corrupt<S: Store>(s: Arc<S>) {
    let old = s.bump(R).await;
    let new = s.bump(R).await;
    assert_eq!(append(&*s, old, &[ev(5)]).await, Err(AppendError::Fenced { current: new }));
}

async fn a_gap_a_replay_or_a_bad_start_is_corrupt_and_writes_nothing<S: Store>(s: Arc<S>) {
    let e = s.bump(R).await;
    append(&*s, e, &[ev(1)]).await.unwrap();
    for bad in [
        vec![ev(3)],
        vec![ev(1)],
        vec![ev(2), ev(4)],
        vec![ev(0)],
        vec![ev(2), ev(2)],
        vec![ev(3), ev(2)],
    ] {
        let r = put(&*s, R, e, &bad, &[cur(1, 9)]).await;
        assert!(matches!(r, Err(AppendError::Corrupt(_))), "{bad:?}: {r:?}");
    }
    assert_eq!(log(&*s, R).await, [ev(1)]);
    assert_eq!(s.read_cursors(R).await.unwrap(), [], "a corrupt batch moved a cursor");
}

async fn an_empty_batch_is_fine_and_cursors_only_ever_rise<S: Store>(s: Arc<S>) {
    let e = s.bump(R).await;
    assert_eq!(put(&*s, R, e, &[], &[]).await.unwrap().head, Seq(0));
    for applied in [5, 2, 7, 7] {
        put(&*s, R, e, &[], &[cur(1, applied)]).await.unwrap();
    }
    assert_eq!(s.read_cursors(R).await.unwrap(), [cur(1, 7)]);
}

async fn duplicate_slots_in_one_batch_keep_the_highest_and_slots_read_in_order<S: Store>(
    s: Arc<S>,
) {
    let e = s.bump(R).await;
    put(&*s, R, e, &[ev(1)], &[cur(3, 2), cur(1, 4), cur(3, 9), cur(3, 1), cur(0, 0)])
        .await
        .unwrap();
    assert_eq!(s.read_cursors(R).await.unwrap(), [cur(0, 0), cur(1, 4), cur(3, 9)]);
}

async fn snapshots_are_fenced_and_cannot_be_ahead_of_the_log<S: Store>(s: Arc<S>) {
    let old = s.bump(R).await;
    append(&*s, old, &[ev(1)]).await.unwrap();
    assert!(matches!(
        s.snapshot_put(R, snap(2, vec![1, 2]), old).await,
        Err(AppendError::Corrupt(_))
    ));
    s.snapshot_put(R, snap(1, vec![1, 2]), old).await.unwrap();
    assert_eq!(s.snapshot_get(R).await.unwrap(), Some(snap(1, vec![1, 2])));
    let new = s.bump(R).await;
    assert_eq!(
        s.snapshot_put(R, snap(0, vec![1, 2]), old).await,
        Err(AppendError::Fenced { current: new })
    );
    assert_eq!(s.snapshot_get(R).await.unwrap(), Some(snap(1, vec![1, 2])));
}

async fn an_older_snapshot_never_replaces_a_newer_one<S: Store>(s: Arc<S>) {
    let e = s.bump(R).await;
    append(&*s, e, &evs(1..=2)).await.unwrap();
    s.snapshot_put(R, snap(2, vec![2]), e).await.unwrap();
    assert!(matches!(s.snapshot_put(R, snap(1, vec![1]), e).await, Err(AppendError::Corrupt(_))));
    assert_eq!(s.snapshot_get(R).await.unwrap(), Some(snap(2, vec![2])));
    // The same position may be rewritten (its cursors can have moved), with an empty body too.
    s.snapshot_put(R, snap(2, vec![]), e).await.unwrap();
    assert_eq!(s.snapshot_get(R).await.unwrap(), Some(snap(2, vec![])));
}

/// One package for two rooms of a game: stored once, read back whole; a second put of the
/// same key is a no-op, a put under a key the bytes do not hash to is refused.
async fn blobs_are_stored_once_by_key_and_checked_on_write<S: Store>(s: Arc<S>) {
    let (a, b) = (b"package a".to_vec(), b"package b".to_vec());
    let (ka, kb) = (blob_key(&a), blob_key(&b));
    assert_eq!(s.get(&ka).await.unwrap(), None);
    s.put(&ka, &a).await.unwrap();
    s.put(&ka, &a).await.unwrap();
    assert_eq!(s.get(&ka).await.unwrap(), Some(a.clone()));
    assert!(matches!(s.put(&kb, &a).await, Err(StoreError::Corrupt(_))));
    assert_eq!(s.get(&kb).await.unwrap(), None, "the refused put wrote nothing");
}

fn manifest(name: &str, game: &str, body: &[u8]) -> ManifestRecord {
    ManifestRecord {
        id: blob_key(body),
        name: name.to_owned(),
        game: game.to_owned(),
        version: "1".to_owned(),
        bytes: body.len() as u64,
        items: 2,
        locations: 3,
    }
}

/// A manifest is stored once by id with its listing columns, read back whole and checked on
/// write; a listing pages by id after a cursor, filters by game, and never holds the bytes.
async fn manifests_are_deduplicated_checked_and_listed_in_pages<S: Store>(s: Arc<S>) {
    let bodies: Vec<Vec<u8>> = (0..5).map(|i| format!("manifest {i}").into_bytes()).collect();
    let mut want: Vec<ManifestRecord> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| manifest(&format!("m{i}"), if i % 2 == 0 { "Even" } else { "Odd" }, b))
        .collect();
    for (r, b) in want.iter().zip(&bodies) {
        s.manifest_put(r, b).await.unwrap();
    }
    // The same manifest again keeps its first record and bytes.
    s.manifest_put(&manifest("renamed", "Other", &bodies[0]), &bodies[0]).await.unwrap();
    assert_eq!(s.manifest_record(&want[0].id).await.unwrap().as_ref(), Some(&want[0]));
    assert_eq!(s.manifest_bytes(&want[0].id).await.unwrap(), Some(bodies[0].clone()));
    assert_eq!(s.manifest_record(&blob_key(b"none")).await.unwrap(), None);
    assert_eq!(s.manifest_bytes(&blob_key(b"none")).await.unwrap(), None);
    // A put under an id the bytes do not hash to is refused and writes nothing.
    let lie = manifest("lie", "Even", b"other bytes");
    assert!(matches!(s.manifest_put(&lie, b"not those").await, Err(StoreError::Corrupt(_))));
    assert_eq!(s.manifest_record(&lie.id).await.unwrap(), None);
    // A torn row is refused on read and healed by the next put of the right bytes.
    s.tear_manifest_for_tests(&want[0].id).await;
    assert!(matches!(s.manifest_bytes(&want[0].id).await, Err(StoreError::Corrupt(_))), "torn");
    s.manifest_put(&want[0], &bodies[0]).await.unwrap();
    assert_eq!(s.manifest_bytes(&want[0].id).await.unwrap(), Some(bodies[0].clone()), "healed");

    want.sort_by_key(|r| r.id);
    let first = s.manifest_list(None, None, 2).await.unwrap();
    assert_eq!(first, want[..2]);
    let next = s.manifest_list(None, Some(&first[1].id), 2).await.unwrap();
    assert_eq!(next, want[2..4]);
    let rest = s.manifest_list(None, Some(&next[1].id), 10).await.unwrap();
    assert_eq!(rest, want[4..]);
    let odd: Vec<_> = want.iter().filter(|r| r.game == "Odd").cloned().collect();
    assert_eq!(s.manifest_list(Some("Odd"), None, 10).await.unwrap(), odd);
    assert_eq!(s.manifest_list(Some("None"), None, 10).await.unwrap(), []);
    assert_eq!(s.manifest_list(None, None, 0).await.unwrap(), []);
}

/// A room's seed is written once; the same seed again is fine, a different one is refused
/// (never dropped while reporting success), and rooms keep their own.
async fn a_seed_is_written_once_and_a_different_one_is_refused<S: Store>(s: Arc<S>) {
    assert_eq!(s.seed_get(R).await.unwrap(), None);
    s.seed_put(R, b"seed one").await.unwrap();
    s.seed_put(R, b"seed one").await.unwrap();
    assert!(matches!(s.seed_put(R, b"seed two").await, Err(StoreError::Corrupt(_))));
    assert_eq!(s.seed_get(R).await.unwrap(), Some(b"seed one".to_vec()));
    assert_eq!(s.seed_get(RoomId(u64::MAX)).await.unwrap(), None);
    s.seed_put(RoomId(u64::MAX), b"").await.unwrap();
    assert_eq!(s.seed_get(RoomId(u64::MAX)).await.unwrap(), Some(Vec::new()));
}

async fn rooms_do_not_see_each_other<S: Store>(s: Arc<S>) {
    let e = s.bump(R).await;
    put(&*s, R, e, &[ev(1)], &[cur(1, 1)]).await.unwrap();
    s.snapshot_put(R, snap(1, vec![1]), e).await.unwrap();
    let other = RoomId(u64::MAX);
    assert_eq!(log(&*s, other).await, []);
    assert_eq!(s.read_cursors(other).await.unwrap(), []);
    assert_eq!(s.snapshot_get(other).await.unwrap(), None);
    // A second room's epochs and seqs are its own.
    let e2 = s.bump(other).await;
    assert_eq!(e2, Epoch(1));
    assert_eq!(put(&*s, other, e2, &[ev(1)], &[]).await.unwrap().head, Seq(1));
}

/// Gate G-DB fence race, many rooms. A writer appends under the old epoch until fenced while a
/// claim races it. `claimed` is set only after `bump` returned and read before each append
/// starts, so an `Ok` seen with the flag already set is an old epoch committing after the
/// fence. And the log read right after the claim must hold exactly the writer's `Ok`s: none
/// commits after the claim returns. Not timing dependent; the timing only varies the overlap.
async fn no_append_that_starts_after_a_claim_returns_commits_under_the_old_epoch<S: Store>(
    s: Arc<S>,
) {
    fence_race(s, false, 1000).await;
}

/// The same race against a room's first claim: the writer uses epoch 0 on a room nobody
/// claimed yet (allowed, as in `MemLogStore`), so on Postgres there is no owner row to lock
/// until the writer creates one. A writer that skipped that and wrote unlocked could commit
/// after the first claim returned.
async fn no_epoch_0_append_commits_after_the_first_claim_returns<S: Store>(s: Arc<S>) {
    fence_race(s, true, 2000).await;
}

async fn fence_race<S: Store>(s: Arc<S>, unclaimed: bool, base: u64) {
    for i in 0..100_u64 {
        let room = RoomId(base + i);
        let old = if unclaimed { Epoch(0) } else { s.bump(room).await };
        let claimed = Arc::new(AtomicBool::new(false));
        let first = Arc::new(Notify::new());
        let mut writer = tokio::spawn({
            let (s, claimed, first) = (Arc::clone(&s), Arc::clone(&claimed), Arc::clone(&first));
            async move {
                let mut next = 1_u64;
                loop {
                    let started_after_claim = claimed.load(Ordering::SeqCst);
                    match put(&*s, room, old, &[ev(next)], &[cur(1, next)]).await {
                        Ok(_) => {
                            assert!(
                                !started_after_claim,
                                "room {room:?}: old epoch committed after the claim"
                            );
                            first.notify_one();
                            next += 1;
                        }
                        Err(AppendError::Fenced { .. }) => break next - 1,
                        Err(e) => panic!("room {room:?}: {e:?}"),
                    }
                }
            }
        });
        // Half the rounds claim mid-stream, half as early as possible.
        // A writer that ends (fenced, or panicking) before its first commit would leave this
        // wait hanging; it is a failure, so it must fail fast.
        if i % 2 == 0 {
            tokio::select! {
                () = first.notified() => {}
                r = &mut writer => panic!("room {room:?}: writer ended before committing: {r:?}"),
            }
        }
        let new = s.bump(room).await;
        claimed.store(true, Ordering::SeqCst);
        let at_claim = log(&*s, room).await;
        let committed = writer.await.unwrap();
        assert_eq!(at_claim, evs(1..=committed), "room {room:?}");
        assert_eq!(log(&*s, room).await, at_claim, "room {room:?}: a write landed after the claim");
        let cursor = if committed == 0 { vec![] } else { vec![cur(1, committed)] };
        assert_eq!(s.read_cursors(room).await.unwrap(), cursor, "room {room:?}");
        assert_eq!(
            append_in(&*s, room, new, &[ev(committed + 1)]).await.unwrap().head,
            Seq(committed + 1)
        );
    }
}

async fn append_in<S: Store>(
    s: &S,
    room: RoomId,
    epoch: Epoch,
    events: &[Event],
) -> Result<AppendReceipt, AppendError> {
    put(s, room, epoch, events, &[]).await
}

/// xorshift64: test-only randomness from a printed seed (docs/TESTING.md rule 7).
const fn next_rand(x: u64) -> u64 {
    let x = x ^ (x << 13);
    let x = x ^ (x >> 7);
    x ^ (x << 17)
}

/// Gate G-DB cancel safety (sqlx issue 2805). Appends are dropped at random points while in
/// flight. Recovery is what a room does after losing track of a write: claim again, read the
/// head, carry on. Every batch (three events and a cursor) must land whole or not at all, the
/// next append on the same pool must succeed, and the log ends gapless and duplicate free.
async fn a_cancelled_append_lands_whole_or_not_at_all_and_the_next_one_works<S: Store>(s: Arc<S>) {
    const SEED: u64 = 0x2805_2805_2805_2805;
    let mut rng = SEED;
    let mut epoch = s.bump(R).await;
    let mut head = 0_u64;
    let mut cancels = 0;
    for i in 0..200 {
        rng = next_rand(rng);
        let wait = Duration::from_micros(rng % 4000);
        let batch = evs(head + 1..=head + 3);
        let c = [cur(1, head / 3 + 1)];
        let done = tokio::time::timeout(wait, put(&*s, R, epoch, &batch, &c)).await;
        if let Ok(r) = done {
            assert_eq!(r, Ok(AppendReceipt { head: Seq(head + 3) }), "seed {SEED:#x} round {i}");
        } else {
            cancels += 1;
            epoch = s.bump(R).await;
            let landed = s.read_tail(R, Seq(head), usize::MAX).await.unwrap();
            assert!(
                landed.is_empty() || landed == batch,
                "seed {SEED:#x} round {i}: partial batch {landed:?}"
            );
            head += u64::try_from(landed.len()).unwrap();
            // The next batch on the same pool, not cancelled, must just work.
            let next = evs(head + 1..=head + 3);
            let r = put(&*s, R, epoch, &next, &[cur(1, head / 3 + 1)]).await;
            assert_eq!(r, Ok(AppendReceipt { head: Seq(head + 3) }), "seed {SEED:#x} round {i}");
        }
        head += 3;
    }
    assert_eq!(log(&*s, R).await, evs(1..=head), "seed {SEED:#x}: gap or duplicate");
    assert_eq!(s.read_cursors(R).await.unwrap(), [cur(1, head / 3)], "seed {SEED:#x}");
    // Printed with --nocapture, so CI shows how many rounds were really cancelled.
    println!("cancel safety: {cancels} of 200 rounds cancelled, head {head}");
}

fn upload(kind: UploadKind, game: &str, body: &[u8]) -> UploadRecord {
    let id = blob_key(body);
    let meta = format!("{game} {}", String::from_utf8_lossy(body));
    UploadRecord { kind, id, game: game.to_owned(), meta }
}

/// An upload is stored once by kind and id with its record, read back whole and checked on
/// write; the two kinds never see each other, and a listing is one kind's, ascending by id.
async fn uploads_are_deduplicated_by_kind_checked_and_listed<S: Store>(s: Arc<S>) {
    let bodies: Vec<Vec<u8>> = (0..4).map(|i| format!("upload {i}").into_bytes()).collect();
    let worlds: Vec<UploadRecord> =
        bodies.iter().map(|b| upload(UploadKind::Apworld, "g", b)).collect();
    for (r, b) in worlds.iter().zip(&bodies) {
        s.upload_put(r, b).await.unwrap();
    }
    // The same file again keeps its first record and bytes.
    s.upload_put(&upload(UploadKind::Apworld, "other", &bodies[0]), &bodies[0]).await.unwrap();
    let (k, id) = (UploadKind::Apworld, worlds[0].id);
    assert_eq!(s.upload_meta(k, &id).await.unwrap(), Some(worlds[0].meta.clone()));
    assert_eq!(s.upload_bytes(k, &id).await.unwrap(), Some(bodies[0].clone()));
    // The same bytes as a player file are another upload.
    assert_eq!(s.upload_meta(UploadKind::Yaml, &id).await.unwrap(), None);
    let player = upload(UploadKind::Yaml, "p", &bodies[0]);
    s.upload_put(&player, &bodies[0]).await.unwrap();
    assert_eq!(s.upload_list(UploadKind::Yaml).await.unwrap(), [player.meta]);
    // Bytes that are not the id's are refused and leave nothing.
    let lie = upload(UploadKind::Apworld, "g", b"other bytes");
    assert!(matches!(s.upload_put(&lie, b"not those").await, Err(StoreError::Corrupt(_))));
    assert_eq!(s.upload_meta(UploadKind::Apworld, &lie.id).await.unwrap(), None);
    assert_eq!(s.upload_bytes(UploadKind::Apworld, &blob_key(b"none")).await.unwrap(), None);
    // Whole when stored; a missing one, or one of the other kind, is not.
    assert!(s.upload_intact(k, &id).await.unwrap());
    // A torn row (tampered bytes) is not intact on either store.
    s.tear_for_tests(k, &id).await;
    assert!(!s.upload_intact(k, &id).await.unwrap(), "torn");
    assert!(matches!(s.upload_bytes(k, &id).await, Err(StoreError::Corrupt(_))), "torn read");
    s.upload_put(&worlds[0], &bodies[0]).await.unwrap();
    assert!(s.upload_intact(k, &id).await.unwrap(), "healed");
    assert!(!s.upload_intact(UploadKind::Apworld, &blob_key(b"none")).await.unwrap());
    assert!(!s.upload_intact(UploadKind::Yaml, &worlds[1].id).await.unwrap());
    let mut want = worlds;
    want.sort_by_key(|r| r.id);
    let metas: Vec<String> = want.into_iter().map(|r| r.meta).collect();
    assert_eq!(s.upload_list(UploadKind::Apworld).await.unwrap(), metas);
}

/// A job's status is written whole and read back; only queued and running jobs go stale, a
/// stale one is failed once, and a job marked alive since it was listed is not failed.
async fn jobs_are_read_back_and_only_stale_unfinished_ones_fail<S: Store>(s: Arc<S>) {
    let id = |c: char| c.to_string().repeat(64);
    let (q, r, d) = (id('a'), id('b'), id('c'));
    s.job_put(&q, JobState::Queued, "q1").await.unwrap();
    s.job_put(&r, JobState::Running, "r1").await.unwrap();
    s.job_put(&d, JobState::Done, "d1").await.unwrap();
    let status = async |id: &str| s.job_status(id).await.unwrap().map(|(status, _)| status);
    assert_eq!(status(&q).await.as_deref(), Some("q1"));
    let (_, ago) = s.job_status(&q).await.unwrap().unwrap();
    assert!(ago < Duration::from_secs(60), "just written: {ago:?}");
    assert_eq!(status(&id('f')).await, None);
    let hour = Duration::from_secs(3600);
    assert_eq!(s.job_fail_all_stale(hour, "lost").await.unwrap(), Vec::<String>::new());
    assert!(!s.job_fail_stale(&q, "failed", hour).await.unwrap());
    assert!(s.job_fail_stale(&r, "r failed", Duration::ZERO).await.unwrap());
    assert!(!s.job_fail_stale(&r, "again", Duration::ZERO).await.unwrap(), "once");
    assert!(!s.job_fail_stale(&d, "done stays", Duration::ZERO).await.unwrap());
    assert_eq!(status(&r).await.as_deref(), Some("r failed"));
    assert_eq!(status(&d).await.as_deref(), Some("d1"));
    // All the stale ones at once, JSON kept and marked failed; a done one is left alone.
    let (j1, j2) = (id('d'), id('e'));
    s.job_put(&j1, JobState::Running, r#"{"id": "d", "slots": 2}"#).await.unwrap();
    s.job_put(&j2, JobState::Queued, "not json").await.unwrap();
    assert_eq!(s.job_fail_all_stale(hour, "lost").await.unwrap(), Vec::<String>::new());
    let failed_now = s.job_fail_all_stale(Duration::ZERO, "lost").await.unwrap();
    assert_eq!(failed_now, [q.clone(), j1.clone(), j2.clone()], "ascending; the done one stays");
    let json = async |id: &str| -> serde_json::Value {
        serde_json::from_str(&status(id).await.unwrap()).unwrap()
    };
    let failed = serde_json::json!({"id": "d", "slots": 2, "status": "failed", "error": "lost"});
    assert_eq!(json(&j1).await, failed);
    assert_eq!(json(&j2).await, serde_json::json!({"status": "failed", "error": "lost"}));
    assert!(s.job_fail_all_stale(Duration::ZERO, "again").await.unwrap().is_empty(), "once");
    // Text that is not a JSON object never aborts the sweep: it is replaced, the rest fail.
    let bad = ["{broken", r#"{"a": "\u0000"}"#, r#"{"a": ["\u0000"]}"#, "[1]"];
    let ids: Vec<String> = ('1'..='5').map(id).collect();
    for (i, text) in bad.iter().chain(&[r#"{"ok": 1}"#]).enumerate() {
        s.job_put(&ids[i], JobState::Running, text).await.unwrap();
    }
    let all = s.job_fail_all_stale(Duration::ZERO, "lost").await.unwrap();
    assert_eq!(all, ids, "every one failed, none aborted the sweep");
    let bare = serde_json::json!({"status": "failed", "error": "lost"});
    for (i, text) in bad.iter().enumerate() {
        assert_eq!(json(&ids[i]).await, bare, "{text}");
    }
    assert_eq!(json(&ids[bad.len()]).await["ok"], 1);
    // A put replaces the status; a touch of a finished job changes nothing.
    s.job_put(&q, JobState::Done, "q2").await.unwrap();
    s.job_touch(&q).await.unwrap();
    assert!(s.job_fail_all_stale(Duration::ZERO, "x").await.unwrap().is_empty());
    assert_eq!(status(&q).await.as_deref(), Some("q2"));
    // A server that comes back finishes over a sweep's failed.
    s.job_put(&r, JobState::Done, "r done").await.unwrap();
    assert_eq!(status(&r).await.as_deref(), Some("r done"));
    // A touch marks an unfinished job alive again: of two lost an hour ago, only the untouched
    // one goes stale.
    let (kept, lost) = (id('6'), id('7'));
    for j in [&kept, &lost] {
        s.job_put(j, JobState::Running, "{}").await.unwrap();
        s.age_job_for_tests(j, hour).await;
    }
    s.job_touch(&kept).await.unwrap();
    assert_eq!(s.job_fail_all_stale(hour / 2, "lost").await.unwrap(), [lost]);
}

/// A status put only when there is none never replaces one that is stored, of any state.
async fn a_job_put_if_absent_never_replaces_a_stored_status<S: Store>(s: Arc<S>) {
    let id = "a".repeat(64);
    assert!(s.job_put_if_absent(&id, JobState::Running, "old file").await.unwrap());
    assert!(!s.job_put_if_absent(&id, JobState::Failed, "newer?").await.unwrap());
    s.job_put(&id, JobState::Done, "newer row").await.unwrap();
    assert!(!s.job_put_if_absent(&id, JobState::Running, "stale file").await.unwrap());
    let (status, _) = s.job_status(&id).await.unwrap().unwrap();
    assert_eq!(status, "newer row");
}

async fn accounts_are_unique_and_a_login_ends_when_it_expires_or_is_deleted<S: Store>(s: Arc<S>) {
    let name = format!("ann-{}", s.bump(RoomId(1)).await.0);
    assert!(s.user_create(&name, "hash-1").await.unwrap());
    assert!(!s.user_create(&name, "hash-2").await.unwrap(), "a name is taken once");
    assert_eq!(s.user_hash(&name).await.unwrap().as_deref(), Some("hash-1"));
    assert_eq!(s.user_hash("nobody-here").await.unwrap(), None);
    let (live, dead, gone) = ([1; 32], [2; 32], [3; 32]);
    let hour = Duration::from_secs(3600);
    s.session_put(&live, &name, hour).await.unwrap();
    s.session_put(&dead, &name, Duration::ZERO).await.unwrap();
    s.session_put(&gone, &name, hour).await.unwrap();
    assert_eq!(s.session_user(&live).await.unwrap().as_deref(), Some(name.as_str()));
    assert_eq!(s.session_user(&dead).await.unwrap(), None, "an expired login is dead");
    s.session_delete(&gone).await.unwrap();
    assert_eq!(s.session_user(&gone).await.unwrap(), None);
    assert_eq!(s.session_user(&[9; 32]).await.unwrap(), None);
}

/// Rooms 11, 12 and 13 must be listed already (Postgres reads them from the directory).
async fn a_host_lists_only_its_own_rooms_and_the_admin_view_lists_all<S: Store>(s: &S) {
    let room = |n: u8| crate::OwnedRoom {
        world_id: Some(format!("w{n}")),
        seed_name: format!("seed{}", 10 + n),
        room_key: format!("key{n}"),
        owner: None,
        // Ignored: a listed room's age is the store's, from when the room was claimed.
        age_secs: 1 << 40,
    };
    s.room_claim(RoomId(11), Some("pw:a"), &room(1)).await.unwrap();
    s.room_claim(RoomId(12), Some("pw:b"), &room(2)).await.unwrap();
    s.room_claim(RoomId(11), Some("pw:b"), &room(1)).await.unwrap();
    let mine = s.rooms_owned(Some("pw:a")).await.unwrap();
    assert_eq!(mine.iter().map(|r| r.seed_name.as_str()).collect::<Vec<_>>(), ["seed11"]);
    assert_eq!(mine[0].owner.as_deref(), Some("pw:a"), "an owner is kept");
    assert!(s.rooms_owned(Some("pw:nobody")).await.unwrap().is_empty());
    s.room_claim(RoomId(13), None, &room(3)).await.unwrap();
    let all = s.rooms_owned(None).await.unwrap();
    assert!(all.iter().any(|r| r.owner.is_none()), "the admin view lists rooms with no owner");
    assert!(all.len() >= 3);
    assert!(all.iter().all(|r| r.age_secs < 3600), "ages are the store's: {all:?}");
}

/// Room 21 must be listed already with key `aaaaaaaaaa` (Postgres reads it from the directory).
async fn room_admins_are_granted_once_found_by_key_and_end_with_their_account<S: Store>(s: &S) {
    let (key, other) = ("aaaaaaaaaa", "bbbbbbbbbb");
    let room = crate::OwnedRoom {
        world_id: Some("w21".into()),
        seed_name: "seed21".into(),
        room_key: key.into(),
        owner: None,
        age_secs: 0,
    };
    s.room_claim(RoomId(21), Some("pw:own"), &room).await.unwrap();
    assert_eq!(s.room_by_key(key).await.unwrap().unwrap().seed_name, "seed21");
    assert_eq!(s.room_by_key(other).await.unwrap(), None);
    for name in ["bea", "cy"] {
        assert!(s.user_create(name, "h").await.unwrap());
    }
    assert!(s.room_admins(key).await.unwrap().is_empty());
    assert!(s.room_admin_add(key, "bea", "own").await.unwrap());
    assert!(s.room_admin_add(key, "bea", "cy").await.unwrap(), "a repeat grant is fine");
    assert_eq!(s.room_admins(key).await.unwrap(), ["bea"], "listed once");
    assert!(!s.room_admin_add(key, "ghost", "own").await.unwrap(), "no such account");
    assert!(!s.room_admin_add(other, "cy", "own").await.unwrap(), "no such room");
    let seen =
        |rooms: Vec<crate::OwnedRoom>| rooms.into_iter().map(|r| r.room_key).collect::<Vec<_>>();
    assert_eq!(seen(s.rooms_administered("bea").await.unwrap()), [key]);
    assert!(s.rooms_administered("cy").await.unwrap().is_empty(), "only its own adminships");
    assert!(s.room_admin_remove(key, "bea").await.unwrap());
    assert!(!s.room_admin_remove(key, "bea").await.unwrap(), "nothing left to remove");
    assert!(s.rooms_administered("bea").await.unwrap().is_empty());
    let hour = Duration::from_secs(3600);
    for (name, token) in [("bea", [7; 32]), ("cy", [8; 32])] {
        assert!(s.room_admin_add(key, name, "own").await.unwrap());
        s.session_put(&token, name, hour).await.unwrap();
    }
    s.user_delete("bea").await.unwrap();
    assert_eq!(s.session_user(&[7; 32]).await.unwrap(), None, "a deleted account's login ends");
    assert_eq!(s.session_user(&[8; 32]).await.unwrap().as_deref(), Some("cy"), "others' stay");
    assert_eq!(s.room_admins(key).await.unwrap(), ["cy"], "a deleted account is no admin");
    assert_eq!(s.user_hash("bea").await.unwrap(), None);
    assert!(s.rooms_administered("bea").await.unwrap().is_empty());
}

macro_rules! suite {
    ($store:ident, $make:ident, [$($name:ident),* $(,)?]) => {
        mod $store {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(std::sync::Arc::new(super::$make().await)).await;
                }
            )*
        }
    };
}

macro_rules! contract {
    ($store:ident, $make:ident) => {
        suite!(
            $store,
            $make,
            [
                an_empty_room_reads_as_empty,
                appended_events_read_back_as_tail_and_range,
                every_event_kind_and_extreme_ids_read_back_unchanged,
                an_old_epoch_is_fenced_and_writes_nothing,
                a_future_or_unclaimed_epoch_is_fenced_too,
                an_unclaimed_room_takes_epoch_zero_until_its_first_claim,
                a_fenced_batch_does_not_move_cursors,
                a_fenced_batch_with_a_gap_is_fenced_not_corrupt,
                a_gap_a_replay_or_a_bad_start_is_corrupt_and_writes_nothing,
                an_empty_batch_is_fine_and_cursors_only_ever_rise,
                duplicate_slots_in_one_batch_keep_the_highest_and_slots_read_in_order,
                snapshots_are_fenced_and_cannot_be_ahead_of_the_log,
                an_older_snapshot_never_replaces_a_newer_one,
                rooms_do_not_see_each_other,
                blobs_are_stored_once_by_key_and_checked_on_write,
                manifests_are_deduplicated_checked_and_listed_in_pages,
                uploads_are_deduplicated_by_kind_checked_and_listed,
                jobs_are_read_back_and_only_stale_unfinished_ones_fail,
                a_job_put_if_absent_never_replaces_a_stored_status,
                accounts_are_unique_and_a_login_ends_when_it_expires_or_is_deleted,
                a_seed_is_written_once_and_a_different_one_is_refused,
                no_append_that_starts_after_a_claim_returns_commits_under_the_old_epoch,
                no_epoch_0_append_commits_after_the_first_claim_returns,
                a_cancelled_append_lands_whole_or_not_at_all_and_the_next_one_works,
            ]
        );
    };
}

async fn mem_store() -> MemLogStore {
    MemLogStore::new()
}

contract!(mem, mem_store);

#[tokio::test]
async fn mem_hosts_list_only_their_own_rooms() {
    a_host_lists_only_its_own_rooms_and_the_admin_view_lists_all(&MemLogStore::new()).await;
}

#[tokio::test]
async fn mem_room_admins_are_granted_once_found_by_key_and_end_with_their_account() {
    room_admins_are_granted_once_found_by_key_and_end_with_their_account(&MemLogStore::new()).await;
}

#[cfg(feature = "pg-tests")]
mod pg_only {
    use super::*;
    use crate::{OpenError, PgLogStore, World};
    use sqlx::Connection as _;

    fn pool(connections: u32) -> config::Limits {
        config::Limits { db_pool_connections: connections, ..config::Limits::DESIGN }
    }

    async fn schema() -> String {
        crate::testing::fresh_schema(
            &config::load_secrets().unwrap().database_url.expect("CONSTELLATION_DATABASE_URL"),
        )
        .await
        .unwrap()
    }

    /// Another migrator holding sqlx's advisory lock (a second server starting, a parallel
    /// test) makes this connect wait longer than the runtime `lock_timeout`; it must still
    /// connect once the lock is free. Fails when the migrations run under `lock_timeout`.
    #[tokio::test]
    async fn a_connect_waits_for_another_migrator_past_the_runtime_lock_timeout() {
        use sqlx::migrate::Migrate as _;
        let url = schema().await;
        let mut holder = sqlx::PgConnection::connect(&url).await.unwrap();
        holder.lock().await.unwrap();
        let limits = config::Limits { db_lock_timeout_ms: 100, ..pool(2) };
        let mut connecting = tokio::spawn(async move { PgLogStore::connect(&url, &limits).await });
        let waited = tokio::time::timeout(Duration::from_millis(500), &mut connecting).await;
        assert!(waited.is_err(), "still waiting for the lock, not failed: {waited:?}");
        holder.unlock().await.unwrap();
        connecting.await.unwrap().expect("connected once the lock was free");
    }

    /// Gate G-DB: a writer stuck inside a transaction while holding the owner row lock must
    /// not block a claim forever; with `lock_timeout` the claim errors, and once the stuck
    /// session ends the claim works.
    #[tokio::test]
    async fn a_claim_behind_a_stuck_lock_holder_errors_at_the_lock_timeout() {
        use sqlx::Connection;
        let url = schema().await;
        let limits = config::Limits { db_lock_timeout_ms: 200, ..pool(2) };
        let s = PgLogStore::connect(&url, &limits).await.unwrap();
        assert_eq!(s.claim(R).await.unwrap(), Epoch(1));
        let mut stuck = sqlx::PgConnection::connect(&url).await.unwrap();
        sqlx::raw_sql("BEGIN; SELECT epoch FROM room_owner FOR SHARE")
            .execute(&mut stuck)
            .await
            .unwrap();
        let r = tokio::time::timeout(Duration::from_secs(10), s.claim(R)).await;
        assert_eq!(
            r.expect("the claim hung past its lock timeout"),
            Err(crate::StoreError::Unavailable)
        );
        sqlx::raw_sql("ROLLBACK").execute(&mut stuck).await.unwrap();
        assert_eq!(s.claim(R).await.unwrap(), Epoch(2));
    }

    /// A store on a fresh schema of the database at `CONSTELLATION_DATABASE_URL`.
    pub(super) async fn pg_store() -> PgLogStore {
        let url = config::load_secrets().unwrap().database_url.expect(
            "pg-tests needs CONSTELLATION_DATABASE_URL (docs/TESTING.md rule 5: no silent skip)",
        );
        PgLogStore::connect(&crate::testing::fresh_schema(&url).await.unwrap(), &pool(4))
            .await
            .unwrap()
    }

    async fn open(
        s: &PgLogStore,
        room: RoomId,
        hash: &[u8],
        key: &str,
    ) -> Result<String, OpenError> {
        s.open_room(room, &format!("seed{}", room.0), 1, hash, key, World::V1(b"{}"))
            .await
            .map(|o| o.key)
    }

    #[tokio::test]
    async fn pg_hosts_list_only_their_own_rooms() {
        let s = pg_store().await;
        open(&s, RoomId(11), &[1], "aaaaaaaaaa").await.unwrap();
        open(&s, RoomId(12), &[2], "bbbbbbbbbb").await.unwrap();
        open(&s, RoomId(13), &[3], "cccccccccc").await.unwrap();
        a_host_lists_only_its_own_rooms_and_the_admin_view_lists_all(&s).await;
    }

    #[tokio::test]
    async fn pg_room_admins_are_granted_once_found_by_key_and_end_with_their_account() {
        let s = pg_store().await;
        open(&s, RoomId(21), &[1], "aaaaaaaaaa").await.unwrap();
        room_admins_are_granted_once_found_by_key_and_end_with_their_account(&s).await;
    }

    /// A store that migrated as the schema's owner (the test's own login) and runs as the
    /// runtime role `config::DB_APP_ROLE`, made here with a login and no other right.
    async fn restricted_store() -> PgLogStore {
        use sqlx::postgres::PgConnectOptions;
        use std::str::FromStr;
        const PASSWORD: &str = "restricted-test-only";
        let url = schema().await;
        let role = config::DB_APP_ROLE;
        let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
        // Another test may make the role at the same time: the duplicate is fine.
        let make = format!(
            "DO $r$ BEGIN CREATE ROLE {role} LOGIN PASSWORD '{PASSWORD}'; \
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $r$"
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(make)).execute(&mut c).await.unwrap();
        let owner = PgConnectOptions::from_str(&url).unwrap();
        let runtime = owner.clone().username(role).password(PASSWORD);
        PgLogStore::connect_with(runtime, Some(owner), &pool(4)).await.unwrap()
    }

    /// The runtime role does the server's normal work (rooms, accounts, admins) and cannot
    /// change the schema or the migration history, and is no superuser. Fails if the runtime
    /// pool logs in as the owner, or the grants cover more than data.
    #[tokio::test]
    async fn the_runtime_role_does_normal_work_but_cannot_change_the_schema() {
        let s = restricted_store().await;
        open(&s, RoomId(21), &[1], "aaaaaaaaaa").await.unwrap();
        room_admins_are_granted_once_found_by_key_and_end_with_their_account(&s).await;
        accounts_are_unique_and_a_login_ends_when_it_expires_or_is_deleted(Arc::new(s)).await;
    }

    /// A store opened as the test's login (a superuser) is warned about; one running as the
    /// runtime role is not.
    #[tokio::test]
    async fn only_a_pool_with_more_than_data_rights_gets_a_privilege_warning() {
        let (root, app) = (pg_store().await, restricted_store().await);
        let warned = crate::pg::pool_privilege_warning(root.pool_for_tests()).await;
        assert!(warned.is_some_and(|w| w.contains("superuser")), "the test login is a superuser");
        assert_eq!(crate::pg::pool_privilege_warning(app.pool_for_tests()).await, None);
    }

    #[tokio::test]
    async fn the_runtime_role_is_refused_every_schema_change() {
        let s = restricted_store().await;
        let pool = s.pool_for_tests();
        let who: String = sqlx::query_scalar("SELECT current_user").fetch_one(pool).await.unwrap();
        assert_eq!(who, config::DB_APP_ROLE, "the pool runs as the runtime role");
        let superuser: bool =
            sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(!superuser);
        for ddl in [
            "CREATE TABLE sneaky (a int)",
            "DROP TABLE rooms",
            "DROP TABLE users",
            "ALTER TABLE users ADD COLUMN x int",
            "TRUNCATE users",
            "DELETE FROM _sqlx_migrations",
            "CREATE SCHEMA sneaky",
        ] {
            let e = sqlx::raw_sql(ddl).execute(pool).await.expect_err(ddl);
            let text = e.to_string();
            assert!(
                text.contains("permission denied") || text.contains("must be owner"),
                "{ddl}: {text}"
            );
        }
    }

    #[tokio::test]
    async fn a_room_reopens_with_its_bundle_and_refuses_another() {
        let s = pg_store().await;
        open(&s, R, &[1, 2], "aaaaaaaaaa").await.unwrap();
        open(&s, R, &[1, 2], "bbbbbbbbbb").await.unwrap();
        assert_eq!(open(&s, R, &[1, 3], "cccccccccc").await, Err(OpenError::OtherBundle));
        open(&s, RoomId(2), &[1, 3], "dddddddddd").await.unwrap();
    }

    #[tokio::test]
    async fn a_room_keeps_the_key_it_first_opened_with_and_keys_are_unique() {
        let s = pg_store().await;
        assert_eq!(open(&s, R, &[1], "aaaaaaaaaa").await.unwrap(), "aaaaaaaaaa");
        // A reopen draws a new key, but the stored one wins: the address never changes.
        assert_eq!(open(&s, R, &[1], "bbbbbbbbbb").await.unwrap(), "aaaaaaaaaa");
        // Another room cannot take a key in use; the caller draws again.
        assert!(matches!(open(&s, RoomId(2), &[2], "aaaaaaaaaa").await, Err(OpenError::Store(_))));
        assert_eq!(open(&s, RoomId(2), &[2], "cccccccccc").await.unwrap(), "cccccccccc");
        // Only the form the server parses is stored: lowercase base32, 10 characters.
        for bad in ["AAAAAAAAAB", "aaaaaaaab", "aaaaaaaaa1"] {
            assert!(
                matches!(open(&s, RoomId(3), &[3], bad).await, Err(OpenError::Store(_))),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn the_directory_lists_open_rooms_with_their_bundles_and_drops_finished_ones() {
        let s = pg_store().await;
        s.open_room(R, "alpha", 1, &[1], "aaaaaaaaaa", World::V1(b"bundle a")).await.unwrap();
        s.open_room(RoomId(2), "beta", 1, &[2], "bbbbbbbbbb", World::V1(b"bundle b"))
            .await
            .unwrap();
        let listed = s.list_rooms().await.unwrap();
        let names: Vec<_> = listed.iter().map(|l| (l.seed_name.as_str(), l.key.as_str())).collect();
        assert_eq!(names, [("alpha", "aaaaaaaaaa"), ("beta", "bbbbbbbbbb")]);
        assert!(listed.iter().all(|l| l.idle_secs < 60), "a new room was just active");
        assert_eq!(s.room_bundle(R).await.unwrap().as_deref(), Some(&b"bundle a"[..]));
        assert_eq!(s.room_bundle(RoomId(99)).await.unwrap(), None);
        // A reopen never replaces the stored file.
        s.open_room(R, "alpha", 1, &[1], "cccccccccc", World::V1(b"other")).await.unwrap();
        assert_eq!(s.room_bundle(R).await.unwrap().as_deref(), Some(&b"bundle a"[..]));
        assert!(s.finish(R, "abandoned", 0).await.unwrap());
        let left: Vec<_> = s.list_rooms().await.unwrap().into_iter().map(|l| l.seed_name).collect();
        assert_eq!(left, ["beta"]);
        assert_eq!(
            s.open_room(R, "alpha", 1, &[1], "dddddddddd", World::V1(b"bundle a")).await,
            Err(OpenError::Finished { reason: "abandoned".to_owned() })
        );
    }

    #[tokio::test]
    async fn touch_sets_how_long_each_room_has_been_idle_in_one_call() {
        let s = pg_store().await;
        s.open_room(R, "alpha", 1, &[1], "aaaaaaaaaa", World::V1(b"{}")).await.unwrap();
        s.open_room(RoomId(2), "beta", 1, &[2], "bbbbbbbbbb", World::V1(b"{}")).await.unwrap();
        s.touch(&[]).await.unwrap();
        s.touch(&[(R, 3 * 86_400), (RoomId(2), 86_400), (RoomId(99), 0)]).await.unwrap();
        let idle: Vec<u64> = s.list_rooms().await.unwrap().iter().map(|l| l.idle_secs).collect();
        let [a, b] = idle[..] else { panic!("{idle:?}") };
        assert!((3 * 86_400..3 * 86_400 + 60).contains(&a), "{a}");
        assert!((86_400..86_400 + 60).contains(&b), "{b}");
        let reopened =
            s.open_room(R, "alpha", 1, &[1], "bbbbbbbbbb", World::V1(b"{}")).await.unwrap();
        assert!(reopened.idle_secs >= 3 * 86_400, "opening is not a connection");
    }

    /// D-30 is decided by the database: a room used more recently than the limit is not
    /// ended, whatever the caller's clock says.
    #[tokio::test]
    async fn finish_ends_a_room_only_when_the_database_says_it_was_idle_long_enough() {
        let s = pg_store().await;
        s.open_room(R, "alpha", 1, &[1], "aaaaaaaaaa", World::V1(b"{}")).await.unwrap();
        assert!(!s.finish(R, "abandoned", 3_600).await.unwrap(), "used just now");
        assert_eq!(s.list_rooms().await.unwrap().len(), 1);
        s.touch(&[(R, 7_200)]).await.unwrap();
        assert!(s.finish(R, "abandoned", 3_600).await.unwrap());
        assert!(s.list_rooms().await.unwrap().is_empty());
        assert!(!s.finish(R, "abandoned", 0).await.unwrap(), "a finished room ends once");
        assert!(!s.finish(RoomId(99), "abandoned", 0).await.unwrap(), "unknown room");
    }

    /// A room stored under migration 0001 has no key and no bundle. It is not listed until
    /// its next open fills both, and then keeps them.
    #[tokio::test]
    async fn a_room_stored_before_the_directory_gets_its_key_and_bundle_on_its_next_open() {
        let s = pg_store().await;
        sqlx::query(
            "INSERT INTO rooms (room_id, seed_name, bundle_format, bundle_hash) \
             VALUES ($1, 'alpha', 1, $2)",
        )
        .bind(i64::from_be_bytes(R.0.to_be_bytes()))
        .bind(&[1_u8][..])
        .execute(s.pool_for_tests())
        .await
        .unwrap();
        assert!(s.list_rooms().await.unwrap().is_empty(), "no key or bundle yet");
        assert_eq!(s.room_bundle(R).await.unwrap(), None);
        // Another bundle for the same room id fills nothing.
        assert_eq!(
            s.open_room(R, "alpha", 1, &[2], "bbbbbbbbbb", World::V1(b"other")).await,
            Err(OpenError::OtherBundle)
        );
        assert_eq!(s.room_bundle(R).await.unwrap(), None);
        let opened =
            s.open_room(R, "alpha", 1, &[1], "aaaaaaaaaa", World::V1(b"bundle a")).await.unwrap();
        assert_eq!(opened.key, "aaaaaaaaaa");
        let listed = s.list_rooms().await.unwrap();
        assert_eq!((listed[0].seed_name.as_str(), listed[0].key.as_str()), ("alpha", "aaaaaaaaaa"));
        assert_eq!(s.room_bundle(R).await.unwrap().as_deref(), Some(&b"bundle a"[..]));
    }

    /// A room's seed is written with the room, and only for the bundle it was stored from: an
    /// open from another bundle writes nothing, the first seed stays, and a seed for a stored
    /// room cannot come in through `seed_put` (a generated world naming a hosted seed).
    #[tokio::test]
    async fn a_room_gets_its_own_seed_and_never_another_bundles() {
        let s = pg_store().await;
        s.open_room(R, "alpha", 1, &[1], "aaaaaaaaaa", World::Seed(b"seed a")).await.unwrap();
        assert_eq!(
            s.open_room(R, "alpha", 1, &[2], "bbbbbbbbbb", World::Seed(b"seed b")).await,
            Err(OpenError::OtherBundle)
        );
        assert_eq!(s.seed_get(R).await.unwrap().as_deref(), Some(&b"seed a"[..]));
        // A reopen of the same bundle keeps the seed it has.
        s.open_room(R, "alpha", 1, &[1], "cccccccccc", World::Seed(b"seed a2")).await.unwrap();
        assert_eq!(s.seed_get(R).await.unwrap().as_deref(), Some(&b"seed a"[..]));
        assert_eq!(s.list_rooms().await.unwrap()[0].bundle_hash, vec![1]);
        // A legacy room (a v1 file, no seed) takes no seed from outside.
        let legacy = RoomId(2);
        s.open_room(legacy, "beta", 1, &[3], "dddddddddd", World::V1(b"bundle")).await.unwrap();
        assert!(matches!(s.seed_put(legacy, b"seed c").await, Err(StoreError::Corrupt(_))));
        assert!(matches!(s.seed_put(R, b"seed a").await, Err(StoreError::Corrupt(_))));
        assert_eq!(s.seed_get(legacy).await.unwrap(), None);
    }

    /// Two writers with the same epoch (a bug upstream, but the store must say so): both pass
    /// the shared fence lock and read head 0, both insert seq 1. The loser hits the primary
    /// key and must get `Corrupt`, never `Unavailable` (which would invite a blind retry).
    /// Deterministic: a test transaction holds `slot_cursors` locked, so the first writer
    /// stops after its events insert and the second waits on the first's seq 1 row; only then
    /// is the lock released.
    #[tokio::test]
    async fn two_same_epoch_writers_on_one_seq_leave_one_ok_and_one_corrupt() {
        use sqlx::Connection;
        let url = schema().await;
        let app = format!("dup_{}", url.rsplit('=').next().unwrap_or("x"));
        let s = Arc::new(
            PgLogStore::connect(&format!("{url}&application_name={app}"), &pool(2)).await.unwrap(),
        );
        let e = s.claim(R).await.unwrap();
        let mut gate = sqlx::PgConnection::connect(&url).await.unwrap();
        sqlx::raw_sql("BEGIN; LOCK TABLE slot_cursors IN EXCLUSIVE MODE")
            .execute(&mut gate)
            .await
            .unwrap();
        let racer = |s: Arc<PgLogStore>| {
            tokio::spawn(async move { put(&*s, R, e, &[ev(1)], &[cur(1, 1)]).await })
        };
        let (first, second) = (racer(Arc::clone(&s)), racer(Arc::clone(&s)));
        // Polled: Postgres has no event for "a session started waiting on a lock". From its own
        // connection: inside `gate`'s transaction pg_stat_activity would stay frozen at the
        // first poll (it is snapshotted once per transaction).
        let mut watch = sqlx::PgConnection::connect(&url).await.unwrap();
        let waiting = "SELECT count(*) FROM pg_stat_activity \
                       WHERE application_name = $1 AND wait_event_type = 'Lock'";
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let n: i64 =
                sqlx::query_scalar(waiting).bind(&app).fetch_one(&mut watch).await.unwrap();
            if n == 2 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the writers never both waited ({n})");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sqlx::raw_sql("ROLLBACK").execute(&mut gate).await.unwrap();
        let mut got = [first.await.unwrap(), second.await.unwrap()];
        got.sort_by_key(Result::is_err);
        assert_eq!(got[0], Ok(AppendReceipt { head: Seq(1) }));
        assert!(
            matches!(&got[1], Err(AppendError::Corrupt(why)) if why.contains("duplicate")),
            "{:?}",
            got[1]
        );
        assert_eq!(log(&*s, R).await, [ev(1)]);
    }

    /// Issue 2805 workaround: a failed call never hands its connection back to the pool.
    /// With one pooled connection, a success keeps the same backend and a failure replaces it.
    #[tokio::test]
    async fn an_error_closes_its_connection_instead_of_pooling_it() {
        let url = schema().await;
        let s = PgLogStore::connect(&url, &pool(1)).await.unwrap();
        let backend = || async {
            sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
                .fetch_one(s.pool_for_tests())
                .await
                .unwrap()
        };
        let e = s.claim(R).await.unwrap();
        append(&s, e, &[ev(1)]).await.unwrap();
        let before = backend().await;
        append(&s, e, &[ev(2)]).await.unwrap();
        assert_eq!(backend().await, before, "a success must keep its pooled connection");
        assert!(matches!(
            append(&s, Epoch(e.0 + 1), &[ev(3)]).await,
            Err(AppendError::Fenced { .. })
        ));
        assert_ne!(backend().await, before, "the failed append's connection went back to the pool");
        assert!(matches!(append(&s, e, &[ev(9)]).await, Err(AppendError::Corrupt(_))));
        let after_corrupt = backend().await;
        append(&s, e, &[ev(3)]).await.unwrap();
        assert_eq!(backend().await, after_corrupt);
    }

    #[tokio::test]
    async fn the_store_survives_a_reconnect_with_its_log_and_epoch() {
        let url = schema().await;
        let a = PgLogStore::connect(&url, &pool(1)).await.unwrap();
        let e = a.claim(R).await.unwrap();
        put(&a, R, e, &evs(1..=2), &[cur(2, 1)]).await.unwrap();
        drop(a);
        // A second connect runs the migrations again (a no-op) and sees everything.
        let b = PgLogStore::connect(&url, &pool(1)).await.unwrap();
        assert_eq!(log(&b, R).await, evs(1..=2));
        assert_eq!(b.read_cursors(R).await.unwrap(), [cur(2, 1)]);
        assert_eq!(b.claim(R).await.unwrap(), Epoch(2));
    }
}

#[cfg(feature = "pg-tests")]
use pg_only::pg_store;

#[cfg(feature = "pg-tests")]
contract!(pg, pg_store);
