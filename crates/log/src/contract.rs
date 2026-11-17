//! The `LogStore` contract: one suite, run against `MemLogStore` always and against
//! `PgLogStore` with `--features pg-tests` (needs `CONSTELLATION_DATABASE_URL`; without it the
//! Postgres tests fail, they never skip). Each Postgres test gets its own schema.
//!
//! The race tests (fence race, cancel safety) are gate G-DB items (doc 11 section 4.8).
#![allow(clippy::arithmetic_side_effects, reason = "test counters stay far below u64::MAX")]
use crate::{
    AppendBatch, AppendError, AppendReceipt, CursorUpdate, LogStore, MemLogStore, Snapshot,
};
use proto::{Cid, Epoch, Event, EventKind, ItemSeq, RoomId, Seq, SlotNo, Timestamp};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

const R: RoomId = RoomId(1);

/// A store plus the directory's claim, which is not part of the trait.
trait Store: LogStore {
    fn bump(&self, room: RoomId) -> impl Future<Output = Epoch> + Send;
}

impl Store for MemLogStore {
    fn bump(&self, room: RoomId) -> impl Future<Output = Epoch> + Send {
        std::future::ready(self.claim(room))
    }
}

#[cfg(feature = "pg-tests")]
impl Store for crate::PgLogStore {
    async fn bump(&self, room: RoomId) -> Epoch {
        self.claim(room).await.unwrap()
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
        let writer = tokio::spawn({
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
        if i % 2 == 0 {
            first.notified().await;
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

#[cfg(feature = "pg-tests")]
mod pg_only {
    use super::*;
    use crate::{OpenError, PgLogStore};

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

    #[tokio::test]
    async fn a_room_reopens_with_its_bundle_and_refuses_another() {
        let s = pg_store().await;
        s.open_room(R, "seed", 1, &[1, 2]).await.unwrap();
        s.open_room(R, "seed", 1, &[1, 2]).await.unwrap();
        assert_eq!(s.open_room(R, "seed", 1, &[1, 3]).await, Err(OpenError::OtherBundle));
        s.open_room(RoomId(2), "seed", 1, &[1, 3]).await.unwrap();
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
