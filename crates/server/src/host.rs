//! The rooms a running process holds, and how they change while it runs.
//!
//! With `--bundle` the process runs one room for its whole life, and the room's end is the
//! process's end, as before. With `--rooms` the folder is the list of rooms: every
//! `rooms_scan_ms` the host looks in it again, starts a room for each new seed bundle and
//! removes rooms past `room_lifetime_secs`. One room's trouble never stops the others: a bad
//! bundle is skipped with a warning, and a room whose task fails is dropped and started again
//! from the log on the next scan, under the same key. Only a fence ends the process, because
//! then another process owns the database.
//!
//! The lifetime is a stopgap until rooms have a real lifecycle (see #56). An expired room's
//! task is stopped like a crash of that room alone (its log is durable up to its last reply),
//! its clients are closed with "room unavailable", new connections get 404, and its bundle file
//! is renamed `<name>.json.expired` so neither a scan nor a restart brings it back. Its data
//! stays in the log store.

use crate::rooms::{Room, RoomKey, Rooms};
use crate::{Loaded, Prepared, RunError, load_bundle, open_with_key, prepare, room_ended, start};
use config::Limits;
use log::{MemLogStore, PgLogStore};
use proto::RoomId;
use runtime::Stopped;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::task::{AbortHandle, JoinError, JoinSet};

/// Where the rooms' logs live.
enum Store {
    Pg(Arc<PgLogStore>),
    /// In memory, with the key each room was first given, so a room restarted after a failure
    /// keeps its address.
    Mem(Arc<MemLogStore>, BTreeMap<RoomId, RoomKey>),
}

/// A room this host started and has not removed.
struct Live {
    id: RoomId,
    /// Its bundle file, for a room from the rooms folder.
    path: Option<PathBuf>,
    /// Tells this start of the room from an earlier one under the same key.
    run: u64,
    task: AbortHandle,
}

/// What a room task's end is reported with: its key, which start it was, and why it ended.
type Ended = (RoomKey, u64, Result<Stopped, JoinError>);

/// A file's length and modification time: a scan rereads a file only when they change.
type Seen = (u64, Option<SystemTime>);

/// The rooms of a process and their tasks.
pub struct Host {
    rooms: Arc<RwLock<Rooms>>,
    store: Store,
    password: Option<String>,
    limits: Limits,
    /// The rooms folder (`--rooms`); `None` for `--bundle`.
    folder: Option<PathBuf>,
    tasks: JoinSet<Ended>,
    live: BTreeMap<RoomKey, Live>,
    runs: u64,
    /// When this process first started each room; a restart after a failure keeps it.
    started: BTreeMap<RoomId, Instant>,
    /// Rooms past their lifetime: never started again by this process.
    expired: BTreeSet<RoomId>,
    /// Every bundle file as the last scan saw it, whatever came of it.
    seen: BTreeMap<PathBuf, Seen>,
}

impl core::fmt::Debug for Host {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Host")
            .field("folder", &self.folder)
            .field("rooms", &self.live.len())
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
            Store::Mem(Arc::new(MemLogStore::new()), BTreeMap::new())
        };
        Ok(Self {
            rooms: Arc::new(RwLock::new(Rooms::default())),
            store,
            password,
            limits,
            folder,
            tasks: JoinSet::new(),
            live: BTreeMap::new(),
            runs: 0,
            started: BTreeMap::new(),
            expired: BTreeSet::new(),
            seen: BTreeMap::new(),
        })
    }

    /// The room list the router reads.
    pub(crate) fn rooms(&self) -> Arc<RwLock<Rooms>> {
        Arc::clone(&self.rooms)
    }

    /// The rooms folder and a view of its running rooms (`--rooms`); `None` for `--bundle`.
    pub(crate) fn folder(&self) -> Option<RoomsFolder> {
        let dir = self.folder.clone()?;
        Some(RoomsFolder { dir, rooms: self.rooms() })
    }

    /// How many rooms run.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// No room runs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Claim the room, rebuild it from the log, start its task and list it under its key.
    pub(crate) async fn add(
        &mut self,
        loaded: Loaded,
        path: Option<PathBuf>,
    ) -> Result<(), String> {
        let p: Prepared = prepare(loaded, self.password.as_ref(), self.limits)?;
        let id = p.id;
        let room = Room { file: path.clone(), ..p.room.clone() };
        let (key, handle) = match &mut self.store {
            Store::Pg(store) => {
                let key = open_with_key(store, &p).await?;
                let epoch = store.claim(id).await.map_err(|e| format!("postgres: {e:?}"))?;
                let handle = start(p, key.clone(), epoch, Arc::clone(store), &self.limits).await?;
                (key, handle)
            }
            Store::Mem(store, keys) => {
                let key = match keys.get(&id) {
                    Some(key) => key.clone(),
                    None => RoomKey::random()?,
                };
                keys.insert(id, key.clone());
                let epoch = store.claim(id);
                let handle = start(p, key.clone(), epoch, Arc::clone(store), &self.limits).await?;
                (key, handle)
            }
        };
        let task = handle.abort_handle();
        if let Err(e) = write(&self.rooms).insert(key.clone(), room) {
            task.abort();
            return Err(e);
        }
        self.runs += 1;
        let run = self.runs;
        let ended = key.clone();
        self.tasks.spawn(async move { (ended, run, handle.await) });
        self.started.entry(id).or_insert_with(Instant::now);
        self.live.insert(key, Live { id, path, run, task });
        Ok(())
    }

    /// Run until a room's end must end the process, rescanning the rooms folder meanwhile.
    pub(crate) async fn supervise(&mut self) -> RunError {
        let every = Duration::from_millis(self.limits.rooms_scan_ms.max(1));
        let mut scan = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let watching = self.folder.is_some();
        loop {
            tokio::select! {
                Some(end) = self.tasks.join_next() => match end {
                    Ok(end) => {
                        if let Some(fatal) = self.ended(end) {
                            return fatal;
                        }
                    }
                    // A watcher only awaits its room's handle and is never aborted (only the
                    // room task inside it is), so this cannot happen; the room's own end is
                    // what matters.
                    Err(e) => tracing::error!(error = %e, "a room watcher ended without a value"),
                },
                _ = scan.tick(), if watching => {
                    self.expire();
                    self.scan().await;
                }
                else => std::future::pending::<()>().await,
            }
        }
    }

    /// Handle a room task's end; `Some` when it must end the process.
    fn ended(&mut self, (key, run, why): Ended) -> Option<RunError> {
        if !self.live.get(&key).is_some_and(|l| l.run == run) {
            // A room this host stopped on purpose.
            return None;
        }
        if self.folder.is_none() || matches!(why, Ok(Stopped::Fenced { .. })) {
            return Some(room_ended(why));
        }
        write(&self.rooms).remove(&key);
        if let Some(gone) = self.live.remove(&key)
            && let Some(path) = gone.path
        {
            // Forget the file, so the next scan starts the room again from its log.
            self.seen.remove(&path);
        }
        tracing::error!(%key, ?why, "room stopped; starting it again from its log on the next scan");
        None
    }

    /// Remove every room from the folder that is older than `room_lifetime_secs` (see #56).
    fn expire(&mut self) {
        let lifetime = Duration::from_secs(self.limits.room_lifetime_secs);
        let old: Vec<RoomKey> = self
            .live
            .iter()
            .filter(|(_, l)| {
                l.path.is_some() && self.started.get(&l.id).is_some_and(|t| t.elapsed() >= lifetime)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in old {
            let Some(gone) = self.live.remove(&key) else { continue };
            write(&self.rooms).remove(&key);
            gone.task.abort();
            self.expired.insert(gone.id);
            let renamed = gone.path.as_deref().map(retire_file);
            tracing::info!(
                %key,
                hours = lifetime.as_secs() / 3600,
                ?renamed,
                "room expired and removed (a temporary rule, see #56)"
            );
        }
    }

    /// Start a room for each bundle in the folder that is new or changed since the last scan.
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
            if let Err(why) = self.consider(&path).await {
                tracing::warn!(file = %path.display(), "skipped: {why}");
            }
        }
    }

    /// Start the room in `path`, unless it runs already, expired, or the host is full.
    async fn consider(&mut self, path: &Path) -> Result<(), String> {
        let loaded = load_bundle(path, &self.limits)?;
        let id = crate::room_id(&loaded.bundle.seed_name);
        if self.expired.contains(&id) {
            return Err("this room expired; it is not started again (see #56)".to_owned());
        }
        if let Some((key, other)) = self.live.iter().find(|(_, l)| l.id == id) {
            let from = other.path.as_deref().map_or_else(String::new, |p| p.display().to_string());
            return Err(format!(
                "seed {:?} already runs as room {key} (from {from}); a running room's file is \
                 not reread",
                loaded.bundle.seed_name
            ));
        }
        if self.live.len() >= self.limits.max_rooms {
            return Err(format!("the server runs {} rooms, its most", self.limits.max_rooms));
        }
        self.add(loaded, Some(path.to_owned())).await
    }
}

/// The rooms folder of a running host: where to put a bundle so the next scan starts it, and
/// which key the room from a file got.
#[derive(Debug, Clone)]
pub(crate) struct RoomsFolder {
    dir: PathBuf,
    rooms: Arc<RwLock<Rooms>>,
}

impl RoomsFolder {
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The running room started from `file` and its key, if any.
    pub(crate) fn room_of(&self, file: &Path) -> Option<(RoomKey, Room)> {
        crate::read(&self.rooms)
            .iter()
            .find(|(_, r)| r.file.as_deref() == Some(file))
            .map(|(k, r)| (k.clone(), r.clone()))
    }
}

fn write(rooms: &RwLock<Rooms>) -> std::sync::RwLockWriteGuard<'_, Rooms> {
    // A writer that panicked left the map whole (each change is one map call), so a poisoned
    // lock is still safe to use.
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

/// Rename an expired room's bundle to `<name>.json.expired`; the new name, or why it failed
/// (the room is still never started again by this process).
fn retire_file(path: &Path) -> Result<PathBuf, String> {
    let mut to = path.as_os_str().to_owned();
    to.push(".expired");
    let to = PathBuf::from(to);
    std::fs::rename(path, &to).map_err(|e| format!("rename {}: {e}", path.display()))?;
    Ok(to)
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
            let fixture =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
            let mut v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
            v["seed_name"] = serde_json::json!(seed);
            std::fs::write(self.0.join(file), v.to_string()).unwrap();
        }

        fn has(&self, file: &str) -> bool {
            self.0.join(file).exists()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    async fn host(dir: &TempDir, limits: Limits) -> Host {
        app(&Source::Rooms(dir.0.clone()), None, None, limits).await.unwrap().1
    }

    fn seeds(host: &Host) -> Vec<String> {
        read_rooms(host)
            .iter()
            .map(|(_, r)| r.seed.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn read_rooms(host: &Host) -> Rooms {
        host.rooms.read().unwrap().clone()
    }

    fn key_of(host: &Host, seed: &str) -> RoomKey {
        read_rooms(host).iter().find(|(_, r)| r.seed == seed).unwrap().0.clone()
    }

    #[tokio::test]
    async fn every_json_bundle_in_the_folder_starts_a_room() {
        let dir = TempDir::new("start");
        dir.bundle("b.json", "beta");
        dir.bundle("a.json", "alpha");
        std::fs::write(dir.0.join("notes.txt"), "not a bundle").unwrap();
        let host = host(&dir, Limits::DESIGN).await;
        assert_eq!(seeds(&host), ["alpha", "beta"]);
        assert_eq!(host.len(), 2);
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
        assert_eq!(
            seeds(&host),
            ["fine"],
            "a.json runs; b.json repeats it, c.json is past the cap"
        );
    }

    #[tokio::test]
    async fn a_scan_starts_bundles_added_while_the_server_runs() {
        let dir = TempDir::new("add");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let alpha = key_of(&host, "alpha");
        dir.bundle("b.json", "beta");
        host.scan().await;
        assert_eq!(seeds(&host), ["alpha", "beta"]);
        assert_eq!(key_of(&host, "alpha"), alpha, "a running room keeps its key");
        host.scan().await;
        assert_eq!(host.len(), 2, "an unchanged file is not started twice");
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
    async fn a_room_past_its_lifetime_is_removed_its_file_renamed_and_never_restarted() {
        let dir = TempDir::new("expire");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits { room_lifetime_secs: 0, ..Limits::DESIGN }).await;
        let key = key_of(&host, "alpha");
        let task = host.live[&key].task.clone();
        host.expire();
        assert!(host.is_empty() && read_rooms(&host).is_empty());
        assert!(!dir.has("a.json") && dir.has("a.json.expired"));
        tokio::task::yield_now().await;
        assert!(task.is_finished(), "the room task is stopped");
        // The same seed under a new file name is not started again either.
        dir.bundle("again.json", "alpha");
        host.scan().await;
        assert!(host.is_empty());
    }

    #[tokio::test]
    async fn a_young_room_is_kept() {
        let dir = TempDir::new("young");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        host.expire();
        assert_eq!(host.len(), 1);
        assert!(dir.has("a.json"));
    }

    #[tokio::test]
    async fn a_failed_room_alone_is_dropped_and_started_again_under_its_key() {
        let dir = TempDir::new("failed");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits::DESIGN).await;
        let alpha = key_of(&host, "alpha");
        let run = host.live[&alpha].run;
        assert!(host.ended((alpha.clone(), run, Ok(Stopped::Failed))).is_none());
        assert_eq!(seeds(&host), ["beta"], "the other room keeps running");
        host.scan().await;
        assert_eq!(seeds(&host), ["alpha", "beta"]);
        assert_eq!(key_of(&host, "alpha"), alpha);
        // The end of a start this host already replaced is ignored.
        assert!(host.ended((alpha.clone(), run, Ok(Stopped::Failed))).is_none());
        assert_eq!(host.len(), 2);
    }

    #[tokio::test]
    async fn a_fence_ends_the_process_in_a_rooms_folder_too() {
        let dir = TempDir::new("fenced");
        dir.bundle("a.json", "alpha");
        let mut host = host(&dir, Limits::DESIGN).await;
        let key = key_of(&host, "alpha");
        let run = host.live[&key].run;
        let fenced = Ok(Stopped::Fenced { current: proto::Epoch(9) });
        assert_eq!(host.ended((key, run, fenced)).map(|e| e.exit_code()), Some(3));
    }

    #[tokio::test]
    async fn a_folder_files_room_key_is_found_only_while_its_room_runs() {
        let dir = TempDir::new("key-of");
        dir.bundle("a.json", "alpha");
        dir.bundle("b.json", "beta");
        let mut host = host(&dir, Limits { room_lifetime_secs: 0, ..Limits::DESIGN }).await;
        let folder = host.folder().unwrap();
        assert_eq!(folder.dir(), dir.0.as_path());
        assert_eq!(
            folder.room_of(&dir.0.join("a.json")).map(|r| r.0),
            Some(key_of(&host, "alpha"))
        );
        assert_eq!(folder.room_of(&dir.0.join("b.json")).map(|r| r.0), Some(key_of(&host, "beta")));
        assert_eq!(folder.room_of(&dir.0.join("c.json")).map(|r| r.0), None, "no such file");
        host.expire();
        assert_eq!(
            folder.room_of(&dir.0.join("a.json")).map(|r| r.0),
            None,
            "an expired room has no key"
        );
    }

    #[tokio::test]
    async fn a_single_bundle_host_has_no_rooms_folder() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let (_, host) = app(&Source::Bundle(fixture), None, None, Limits::DESIGN).await.unwrap();
        assert!(host.folder().is_none());
    }

    #[tokio::test]
    async fn with_one_bundle_any_end_of_the_room_ends_the_process() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/two-slot.json");
        let (_, mut host) =
            app(&Source::Bundle(fixture), None, None, Limits::DESIGN).await.unwrap();
        let (key, live) = host.live.iter().next().unwrap();
        let end = (key.clone(), live.run, Ok(Stopped::Failed));
        assert_eq!(host.ended(end).map(|e| e.exit_code()), Some(1));
    }
}
