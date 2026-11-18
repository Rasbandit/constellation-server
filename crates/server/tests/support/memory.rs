//! The memory harness: bytes a loaded bundle, a room and a connection hold, counted by
//! `allocation-counter`'s global allocator on the server's thread only.
//!
//! The server runs on a current-thread runtime on the calling thread, so everything it
//! allocates is counted; the clients run on their own thread and runtime and are not.
//! "Held" is the net bytes still allocated when a phase ends (`bytes_current`): what stays
//! resident, not the peak. Allocator overhead per block is not included.

use allocation_counter::measure;
use config::Limits;
use seed_bundle::SeedBundle;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::Path;
use tokio::sync::mpsc;

/// Bytes held after each phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Numbers {
    /// The first load of the bundle in this process.
    pub bundle_first: i64,
    /// A second load of the same bundle (what a further room of the same game costs).
    pub bundle_next: i64,
    /// The room built from the loaded bundle: edge, room task and state (`server::app`).
    pub room: i64,
    /// room-core state alone, fresh.
    pub state_fresh: i64,
    /// room-core state after every location of every slot is checked.
    pub state_full: i64,
    /// The first connection: one session plus the process's first pooled compressor.
    pub conn_first: i64,
    /// One more connected session in a fresh room, socket and deflate state included.
    pub conn_fresh: i64,
    /// One connected session that received every item of a fully checked slot.
    pub conn_full: i64,
}

impl Numbers {
    /// One active room: its loaded bundle (game packages shared), the room at its fullest,
    /// and one connection per slot.
    #[must_use]
    pub fn per_room(&self, slots: i64) -> i64 {
        self.bundle_next + self.room + (self.state_full - self.state_fresh) + slots * self.conn_full
    }

    /// A table row.
    #[must_use]
    pub fn row(&self, name: &str) -> String {
        format!(
            "{name}: bundle_first={} bundle_next={} room={} state_fresh={} state_full={} \
             conn_first={} conn_fresh={} conn_full={}",
            self.bundle_first,
            self.bundle_next,
            self.room,
            self.state_fresh,
            self.state_full,
            self.conn_first,
            self.conn_fresh,
            self.conn_full
        )
    }
}

/// How many sockets the second connection phase opens (the number is per socket).
const SOCKETS: i64 = 4;

/// What the client thread is told to do next.
enum Step {
    /// Open one socket as slot 1 and keep it.
    First(String),
    /// Open `SOCKETS` more sockets as slot 1 and keep them.
    Fresh,
    /// Check every location of every slot.
    Fill,
    /// Open one more socket as slot 1 and take its whole inventory.
    Full,
    /// Close everything.
    Done,
}

fn connect_packet(name: &str, game: &str) -> Value {
    json!([{
        "cmd": "Connect", "password": null, "game": game, "name": name, "uuid": "mem",
        "items_handling": 7, "tags": ["AP"], "slot_data": true,
        "version": {"major": 0, "minor": 6, "build": 8, "class": "Version"}
    }])
}

/// Connect as `slot` and wait until the server has finished with the connect: a `Get`
/// sent after `Connect` is answered only after it.
async fn join(url: &str, b: &SeedBundle, slot: u32) -> e2e::Client {
    let s = b.slot(slot).unwrap();
    let mut c = e2e::Client::connect(url).await.unwrap();
    c.next_cmd("RoomInfo").await.unwrap();
    c.send(&connect_packet(&s.name, &s.game)).await.unwrap();
    c.next_cmd("Connected").await.unwrap();
    c.send(&json!([{"cmd": "Get", "keys": []}])).await.unwrap();
    c.next_cmd("Retrieved").await.unwrap();
    c
}

fn client_thread(
    b: SeedBundle,
    mut steps: mpsc::Receiver<Step>,
    done: mpsc::Sender<()>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let mut kept = Vec::new();
            let mut url = String::new();
            while let Some(step) = steps.recv().await {
                match step {
                    Step::First(u) => {
                        url = u;
                        kept.push(join(&url, &b, 1).await);
                    }
                    Step::Fresh => {
                        for _ in 0..SOCKETS {
                            kept.push(join(&url, &b, 1).await);
                        }
                    }
                    Step::Fill => {
                        for s in &b.slots {
                            let mut c = join(&url, &b, s.id).await;
                            let all: Vec<i64> = s.locations.iter().map(|p| p.location).collect();
                            c.check(&all).await.unwrap();
                            let mut seen = 0;
                            while seen < all.len() {
                                let (u, _) = c.next_cmd("RoomUpdate").await.unwrap();
                                seen += u["checked_locations"].as_array().map_or(0, Vec::len);
                            }
                            c.close().await.unwrap();
                        }
                    }
                    Step::Full => kept.push(join(&url, &b, 1).await),
                    Step::Done => break,
                }
                done.send(()).await.unwrap();
            }
            drop(kept);
        });
    })
}

/// room-core state of `b`: fresh, then with every location checked.
fn state(load: &Path, b: &SeedBundle) -> (i64, i64) {
    use proto::{Cid, Command, SlotNo, Timestamp};
    let loaded = server::load_bundle(load, &Limits::DESIGN).unwrap();
    let limits = Limits::DESIGN;
    let ctx = room_core::Ctx { now: Timestamp(1), limits: &limits, powf: f64::powf };
    let mut held = None;
    let fresh = measure(|| held = Some(server::fresh_state(&loaded).unwrap())).bytes_current;
    let mut s = held.unwrap();
    let full = measure(|| {
        for slot in &b.slots {
            for (n, p) in slot.locations.iter().enumerate() {
                let cmd = Command::Check {
                    slot: SlotNo(slot.id),
                    cid: Cid(n as u64),
                    location: p.location,
                };
                room_core::apply(&mut s, &ctx, cmd).unwrap();
            }
        }
        s.mark_durable(s.head()).unwrap();
    })
    .bytes_current;
    drop(s);
    (fresh, fresh + full)
}

/// Measure the bundle at `load` (v1 JSON or v2), whose v1 form is `b`.
///
/// # Panics
/// On any failure: this is a test harness.
#[must_use]
pub fn measure_bundle(load: &Path, b: &SeedBundle) -> Numbers {
    let limits = Limits::DESIGN;
    let mut first = None;
    let bundle_first = measure(|| first = Some(server::load_bundle(load, &limits).unwrap()));
    let mut next = None;
    let bundle_next = measure(|| next = Some(server::load_bundle(load, &limits).unwrap()));
    drop(next);
    let (state_fresh, state_full) = state(load, b);

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (step_tx, step_rx) = mpsc::channel(1);
    let (done_tx, mut done_rx) = mpsc::channel(1);
    let client = client_thread(b.clone(), step_rx, done_tx);
    let mut router = None;
    let room = measure(|| {
        let loaded = first.take().unwrap();
        router = Some(rt.block_on(server::app(loaded, None, None, limits)).unwrap());
    });
    let (router, _task) = router.unwrap();
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let _serve = rt.spawn(async move {
        axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await
    });
    let mut phase = |step: Step| {
        measure(|| {
            rt.block_on(async {
                step_tx.send(step).await.unwrap();
                done_rx.recv().await.unwrap();
            });
        })
        .bytes_current
    };
    let conn_first = phase(Step::First(url));
    let conn_fresh = phase(Step::Fresh) / SOCKETS;
    phase(Step::Fill);
    let conn_full = phase(Step::Full);
    rt.block_on(step_tx.send(Step::Done)).unwrap();
    client.join().unwrap();
    Numbers {
        bundle_first: bundle_first.bytes_current,
        bundle_next: bundle_next.bytes_current,
        room: room.bytes_current,
        state_fresh,
        state_full,
        conn_first,
        conn_fresh,
        conn_full,
    }
}

/// Write `text` to a fresh temporary file named `name` and return its path.
#[must_use]
pub fn temp_file(name: &str, text: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("memory-harness-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}
