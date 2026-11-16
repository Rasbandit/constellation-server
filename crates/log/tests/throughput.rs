//! Gate G-DB throughput microbenchmark (doc 11 section 4.8): 1,000 rooms, 20 ms windows,
//! reporting committed rows per second. Nightly CI only (`--features pg-bench`), never the PR
//! gate: runner variance would make it flaky. Fails below 60 percent of the 20,000 rows per
//! second design value (doc 02 section 5.4, an assumption to benchmark).
//!
//! Two writers, both fenced the way `PgLogStore` fences (owner rows locked `FOR SHARE`):
//! - **group**: one transaction per window for every room's events, inserted with `UNNEST`.
//!   This is the design's batching (group commit across rooms); gated.
//! - **store**: `PgLogStore::append` per room, all rooms concurrently each window (what the
//!   server runs today, one transaction per room batch). Reported, not gated.
//!
//! `COPY` into a temp table is not measured: it needs a second statement per window and only
//! pays off for much larger batches than 1,000 to 2,000 rows (deferred, docs/TESTING.md).
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the log crate"
)]
#![allow(clippy::arithmetic_side_effects, reason = "benchmark counters stay small")]
#![allow(clippy::print_stdout, reason = "the benchmark prints its numbers for the CI log")]

#[cfg(test)]
mod bench {
    use log::{AppendBatch, LogStore, PgLogStore};
    use proto::{Cid, Epoch, Event, EventKind, RoomId, Seq, SlotNo, Timestamp};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const ROOMS: u64 = 1_000;
    const WINDOW: Duration = Duration::from_millis(20);
    /// Events per room per window offered to the group writer: 2,000 rows per 20 ms window is
    /// 100,000 rows per second offered, five times the design value, so the writer is the limit.
    const ROWS_PER_ROOM: u64 = 2;
    const RUN: Duration = Duration::from_secs(10);
    const DESIGN_ROWS_PER_SEC: f64 = 20_000.0;
    const FLOOR: f64 = 0.6 * DESIGN_ROWS_PER_SEC;

    fn ev(seq: u64) -> Event {
        Event {
            seq: Seq(seq),
            slot: Some(SlotNo(1)),
            kind: EventKind::Checked {
                cid: Cid(seq),
                location: 123_456,
                at: Timestamp(1_763_000_000),
            },
        }
    }

    async fn setup() -> (PgLogStore, sqlx::PgPool, Vec<Epoch>) {
        let base = config::load_secrets()
            .unwrap()
            .database_url
            .expect("pg-bench needs CONSTELLATION_DATABASE_URL (no silent skip)");
        let url = log::testing::fresh_schema(&base).await.unwrap();
        let store = PgLogStore::connect(&url, 8).await.unwrap();
        let pool =
            sqlx::postgres::PgPoolOptions::new().max_connections(8).connect(&url).await.unwrap();
        // One bulk claim for every room (epoch 1, as a first `claim` returns).
        sqlx::query(
            "INSERT INTO room_owner (room_id, epoch) SELECT r, 1 FROM generate_series(1, $1) r",
        )
        .bind(i64::try_from(ROOMS).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        (store, pool, (0..ROOMS).map(|_| Epoch(1)).collect())
    }

    fn report(name: &str, rows: u64, elapsed: Duration) -> f64 {
        let rate = f64::from(u32::try_from(rows).unwrap()) / elapsed.as_secs_f64();
        println!(
            "G-DB throughput [{name}]: {rows} rows in {elapsed:.2?} = {rate:.0} rows/s (floor {FLOOR:.0}, design {DESIGN_ROWS_PER_SEC:.0}; measured on this runner)"
        );
        rate
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_writer_commits_at_least_60_percent_of_the_design_rows_per_second() {
        let (store, pool, _) = setup().await;
        let rooms: Vec<i64> = (1..=i64::try_from(ROOMS).unwrap()).collect();
        let body = serde_json::to_vec(&ev(1)).unwrap();

        // Group commit: every room's window in one fenced transaction.
        let start = Instant::now();
        let (mut window, mut rows) = (0_u64, 0_u64);
        while start.elapsed() < RUN {
            let tick = Instant::now();
            let (mut room_col, mut seq_col) = (Vec::new(), Vec::new());
            for &r in &rooms {
                for k in 1..=ROWS_PER_ROOM {
                    room_col.push(r);
                    seq_col.push(i64::try_from(window * ROWS_PER_ROOM + k).unwrap());
                }
            }
            let bodies = vec![body.clone(); room_col.len()];
            let mut tx = pool.begin().await.unwrap();
            let fenced: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM (SELECT 1 FROM room_owner \
                 WHERE room_id = ANY($1) AND epoch = 1 FOR SHARE) t",
            )
            .bind(&rooms)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            assert_eq!(fenced, i64::try_from(ROOMS).unwrap(), "every room's epoch is current");
            sqlx::query(
                "INSERT INTO events (room_id, seq, body) \
                 SELECT * FROM UNNEST($1::int8[], $2::int8[], $3::bytea[])",
            )
            .bind(&room_col)
            .bind(&seq_col)
            .bind(&bodies)
            .execute(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            rows += u64::try_from(room_col.len()).unwrap();
            window += 1;
            if let Some(rest) = WINDOW.checked_sub(tick.elapsed()) {
                tokio::time::sleep(rest).await; // pacing the windows, not waiting for a result
            }
        }
        let group = report("group commit, UNNEST", rows, start.elapsed());

        // The store as the server uses it: one transaction per room per window, rooms in
        // parallel. These rooms start fresh (ids above the group rooms).
        let store = Arc::new(store);
        let start = Instant::now();
        let (mut window, mut rows) = (0_u64, 0_u64);
        while start.elapsed() < RUN {
            let tick = Instant::now();
            let tasks: Vec<_> = (0..ROOMS)
                .map(|r| {
                    let s = Arc::clone(&store);
                    tokio::spawn(async move {
                        let room = RoomId(ROOMS + 1 + r);
                        let events = [ev(window + 1)];
                        s.append(AppendBatch {
                            room,
                            epoch: Epoch(0),
                            events: &events,
                            cursors: &[],
                        })
                        .await
                        .unwrap();
                    })
                })
                .collect();
            for t in tasks {
                t.await.unwrap();
            }
            rows += ROOMS;
            window += 1;
            if let Some(rest) = WINDOW.checked_sub(tick.elapsed()) {
                tokio::time::sleep(rest).await;
            }
        }
        report("PgLogStore::append per room", rows, start.elapsed());

        assert!(
            group >= FLOOR,
            "group commit {group:.0} rows/s is below the G-DB floor {FLOOR:.0}"
        );
    }
}
