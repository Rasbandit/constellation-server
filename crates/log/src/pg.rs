//! Postgres `LogStore` (D-98, gate G-DB): plain SQL through `sqlx`, migrations from the
//! workspace `migrations/` directory, schema per doc 02 section 5.5 cut to what is used today.
//!
//! **Fence.** Every write locks the room's `room_owner` row `FOR SHARE` before it checks the
//! epoch, and holds the lock to commit. A claim is an `UPDATE` of that row, which needs a
//! `FOR NO KEY UPDATE` row lock, and the Postgres docs (explicit locking, "Conflicting Row-Level
//! Locks") list that as conflicting with `FOR SHARE`. So a claim waits for every in-flight
//! write, and a write that reaches the lock after a claim waits for it, then reads the new epoch
//! (READ COMMITTED re-reads a row it waited on) and is fenced. A plain `SELECT` could read the
//! old epoch and commit after the claim; `FOR KEY SHARE` does not conflict with a non-key
//! `UPDATE`, so it would not work either. `FOR UPDATE` works but is stronger than needed.
//!
//! **Cancel safety** (sqlx issue 2805: dropping `commit()` or `rollback()` mid-flight can leave
//! the connection out of step). Every call runs in its own spawned task, so dropping the
//! caller's future never cancels a transaction halfway; the caller just stops waiting. A call
//! that fails for any reason closes its connection instead of returning it to the pool.
//!
//! **Ranges.** Postgres has no unsigned 64-bit integer. Room ids are stored bit for bit as
//! `bigint`; a `seq`, `item_seq` or epoch above `i64::MAX` cannot be stored and is refused as
//! `Corrupt` (an epoch that large can never be current, so it is `Fenced`). `MemLogStore`
//! accepts them; no real room gets near that.

use crate::{
    AppendBatch, AppendError, AppendReceipt, CursorUpdate, LogStore, Snapshot, StoreError,
};
use config::Limits;
use proto::{Epoch, Event, ItemSeq, RoomId, Seq, SlotNo};
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPoolOptions};
use sqlx::{PgPool, Postgres};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::str::FromStr;

/// The embedded migrations (`migrations/` at the workspace root).
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// The Postgres store. Cheap to clone (the pool is shared).
#[derive(Debug, Clone)]
pub struct PgLogStore {
    pool: PgPool,
}

/// Why a stored room cannot be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The database failed.
    Store(StoreError),
    /// The room id is taken by a room made from another bundle.
    OtherBundle,
}

type Fut<'c, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>;

/// The two error types the store returns, so one helper can map database failures for both.
trait PgError: Send + 'static {
    fn unavailable() -> Self;
    fn corrupt(why: String) -> Self;
}

impl PgError for AppendError {
    fn unavailable() -> Self {
        Self::Unavailable
    }
    fn corrupt(why: String) -> Self {
        Self::Corrupt(why)
    }
}

impl PgError for StoreError {
    fn unavailable() -> Self {
        Self::Unavailable
    }
    fn corrupt(why: String) -> Self {
        Self::Corrupt(why)
    }
}

/// A duplicate key means two writers raced past the fence with the same epoch: corrupt, not
/// transient. Everything else (IO, pool, protocol) is worth a retry.
fn db<E: PgError>(e: &sqlx::Error) -> E {
    tracing::warn!(error = %e, "postgres call failed");
    match e.as_database_error() {
        Some(d) if d.is_unique_violation() => E::corrupt(format!("duplicate key: {d}")),
        _ => E::unavailable(),
    }
}

/// Room ids are equality keys only, so a bit-for-bit cast keeps every value.
const fn room_key(room: RoomId) -> i64 {
    i64::from_be_bytes(room.0.to_be_bytes())
}

/// A read bound: no stored value is above `i64::MAX`, so clamping changes no result.
fn bound(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn stored<E: PgError>(v: i64, what: &str) -> Result<u64, E> {
    u64::try_from(v).map_err(|_| E::corrupt(format!("negative {what} {v} in the database")))
}

fn storable(v: u64, what: &str) -> Result<i64, AppendError> {
    i64::try_from(v).map_err(|_| AppendError::Corrupt(format!("{what} {v} is above i64::MAX")))
}

fn decode(seq: i64, body: &[u8]) -> Result<Event, StoreError> {
    let e: Event = serde_json::from_slice(body)
        .map_err(|e| StoreError::Corrupt(format!("event {seq}: {e}")))?;
    if Some(e.seq.0) == u64::try_from(seq).ok() {
        Ok(e)
    } else {
        Err(StoreError::Corrupt(format!("event row {seq} holds seq {}", e.seq.0)))
    }
}

impl PgLogStore {
    /// Connect with the pool size and session timeouts from `limits`, and apply any pending
    /// migrations. The timeouts bound every wait: a writer that hangs mid-transaction is ended
    /// by the server (`idle_in_transaction_session_timeout`), and a claim or write queued
    /// behind it errors (`lock_timeout`) rather than blocking startup or the room forever.
    ///
    /// # Errors
    /// `Unavailable` when the URL is invalid, the database cannot be reached or a migration
    /// fails.
    pub async fn connect(url: &str, limits: &Limits) -> Result<Self, StoreError> {
        // These session settings apply to the migrations below too (a stuck migration lock errors).
        let options = PgConnectOptions::from_str(url).map_err(|e| db::<StoreError>(&e))?.options([
            ("statement_timeout", limits.db_statement_timeout_ms),
            ("lock_timeout", limits.db_lock_timeout_ms),
            ("idle_in_transaction_session_timeout", limits.db_idle_in_transaction_timeout_ms),
        ]);
        let pool = PgPoolOptions::new()
            .max_connections(limits.db_pool_connections.max(1))
            .connect_with(options)
            .await
            .map_err(|e| db::<StoreError>(&e))?;
        MIGRATOR.run(&pool).await.map_err(|e| {
            tracing::error!(error = %e, "migration failed");
            StoreError::Unavailable
        })?;
        Ok(Self { pool })
    }

    /// The pool, for tests that look at connections directly.
    #[cfg(test)]
    #[must_use]
    pub const fn pool_for_tests(&self) -> &PgPool {
        &self.pool
    }

    /// Run `body` on one pooled connection in its own task. Dropping the returned future does
    /// not cancel `body`; an error closes the connection rather than pooling it (issue 2805).
    async fn isolated<T, E>(
        &self,
        body: impl for<'c> FnOnce(&'c mut PgConnection) -> Fut<'c, T, E> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: PgError,
    {
        let pool = self.pool.clone();
        let task = tokio::spawn(async move {
            let mut conn: PoolConnection<Postgres> =
                pool.acquire().await.map_err(|e| db::<E>(&e))?;
            let r = body(&mut conn).await;
            if r.is_err() {
                conn.close_on_drop();
            }
            r
        });
        // A panic in the task is a bug; the caller sees the store as unavailable.
        task.await.unwrap_or_else(|_| Err(E::unavailable()))
    }

    /// Record which bundle made `room` (its format version and content hash), or check the
    /// hash against the record.
    ///
    /// # Errors
    /// `OtherBundle` when the room was created from a different bundle, `Store` on failure.
    pub async fn open_room(
        &self,
        room: RoomId,
        seed_name: &str,
        bundle_format: u32,
        bundle_hash: &[u8],
    ) -> Result<(), OpenError> {
        let (seed, hash) = (seed_name.to_owned(), bundle_hash.to_vec());
        let stored = self
            .isolated(move |c| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO rooms (room_id, seed_name, bundle_format, bundle_hash) \
                         VALUES ($1, $2, $3, $4) ON CONFLICT (room_id) DO NOTHING",
                    )
                    .bind(room_key(room))
                    .bind(seed)
                    .bind(i64::from(bundle_format))
                    .bind(&hash)
                    .execute(&mut *c)
                    .await
                    .map_err(|e| db::<StoreError>(&e))?;
                    sqlx::query_scalar::<_, Vec<u8>>(
                        "SELECT bundle_hash FROM rooms WHERE room_id = $1",
                    )
                    .bind(room_key(room))
                    .fetch_one(&mut *c)
                    .await
                    .map_err(|e| db::<StoreError>(&e))
                    .map(|stored| stored == hash)
                })
            })
            .await
            .map_err(OpenError::Store)?;
        if stored { Ok(()) } else { Err(OpenError::OtherBundle) }
    }

    /// Take the room: bump its owner epoch and return the new one (the directory's claim, like
    /// `MemLogStore::claim`; the first claim returns 1). Returns only after every write under
    /// an older epoch has committed or failed, and no older epoch commits after it.
    ///
    /// # Errors
    /// `Unavailable` when the database fails.
    pub async fn claim(&self, room: RoomId) -> Result<Epoch, StoreError> {
        self.isolated(move |c| {
            Box::pin(async move {
                let e: i64 = sqlx::query_scalar(
                    "INSERT INTO room_owner (room_id, epoch) VALUES ($1, 1) \
                     ON CONFLICT (room_id) DO UPDATE SET epoch = room_owner.epoch + 1 \
                     RETURNING epoch",
                )
                .bind(room_key(room))
                .fetch_one(&mut *c)
                .await
                .map_err(|e| db::<StoreError>(&e))?;
                stored(e, "epoch").map(Epoch)
            })
        })
        .await
    }
}

/// Lock the owner row `FOR SHARE` (see the module docs) and check `epoch` against it. An
/// unclaimed room is at epoch 0, as in `MemLogStore`; its row is created first so even epoch 0
/// has a row to lock against a concurrent first claim.
async fn fence(c: &mut PgConnection, room: i64, epoch: Epoch) -> Result<(), AppendError> {
    const LOCK: &str = "SELECT epoch FROM room_owner WHERE room_id = $1 FOR SHARE";
    let mut current: Option<i64> = sqlx::query_scalar(LOCK)
        .bind(room)
        .fetch_optional(&mut *c)
        .await
        .map_err(|e| db::<AppendError>(&e))?;
    // `||` here would be an equivalent mutant (excluded in nightly.yml): the extra insert either
    // hits an existing row (`DO NOTHING`) or adds epoch 0 for a non-zero epoch, which is then
    // fenced, and every caller rolls the transaction back on that error.
    if current.is_none() && epoch.0 == 0 {
        sqlx::query(
            "INSERT INTO room_owner (room_id, epoch) VALUES ($1, 0) ON CONFLICT DO NOTHING",
        )
        .bind(room)
        .execute(&mut *c)
        .await
        .map_err(|e| db::<AppendError>(&e))?;
        current = sqlx::query_scalar(LOCK)
            .bind(room)
            .fetch_optional(&mut *c)
            .await
            .map_err(|e| db::<AppendError>(&e))?;
    }
    let current = Epoch(current.map_or(Ok(0), |e| stored::<AppendError>(e, "epoch"))?);
    if current == epoch { Ok(()) } else { Err(AppendError::Fenced { current }) }
}

/// The log head, read after the fence lock so it sees every commit of earlier owners.
async fn head(c: &mut PgConnection, room: i64) -> Result<u64, AppendError> {
    let h: i64 = sqlx::query_scalar("SELECT coalesce(max(seq), 0) FROM events WHERE room_id = $1")
        .bind(room)
        .fetch_one(&mut *c)
        .await
        .map_err(|e| db::<AppendError>(&e))?;
    stored(h, "head")
}

/// An append with everything owned, so it can run in its own task.
struct Job {
    room: i64,
    epoch: Epoch,
    seqs: Vec<u64>,
    bodies: Vec<Vec<u8>>,
    /// Slot to highest applied, merged like `MemLogStore` (the maximum wins).
    cursors: BTreeMap<SlotNo, ItemSeq>,
}

impl Job {
    fn new(batch: &AppendBatch<'_>) -> Result<Self, AppendError> {
        let bodies = batch
            .events
            .iter()
            .map(serde_json::to_vec)
            .collect::<Result<_, _>>()
            .map_err(|e| AppendError::Corrupt(format!("event encoding: {e}")))?;
        let mut cursors = BTreeMap::new();
        for c in batch.cursors {
            let v: &mut ItemSeq = cursors.entry(c.slot).or_default();
            *v = (*v).max(c.applied);
        }
        Ok(Self {
            room: room_key(batch.room),
            epoch: batch.epoch,
            seqs: batch.events.iter().map(|e| e.seq.0).collect(),
            bodies,
            cursors,
        })
    }

    /// One transaction: fence, continuity check, events, cursors. All or nothing.
    async fn run(self, c: &mut PgConnection) -> Result<AppendReceipt, AppendError> {
        let mut tx = sqlx::Connection::begin(&mut *c).await.map_err(|e| db::<AppendError>(&e))?;
        fence(&mut tx, self.room, self.epoch).await?;
        let mut expected = head(&mut tx, self.room).await?;
        for &seq in &self.seqs {
            expected = expected
                .checked_add(1)
                .ok_or_else(|| AppendError::Corrupt("seq overflow".into()))?;
            if seq != expected {
                return Err(AppendError::Corrupt(format!("expected seq {expected}, got {seq}")));
            }
        }
        if !self.seqs.is_empty() {
            let seqs =
                self.seqs.iter().map(|&s| storable(s, "seq")).collect::<Result<Vec<_>, _>>()?;
            sqlx::query(
                "INSERT INTO events (room_id, seq, body) \
                 SELECT $1, s, b FROM UNNEST($2::int8[], $3::bytea[]) AS t(s, b)",
            )
            .bind(self.room)
            .bind(seqs)
            .bind(self.bodies)
            .execute(&mut *tx)
            .await
            .map_err(|e| db::<AppendError>(&e))?;
        }
        if !self.cursors.is_empty() {
            let slots: Vec<i64> = self.cursors.keys().map(|s| i64::from(s.0)).collect();
            let applied = self
                .cursors
                .values()
                .map(|a| storable(a.0, "item_seq"))
                .collect::<Result<Vec<_>, _>>()?;
            sqlx::query(
                "INSERT INTO slot_cursors (room_id, slot_no, applied) \
                 SELECT $1, s, a FROM UNNEST($2::int8[], $3::int8[]) AS t(s, a) \
                 ON CONFLICT (room_id, slot_no) \
                 DO UPDATE SET applied = GREATEST(slot_cursors.applied, EXCLUDED.applied)",
            )
            .bind(self.room)
            .bind(slots)
            .bind(applied)
            .execute(&mut *tx)
            .await
            .map_err(|e| db::<AppendError>(&e))?;
        }
        tx.commit().await.map_err(|e| db::<AppendError>(&e))?;
        Ok(AppendReceipt { head: Seq(expected) })
    }
}

impl LogStore for PgLogStore {
    async fn append(&self, batch: AppendBatch<'_>) -> Result<AppendReceipt, AppendError> {
        let job = Job::new(&batch)?;
        self.isolated(move |c| Box::pin(job.run(c))).await
    }

    async fn read_tail(
        &self,
        room: RoomId,
        after: Seq,
        limit: usize,
    ) -> Result<Vec<Event>, StoreError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.isolated(move |c| {
            Box::pin(async move {
                let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
                    "SELECT seq, body FROM events WHERE room_id = $1 AND seq > $2 \
                     ORDER BY seq LIMIT $3",
                )
                .bind(room_key(room))
                .bind(bound(after.0))
                .bind(limit)
                .fetch_all(&mut *c)
                .await
                .map_err(|e| db::<StoreError>(&e))?;
                rows.iter().map(|(s, b)| decode(*s, b)).collect()
            })
        })
        .await
    }

    async fn read_range(&self, room: RoomId, from: Seq, to: Seq) -> Result<Vec<Event>, StoreError> {
        self.isolated(move |c| {
            Box::pin(async move {
                let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
                    "SELECT seq, body FROM events WHERE room_id = $1 AND seq BETWEEN $2 AND $3 \
                     ORDER BY seq",
                )
                .bind(room_key(room))
                .bind(bound(from.0))
                .bind(bound(to.0))
                .fetch_all(&mut *c)
                .await
                .map_err(|e| db::<StoreError>(&e))?;
                rows.iter().map(|(s, b)| decode(*s, b)).collect()
            })
        })
        .await
    }

    async fn read_cursors(&self, room: RoomId) -> Result<Vec<CursorUpdate>, StoreError> {
        self.isolated(move |c| {
            Box::pin(async move {
                let rows: Vec<(i64, i64)> = sqlx::query_as(
                    "SELECT slot_no, applied FROM slot_cursors WHERE room_id = $1 ORDER BY slot_no",
                )
                .bind(room_key(room))
                .fetch_all(&mut *c)
                .await
                .map_err(|e| db::<StoreError>(&e))?;
                rows.into_iter()
                    .map(|(s, a)| {
                        let slot = u32::try_from(s)
                            .map_err(|_| StoreError::Corrupt(format!("slot {s} out of range")))?;
                        Ok(CursorUpdate {
                            slot: SlotNo(slot),
                            applied: ItemSeq(stored(a, "cursor")?),
                        })
                    })
                    .collect()
            })
        })
        .await
    }

    async fn snapshot_get(&self, room: RoomId) -> Result<Option<Snapshot>, StoreError> {
        self.isolated(move |c| {
            Box::pin(async move {
                let row: Option<(i64, Vec<u8>)> =
                    sqlx::query_as("SELECT upto_seq, body FROM snapshots WHERE room_id = $1")
                        .bind(room_key(room))
                        .fetch_optional(&mut *c)
                        .await
                        .map_err(|e| db::<StoreError>(&e))?;
                row.map(|(s, data)| Ok(Snapshot { seq: Seq(stored(s, "snapshot seq")?), data }))
                    .transpose()
            })
        })
        .await
    }

    async fn snapshot_put(
        &self,
        room: RoomId,
        snap: Snapshot,
        epoch: Epoch,
    ) -> Result<(), AppendError> {
        self.isolated(move |c| {
            Box::pin(async move {
                let room = room_key(room);
                let mut tx =
                    sqlx::Connection::begin(&mut *c).await.map_err(|e| db::<AppendError>(&e))?;
                fence(&mut tx, room, epoch).await?;
                if snap.seq.0 > head(&mut tx, room).await? {
                    return Err(AppendError::Corrupt("snapshot ahead of the log head".into()));
                }
                // The same position may be rewritten; an older one never replaces a newer one.
                let written = sqlx::query(
                    "INSERT INTO snapshots (room_id, upto_seq, body) VALUES ($1, $2, $3) \
                     ON CONFLICT (room_id) DO UPDATE \
                     SET upto_seq = EXCLUDED.upto_seq, body = EXCLUDED.body \
                     WHERE snapshots.upto_seq <= EXCLUDED.upto_seq",
                )
                .bind(room)
                .bind(storable(snap.seq.0, "seq")?)
                .bind(snap.data)
                .execute(&mut *tx)
                .await
                .map_err(|e| db::<AppendError>(&e))?;
                if written.rows_affected() == 0 {
                    return Err(AppendError::Corrupt("snapshot older than the stored one".into()));
                }
                tx.commit().await.map_err(|e| db::<AppendError>(&e))
            })
        })
        .await
    }
}

/// Test support: a fresh schema per test, so tests share one database and never each other's
/// rows (docs/TESTING.md, end-to-end rules). Behind `pg-tests`; never used by the product.
#[cfg(feature = "pg-tests")]
pub mod testing {
    use sqlx::Connection;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Create an empty schema in the database at `url` and return a URL whose connections
    /// use it (`options[search_path]`, understood by sqlx).
    ///
    /// # Errors
    /// When the database cannot be reached; callers fail the test (a `pg-tests` run without
    /// Postgres must fail, never skip).
    pub async fn fresh_schema(url: &str) -> Result<String, String> {
        static N: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let schema =
            format!("t_{}_{}_{nanos}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let mut c = sqlx::PgConnection::connect(url)
            .await
            .map_err(|e| format!("pg-tests: cannot reach CONSTELLATION_DATABASE_URL: {e}"))?;
        // The name is built from digits and underscores above, never from input.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut c)
            .await
            .map_err(|e| format!("pg-tests: create schema: {e}"))?;
        let sep = if url.contains('?') { '&' } else { '?' };
        Ok(format!("{url}{sep}options[search_path]={schema}"))
    }
}

/// The nightly workflow excludes `fence`'s `&&` to `||` mutant by name (equivalent, see the
/// comment in `fence`). A second `&&` in `fence` would be excluded silently with it, so this
/// fails first: whoever adds one must narrow the exclusion in nightly.yml.
#[cfg(test)]
#[test]
fn fence_has_exactly_the_one_and_the_mutants_exclusion_names() {
    let (_, from_fence) = include_str!("pg.rs").split_once("async fn fence(").expect("fence");
    let (body, _) = from_fence.split_once("\n}\n").expect("fence ends");
    assert_eq!(body.matches("&&").count(), 1, "narrow the fence exclusion in nightly.yml");
}
