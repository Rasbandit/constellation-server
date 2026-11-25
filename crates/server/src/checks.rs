//! The checks a catalogue version passes before it goes live (#189, design in #173).
//!
//! 1. Size: the `.apworld` is within `apworld_max_bytes` (its type is checked by 2).
//! 2. It is inspected in the sandbox as an upload is, under today's limits.
//! 4. The code scan (`ap_gen.py scan-apworld`) lists code a person should look at. A flag never
//!    fails a version: the list is saved with it for the admin who reviews it (#192).
//! 3. It generates `catalogue_check_seeds` test seeds, each with a random seed and a default
//!    player, within the generation limits. The sandbox has no network, so a world that needs
//!    it fails here.
//!
//! The scan runs before the seeds, so its flags are saved even for a version that fails.
//! A version passes all of them to be `live`; the first that fails makes it `failed`, with
//! which check and the error. The game's other versions are untouched, so its previous live
//! version stays live. Each test seed waits its turn for a generation slot in the same line as
//! players' worlds, and at most `catalogue_check_concurrency` test seeds run or wait at once,
//! so a player is never behind more than that many.
//!
//! A check runs in this process: a restart ends it with the version still `checking`, and the
//! next start runs its checks again from the beginning ([`resume`]).

use crate::sandbox::Failure;
use crate::worlds::Worlds;
use log::{GameVersion, StoreError, UploadKind, VersionState};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::Semaphore;

/// One step of a check that runs in the sandbox, on the version's `.apworld`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// `inspect-apworld`: its record (`module`, `game`, ...).
    Inspect,
    /// `scan-apworld`: the flags, a JSON array.
    Scan,
    /// `check`: one test seed of the world in `module`.
    Seed { module: String, seed: u32 },
}

/// A test's stand-in for the sandbox: what a step prints, or how it fails.
#[cfg(test)]
pub(crate) type FakeRunner = Arc<dyn Fn(&Step, &[u8]) -> Result<Vec<u8>, Failure> + Send + Sync>;

/// The checks running in this process.
pub(crate) struct Checks {
    /// Test seeds generating or waiting to (`catalogue_check_concurrency`).
    seeds: Semaphore,
    /// The versions being checked, by (game key, version): one check each at a time.
    running: Mutex<BTreeSet<(String, String)>>,
    /// Tests run the steps here instead of in the sandbox.
    #[cfg(test)]
    pub(crate) fake: Option<FakeRunner>,
}

impl Checks {
    pub(crate) fn new(limits: &config::Limits) -> Self {
        Self {
            seeds: Semaphore::new(limits.catalogue_check_concurrency.max(1)),
            running: Mutex::default(),
            #[cfg(test)]
            fake: None,
        }
    }

    async fn step(&self, w: &Worlds, step: &Step, apworld: &[u8]) -> Result<Vec<u8>, Failure> {
        #[cfg(test)]
        if let Some(fake) = &self.fake {
            return fake(step, apworld);
        }
        w.sandboxed_check(step, apworld).await
    }
}

impl core::fmt::Debug for Checks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Checks").finish_non_exhaustive()
    }
}

/// Takes a version off the running list when its check ends, however it ends.
struct Running<'a>(&'a Checks, (String, String));

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.1);
    }
}

/// Check `game`'s `version` in the background, unless it is being checked already.
pub(crate) fn start(w: &Arc<Worlds>, game: &str, version: &str) {
    let key = (log::game_key(game), version.to_owned());
    let fresh =
        w.checks().running.lock().unwrap_or_else(PoisonError::into_inner).insert(key.clone());
    if !fresh {
        return;
    }
    let (w, game, version) = (Arc::clone(w), game.to_owned(), version.to_owned());
    tokio::spawn(async move {
        let _running = Running(w.checks(), key);
        if let Err(e) = run(&w, &game, &version).await {
            // Left `checking`: the next start runs it again.
            tracing::error!(game, version, error = ?e, "catalogue check stopped: store failed");
        }
    });
}

/// At startup: check again every version a stopped process left `checking`.
pub(crate) async fn resume(w: Arc<Worlds>) {
    match w.db().versions_checking().await {
        Ok(left) => {
            for v in left {
                tracing::info!(game = v.game, version = v.version, "catalogue check resumed");
                start(&w, &v.game, &v.version);
            }
        }
        Err(e) => tracing::error!(error = ?e, "catalogue checks not resumed: store failed"),
    }
}

/// The version, while it is still `checking`.
async fn still_checking(
    w: &Worlds,
    game: &str,
    version: &str,
) -> Result<Option<GameVersion>, StoreError> {
    let all = w.db().versions(game).await?;
    Ok(all.into_iter().find(|v| v.version == version && v.state == VersionState::Checking))
}

async fn run(w: &Worlds, game: &str, version: &str) -> Result<(), StoreError> {
    let Some(v) = still_checking(w, game, version).await? else { return Ok(()) };
    let db = w.db();
    let Some(generator) = w.generator() else {
        let why = "the server's generator version is unknown (its Utils.py has no __version__)";
        db.version_set(game, version, VersionState::Failed, "", Some(why)).await?;
        tracing::warn!(game, version, error = why, "catalogue check failed");
        return Ok(());
    };
    // Recorded first: the version is checked with this generator.
    db.version_set(game, version, VersionState::Checking, generator, None).await?;
    tracing::info!(game, version, generator, "catalogue check running");
    match checks(w, &v).await? {
        Outcome::Passed => {
            db.version_set(game, version, VersionState::Live, generator, None).await?;
            tracing::info!(game, version, generator, "catalogue check passed: live");
        }
        Outcome::Failed(why) => {
            db.version_set(game, version, VersionState::Failed, generator, Some(&why)).await?;
            tracing::warn!(game, version, error = why, "catalogue check failed");
        }
        Outcome::Stopped => {
            tracing::info!(game, version, "catalogue check stopped: no longer checking")
        }
    }
    Ok(())
}

/// How a version's checks ended.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Every check passed.
    Passed,
    /// A check failed: which, and why.
    Failed(String),
    /// The version left `checking` while they ran (withdrawn, or another process ended its
    /// check): nothing is recorded.
    Stopped,
}

/// Run the checks in order, stopping at the first that fails.
async fn checks(w: &Worlds, v: &GameVersion) -> Result<Outcome, StoreError> {
    let checks = w.checks();
    let l = w.limits();
    let apworld = match w.db().upload_bytes(UploadKind::Apworld, &v.apworld).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Ok(Outcome::Failed(
                "check 1 (size): the .apworld upload is missing".to_owned(),
            ));
        }
        // Stored bytes that no longer hash to their id: not the file the owner posted.
        Err(StoreError::Corrupt(why)) => {
            return Ok(Outcome::Failed(format!(
                "check 1 (size): the stored .apworld is damaged: {why}"
            )));
        }
        Err(e) => return Err(e),
    };
    if apworld.len() > l.apworld_max_bytes {
        let cap = l.apworld_max_bytes;
        return Ok(Outcome::Failed(format!(
            "check 1 (size): the .apworld is larger than {cap} bytes"
        )));
    }
    let record = match checks.step(w, &Step::Inspect, &apworld).await {
        Ok(out) => serde_json::from_slice::<Value>(&out).ok(),
        Err(e) => return Ok(Outcome::Failed(format!("check 2 (inspect): {e}"))),
    };
    let module = record.as_ref().and_then(|r| r["module"].as_str()).map(str::to_owned);
    let Some(module) = module.filter(|m| crate::worlds::valid_module(m)) else {
        return Ok(Outcome::Failed(
            "check 2 (inspect): the .apworld has no valid module name".to_owned(),
        ));
    };
    // A flag never fails a version; a scan that cannot run is itself a flag.
    let scan = match checks.step(w, &Step::Scan, &apworld).await {
        Ok(out) if serde_json::from_slice::<Vec<Value>>(&out).is_ok() => {
            String::from_utf8(out).unwrap_or_else(|_| "[]".to_owned())
        }
        Ok(_) => unscanned("the scan printed something that is not a list"),
        Err(e) => unscanned(&e.to_string()),
    };
    w.db().version_scan_set(&v.game, &v.version, &scan).await?;
    let total = l.catalogue_check_seeds;
    for n in 1..=total {
        if still_checking(w, &v.game, &v.version).await?.is_none() {
            return Ok(Outcome::Stopped);
        }
        let seed = random_seed();
        let step = Step::Seed { module: module.clone(), seed };
        let Ok(_seed_slot) = checks.seeds.acquire().await else { return Ok(Outcome::Stopped) };
        let Some(_turn) = w.generation_turn().await else { return Ok(Outcome::Stopped) };
        if let Err(e) = checks.step(w, &step, &apworld).await {
            let why = format!("check 3 (test seed {n} of {total}, seed {seed}): {e}");
            return Ok(Outcome::Failed(why));
        }
    }
    // Stopped meanwhile (the last seed ran after the last look): recorded as nothing.
    if still_checking(w, &v.game, &v.version).await?.is_none() {
        return Ok(Outcome::Stopped);
    }
    Ok(Outcome::Passed)
}

/// The scan's result when it could not run: one flag that says why.
fn unscanned(why: &str) -> String {
    let reason = format!("the scan could not run: {why}");
    let flag = serde_json::json!({"file": "", "line": 0, "reason": reason});
    Value::Array(vec![flag]).to_string()
}

/// A test seed: random, and small enough to type when reproducing a failure.
fn random_seed() -> u32 {
    let mut b = [0_u8; 4];
    // No random source: a fixed seed still tests the world, just not a new shuffle.
    if getrandom::fill(&mut b).is_err() {
        tracing::warn!("no random source for a test seed");
    }
    u32::from_le_bytes(b) >> 1
}
