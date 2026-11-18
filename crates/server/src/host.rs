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
//! No room ever runs without a connection (there are no room timers yet), so "no connection"
//! here is also "no event", as D-30 asks.
//!
//! With `--bundle` the one room is loaded at startup, never leaves memory and is never
//! abandoned, and its end is the process's end, as before. With `--rooms`, one room's trouble never stops the others: a
//! bad bundle is skipped with a warning, and a room whose task fails goes cold, so its next
//! connect loads it again from its log. Only a fence ends the process, because then another
//! process owns the database.

use crate::rooms::{Activity, Listing, Moment, RoomKey, Rooms};
use crate::{
    Loaded, RunError, load_bundle, open_with_key, parse_bundle, prepare, room_ended, start,
};
use config::Limits;
use log::{MemLogStore, PgLogStore};
use proto::RoomId;
use runtime::Stopped;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinError, JoinSet};

/// Where the directory and the rooms' logs live.
enum Store {
    Pg(Arc<PgLogStore>),
    /// In memory: the log store, each listed room's bundle file, and the rooms that finished.
    Mem {
        log: Arc<MemLogStore>,
        bundles: BTreeMap<RoomId, String>,
        finished: BTreeSet<RoomId>,
    },
}

/// A room in memory, as the host tracks its task.
struct Running {
    /// Tells this load of the room from an earlier one under the same key.
    run: u64,
    task: AbortHandle,
}

/// What a room task's end is reported with: its key, which load it was, and why it ended.
type Ended = (RoomKey, u64, Result<Stopped, JoinError>);

/// The reason a room left alone for `room_abandon_secs` ends with (D-30).
const ABANDONED: &str = "abandoned";

/// Seconds in a day, for the log line of an abandoned room.
const DAY_SECS: u64 = 24 * 60 * 60;

/// A file's length and modification time: a scan rereads a file only when they change.
type Seen = (u64, Option<SystemTime>);

/// A request to load a cold room, answered once it is in memory.
struct Wake {
    key: RoomKey,
    done: oneshot::Sender<Result<(), String>>,
}

/// Asks the [`Host`] to load a cold room (the router's side).
#[derive(Debug, Clone)]
pub struct Waker(mpsc::UnboundedSender<Wake>);

impl Waker {
    /// Load the cold room `key` and wait until it is in memory.
    ///
    /// # Errors
    /// Why the room could not be loaded.
    pub(crate) async fn wake(&self, key: RoomKey) -> Result<(), String> {
        let (done, answer) = oneshot::channel();
        self.0.send(Wake { key, done }).map_err(|_| "the server is stopping".to_owned())?;
        answer.await.map_err(|_| "the server is stopping".to_owned())?
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
    /// Every bundle file as the last scan saw it, whatever came of it.
    seen: BTreeMap<PathBuf, Seen>,
    // Unbounded, but every request holds a connection permit (`max_connections`).
    wakes: mpsc::UnboundedReceiver<Wake>,
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
    /// A host with no rooms yet, on Postgres when `database_url` is set, and the waker the
    /// router uses.
    pub(crate) async fn new(
        database_url: Option<&str>,
        password: Option<String>,
        limits: Limits,
        folder: Option<PathBuf>,
    ) -> Result<(Self, Waker), String> {
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
            Store::Mem {
                log: Arc::new(MemLogStore::new()),
                bundles: BTreeMap::new(),
                finished: BTreeSet::new(),
            }
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
        };
        Ok((host, Waker(tx)))
    }

    /// The room list the router reads.
    pub(crate) fn rooms(&self) -> Arc<RwLock<Rooms>> {
        Arc::clone(&self.rooms)
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
            let key = RoomKey::parse(&room.key).map_err(|e| format!("stored {e}"))?;
            let idle = Duration::from_secs(room.idle_secs);
            self.list(key, room.room, room.seed_name, idle)?;
        }
        Ok(())
    }

    fn list(
        &mut self,
        key: RoomKey,
        id: RoomId,
        seed: String,
        idle: Duration,
    ) -> Result<(), String> {
        tracing::info!(%key, seed, "room ready");
        let activity = Activity::new(idle);
        let used = activity.last_used(Moment::now());
        write(&self.rooms).insert(key.clone(), Listing { id, seed, room: None, activity })?;
        self.recorded.insert(key, used);
        Ok(())
    }

    /// Add a room to the directory (cold) from its bundle and return its key. A seed that is
    /// already listed returns its key unchanged.
    pub(crate) async fn import(&mut self, loaded: Loaded) -> Result<RoomKey, String> {
        let id = crate::room_id(&loaded.bundle.seed_name);
        if let Some(key) = read(&self.rooms).key_of(id) {
            return Ok(key.clone());
        }
        if self.len() >= self.limits.max_rooms {
            return Err(format!("the server lists {} rooms, its most", self.limits.max_rooms));
        }
        let seed = loaded.bundle.seed_name.clone();
        let (key, idle) = match &mut self.store {
            Store::Pg(store) => open_with_key(store, id, &loaded).await?,
            Store::Mem { bundles, finished, .. } => {
                if finished.contains(&id) {
                    return Err(format!("room {seed:?} has finished; it is not served again"));
                }
                bundles.insert(id, loaded.text);
                (RoomKey::random()?, 0)
            }
        };
        self.list(key.clone(), id, seed, Duration::from_secs(idle))?;
        Ok(key)
    }

    /// Load the cold room `key` into memory: claim it, rebuild it from the log and start its
    /// task. A room already in memory is left as it is.
    pub(crate) async fn wake(&mut self, key: &RoomKey) -> Result<(), String> {
        let (id, seed) = {
            let rooms = read(&self.rooms);
            let listing = rooms.get(key).ok_or_else(|| format!("no room with key {key}"))?;
            if listing.room.is_some() {
                return Ok(());
            }
            (listing.id, listing.seed.clone())
        };
        let text = match &self.store {
            Store::Pg(store) => {
                let file = store.room_bundle(id).await.map_err(|e| format!("postgres: {e:?}"))?;
                let file =
                    file.ok_or_else(|| format!("room {seed:?} is stored without its bundle"))?;
                String::from_utf8(file).map_err(|e| format!("room {seed:?}: stored bundle: {e}"))?
            }
            Store::Mem { bundles, .. } => {
                bundles.get(&id).cloned().ok_or_else(|| format!("room {seed:?} has no bundle"))?
            }
        };
        let loaded =
            parse_bundle(text).map_err(|e| format!("room {seed:?}: stored bundle: {e}"))?;
        let p = prepare(loaded, self.password.as_ref(), self.limits)?;
        let room = p.room.clone();
        let handle = match &self.store {
            Store::Pg(store) => {
                let epoch = store.claim(id).await.map_err(|e| format!("postgres: {e:?}"))?;
                start(p, key.clone(), epoch, Arc::clone(store), &self.limits).await?
            }
            Store::Mem { log, .. } => {
                let epoch = log.claim(id);
                start(p, key.clone(), epoch, Arc::clone(log), &self.limits).await?
            }
        };
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
        self.running.insert(key.clone(), Running { run, task });
        Ok(())
    }

    /// Run until a room's end must end the process: load rooms on request, and on every tick
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
                Some(Wake { key, done }) = self.wakes.recv() => {
                    let woke = self.wake(&key).await;
                    if let Err(why) = &woke {
                        tracing::warn!(%key, "room not loaded: {why}");
                    }
                    drop(done.send(woke));
                }
                _ = tick.tick() => self.tick().await,
            }
        }
    }

    /// One supervisor tick: unload idle rooms, record when rooms were last used, end
    /// abandoned ones and rescan the rooms folder.
    async fn tick(&mut self) {
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
        self.running.remove(&key);
        if let Some(listing) = write(&self.rooms).get_mut(&key) {
            listing.room = None;
        }
        tracing::error!(%key, ?why, "room stopped; its next connect loads it again from its log");
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
                Store::Mem { bundles, finished, .. } => {
                    bundles.remove(&id);
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
            if let Some(r) = self.running.remove(&key) {
                r.task.abort();
            }
            let days = limit.as_secs() / DAY_SECS;
            tracing::info!(%key, days, "room abandoned; it has ended (D-30)");
        }
    }

    /// List a room for each bundle in the folder that is new or changed since the last scan.
    /// A file that does not make a room is skipped with a warning until it changes.
    pub(crate) async fn scan(&mut self) {
        let Some(folder) = self.folder.clone() else { return };
        let files = match bundle_files(&folder) {
            Ok(files) => files,
            Err(e) => {
                tracing::warn!("rooms folder: {e}");
                return;
            }
        };
        self.seen.retain(|path, _| files.iter().any(|(p, _)| p == path));
        for (path, seen) in files {
            if self.seen.get(&path) == Some(&seen) {
                continue;
            }
            self.seen.insert(path.clone(), seen);
            let listed = match load_bundle(&path, &self.limits) {
                Ok(loaded) => self.import(loaded).await.map(drop),
                Err(e) => Err(e),
            };
            if let Err(why) = listed {
                tracing::warn!(file = %path.display(), "skipped: {why}");
            }
        }
    }
}

fn read(rooms: &RwLock<Rooms>) -> std::sync::RwLockReadGuard<'_, Rooms> {
    // Each change to the map is one call, so a writer that panicked left it whole.
    rooms.read().unwrap_or_else(PoisonError::into_inner)
}

fn write(rooms: &RwLock<Rooms>) -> std::sync::RwLockWriteGuard<'_, Rooms> {
    rooms.write().unwrap_or_else(PoisonError::into_inner)
}

/// Every `*.json` file in `folder`, in name order, with what a scan compares.
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
        let mut host = host(&dir, Limits { room_idle_secs: 1, ..Limits::DESIGN }).await;
        let key = key_of(&host, "alpha");
        host.wake(&key).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        host.unload_idle();
        assert!(!in_memory(&host, &key), "idle for longer than the idle time");
        // Woken after a long sleep: the clock starts at the wake, not at the last connection.
        host.wake(&key).await.unwrap();
        host.unload_idle();
        assert!(in_memory(&host, &key));
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

    #[tokio::test]
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
        host.wake(&alpha).await.unwrap();
        assert!(in_memory(&host, &alpha));
        // The end of a load this host already replaced is ignored.
        assert!(host.ended((alpha.clone(), run, Ok(Stopped::Failed))).is_none());
        assert!(in_memory(&host, &alpha));
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
    async fn one_bundle_is_loaded_at_startup_never_abandoned_and_its_end_ends_the_process() {
        let limits = Limits { room_abandon_secs: 0, ..Limits::DESIGN };
        let (_, mut host) = app(&Source::Bundle(fixture()), None, None, limits).await.unwrap();
        assert_eq!(host.resident(), 1);
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
            read(&host.rooms).get(&key).unwrap().activity.age(Duration::from_secs(120));
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
