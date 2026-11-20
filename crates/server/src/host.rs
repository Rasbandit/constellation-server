//! The rooms a process lists, and how they move between states (doc 02 section 4.3).
//!
//! The directory is the list of rooms. With Postgres it is the `rooms` table: every stored room
//! that has not finished is listed at startup, so rooms come from the database and the folder
//! is only a way to add them. In memory the host keeps the directory itself.
//!
//! A listed room is **cold** until its first connect, which loads it (snapshot plus log tail,
//! claimed under a new epoch) and makes it **resident**. A resident room with no connection
//! for `room_idle_secs` (15 min, design value) leaves memory and is cold again; its next
//! connect loads it again, under the same key. A room with no connection for
//! `room_abandon_secs` (30 days, D-30) **finishes** as abandoned: it is no longer listed,
//! Postgres marks it finished so neither a scan nor a restart brings it back, and its data is
//! kept. Retention after that (doc 02 sections 4.3 and 5.4) and the other ends of a room (all
//! goals, the host closes it) are #56.
//!
//! At most `max_resident_rooms` rooms are resident at once. A cold room that must load past
//! the cap puts the resident room unused for the longest to sleep first, as the idle check
//! would later (memory-pressure eviction, doc 02 D5). A room with a connection open is never
//! put to sleep: when every resident room has one, the load is refused until one frees.
//!
//! No room ever runs without a connection (there are no room timers yet), so "no connection"
//! here is also "no event", as D-30 asks.
//!
//! With `--bundle` the one room is loaded at startup, never leaves memory and is never
//! abandoned, and its end is the process's end, as before. With `--rooms`, one room's trouble never stops the others: a
//! bad bundle is skipped with a warning, and a room whose task fails goes cold, so its next
//! connect loads it again from its log. Only a fence ends the process, because then another
//! process owns the database.
//!
//! A room that fails quickly (its load fails, or its task ends within `room_healthy_secs` of
//! its load) waits before it loads again: `room_retry_first_ms`, doubled for each quick
//! failure in a row, at most `room_retry_max_ms` (#64). Until then a connect is refused
//! without a load. Its count is forgotten once it has been in memory for `room_healthy_secs`.
//! A room that fails on every load, whatever the wait, is frozen instead in #96.

use crate::rooms::{Activity, Listing, Moment, RoomKey, Rooms, read, write};
use crate::{
    Loaded, RunError, load_bundle, open_with_key, parse_bundle, prepare, room_ended, start,
};
use config::Limits;
use log::{BlobKey, BlobStore, LogStore, MemLogStore, PgLogStore};
use proto::RoomId;
use runtime::Stopped;
use seed_bundle::{GamePackage, Seed};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinError, JoinHandle, JoinSet};
use tokio::time::Instant;

/// Where the directory and the rooms' logs live.
enum Store {
    Pg(Arc<PgLogStore>),
    /// In memory: the log store (which also keeps seeds and packages) and the rooms that
    /// finished.
    Mem {
        log: Arc<MemLogStore>,
        finished: BTreeSet<RoomId>,
    },
}

/// A room in memory, as the host tracks its task.
struct Running {
    /// Tells this load of the room from an earlier one under the same key.
    run: u64,
    task: AbortHandle,
    /// When it loaded, to tell a quick failure from a healthy run.
    since: Instant,
}

/// A room whose last loads failed quickly: how many in a row, and when it may load again.
struct Failing {
    count: u32,
    until: Instant,
}

/// How long a room waits before its next load after `count` quick failures in a row: the
/// first wait, doubled for each failure after the first, at most the longest.
fn retry_wait(limits: &Limits, count: u32) -> Duration {
    let first = limits.room_retry_first_ms;
    let doubled = first.saturating_mul(1 << count.saturating_sub(1).min(32));
    Duration::from_millis(doubled.min(limits.room_retry_max_ms.max(first)))
}

/// What a room task's end is reported with: its key, which load it was, and why it ended.
type Ended = (RoomKey, u64, Result<Stopped, JoinError>);

/// The reason a room left alone for `room_abandon_secs` ends with (D-30).
const ABANDONED: &str = "abandoned";

/// Seconds in a day, for the log line of an abandoned room.
const DAY_SECS: u64 = 24 * 60 * 60;

/// How many whole days `d` is.
const fn whole_days(d: Duration) -> u64 {
    d.as_secs() / DAY_SECS
}

/// A room just loaded: what its sessions share, and its task.
type Ready = (crate::rooms::Room, JoinHandle<Stopped>);

/// The end of a load run apart from the host ([`Host::load`]).
type LoadEnd = (RoomKey, Result<Ready, String>);

/// Where a load (or an import) reads and writes the room.
enum LoadFrom {
    Pg(Arc<PgLogStore>),
    Mem(Arc<MemLogStore>),
}

/// Put a room's package files into the store (write once, by key): the store is where a room
/// is loaded from, never a file. Its seed goes in with the room ([`import_work`]).
async fn store_packages<S: BlobStore>(s: &S, l: &Loaded) -> Result<(), String> {
    for f in &l.packages {
        s.put(&log::blob_key(f), f).await.map_err(|e| format!("store a package: {e:?}"))?;
    }
    Ok(())
}

/// What an import stored: the room's key and idle time when it is to be listed.
type Stored = Result<Option<(RoomKey, u64)>, Skip>;

/// The store side of an import, which needs nothing from the host so it runs in its own
/// task: the packages, then the seed. On Postgres a listed room's seed is written with the
/// room (`open_room`, after its hash check); a seed that is not listed (generated, not
/// hosted) is refused for a stored room, and on either store a different seed is refused.
async fn import_work(from: LoadFrom, loaded: Loaded, list: bool, limits: Limits) -> Stored {
    let id = crate::room_id(loaded.bundle.seed_name());
    let seed = |e| Skip::Final(format!("seed {:?}: {e:?}", loaded.bundle.seed_name()));
    match from {
        LoadFrom::Pg(store) => {
            store_packages(&*store, &loaded).await.map_err(Skip::Later)?;
            if list {
                return open_with_key(&store, id, &loaded, limits, RoomKey::random).await.map(Some);
            }
            store.seed_put(id, loaded.bundle.bytes()).await.map_err(seed)?;
        }
        LoadFrom::Mem(log) => {
            store_packages(&*log, &loaded).await.map_err(Skip::Later)?;
            log.seed_put(id, loaded.bundle.bytes()).await.map_err(seed)?;
            if list {
                return Ok(Some((RoomKey::random().map_err(Skip::Later)?, 0)));
            }
        }
    }
    Ok(None)
}

/// A room's stored seed and the package files this process lacks, each checked against its
/// key ([`BlobStore::get`]), with this process's copies of the others held so they stay.
type Fetched = (Vec<u8>, BTreeMap<BlobKey, Vec<u8>>, Vec<Arc<GamePackage>>);

async fn fetch_seed<S: LogStore + BlobStore>(s: &S, seed: Vec<u8>) -> Result<Fetched, String> {
    let keys = seed_bundle::v2::package_keys(&seed).map_err(|e| e.to_string())?;
    let (mut files, mut held) = (BTreeMap::new(), Vec::new());
    for key in keys {
        if let Some(p) = seed_bundle::package::cached(&key) {
            held.push(p);
            continue;
        }
        let file = s.get(&key).await.map_err(|e| format!("a package: {e:?}"))?;
        files.insert(key, file.ok_or("a package the seed names is not in the store")?);
    }
    Ok((seed, files, held))
}

/// Room `room`'s stored seed as v1 JSON text, every package read from the store (checked). A
/// seed of another bundle than `hash` (hex, the world's) is refused.
async fn export<S: LogStore + BlobStore>(
    s: &S,
    room: RoomId,
    hash: &str,
    limits: &Limits,
) -> Result<String, String> {
    let seed = s.seed_get(room).await.map_err(|e| format!("{e:?}"))?.ok_or("no stored seed")?;
    let made_from = seed_bundle::v2::bundle_hash_of(&seed).map_err(|e| e.to_string())?;
    if crate::hex(&made_from) != hash {
        return Err("the stored seed is of another bundle with this seed name".to_owned());
    }
    let mut files = BTreeMap::new();
    for key in seed_bundle::v2::package_keys(&seed).map_err(|e| e.to_string())? {
        let file = s.get(&key).await.map_err(|e| format!("a package: {e:?}"))?;
        files.insert(key, file.ok_or("a package the seed names is not in the store")?);
    }
    let max = limits.bundle_max_bytes;
    let decoded = tokio::task::spawn_blocking(move || {
        let fetch = |k: &BlobKey| {
            files.get(k).cloned().ok_or_else(|| seed_bundle::BundleError::Parse("unfetched".into()))
        };
        seed_bundle::v2::decode(seed, fetch, max).map_err(|e| e.to_string())
    });
    let v1 = decoded.await.map_err(|e| e.to_string())??;
    serde_json::to_string(&v1).map_err(|e| e.to_string())
}

/// Open a fetched seed (CPU work: run it off the runtime).
fn open_fetched((seed, files, _held): Fetched, limits: &Limits) -> Result<Loaded, String> {
    let fetch = |k: &BlobKey| {
        files.get(k).cloned().ok_or_else(|| seed_bundle::BundleError::Parse("unfetched".into()))
    };
    let seed = Seed::read(seed, fetch, limits.bundle_max_bytes).map_err(|e| e.to_string())?;
    Ok(Loaded { hash: seed.hash().to_vec(), bundle: Arc::new(seed), packages: Vec::new() })
}

/// A file's length and modification time: a scan rereads a file only when they change.
type Seen = (u64, Option<SystemTime>);

/// Why a bundle was not listed.
#[derive(Debug)]
pub(crate) enum Skip {
    /// Not until the file changes: an invalid bundle, a room made from another bundle, or a
    /// finished room.
    Final(String),
    /// Maybe on a later scan: the database failed, or the room cap is reached.
    Later(String),
}

impl From<Skip> for String {
    fn from(skip: Skip) -> Self {
        match skip {
            Skip::Final(why) | Skip::Later(why) => why,
        }
    }
}

/// A request to load a cold room, answered once it is in memory.
struct Wake {
    key: RoomKey,
    done: oneshot::Sender<Result<(), String>>,
}

/// A request to put a seed (a generated world) into the store, and with `list` to list it as
/// a room; answered with its room key when listed.
struct Import {
    loaded: Loaded,
    list: bool,
    done: oneshot::Sender<Result<Option<RoomKey>, String>>,
}

/// A request for a stored seed decoded back to its v1 JSON (the debugging view).
struct Export {
    room: RoomId,
    /// The world's bundle hash (hex): the seed must be of it.
    hash: String,
    done: oneshot::Sender<Result<String, String>>,
}

/// An import whose store side ended: list the room, then answer.
type ImportEnd = (Loaded, Stored, oneshot::Sender<Result<Option<RoomKey>, String>>);

/// What the host's channel carries.
enum Request {
    Wake(Wake),
    Import(Import),
    Export(Export),
}

/// Asks the [`Host`] to load a cold room (the router's side).
#[derive(Debug, Clone)]
pub struct Waker(mpsc::UnboundedSender<Request>);

impl Waker {
    /// Load the cold room `key` and wait until it is in memory.
    ///
    /// # Errors
    /// Why the room could not be loaded.
    pub(crate) async fn wake(&self, key: RoomKey) -> Result<(), String> {
        let (done, answer) = oneshot::channel();
        let stopping = || "the server is stopping".to_owned();
        self.0.send(Request::Wake(Wake { key, done })).map_err(|_| stopping())?;
        answer.await.map_err(|_| stopping())?
    }

    /// Put `loaded` (its seed and packages) into the host's store and, with `list`, list it as
    /// a room; its key when listed.
    ///
    /// # Errors
    /// Why it was not stored or listed.
    pub(crate) async fn import(
        &self,
        loaded: Loaded,
        list: bool,
    ) -> Result<Option<RoomKey>, String> {
        let (done, answer) = oneshot::channel();
        let stopping = || "the server is stopping".to_owned();
        self.0.send(Request::Import(Import { loaded, list, done })).map_err(|_| stopping())?;
        answer.await.map_err(|_| stopping())?
    }

    /// The stored seed of room `room`, decoded to its v1 JSON text with its stored packages;
    /// `hash` (hex) is the bundle it must be of.
    ///
    /// # Errors
    /// No such seed, one of another bundle, a package missing or not matching its key, or the
    /// server stopping.
    pub(crate) async fn export(&self, room: RoomId, hash: String) -> Result<String, String> {
        let (done, answer) = oneshot::channel();
        let stopping = || "the server is stopping".to_owned();
        self.0.send(Request::Export(Export { room, hash, done })).map_err(|_| stopping())?;
        answer.await.map_err(|_| stopping())?
    }
}

/// The rooms of a process and their tasks.
pub struct Host {
    rooms: Arc<RwLock<Rooms>>,
    store: Store,
    password: Option<String>,
    limits: Limits,
    /// The rooms folder (`--rooms`); `None` for `--bundle`.
    folder: Option<PathBuf>,
    tasks: JoinSet<Ended>,
    running: BTreeMap<RoomKey, Running>,
    runs: u64,
    /// For each listed room, the time of last use the database holds (`rooms.last_active`).
    recorded: BTreeMap<RoomKey, Moment>,
    /// Every bundle file as the last scan saw it, and whether that was final (listed, or
    /// [`Skip::Final`]); a file skipped with [`Skip::Later`] is tried again.
    seen: BTreeMap<PathBuf, (Seen, bool)>,
    // Unbounded, but every request holds a connection permit (`max_connections`).
    wakes: mpsc::UnboundedReceiver<Request>,
    waker: Waker,
    /// Loads running apart from the supervisor loop, at most one per room.
    loads: JoinSet<LoadEnd>,
    /// The wakes waiting on each running load.
    waiting: BTreeMap<RoomKey, Vec<oneshot::Sender<Result<(), String>>>>,
    /// The rooms that failed quickly, at most one entry per listed room.
    failing: BTreeMap<RoomKey, Failing>,
    /// Imports and exports from the generation API, apart from the loop like loads.
    imports: JoinSet<ImportEnd>,
    exports: JoinSet<()>,
}

impl core::fmt::Debug for Host {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Host")
            .field("folder", &self.folder)
            .field("resident", &self.running.len())
            .finish_non_exhaustive()
    }
}

impl Host {
    /// A host with no rooms yet, on Postgres when `database_url` is set.
    pub(crate) async fn new(
        database_url: Option<&str>,
        password: Option<String>,
        limits: Limits,
        folder: Option<PathBuf>,
    ) -> Result<Self, String> {
        let store = if let Some(url) = database_url {
            let pg = PgLogStore::connect(url, &limits)
                .await
                .map_err(|e| format!("postgres: {e:?} (see the log above)"))?;
            tracing::info!("log store: postgres");
            Store::Pg(Arc::new(pg))
        } else {
            tracing::warn!(
                "log store: IN MEMORY, no CONSTELLATION_DATABASE_URL; a restart loses the rooms \
                 and draws new room keys"
            );
            Store::Mem { log: Arc::new(MemLogStore::new()), finished: BTreeSet::new() }
        };
        let (tx, wakes) = mpsc::unbounded_channel();
        let host = Self {
            rooms: Arc::new(RwLock::new(Rooms::default())),
            store,
            password,
            limits,
            folder,
            tasks: JoinSet::new(),
            running: BTreeMap::new(),
            runs: 0,
            recorded: BTreeMap::new(),
            seen: BTreeMap::new(),
            wakes,
            waker: Waker(tx),
            loads: JoinSet::new(),
            waiting: BTreeMap::new(),
            failing: BTreeMap::new(),
            imports: JoinSet::new(),
            exports: JoinSet::new(),
        };
        Ok(host)
    }

    /// What the router asks to load a cold room with; answered while [`Host::supervise`]
    /// runs.
    pub(crate) fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// The room list the router reads.
    pub(crate) fn rooms(&self) -> Arc<RwLock<Rooms>> {
        Arc::clone(&self.rooms)
    }

    /// The rooms folder and a view of its rooms (`--rooms`); `None` for `--bundle`.
    pub(crate) fn folder(&self) -> Option<RoomsFolder> {
        self.folder.as_ref()?;
        Some(RoomsFolder { rooms: self.rooms(), wake: self.waker() })
    }

    /// How many rooms are listed.
    #[must_use]
    pub fn len(&self) -> usize {
        read(&self.rooms).len()
    }

    /// No room is listed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        read(&self.rooms).is_empty()
    }

    /// How many rooms are in memory.
    #[must_use]
    pub fn resident(&self) -> usize {
        self.running.len()
    }

    /// List every room stored in Postgres that has not finished, cold (nothing in memory).
    pub(crate) async fn list_stored(&mut self) -> Result<(), String> {
        let Store::Pg(store) = &self.store else { return Ok(()) };
        let stored = store.list_rooms().await.map_err(|e| format!("postgres: {e:?}"))?;
        for room in stored {
            if self.len() >= self.limits.max_rooms {
                tracing::warn!(max = self.limits.max_rooms, "stored rooms past the cap not listed");
                break;
            }
            // One bad row skips its room, never the others.
            let listed =
                RoomKey::parse(&room.key).map_err(|e| format!("stored {e}")).and_then(|key| {
                    let idle = Duration::from_secs(room.idle_secs);
                    self.list(key, room.room, room.seed_name, room.bundle_hash, idle)
                });
            if let Err(why) = listed {
                tracing::warn!(room = room.room.0, "stored room not listed: {why}");
            }
        }
        Ok(())
    }

    fn list(
        &mut self,
        key: RoomKey,
        id: RoomId,
        seed: String,
        hash: Vec<u8>,
        idle: Duration,
    ) -> Result<(), String> {
        tracing::info!(%key, seed, "room ready");
        let activity = Activity::new(idle);
        let used = activity.last_used(Moment::now());
        let listing = Listing { id, seed, hash, room: None, activity };
        write(&self.rooms).insert(key.clone(), listing)?;
        self.recorded.insert(key, used);
        Ok(())
    }

    /// Add a room to the directory (cold) from its bundle and return its key. A seed that is
    /// already listed returns its key unchanged.
    pub(crate) async fn import(&mut self, loaded: Loaded) -> Result<RoomKey, Skip> {
        let work = match self.admit(&loaded, true)? {
            Ok(work) => work,
            Err(key) => return Ok(key),
        };
        let stored = work.await;
        self.finish_import(&loaded, stored)?.ok_or_else(|| Skip::Later("not listed".to_owned()))
    }

    /// Check an import against the directory: `Err(key)` for a seed listed already (with
    /// `list`), else the store work to run ([`import_work`]). A seed that is not to be listed
    /// is refused when this host serves or has ended a room of that seed name.
    fn admit(
        &self,
        loaded: &Loaded,
        list: bool,
    ) -> Result<Result<impl Future<Output = Stored> + Send + 'static, RoomKey>, Skip> {
        let seed = loaded.bundle.seed_name();
        let id = crate::room_id(seed);
        let hosted = || Skip::Final(format!("seed {seed:?} is already hosted on this server"));
        {
            let rooms = read(&self.rooms);
            if let Some(key) = rooms.key_of(id) {
                if !list {
                    return Err(hosted());
                }
                // A listed room keeps the world it was listed with; another bundle is refused.
                if rooms.get(key).is_some_and(|l| l.hash != loaded.hash) {
                    return Err(Skip::Final(format!(
                        "room {seed:?} is listed from a different bundle with this seed name"
                    )));
                }
                return Ok(Err(key.clone()));
            }
        }
        if list && self.len() >= self.limits.max_rooms {
            let why = format!("the server lists {} rooms, its most", self.limits.max_rooms);
            return Err(Skip::Later(why));
        }
        let from = match &self.store {
            Store::Pg(store) => LoadFrom::Pg(Arc::clone(store)),
            Store::Mem { log, finished } => {
                if finished.contains(&id) {
                    let why = format!("room {seed:?} has finished; it is not served again");
                    return Err(Skip::Final(why));
                }
                LoadFrom::Mem(Arc::clone(log))
            }
        };
        Ok(Ok(import_work(from, loaded.clone(), list, self.limits)))
    }

    /// List what an import stored (when it was to be listed). A seed another import listed
    /// meanwhile keeps that listing.
    fn finish_import(&mut self, loaded: &Loaded, stored: Stored) -> Result<Option<RoomKey>, Skip> {
        let Some((key, idle)) = stored? else { return Ok(None) };
        let seed = loaded.bundle.seed_name().to_owned();
        let id = crate::room_id(&seed);
        if let Some(listed) = read(&self.rooms).key_of(id) {
            return Ok(Some(listed.clone()));
        }
        let hash = loaded.hash.clone();
        self.list(key.clone(), id, seed, hash, Duration::from_secs(idle)).map_err(Skip::Later)?;
        Ok(Some(key))
    }

    /// An import from the generation API: store the seed in a task of its own, and list it
    /// when asked once that ends ([`Host::imported`]).
    fn import_request(&mut self, i: Import) {
        match self.admit(&i.loaded, i.list) {
            Ok(Ok(work)) => {
                self.imports.spawn(async move {
                    let stored = tokio::spawn(work).await.unwrap_or_else(|e| {
                        Err(Skip::Later(format!("the import ended without a value: {e}")))
                    });
                    (i.loaded, stored, i.done)
                });
            }
            Ok(Err(key)) => drop(i.done.send(Ok(Some(key)))),
            Err(why) => drop(i.done.send(Err(why.into()))),
        }
    }

    /// An import's store side ended: list it, and answer.
    fn imported(&mut self, (loaded, stored, done): ImportEnd) {
        drop(done.send(self.finish_import(&loaded, stored).map_err(String::from)));
    }

    /// An export from the generation API, in a task of its own: it reads the store and
    /// decodes, and needs nothing from the host.
    fn export_request(&mut self, x: Export) {
        let from = match &self.store {
            Store::Pg(s) => LoadFrom::Pg(Arc::clone(s)),
            Store::Mem { log, .. } => LoadFrom::Mem(Arc::clone(log)),
        };
        let limits = self.limits;
        self.exports.spawn(async move {
            let res = match from {
                LoadFrom::Pg(s) => export(&*s, x.room, &x.hash, &limits).await,
                LoadFrom::Mem(s) => export(&*s, x.room, &x.hash, &limits).await,
            };
            drop(x.done.send(res));
        });
    }

    /// Load the cold room `key` into memory now: claim it, rebuild it from the log and start
    /// its task. A room already in memory is left as it is. The supervisor runs the same load
    /// in its own task instead ([`Host::supervise`]).
    pub(crate) async fn wake(&mut self, key: &RoomKey) -> Result<(), String> {
        match self.load(key)? {
            Some(load) => {
                self.backing_off(key)?;
                self.make_space()?;
                let loaded = load.await;
                self.settle(key, loaded)
            }
            None => Ok(()),
        }
    }

    /// The load of the cold room `key`, which needs nothing from the host so it can run in
    /// its own task; `None` when the room is in memory already.
    fn load(
        &self,
        key: &RoomKey,
    ) -> Result<Option<impl Future<Output = Result<Ready, String>> + Send + 'static>, String> {
        let (id, seed, hash) = {
            let rooms = read(&self.rooms);
            let listing = rooms.get(key).ok_or_else(|| format!("no room with key {key}"))?;
            if listing.room.is_some() {
                return Ok(None);
            }
            (listing.id, listing.seed.clone(), listing.hash.clone())
        };
        let from = match &self.store {
            Store::Pg(store) => LoadFrom::Pg(Arc::clone(store)),
            Store::Mem { log, .. } => LoadFrom::Mem(Arc::clone(log)),
        };
        let (password, limits, key) = (self.password.clone(), self.limits, key.clone());
        Ok(Some(async move {
            let missing = || format!("room {seed:?} is stored without its seed");
            // The seed from room_seeds; a room stored before seeds has its v1 file instead.
            let stored = match &from {
                LoadFrom::Pg(store) => match store.seed_get(id).await {
                    Ok(Some(s)) => Ok(fetch_seed(&**store, s).await?),
                    Ok(None) => Err(store
                        .room_bundle(id)
                        .await
                        .map_err(|e| format!("postgres: {e:?}"))?
                        .ok_or_else(missing)?),
                    Err(e) => return Err(format!("postgres: {e:?}")),
                },
                LoadFrom::Mem(log) => {
                    let s = log.seed_get(id).await.map_err(|e| format!("{e:?}"))?;
                    Ok(fetch_seed(&**log, s.ok_or_else(missing)?).await?)
                }
            };
            // Opening a seed (or importing a v1 file) is CPU work: off the runtime.
            let p = tokio::task::spawn_blocking(move || {
                let loaded = match stored {
                    Ok(fetched) => open_fetched(fetched, &limits),
                    Err(file) => parse_bundle(&file, &limits),
                };
                let loaded = loaded.map_err(|e| format!("room {seed:?}: stored seed: {e}"))?;
                // The room plays the world it was listed with, never another bundle's seed.
                if loaded.hash != hash {
                    return Err(format!("room {seed:?}: its stored seed is of another bundle"));
                }
                prepare(loaded, password.as_ref(), limits)
            })
            .await
            .map_err(|e| format!("room {key}: the bundle check failed: {e}"))??;
            let room = p.room.clone();
            let handle = match from {
                LoadFrom::Pg(store) => {
                    let epoch = store.claim(id).await.map_err(|e| format!("postgres: {e:?}"))?;
                    start(p, key, epoch, store, &limits).await?
                }
                LoadFrom::Mem(log) => {
                    let epoch = log.claim(id);
                    start(p, key, epoch, log, &limits).await?
                }
            };
            Ok((room, handle))
        }))
    }

    /// Put a finished load in place: the room is in memory and its task watched.
    fn install(&mut self, key: &RoomKey, loaded: Result<Ready, String>) -> Result<(), String> {
        let (room, handle) = loaded?;
        let task = handle.abort_handle();
        match write(&self.rooms).get_mut(key) {
            Some(listing) => {
                // The connection that asked for the wake has not entered yet; without this the
                // next idle check could unload the room before it does.
                listing.activity.touch();
                listing.room = Some(room);
            }
            None => {
                task.abort();
                return Err(format!("room {key} was removed while it loaded"));
            }
        }
        self.runs += 1;
        let run = self.runs;
        let ended = key.clone();
        self.tasks.spawn(async move { (ended, run, handle.await) });
        self.running.insert(key.clone(), Running { run, task, since: Instant::now() });
        Ok(())
    }

    /// Put a finished load in place ([`Host::install`]); a failed load counts toward its
    /// room's wait before the next.
    fn settle(&mut self, key: &RoomKey, loaded: Result<Ready, String>) -> Result<(), String> {
        if let Err(why) = &loaded {
            let (failures, wait) = self.failed(key);
            tracing::warn!(
                %key,
                failures,
                retry_in_ms = wait.as_millis(),
                "room not loaded: {why}"
            );
            return Err(why.clone());
        }
        let woke = self.install(key, loaded);
        if let Err(why) = &woke {
            tracing::warn!(%key, "room not loaded: {why}");
        }
        woke
    }

    /// Count a quick failure of room `key`: its next load waits ([`retry_wait`]). How many
    /// in a row, and the wait.
    fn failed(&mut self, key: &RoomKey) -> (u32, Duration) {
        let now = Instant::now();
        let f = self.failing.entry(key.clone()).or_insert(Failing { count: 0, until: now });
        f.count = f.count.saturating_add(1);
        let wait = retry_wait(&self.limits, f.count);
        f.until = now + wait;
        (f.count, wait)
    }

    /// Refuse a load of room `key` while it waits after quick failures.
    ///
    /// # Errors
    /// While the wait runs.
    fn backing_off(&self, key: &RoomKey) -> Result<(), String> {
        let Some(f) = self.failing.get(key) else { return Ok(()) };
        let left = f.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        Err(format!(
            "room {key}: {} quick failures in a row; it may load again in {} ms",
            f.count,
            left.as_millis()
        ))
    }

    /// Forget the failures of every room in memory for `room_healthy_secs`.
    fn forget_healthy(&mut self) {
        let healthy = Duration::from_secs(self.limits.room_healthy_secs);
        let running = &self.running;
        self.failing
            .retain(|key, _| !running.get(key).is_some_and(|r| r.since.elapsed() >= healthy));
    }

    /// Make space under `max_resident_rooms` for one more room, before its load starts. Loads
    /// under way count, as each ends resident. At the cap, the resident room unused for the
    /// longest leaves memory, as [`Host::unload_idle`] would unload it later; a room with a
    /// connection open never does, nor a room still loading, so when none can leave this refuses. The one room of
    /// `--bundle` needs no space.
    ///
    /// # Errors
    /// When no room can leave memory now.
    fn make_space(&mut self) -> Result<(), String> {
        if self.folder.is_none() {
            return Ok(());
        }
        let cap = self.limits.max_resident_rooms.max(1);
        // The write lock keeps connections out: none can enter a room while it is chosen.
        let mut rooms = write(&self.rooms);
        while self.running.len() + self.waiting.len() >= cap {
            let quietest = self
                .running
                .keys()
                .filter_map(|key| Some((key, rooms.get(key)?.activity.idle_for()?)))
                .max_by_key(|(_, idle)| *idle)
                .map(|(key, idle)| (key.clone(), idle));
            let Some((key, idle)) = quietest else {
                return Err(format!(
                    "the server holds {cap} rooms in memory, its most, and none can leave \
                     memory now (each has a connection open or is still loading)"
                ));
            };
            if let Some(listing) = rooms.get_mut(&key) {
                listing.room = None;
            }
            self.running.remove(&key);
            tracing::info!(
                %key,
                idle_secs = idle.as_secs(),
                "room unused the longest; left memory to make space"
            );
        }
        Ok(())
    }

    /// A wake request from the router: start the room's load, or join the one running.
    fn request(&mut self, key: RoomKey, done: oneshot::Sender<Result<(), String>>) {
        if let Some(waiting) = self.waiting.get_mut(&key) {
            waiting.push(done);
            return;
        }
        match self.load(&key) {
            Ok(Some(load)) => {
                if let Err(why) = self.backing_off(&key) {
                    // Each connect attempt asks; the failure itself was logged once.
                    tracing::debug!(%key, "room not loaded: {why}");
                    drop(done.send(Err(why)));
                    return;
                }
                if let Err(why) = self.make_space() {
                    tracing::warn!(%key, "room not loaded: {why}");
                    drop(done.send(Err(why)));
                    return;
                }
                self.waiting.insert(key.clone(), vec![done]);
                self.loads.spawn(async move {
                    // Its own task, so a panic in the load still answers the waiting wakes.
                    let loaded = tokio::spawn(load)
                        .await
                        .unwrap_or_else(|e| Err(format!("the load ended without a value: {e}")));
                    (key, loaded)
                });
            }
            Ok(None) => drop(done.send(Ok(()))),
            Err(why) => {
                tracing::warn!(%key, "room not loaded: {why}");
                drop(done.send(Err(why)));
            }
        }
    }

    /// A load started by [`Host::request`] ended: put it in place and answer its wakes.
    fn loaded(&mut self, (key, loaded): LoadEnd) {
        let woke = self.settle(&key, loaded);
        for done in self.waiting.remove(&key).unwrap_or_default() {
            drop(done.send(woke.clone()));
        }
    }

    /// Run until a room's end must end the process: load rooms on request (each load in its
    /// own task, so a slow room never holds up the others), and on every tick
    /// unload idle rooms, end abandoned ones and rescan the rooms folder.
    pub(crate) async fn supervise(&mut self) -> RunError {
        let every = Duration::from_millis(self.limits.rooms_scan_ms.max(1));
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                Some(end) = self.tasks.join_next() => match end {
                    Ok(end) => {
                        if let Some(fatal) = self.ended(end) {
                            return fatal;
                        }
                    }
                    // A watcher only awaits its room's handle and is never aborted (only the
                    // room task inside it is), so this cannot happen.
                    Err(e) => tracing::error!(error = %e, "a room watcher ended without a value"),
                },
                Some(r) = self.wakes.recv() => match r {
                    Request::Wake(Wake { key, done }) => self.request(key, done),
                    Request::Import(i) => self.import_request(i),
                    Request::Export(x) => self.export_request(x),
                },
                Some(end) = self.imports.join_next() => match end {
                    Ok(end) => self.imported(end),
                    // The work runs in a task of its own inside this one, so this cannot happen.
                    Err(e) => tracing::error!(error = %e, "an import ended without a value"),
                },
                // An export answers its request itself.
                Some(_) = self.exports.join_next() => {}
                Some(end) = self.loads.join_next() => match end {
                    Ok(end) => self.loaded(end),
                    // The load runs in a task of its own inside this one, so this cannot happen.
                    Err(e) => tracing::error!(error = %e, "a room load ended without a value"),
                },
                _ = tick.tick() => self.tick().await,
            }
        }
    }

    /// One supervisor tick: unload idle rooms, record when rooms were last used, end
    /// abandoned ones and rescan the rooms folder.
    async fn tick(&mut self) {
        self.forget_healthy();
        self.unload_idle();
        self.record_use(false).await;
        self.end_abandoned().await;
        self.scan().await;
    }

    /// Before a clean stop: record when every room was last used.
    pub(crate) async fn stop(&mut self) {
        self.record_use(true).await;
    }

    /// Write when rooms were last used to the database (`last_active`, what D-30 and a
    /// restart count from), in one statement: a cold room (it just left memory) as soon as it
    /// was used since the last write, a room in memory at most every `room_touch_secs`, and
    /// with `all` (a clean stop) every room used since. A failed write is tried again on the
    /// next tick.
    async fn record_use(&mut self, all: bool) {
        let Store::Pg(store) = &self.store else { return };
        let every = Duration::from_secs(self.limits.room_touch_secs);
        let now = Moment::now();
        let due: Vec<(RoomKey, RoomId, Moment)> = read(&self.rooms)
            .iter()
            .filter_map(|(key, l)| {
                let used = l.activity.last_used(now);
                let since = used.since(*self.recorded.get(key)?);
                let due = since > Duration::ZERO && (all || l.room.is_none() || since >= every);
                due.then(|| (key.clone(), l.id, used))
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let rows: Vec<(RoomId, u64)> =
            due.iter().map(|(_, id, used)| (*id, now.since(*used).as_secs())).collect();
        match store.touch(&rows).await {
            Ok(()) => self.recorded.extend(due.into_iter().map(|(key, _, used)| (key, used))),
            Err(e) => tracing::warn!(
                rooms = rows.len(),
                error = ?e,
                "could not record when rooms were last used; trying again next tick"
            ),
        }
    }

    /// Handle a room task's end; `Some` when it must end the process.
    fn ended(&mut self, (key, run, why): Ended) -> Option<RunError> {
        if !self.running.get(&key).is_some_and(|r| r.run == run) {
            // A room this host unloaded or ended on purpose.
            return None;
        }
        if self.folder.is_none() || matches!(why, Ok(Stopped::Fenced { .. })) {
            return Some(room_ended(why));
        }
        let ran = self.running.remove(&key).map(|r| r.since.elapsed());
        if let Some(listing) = write(&self.rooms).get_mut(&key) {
            listing.room = None;
        }
        if ran.is_some_and(|ran| ran >= Duration::from_secs(self.limits.room_healthy_secs)) {
            self.failing.remove(&key);
            tracing::error!(%key, ?why, "room stopped; its next connect loads it again from its log");
        } else {
            let (failures, wait) = self.failed(&key);
            tracing::error!(
                %key,
                ?why,
                failures,
                retry_in_ms = wait.as_millis(),
                "room stopped soon after it loaded; its next load waits"
            );
        }
        None
    }

    /// Unload every room in memory with no connection for `room_idle_secs`. Dropping the room
    /// ends its task once it has handled every message already sent (no reply is lost), and
    /// its log is durable up to its last reply. Writing a snapshot first is #47. The one room
    /// of `--bundle` stays in memory: its end is the process's end.
    fn unload_idle(&mut self) {
        if self.folder.is_none() {
            return;
        }
        let limit = Duration::from_secs(self.limits.room_idle_secs);
        let mut gone = Vec::new();
        {
            // The write lock keeps connections out: none can enter a room while it is checked.
            let mut rooms = write(&self.rooms);
            for key in self.running.keys() {
                let Some(listing) = rooms.get_mut(key) else { continue };
                if let Some(idle) = listing.activity.idle_for()
                    && idle >= limit
                {
                    listing.room = None;
                    gone.push((key.clone(), idle));
                }
            }
        }
        for (key, idle) in gone {
            self.running.remove(&key);
            tracing::info!(%key, idle_secs = idle.as_secs(), "room idle; left memory");
        }
    }

    /// End every room from the directory with no connection for `room_abandon_secs` (D-30).
    async fn end_abandoned(&mut self) {
        if self.folder.is_none() {
            return;
        }
        let limit = Duration::from_secs(self.limits.room_abandon_secs);
        let old: Vec<(RoomKey, RoomId)> = read(&self.rooms)
            .iter()
            .filter(|(_, l)| l.activity.idle_for().is_some_and(|idle| idle >= limit))
            .map(|(k, l)| (k.clone(), l.id))
            .collect();
        for (key, id) in old {
            let finished = match &mut self.store {
                Store::Pg(store) => {
                    store.finish(id, ABANDONED, limit.as_secs()).await.map_err(|e| format!("{e:?}"))
                }
                Store::Mem { log, finished } => {
                    // Its seed is never read again; its packages stay (other rooms may share).
                    log.forget_seed(id);
                    finished.insert(id);
                    Ok(true)
                }
            };
            match finished {
                Ok(true) => {}
                Ok(false) => {
                    // The database saw the room used more recently than this clock did; trust
                    // it and start the clock again rather than ask on every tick.
                    if let Some(l) = read(&self.rooms).get(&key) {
                        l.activity.touch();
                        self.recorded.insert(key.clone(), l.activity.last_used(Moment::now()));
                    }
                    tracing::warn!(%key, "room not abandoned: the database says it was used since");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(%key, "room not ended yet, the database failed: {e}");
                    continue;
                }
            }
            write(&self.rooms).remove(&key);
            self.recorded.remove(&key);
            self.failing.remove(&key);
            if let Some(r) = self.running.remove(&key) {
                r.task.abort();
            }
            tracing::info!(%key, days = whole_days(limit), "room abandoned; it has ended (D-30)");
        }
    }

    /// List a room for each bundle in the folder that is new or changed since the last scan.
    /// A file that does not make a room is skipped with a warning: until it changes for a
    /// [`Skip::Final`] reason, and tried again quietly on every scan for a [`Skip::Later`] one
    /// (while the server is at its room cap, only once it is not).
    pub(crate) async fn scan(&mut self) {
        let Some(folder) = self.folder.clone() else { return };
        let files = tokio::task::spawn_blocking(move || bundle_files(&folder)).await;
        let files = match files.map_err(|e| e.to_string()).and_then(|f| f) {
            Ok(files) => files,
            Err(e) => {
                tracing::warn!("rooms folder: {e}");
                return;
            }
        };
        self.seen.retain(|path, _| files.iter().any(|(p, _)| p == path));
        for (path, seen) in files {
            let retry = match self.seen.get(&path) {
                Some(&(was, settled)) if was == seen => {
                    if settled || self.len() >= self.limits.max_rooms {
                        continue;
                    }
                    true
                }
                _ => false,
            };
            let (file, limits) = (path.clone(), self.limits);
            let read = tokio::task::spawn_blocking(move || load_bundle(&file, &limits)).await;
            let listed = match read.map_err(|e| e.to_string()).and_then(|r| r) {
                Ok(loaded) => self.import(loaded).await.map(drop),
                Err(e) => Err(Skip::Final(e)),
            };
            self.seen.insert(path.clone(), (seen, !matches!(listed, Err(Skip::Later(_)))));
            match listed {
                Ok(()) => {}
                Err(Skip::Later(why)) if retry => {
                    tracing::debug!(file = %path.display(), "still skipped: {why}");
                }
                Err(Skip::Later(why)) => {
                    tracing::warn!(file = %path.display(), "skipped for now, tried again: {why}");
                }
                Err(Skip::Final(why)) => tracing::warn!(file = %path.display(), "skipped: {why}"),
            }
        }
    }
}

/// The rooms of a `--rooms` host as the generation API sees them: which key a seed's room got,
/// and how to reach that room.
#[derive(Debug, Clone)]
pub(crate) struct RoomsFolder {
    rooms: Arc<RwLock<Rooms>>,
    wake: Waker,
}

impl RoomsFolder {
    /// The key of the listed room made from seed `seed`, if any.
    pub(crate) fn key_of(&self, seed: &str) -> Option<RoomKey> {
        read(&self.rooms).key_of(crate::room_id(seed)).cloned()
    }

    /// The room `key`, woken first when it is cold, with a visit that keeps it in memory.
    pub(crate) async fn enter(
        &self,
        key: RoomKey,
    ) -> Result<(Arc<edge_compat::CompatRoom>, crate::rooms::Visit), crate::NoRoom> {
        crate::enter(&self.rooms, &self.wake, key).await
    }
}

/// Every v1 bundle file (`*.json`) in `folder`, in name order, with what a scan compares. The
/// folder is an import path: a listed room loads from the store.
fn bundle_files(folder: &Path) -> Result<Vec<(PathBuf, Seen)>, String> {
    let entries = std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", folder.display()))?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_file() {
            files.push((path, (meta.len(), meta.modified().ok())));
        }
    }
    files.sort();
    Ok(files)
}

/// Check that `folder` is a readable folder (it may be empty: rooms can be added later).
pub(crate) fn check_folder(folder: &Path) -> Result<(), String> {
    bundle_files(folder).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Source, app};

    /// A fresh folder under the system temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("constellation-host-{name}-{}", std::process::id()));
            drop(std::fs::remove_dir_all(&dir));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// Write the test fixture under `file` with its seed name replaced by `seed`.
        fn bundle(&self, file: &str, seed: &str) {
            let mut v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
            v["seed_name"] = serde_json::json!(seed);
            std::fs::write(self.0.join(file), v.to_string()).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json")
    }

    async fn host(dir: &TempDir, limits: Limits) -> Host {
        app(&Source::Rooms(dir.0.clone()), None, None, limits).await.unwrap().1
    }

    fn seeds(host: &Host) -> Vec<String> {
        let mut seeds: Vec<String> =
            read(&host.rooms).iter().map(|(_, l)| l.seed.clone()).collect();
        seeds.sort();
        seeds
    }

    fn key_of(host: &Host, seed: &str) -> RoomKey {
        read(&host.rooms).iter().find(|(_, l)| l.seed == seed).unwrap().0.clone()
    }

    fn in_memory(host: &Host, key: &RoomKey) -> bool {
        read(&host.rooms).get(key).is_some_and(|l| l.room.is_some())
    }

    fn visit(host: &Host, key: &RoomKey) -> crate::rooms::Visit {
        read(&host.rooms).get(key).unwrap().activity.enter()
    }

    #[tokio::test]
    async fn a_seeds_room_key_is_found_while_it_is_listed() {
        let dir = TempDir::new("key-of");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let host = host(&dir, Limits::DESIGN).await;
        let folder = host.folder().unwrap();
        assert_eq!(folder.key_of("alpha"), Some(key_of(&host, "alpha")));
        assert_eq!(folder.key_of("beta"), Some(key_of(&host, "beta")));
        assert_eq!(folder.key_of("gamma"), None, "no such seed");
    }

    #[tokio::test]
    async fn a_single_bundle_host_has_no_rooms_folder() {
        let (_, host) = app(&Source::Bundle(fixture()), None, None, Limits::DESIGN).await.unwrap();
        assert!(host.folder().is_none());
    }

    #[tokio::test]
    async fn every_json_bundle_in_the_folder_is_listed_cold() {
        let dir = TempDir::new("start");
        dir.bundle("b.json", "beta");
        dir.bundle("a.json", "alpha");
        std::fs::write(dir.0.join("notes.txt"), "not a bundle").unwrap();
        let host = host(&dir, Limits::DESIGN).await;
        assert_eq!(seeds(&host), ["alpha", "beta"]);
        assert_eq!(host.resident(), 0, "nothing is loaded before its first connect");
    }

    #[tokio::test]
    async fn an_empty_folder_starts_with_no_rooms_and_a_missing_one_is_refused() {
        let dir = TempDir::new("empty");
        assert!(host(&dir, Limits::DESIGN).await.is_empty());
        let missing = Source::Rooms(dir.0.join("missing"));
        assert!(app(&missing, None, None, Limits::DESIGN).await.is_err());
    }

    #[tokio::test]
    async fn a_bad_bundle_a_repeated_seed_or_one_past_the_cap_is_skipped_not_fatal() {
        let dir = TempDir::new("skip");
        dir.bundle("a.json", "fine");
        dir.bundle("b.json", "fine");
        std::fs::write(dir.0.join("broken.json"), "{not json").unwrap();
        dir.bundle("c.json", "third");
        let host = host(&dir, Limits { max_rooms: 1, ..Limits::DESIGN }).await;
        assert_eq!(seeds(&host), ["fine"], "b.json repeats a.json, c.json is past the cap");
    }

    #[tokio::test]
    async fn a_scan_lists_bundles_added_while_the_server_runs() {
        let dir = TempDir::new("add");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let alpha = key_of(&host, "alpha");
        dir.bundle("b.json", "beta");
        host.scan().await;
        assert_eq!(seeds(&host), ["alpha", "beta"]);
        assert_eq!(key_of(&host, "alpha"), alpha, "a listed room keeps its key");
        host.scan().await;
        assert_eq!(host.len(), 2);
        assert!(!host.is_empty());
    }

    #[test]
    fn an_abandon_time_is_logged_in_whole_days() {
        assert_eq!(whole_days(Duration::from_secs(30 * 86_400 + 86_399)), 30);
        assert_eq!(whole_days(Duration::from_secs(86_399)), 0);
        assert_eq!(whole_days(Duration::from_secs(86_400)), 1);
    }

    #[tokio::test]
    async fn a_host_shows_its_folder_and_how_many_rooms_are_in_memory() {
        let dir = TempDir::new("debug");
        let host = host(&dir, Limits::DESIGN).await;
        let shown = format!("{host:?}");
        assert!(shown.starts_with("Host {"), "{shown}");
        assert!(
            shown.contains(&format!("{:?}", dir.0)) && shown.contains("resident: 0"),
            "{shown}"
        );
    }

    /// The supervisor's first tick (scan, unload, abandon) comes one period after it starts,
    /// not at once: startup has just scanned. The clock is tokio's paused one, so nothing
    /// waits for real time (docs/TESTING.md rule 5).
    #[tokio::test(start_paused = true)]
    async fn the_first_supervisor_tick_comes_one_scan_period_after_the_start() {
        let dir = TempDir::new("first-tick");
        let mut host = host(&dir, Limits { rooms_scan_ms: 60_000, ..Limits::DESIGN }).await;
        dir.bundle("a.json", "alpha");
        let early = tokio::time::timeout(Duration::from_secs(59), host.supervise()).await;
        assert!(early.is_err() && host.is_empty(), "no scan before the first period");
        // A new call starts a new period: its first tick is 60 s after this call.
        let late = tokio::time::timeout(Duration::from_secs(61), host.supervise()).await;
        assert!(late.is_err());
        assert_eq!(seeds(&host), ["alpha"], "the tick one period in scanned");
    }

    #[tokio::test]
    async fn a_scan_forgets_files_that_left_the_folder() {
        let dir = TempDir::new("forget");
        for f in ["a.json", "b.json"] {
            std::fs::write(dir.0.join(f), "{not json").unwrap();
        }
        let mut host = host(&dir, Limits::DESIGN).await;
        std::fs::remove_file(dir.0.join("b.json")).unwrap();
        host.scan().await;
        let seen: Vec<&PathBuf> = host.seen.keys().collect();
        assert_eq!(seen, [&dir.0.join("a.json")]);
    }

    /// A file a scan already skipped for good is not read again while its size and time stay
    /// the same, even when its bytes changed (what a scan compares is the size and the time).
    #[tokio::test]
    async fn a_skipped_file_with_the_same_size_and_time_is_not_read_again() {
        let dir = TempDir::new("unchanged");
        dir.bundle("a.json", "alpha");
        let (path, good) = (dir.0.join("a.json"), std::fs::read(dir.0.join("a.json")).unwrap());
        let broken = [b"x", good.get(1..).unwrap()].concat();
        std::fs::write(&path, &broken).unwrap();
        let mut host = host(&dir, Limits::DESIGN).await;
        assert!(host.is_empty());
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, &good).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(time).unwrap();
        host.scan().await;
        assert!(host.is_empty(), "the same size and time: not read again");
    }

    #[tokio::test]
    async fn the_room_list_is_running_until_a_room_in_memory_stops() {
        let dir = TempDir::new("all-running");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = key_of(&host, "alpha");
        assert!(read(&host.rooms).all_running(), "a cold room does not count");
        host.wake(&key).await.unwrap();
        assert!(read(&host.rooms).all_running());
        host.running[&key].task.abort();
        // The watcher returns once the room task is gone, mailbox and all.
        let (_, _, why) = host.tasks.join_next().await.unwrap().unwrap();
        assert!(why.is_err_and(|e| e.is_cancelled()), "aborted");
        assert!(!read(&host.rooms).all_running());
    }

    /// Review of #55, M1: a bundle skipped for a reason that goes away on its own (the room
    /// cap here, a database failure in production) is tried again on the next scan.
    #[tokio::test]
    async fn a_bundle_skipped_at_the_cap_is_listed_once_a_room_ends() {
        let dir = TempDir::new("cap-retry");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits { max_rooms: 1, ..Limits::DESIGN }).await;
        assert_eq!(seeds(&host), ["alpha"]);
        host.scan().await;
        assert_eq!(seeds(&host), ["alpha"], "still at the cap");
        host.limits.room_abandon_secs = 0;
        host.end_abandoned().await;
        assert!(host.is_empty());
        host.scan().await;
        assert_eq!(seeds(&host), ["beta"]);
    }

    #[tokio::test]
    async fn a_broken_file_is_tried_again_once_it_changes() {
        let dir = TempDir::new("retry");
        std::fs::write(dir.0.join("a.json"), "{half written").unwrap();
        let mut host = host(&dir, Limits::DESIGN).await;
        assert!(host.is_empty());
        dir.bundle("a.json", "alpha");
        host.scan().await;
        assert_eq!(seeds(&host), ["alpha"]);
    }

    #[tokio::test]
    async fn a_wake_loads_the_room_once() {
        let dir = TempDir::new("wake");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = key_of(&host, "alpha");
        host.wake(&key).await.unwrap();
        assert!(in_memory(&host, &key));
        host.wake(&key).await.unwrap();
        assert_eq!(host.resident(), 1);
        let unknown = RoomKey::parse("zzzzzzzzzz").unwrap();
        assert!(host.wake(&unknown).await.is_err());
    }

    #[tokio::test]
    async fn an_idle_room_leaves_memory_and_loads_again_under_its_key() {
        let dir = TempDir::new("idle");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits { room_idle_secs: 0, ..Limits::DESIGN }).await;
        let key = key_of(&host, "alpha");
        host.wake(&key).await.unwrap();
        host.unload_idle();
        assert!(!in_memory(&host, &key) && host.resident() == 0);
        assert_eq!(seeds(&host), ["alpha"], "still listed");
        host.wake(&key).await.unwrap();
        assert!(in_memory(&host, &key));
    }

    #[tokio::test]
    async fn a_room_just_woken_waits_a_full_idle_time_for_its_connection() {
        let dir = TempDir::new("woken");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits { room_idle_secs: 60, ..Limits::DESIGN }).await;
        let key = key_of(&host, "alpha");
        let activity = Arc::clone(&read(&host.rooms).get(&key).unwrap().activity);
        host.wake(&key).await.unwrap();
        activity.age(Duration::from_secs(61));
        host.unload_idle();
        assert!(!in_memory(&host, &key), "idle for longer than the idle time");
        // Cold for long: the clock starts again at the wake, not at the last connection.
        activity.age(Duration::from_secs(3_600));
        host.wake(&key).await.unwrap();
        host.unload_idle();
        assert!(in_memory(&host, &key));
    }

    /// The fixture under seed `seed`, with its games' checksums tagged so its packages are
    /// its own (no other test in the process holds them).
    fn own_game(seed: &str, tag: &str) -> Loaded {
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
        v["seed_name"] = serde_json::json!(seed);
        for g in v["games"].as_object_mut().unwrap().values_mut() {
            g["checksum"] = serde_json::json!(format!("{tag}-{}", g["checksum"]));
        }
        parse_bundle(v.to_string().as_bytes(), &Limits::DESIGN).unwrap()
    }

    fn mem(host: &Host) -> Arc<MemLogStore> {
        match &host.store {
            Store::Mem { log, .. } => Arc::clone(log),
            Store::Pg(_) => unreachable!("tests run in memory"),
        }
    }

    #[tokio::test]
    async fn two_rooms_of_one_game_store_its_packages_once_and_each_its_seed() {
        let dir = TempDir::new("store-once");
        let mut host = host(&dir, Limits::DESIGN).await;
        let (a, b) = (own_game("store-a", "once"), own_game("store-b", "once"));
        let keys = seed_bundle::v2::package_keys(a.bundle.bytes()).unwrap();
        assert_eq!(keys, seed_bundle::v2::package_keys(b.bundle.bytes()).unwrap());
        let (seed_a, seed_b) = (a.bundle.bytes().to_vec(), b.bundle.bytes().to_vec());
        host.import(a).await.unwrap();
        host.import(b).await.unwrap();
        let log = mem(&host);
        assert_eq!(log.blobs(), 2, "the game and the generic game, once for both rooms");
        for k in &keys {
            assert!(log.get(k).await.unwrap().is_some());
        }
        let id = |seed| crate::room_id(seed);
        assert_eq!(log.seed_get(id("store-a")).await.unwrap(), Some(seed_a));
        assert_eq!(log.seed_get(id("store-b")).await.unwrap(), Some(seed_b));
    }

    /// The room loads from the store alone: after it leaves memory and no copy of its packages
    /// is left in the process, a wake reads its seed and its packages (checked) back.
    /// A stored seed swapped for another bundle's (same seed name) is refused on load: the
    /// room never plays a world it was not listed with.
    #[tokio::test]
    async fn a_stored_seed_of_another_bundle_is_refused_on_load() {
        let dir = TempDir::new("swapped-seed");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = host.import(own_game("swapped", "one")).await.unwrap();
        let other = own_game("swapped", "two");
        let log = mem(&host);
        for f in &other.packages {
            log.put(&log::blob_key(f), f).await.unwrap();
        }
        let id = crate::room_id("swapped");
        log.forget_seed(id);
        log.seed_put(id, other.bundle.bytes()).await.unwrap();
        let why = host.wake(&key).await.unwrap_err();
        assert!(why.contains("another bundle"), "{why}");
        assert!(!in_memory(&host, &key));
    }

    /// A bundle of another world under a listed seed name is refused, not taken as the room.
    #[tokio::test]
    async fn another_bundle_under_a_listed_seed_name_is_refused() {
        let dir = TempDir::new("listed-name");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = host.import(own_game("listed", "one")).await.unwrap();
        assert_eq!(host.import(own_game("listed", "one")).await.unwrap(), key, "same bundle");
        let why = String::from(host.import(own_game("listed", "two")).await.unwrap_err());
        assert!(why.contains("different bundle"), "{why}");
    }

    /// A generated world (not listed) naming a seed this host serves is refused, and the room
    /// keeps its world.
    #[tokio::test]
    async fn a_generated_world_naming_a_hosted_seed_is_refused() {
        let dir = TempDir::new("hosted-name");
        let mut host = host(&dir, Limits::DESIGN).await;
        let mine = own_game("hosted", "one");
        let key = host.import(mine.clone()).await.unwrap();
        let (done, answer) = oneshot::channel();
        host.import_request(Import { loaded: own_game("hosted", "two"), list: false, done });
        assert!(answer.await.unwrap().is_err());
        let stored = mem(&host).seed_get(crate::room_id("hosted")).await.unwrap();
        assert_eq!(stored.as_deref(), Some(mine.bundle.bytes()));
        host.wake(&key).await.unwrap();
    }

    /// An import's store work runs apart from the loop: the request returns at once, and the
    /// room is listed when the work ends.
    #[tokio::test]
    async fn an_import_runs_apart_from_the_supervisor_loop() {
        let dir = TempDir::new("import-task");
        let mut host = host(&dir, Limits::DESIGN).await;
        let (done, answer) = oneshot::channel();
        host.import_request(Import { loaded: own_game("tasked", "one"), list: true, done });
        assert!(host.is_empty(), "not listed while the work runs");
        let end = host.imports.join_next().await.unwrap().unwrap();
        host.imported(end);
        let key = answer.await.unwrap().unwrap().unwrap();
        host.wake(&key).await.unwrap();
    }

    /// The export refuses a seed of another bundle than the world's, and decodes its own.
    #[tokio::test]
    async fn the_export_refuses_a_seed_of_another_bundle() {
        let dir = TempDir::new("export-hash");
        let mut host = host(&dir, Limits::DESIGN).await;
        let mine = own_game("exported", "one");
        host.import(mine.clone()).await.unwrap();
        let id = crate::room_id("exported");
        let ask = |host: &mut Host, hash: String| {
            let (done, answer) = oneshot::channel();
            host.export_request(Export { room: id, hash, done });
            answer
        };
        let other = crate::hex(&own_game("exported", "two").hash);
        assert!(ask(&mut host, other).await.unwrap().is_err());
        let v1 = ask(&mut host, crate::hex(&mine.hash)).await.unwrap().unwrap();
        assert_eq!(crate::bundle_hash(&v1).unwrap(), mine.hash);
    }

    #[tokio::test]
    async fn a_cold_room_wakes_from_its_stored_seed_and_packages() {
        let dir = TempDir::new("from-store");
        let mut host = host(&dir, Limits { room_idle_secs: 0, ..Limits::DESIGN }).await;
        let loaded = own_game("from-store", "wake");
        let keys = seed_bundle::v2::package_keys(loaded.bundle.bytes()).unwrap();
        let key = host.import(loaded).await.unwrap();
        host.wake(&key).await.unwrap();
        host.unload_idle();
        assert!(!in_memory(&host, &key));
        // The aborted room task lets go of its seed when the runtime next runs it.
        let held = || keys.iter().any(|k| seed_bundle::package::cached(k).is_some());
        for _ in 0..100 {
            if !held() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(!held(), "no copy of the packages is left in the process");
        host.wake(&key).await.unwrap();
        assert!(in_memory(&host, &key));
        assert!(keys.iter().all(|k| seed_bundle::package::cached(k).is_some()), "fetched");
    }

    /// A package row whose bytes do not hash to its key (tampered, or torn) is refused when a
    /// room loads, never used.
    #[tokio::test]
    async fn a_tampered_package_in_the_store_is_refused_on_load() {
        let dir = TempDir::new("tampered");
        let mut host = host(&dir, Limits::DESIGN).await;
        let loaded = own_game("tampered", "bad");
        let keys = seed_bundle::v2::package_keys(loaded.bundle.bytes()).unwrap();
        let key = host.import(loaded).await.unwrap();
        mem(&host).tamper_blob(keys[0], b"not this package");
        let e = host.wake(&key).await.unwrap_err();
        assert!(e.contains("does not match its key"), "{e}");
        assert!(!in_memory(&host, &key));
    }

    /// Ask the host to wake `key`, in order: the request is queued before this returns.
    fn ask(host: &Host, key: &RoomKey) -> oneshot::Receiver<Result<(), String>> {
        let (done, answer) = oneshot::channel();
        host.waker.0.send(Request::Wake(Wake { key: key.clone(), done })).unwrap();
        answer
    }

    #[tokio::test]
    async fn two_wakes_of_one_cold_room_share_one_load() {
        let dir = TempDir::new("shared");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = key_of(&host, "alpha");
        let (first, second) = (ask(&host, &key), ask(&host, &key));
        let both = async { (first.await.unwrap(), second.await.unwrap()) };
        let woke = tokio::select! {
            fatal = host.supervise() => panic!("the host ended: {}", fatal.exit_code()),
            woke = both => woke,
        };
        assert_eq!(woke, (Ok(()), Ok(())));
        assert_eq!((host.runs, host.resident()), (1, 1), "one load");
        let unknown = RoomKey::parse("zzzzzzzzzz").unwrap();
        let refused = ask(&host, &unknown);
        let refused = tokio::select! {
            fatal = host.supervise() => panic!("the host ended: {}", fatal.exit_code()),
            r = refused => r.unwrap(),
        };
        assert!(refused.unwrap_err().contains("no room"));
    }

    #[tokio::test]
    async fn a_room_with_a_connection_open_stays_in_memory() {
        let dir = TempDir::new("busy");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits { room_idle_secs: 0, ..Limits::DESIGN }).await;
        let key = key_of(&host, "alpha");
        host.wake(&key).await.unwrap();
        let open = visit(&host, &key);
        host.unload_idle();
        assert!(in_memory(&host, &key));
        drop(open);
        host.unload_idle();
        assert!(!in_memory(&host, &key));
    }

    fn age(host: &Host, key: &RoomKey, by: Duration) {
        read(&host.rooms).get(key).unwrap().activity.age(by);
    }

    /// At the cap, a load puts the resident room unused for the longest to sleep; it is still
    /// listed and loads again under its key (#66).
    #[tokio::test]
    async fn at_the_resident_cap_the_room_unused_longest_leaves_memory_for_a_new_load() {
        let dir = TempDir::new("cap-evict");
        for seed in ["alpha", "beta", "gamma"] {
            dir.bundle(&format!("{seed}.json"), seed);
        }
        let mut host = host(&dir, Limits { max_resident_rooms: 2, ..Limits::DESIGN }).await;
        let [a, b, c] = ["alpha", "beta", "gamma"].map(|s| key_of(&host, s));
        host.wake(&a).await.unwrap();
        host.wake(&b).await.unwrap();
        age(&host, &a, Duration::from_secs(120));
        age(&host, &b, Duration::from_secs(60));
        host.wake(&c).await.unwrap();
        assert_eq!(host.resident(), 2);
        assert!(!in_memory(&host, &a), "unused the longest");
        assert!(in_memory(&host, &b) && in_memory(&host, &c));
        assert_eq!(seeds(&host), ["alpha", "beta", "gamma"], "still listed");
        // Waking a room already resident needs no space.
        host.wake(&b).await.unwrap();
        assert!(in_memory(&host, &c));
        host.wake(&a).await.unwrap();
        assert!(in_memory(&host, &a) && host.resident() == 2);
    }

    /// A room with a connection open is never put to sleep for the cap: when every resident
    /// room has one, the load is refused, and it goes ahead once a connection ends (#66).
    #[tokio::test]
    async fn at_the_resident_cap_a_room_in_use_stays_and_the_load_is_refused() {
        let dir = TempDir::new("cap-busy");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits { max_resident_rooms: 1, ..Limits::DESIGN }).await;
        let (a, b) = (key_of(&host, "alpha"), key_of(&host, "beta"));
        host.wake(&a).await.unwrap();
        let open = visit(&host, &a);
        let why = host.wake(&b).await.unwrap_err();
        assert!(why.contains("none can leave memory now"), "{why}");
        assert!(in_memory(&host, &a) && !in_memory(&host, &b));
        drop(open);
        host.wake(&b).await.unwrap();
        assert!(!in_memory(&host, &a) && in_memory(&host, &b));
    }

    /// Loads under way count toward the cap: two wakes of different cold rooms at a cap of
    /// one load the first and refuse the second, never both (#66).
    #[tokio::test]
    async fn a_load_under_way_counts_toward_the_resident_cap() {
        let dir = TempDir::new("cap-loading");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits { max_resident_rooms: 1, ..Limits::DESIGN }).await;
        let (a, b) = (key_of(&host, "alpha"), key_of(&host, "beta"));
        let (first, second) = (ask(&host, &a), ask(&host, &b));
        let both = async { (first.await.unwrap(), second.await.unwrap()) };
        let (first, second) = tokio::select! {
            fatal = host.supervise() => panic!("the host ended: {}", fatal.exit_code()),
            woke = both => woke,
        };
        assert_eq!(first, Ok(()));
        assert!(second.unwrap_err().contains("none can leave memory now"));
        assert!(in_memory(&host, &a) && host.resident() == 1);
    }

    #[tokio::test]
    async fn a_room_idle_past_the_abandon_time_ends_and_is_never_listed_again() {
        let dir = TempDir::new("abandon");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits { room_abandon_secs: 0, ..Limits::DESIGN }).await;
        let beta = key_of(&host, "beta");
        let open = visit(&host, &beta);
        host.end_abandoned().await;
        assert_eq!(seeds(&host), ["beta"], "a room with a connection open is not abandoned");
        drop(open);
        host.end_abandoned().await;
        assert!(host.is_empty());
        // Its file is still there, and a copy under another name is refused too.
        dir.bundle("again.json", "alpha");
        host.scan().await;
        assert!(host.is_empty());
    }

    #[tokio::test]
    async fn a_young_room_is_neither_unloaded_nor_abandoned() {
        let dir = TempDir::new("young");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = key_of(&host, "alpha");
        host.wake(&key).await.unwrap();
        host.unload_idle();
        host.end_abandoned().await;
        assert!(in_memory(&host, &key));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_room_alone_goes_cold_and_loads_again_under_its_key() {
        let dir = TempDir::new("failed");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits::DESIGN).await;
        let (alpha, beta) = (key_of(&host, "alpha"), key_of(&host, "beta"));
        host.wake(&alpha).await.unwrap();
        host.wake(&beta).await.unwrap();
        let run = host.running[&alpha].run;
        assert!(host.ended((alpha.clone(), run, Ok(Stopped::Failed))).is_none());
        assert!(!in_memory(&host, &alpha) && in_memory(&host, &beta));
        // It stopped right after it loaded, so it waits the first retry time (#64).
        tokio::time::advance(Duration::from_secs(1)).await;
        host.wake(&alpha).await.unwrap();
        assert!(in_memory(&host, &alpha));
        // The end of a load this host already replaced is ignored.
        assert!(host.ended((alpha.clone(), run, Ok(Stopped::Failed))).is_none());
        assert!(in_memory(&host, &alpha));
    }

    #[test]
    fn the_retry_wait_doubles_from_one_second_up_to_a_minute() {
        let waits: Vec<u64> = (1..=9).map(|n| retry_wait(&Limits::DESIGN, n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(retry_wait(&Limits::DESIGN, u32::MAX), Duration::from_secs(60));
    }

    /// A load that fails is not tried again until its wait ends, and each failure in a row
    /// doubles the wait (#64).
    #[tokio::test(start_paused = true)]
    async fn a_room_that_fails_to_load_waits_longer_after_each_failure() {
        let dir = TempDir::new("retry-load");
        let mut host = host(&dir, Limits::DESIGN).await;
        let loaded = own_game("retry-load", "bad");
        let keys = seed_bundle::v2::package_keys(loaded.bundle.bytes()).unwrap();
        let key = host.import(loaded).await.unwrap();
        mem(&host).tamper_blob(keys[0], b"not this package");
        let load_fails = |e: String| assert!(e.contains("does not match its key"), "{e}");
        let waits = |e: String| assert!(e.contains("1 quick failures in a row"), "{e}");
        load_fails(host.wake(&key).await.unwrap_err());
        waits(host.wake(&key).await.unwrap_err());
        tokio::time::advance(Duration::from_secs(1)).await;
        load_fails(host.wake(&key).await.unwrap_err());
        // Two in a row: two seconds now.
        tokio::time::advance(Duration::from_secs(1)).await;
        let e = host.wake(&key).await.unwrap_err();
        assert!(e.contains("2 quick failures in a row"), "{e}");
        tokio::time::advance(Duration::from_secs(1)).await;
        load_fails(host.wake(&key).await.unwrap_err());
        assert_eq!(host.failing[&key].count, 3);
    }

    /// A room that stops soon after it loads waits before the router can load it again; the
    /// refused wake starts no load. Other rooms load as before (#64).
    #[tokio::test(start_paused = true)]
    async fn a_room_that_stops_soon_after_it_loads_is_not_loaded_again_at_once() {
        let dir = TempDir::new("retry-quick");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits::DESIGN).await;
        let (alpha, beta) = (key_of(&host, "alpha"), key_of(&host, "beta"));
        host.wake(&alpha).await.unwrap();
        let run = host.running[&alpha].run;
        assert!(host.ended((alpha.clone(), run, Ok(Stopped::Failed))).is_none());
        let (refused, other) = (ask(&host, &alpha), ask(&host, &beta));
        let both = async { (refused.await.unwrap(), other.await.unwrap()) };
        let (refused, other) = tokio::select! {
            fatal = host.supervise() => panic!("the host ended: {}", fatal.exit_code()),
            woke = both => woke,
        };
        assert!(refused.unwrap_err().contains("may load again"));
        assert_eq!(other, Ok(()));
        assert!(!in_memory(&host, &alpha) && in_memory(&host, &beta));
        assert_eq!(host.runs, 2, "no load of alpha was started");
    }

    /// A room that stopped after a healthy run loads again at once, and a healthy run
    /// forgets the quick failures before it (#64).
    #[tokio::test(start_paused = true)]
    async fn a_healthy_run_forgets_the_failures_before_it() {
        let dir = TempDir::new("retry-healthy");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = key_of(&host, "alpha");
        let healthy = Duration::from_secs(Limits::DESIGN.room_healthy_secs);
        let stop = |host: &mut Host| {
            let run = host.running[&key].run;
            assert!(host.ended((key.clone(), run, Ok(Stopped::Failed))).is_none());
        };
        host.wake(&key).await.unwrap();
        stop(&mut host);
        tokio::time::advance(Duration::from_secs(1)).await;
        host.wake(&key).await.unwrap();
        tokio::time::advance(healthy).await;
        host.forget_healthy();
        assert!(host.failing.is_empty(), "forgotten while it runs");
        stop(&mut host);
        host.wake(&key).await.unwrap();
        assert!(in_memory(&host, &key));
    }

    #[tokio::test]
    async fn a_fence_ends_the_process_in_a_rooms_folder_too() {
        let dir = TempDir::new("fenced");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = key_of(&host, "alpha");
        host.wake(&key).await.unwrap();
        let run = host.running[&key].run;
        let fenced = Ok(Stopped::Fenced { current: proto::Epoch(9) });
        assert_eq!(host.ended((key, run, fenced)).map(|e| e.exit_code()), Some(3));
    }

    #[tokio::test]
    async fn one_bundle_is_loaded_at_startup_never_unloaded_or_abandoned_and_ends_the_process() {
        let limits = Limits { room_abandon_secs: 0, room_idle_secs: 0, ..Limits::DESIGN };
        let (_, mut host) = app(&Source::Bundle(fixture()), None, None, limits).await.unwrap();
        assert_eq!(host.resident(), 1);
        host.unload_idle();
        assert_eq!(host.resident(), 1, "never leaves memory");
        host.end_abandoned().await;
        assert_eq!(host.len(), 1);
        let (key, run) = host.running.iter().map(|(k, r)| (k.clone(), r.run)).next().unwrap();
        assert_eq!(host.ended((key, run, Ok(Stopped::Failed))).map(|e| e.exit_code()), Some(1));
    }

    /// The directory in Postgres (feature `pg-tests`, docs/TESTING.md): each test on its own
    /// schema.
    #[cfg(feature = "pg-tests")]
    mod pg {
        use super::*;
        use sqlx::Connection;

        async fn database() -> String {
            let url = config::load_secrets().unwrap().database_url.expect(
                "pg-tests needs CONSTELLATION_DATABASE_URL (docs/TESTING.md rule 5: no silent skip)",
            );
            log::testing::fresh_schema(&url).await.unwrap()
        }

        async fn pg_host(dir: &TempDir, db: &str, limits: Limits) -> Host {
            app(&Source::Rooms(dir.0.clone()), None, Some(db), limits).await.unwrap().1
        }

        async fn sql(db: &str, q: &'static str) {
            let mut c = sqlx::PgConnection::connect(db).await.unwrap();
            sqlx::raw_sql(q).execute(&mut c).await.unwrap();
        }

        /// How long ago the database says the only room was last used.
        async fn stored_idle_secs(db: &str) -> i64 {
            let mut c = sqlx::PgConnection::connect(db).await.unwrap();
            sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now() - last_active)::bigint FROM rooms")
                .fetch_one(&mut c)
                .await
                .unwrap()
        }

        /// The fixture under another seed name, as read.
        fn loaded(seed: &str) -> Loaded {
            let mut v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
            v["seed_name"] = serde_json::json!(seed);
            parse_bundle(v.to_string().as_bytes(), &Limits::DESIGN).unwrap()
        }

        /// Everything logged at `debug` and above while `run` runs.
        async fn logged(run: impl Future<Output = ()>) -> String {
            #[derive(Clone, Default)]
            struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
            impl std::io::Write for Buf {
                fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                    self.0.lock().unwrap().extend_from_slice(b);
                    Ok(b.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let buf = Buf::default();
            let out = buf.clone();
            let logs = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .with_writer(move || out.clone())
                .finish();
            {
                // This test's thread only: the scan logs from it (current-thread runtime).
                let _logs = tracing::subscriber::set_default(logs);
                run.await;
            }
            String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
        }

        /// A bundle skipped for a reason that may pass (no key drawn here, as when every draw
        /// collides or the database fails) warns once, then is tried again on each scan with
        /// a debug line only, and is listed once the reason is gone.
        #[tokio::test]
        async fn a_bundle_skipped_for_now_warns_once_then_is_tried_again_quietly() {
            let (db, dir) = (database().await, TempDir::new("pg-later"));
            let limits = Limits { room_key_draws: 0, ..Limits::DESIGN };
            let mut host = pg_host(&dir, &db, limits).await;
            dir.bundle("a.json", "alpha");
            let log = logged(async {
                host.scan().await;
                host.scan().await;
            })
            .await;
            assert_eq!(log.matches("skipped for now, tried again").count(), 1, "{log}");
            assert_eq!(log.matches("still skipped").count(), 1, "{log}");
            host.limits.room_key_draws = Limits::DESIGN.room_key_draws;
            host.scan().await;
            assert_eq!(seeds(&host), ["alpha"]);
        }

        /// A room nobody used since its last write is not written again on a clean stop: the
        /// database keeps what it has (here an older time set behind the host's back).
        #[tokio::test]
        async fn a_clean_stop_leaves_a_room_unused_since_its_last_write_alone() {
            let (db, dir) = (database().await, TempDir::new("pg-unused"));
            dir.bundle("a.json", "alpha");
            let mut host = pg_host(&dir, &db, Limits::DESIGN).await;
            sql(&db, FORTY_DAYS).await;
            host.stop().await;
            assert!(stored_idle_secs(&db).await > 39 * 86_400);
        }

        /// A key source that hands out `keys` in order.
        fn draws(keys: &'static [&'static str]) -> impl FnMut() -> Result<RoomKey, String> {
            let mut keys = keys.iter();
            move || RoomKey::parse(keys.next().ok_or("out of keys")?)
        }

        #[tokio::test]
        async fn a_new_room_draws_its_key_again_on_a_collision_and_gives_up_after_its_draws() {
            let db = database().await;
            let store = PgLogStore::connect(&db, &Limits::DESIGN).await.unwrap();
            let open = |seed: &'static str, limits, keys| {
                let (store, loaded) = (&store, loaded(seed));
                async move {
                    open_with_key(store, crate::room_id(seed), &loaded, limits, draws(keys)).await
                }
            };
            let d = Limits::DESIGN;
            let (alpha, idle) = open("alpha", d, &["aaaaaaaaaa"]).await.unwrap();
            assert_eq!((alpha.as_str(), idle), ("aaaaaaaaaa", 0));
            let (beta, _) = open("beta", d, &["aaaaaaaaaa", "bbbbbbbbbb"]).await.unwrap();
            assert_eq!(beta.as_str(), "bbbbbbbbbb", "drawn again after the collision");
            let two = Limits { room_key_draws: 2, ..d };
            let given_up = open("gamma", two, &["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc"]).await;
            assert!(matches!(given_up, Err(Skip::Later(_))), "{given_up:?}");
            // A reopen keeps the stored key, whatever is drawn.
            let (again, _) = open("alpha", d, &["dddddddddd"]).await.unwrap();
            assert_eq!(again, alpha);
            assert!(store.finish(crate::room_id("beta"), ABANDONED, 0).await.unwrap());
            let finished = open("beta", d, &["eeeeeeeeee"]).await;
            assert!(matches!(finished, Err(Skip::Final(_))), "{finished:?}");
        }

        #[tokio::test]
        async fn a_restart_lists_stored_rooms_only_up_to_the_cap() {
            let (db, dir) = (database().await, TempDir::new("pg-cap"));
            dir.bundle("a.json", "alpha");
            dir.bundle("b.json", "beta");
            drop(pg_host(&dir, &db, Limits::DESIGN).await);
            let empty = TempDir::new("pg-cap-empty");
            let again = pg_host(&empty, &db, Limits { max_rooms: 1, ..Limits::DESIGN }).await;
            assert_eq!(again.len(), 1);
        }

        /// As if `by` passed for the room: its idle clock and the host's note of what the
        /// database holds move back together. Aging only the clock would put its last use
        /// before the last write, which real time never does.
        fn elapse(host: &mut Host, key: &RoomKey, by: Duration) {
            read(&host.rooms).get(key).unwrap().activity.age(by);
            let written = host.recorded.get_mut(key).unwrap();
            *written = written.minus(by);
        }

        const FORTY_DAYS: &str = "UPDATE rooms SET last_active = now() - interval '40 days'";

        /// Review of #55, H1: a connection open longer than the abandon time kept
        /// `last_active` at its old value, so a restart ended a room in use.
        #[tokio::test]
        async fn a_room_in_use_for_longer_than_the_abandon_time_survives_a_restart() {
            let (db, dir) = (database().await, TempDir::new("pg-in-use"));
            dir.bundle("a.json", "alpha");
            let limits = Limits { room_touch_secs: 0, ..Limits::DESIGN };
            let mut host = pg_host(&dir, &db, limits).await;
            let key = key_of(&host, "alpha");
            host.wake(&key).await.unwrap();
            let open = visit(&host, &key);
            // One connection open for 40 days: the database last heard of the room then.
            sql(&db, FORTY_DAYS).await;
            host.tick().await;
            assert!(stored_idle_secs(&db).await < 60, "a room in use is recorded as used");
            drop((open, host));
            let mut again = pg_host(&dir, &db, Limits::DESIGN).await;
            again.tick().await;
            assert_eq!(seeds(&again), ["alpha"], "still served after the restart");
        }

        /// Review of #55, M2: a load stuck on the database (alpha's owner row is locked, so
        /// its claim waits) must not hold up the wake of another room. Before, every wake ran
        /// inline in the supervisor loop and beta waited for alpha's lock timeout.
        #[tokio::test]
        async fn a_load_stuck_on_the_database_does_not_hold_up_another_rooms_wake() {
            let (db, dir) = (database().await, TempDir::new("pg-slow"));
            dir.bundle("a.json", "alpha");
            dir.bundle("b.json", "beta");
            // Long database timeouts, so before the fix beta would wait far past the guard.
            let limits = Limits {
                db_lock_timeout_ms: 60_000,
                db_statement_timeout_ms: 60_000,
                ..Limits::DESIGN
            };
            let mut host = pg_host(&dir, &db, limits).await;
            let (alpha, beta) = (key_of(&host, "alpha"), key_of(&host, "beta"));
            host.wake(&alpha).await.unwrap();
            host.wake(&beta).await.unwrap();
            host.limits.room_idle_secs = 0;
            host.unload_idle();
            host.limits.room_idle_secs = Limits::DESIGN.room_idle_secs;
            assert_eq!(host.resident(), 0);
            let mut lock = sqlx::PgConnection::connect(&db).await.unwrap();
            sqlx::raw_sql("BEGIN").execute(&mut lock).await.unwrap();
            sqlx::query("SELECT 1 FROM room_owner WHERE room_id = $1 FOR UPDATE")
                .bind(i64::from_be_bytes(crate::room_id("alpha").0.to_be_bytes()))
                .execute(&mut lock)
                .await
                .unwrap();
            let (slow, also) = (ask(&host, &alpha), ask(&host, &alpha));
            let waker = host.waker();
            let body = async {
                let fast = tokio::time::timeout(Duration::from_secs(10), waker.wake(beta)).await;
                assert_eq!(fast, Ok(Ok(())), "beta waited for alpha's load");
                sqlx::raw_sql("ROLLBACK").execute(&mut lock).await.unwrap();
                (slow.await.unwrap(), also.await.unwrap())
            };
            let woke = tokio::select! {
                fatal = host.supervise() => panic!("the host ended: {}", fatal.exit_code()),
                woke = body => woke,
            };
            assert_eq!(woke, (Ok(()), Ok(())), "alpha loads once its row is free");
            assert_eq!(host.resident(), 2);
        }

        #[tokio::test]
        async fn a_clean_stop_records_when_each_room_was_last_used() {
            let (db, dir) = (database().await, TempDir::new("pg-stop"));
            dir.bundle("a.json", "alpha");
            let mut host = pg_host(&dir, &db, Limits::DESIGN).await;
            let key = key_of(&host, "alpha");
            host.wake(&key).await.unwrap();
            drop(visit(&host, &key));
            sql(&db, FORTY_DAYS).await;
            host.tick().await;
            // In memory and written less than room_touch_secs ago: not written on a tick.
            assert!(stored_idle_secs(&db).await > 39 * 86_400);
            host.stop().await;
            assert!(stored_idle_secs(&db).await < 60);
        }

        #[tokio::test]
        async fn a_room_that_left_memory_has_its_idle_time_read_back_after_a_restart() {
            let (db, dir) = (database().await, TempDir::new("pg-unload"));
            dir.bundle("a.json", "alpha");
            let limits = Limits { room_idle_secs: 60, room_touch_secs: 3_600, ..Limits::DESIGN };
            let mut host = pg_host(&dir, &db, limits).await;
            let key = key_of(&host, "alpha");
            host.wake(&key).await.unwrap();
            elapse(&mut host, &key, Duration::from_secs(120));
            host.tick().await;
            assert!(!in_memory(&host, &key));
            assert!((120..180).contains(&stored_idle_secs(&db).await));
            drop(host);
            let again = pg_host(&dir, &db, Limits::DESIGN).await;
            let idle = read(&again.rooms).get(&key).unwrap().activity.idle_for().unwrap();
            assert!((120..180).contains(&idle.as_secs()), "{idle:?}");
        }

        #[tokio::test]
        async fn a_room_the_database_saw_recently_is_not_abandoned_on_a_stale_clock() {
            let (db, dir) = (database().await, TempDir::new("pg-stale"));
            dir.bundle("a.json", "alpha");
            let limits =
                Limits { room_abandon_secs: 100, room_touch_secs: 3_600, ..Limits::DESIGN };
            let mut host = pg_host(&dir, &db, limits).await;
            let key = key_of(&host, "alpha");
            read(&host.rooms).get(&key).unwrap().activity.age(Duration::from_secs(200));
            host.end_abandoned().await;
            assert_eq!(seeds(&host), ["alpha"], "the database says it was used just now");
            let idle = read(&host.rooms).get(&key).unwrap().activity.idle_for().unwrap();
            assert!(idle < Duration::from_secs(100), "the clock starts again: {idle:?}");
        }
    }
}
