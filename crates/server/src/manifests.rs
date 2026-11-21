//! Uploaded data-only world manifests: stored in the database once per content hash (never as
//! files, so every server instance sees them), with the listing columns beside them so a listing
//! never reads a manifest, and a size-bounded in-memory cache in front of the bytes a generation
//! reads.

use log::{
    BlobKey, JobState, JobStore, ManifestRecord, ManifestStore, MemLogStore, PgLogStore,
    StoreError, UploadKind, UploadRecord, UploadStore,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Where the manifests, the uploaded world and player files and the generations' statuses live:
/// the host's store.
#[derive(Debug, Clone)]
pub(crate) enum Db {
    Pg(Arc<PgLogStore>),
    Mem(Arc<MemLogStore>),
}

impl Db {
    /// Postgres: what is stored here outlives the process.
    pub(crate) const fn is_durable(&self) -> bool {
        matches!(self, Self::Pg(_))
    }

    pub(crate) async fn upload_put(
        &self,
        r: &UploadRecord,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        match self {
            Self::Pg(s) => s.upload_put(r, bytes).await,
            Self::Mem(s) => s.upload_put(r, bytes).await,
        }
    }

    pub(crate) async fn upload_meta(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<Option<String>, StoreError> {
        match self {
            Self::Pg(s) => s.upload_meta(kind, id).await,
            Self::Mem(s) => s.upload_meta(kind, id).await,
        }
    }

    pub(crate) async fn upload_bytes(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        match self {
            Self::Pg(s) => s.upload_bytes(kind, id).await,
            Self::Mem(s) => s.upload_bytes(kind, id).await,
        }
    }

    pub(crate) async fn upload_list(&self, kind: UploadKind) -> Result<Vec<String>, StoreError> {
        match self {
            Self::Pg(s) => s.upload_list(kind).await,
            Self::Mem(s) => s.upload_list(kind).await,
        }
    }

    pub(crate) async fn job_put(
        &self,
        id: &str,
        state: JobState,
        status: &str,
    ) -> Result<(), StoreError> {
        match self {
            Self::Pg(s) => s.job_put(id, state, status).await,
            Self::Mem(s) => s.job_put(id, state, status).await,
        }
    }

    pub(crate) async fn job_status(
        &self,
        id: &str,
    ) -> Result<Option<(String, Duration)>, StoreError> {
        match self {
            Self::Pg(s) => s.job_status(id).await,
            Self::Mem(s) => s.job_status(id).await,
        }
    }

    pub(crate) async fn job_touch(&self, id: &str) -> Result<(), StoreError> {
        match self {
            Self::Pg(s) => s.job_touch(id).await,
            Self::Mem(s) => s.job_touch(id).await,
        }
    }

    pub(crate) async fn job_stale(
        &self,
        older: Duration,
    ) -> Result<Vec<(String, String)>, StoreError> {
        match self {
            Self::Pg(s) => s.job_stale(older).await,
            Self::Mem(s) => s.job_stale(older).await,
        }
    }

    pub(crate) async fn job_fail_stale(
        &self,
        id: &str,
        status: &str,
        older: Duration,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Pg(s) => s.job_fail_stale(id, status, older).await,
            Self::Mem(s) => s.job_fail_stale(id, status, older).await,
        }
    }

    async fn put(&self, r: &ManifestRecord, bytes: &[u8]) -> Result<(), StoreError> {
        match self {
            Self::Pg(s) => s.manifest_put(r, bytes).await,
            Self::Mem(s) => s.manifest_put(r, bytes).await,
        }
    }

    async fn record(&self, id: &BlobKey) -> Result<Option<ManifestRecord>, StoreError> {
        match self {
            Self::Pg(s) => s.manifest_record(id).await,
            Self::Mem(s) => s.manifest_record(id).await,
        }
    }

    async fn bytes(&self, id: &BlobKey) -> Result<Option<Vec<u8>>, StoreError> {
        match self {
            Self::Pg(s) => s.manifest_bytes(id).await,
            Self::Mem(s) => s.manifest_bytes(id).await,
        }
    }

    async fn list(
        &self,
        game: Option<&str>,
        after: Option<&BlobKey>,
        limit: usize,
    ) -> Result<Vec<ManifestRecord>, StoreError> {
        match self {
            Self::Pg(s) => s.manifest_list(game, after, limit).await,
            Self::Mem(s) => s.manifest_list(game, after, limit).await,
        }
    }
}

/// Manifest bytes held in memory, at most `cap` in all: the least recently used go first. An
/// entry larger than the cap is never kept.
#[derive(Debug)]
struct Cache {
    cap: usize,
    used: usize,
    tick: u64,
    /// id to (bytes, last use).
    held: HashMap<BlobKey, (Arc<Vec<u8>>, u64)>,
    /// last use to id: the first entry is the least recently used.
    order: BTreeMap<u64, BlobKey>,
}

impl Cache {
    fn new(cap: usize) -> Self {
        Self { cap, used: 0, tick: 0, held: HashMap::new(), order: BTreeMap::new() }
    }

    fn get(&mut self, id: &BlobKey) -> Option<Arc<Vec<u8>>> {
        self.tick += 1;
        let (bytes, used) = self.held.get_mut(id)?;
        self.order.remove(used);
        *used = self.tick;
        self.order.insert(self.tick, *id);
        Some(Arc::clone(bytes))
    }

    fn put(&mut self, id: BlobKey, bytes: Arc<Vec<u8>>) {
        if bytes.len() > self.cap || self.held.contains_key(&id) {
            return;
        }
        while self.used + bytes.len() > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((b, _)) = self.held.remove(&oldest) {
                self.used -= b.len();
            }
        }
        self.tick += 1;
        self.used += bytes.len();
        self.order.insert(self.tick, id);
        self.held.insert(id, (bytes, self.tick));
    }
}

/// The manifest store the admin API uses.
#[derive(Debug)]
pub(crate) struct Manifests {
    db: Db,
    cache: Mutex<Cache>,
    page_max: usize,
}

/// The id as the 32 bytes it is the hex of. `id` must be 64 lowercase hex digits.
pub(crate) fn key_of(id: &str) -> Option<BlobKey> {
    let mut key = [0_u8; 32];
    if id.len() != 64 {
        return None;
    }
    for (k, pair) in key.iter_mut().zip(id.as_bytes().chunks(2)) {
        let digit = |b: u8| char::from(b).to_digit(16);
        let (hi, lo) = (digit(*pair.first()?)?, digit(*pair.get(1)?)?);
        *k = u8::try_from(hi * 16 + lo).ok()?;
    }
    (id == crate::hex(&key)).then_some(key)
}

/// A record as the API shows it.
fn shown(r: &ManifestRecord) -> Value {
    json!({
        "id": crate::hex(&r.id), "name": r.name, "game": r.game, "version": r.version,
        "bytes": r.bytes, "items": r.items, "locations": r.locations,
    })
}

impl Manifests {
    pub(crate) fn new(db: Db, limits: &config::Limits) -> Self {
        Self {
            db,
            cache: Mutex::new(Cache::new(limits.manifest_cache_bytes)),
            page_max: limits.list_page_max,
        }
    }

    /// The record of manifest `id`, if stored.
    pub(crate) async fn record(&self, id: &str) -> Result<Option<Value>, StoreError> {
        let Some(key) = key_of(id) else { return Ok(None) };
        Ok(self.db.record(&key).await?.as_ref().map(shown))
    }

    /// The manifest's bytes: from the cache, else from the database (and then cached).
    pub(crate) async fn bytes(&self, id: &str) -> Result<Option<Arc<Vec<u8>>>, StoreError> {
        let Some(key) = key_of(id) else { return Ok(None) };
        let hit = self.cache.lock().unwrap_or_else(PoisonError::into_inner).get(&key);
        if hit.is_some() {
            return Ok(hit);
        }
        let Some(bytes) = self.db.bytes(&key).await? else { return Ok(None) };
        let bytes = Arc::new(bytes);
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).put(key, Arc::clone(&bytes));
        Ok(Some(bytes))
    }

    /// Store a checked manifest (`found` is what the check printed: game, version, items,
    /// locations) and return its record.
    pub(crate) async fn put(
        &self,
        name: &str,
        bytes: &[u8],
        found: &Value,
    ) -> Result<Value, StoreError> {
        let count = |k: &str| u32::try_from(found[k].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
        let text = |k: &str| found[k].as_str().unwrap_or("").to_owned();
        let record = ManifestRecord {
            id: log::blob_key(bytes),
            name: name.to_owned(),
            game: text("game"),
            version: text("version"),
            bytes: bytes.len() as u64,
            items: count("items"),
            locations: count("locations"),
        };
        self.db.put(&record, bytes).await?;
        // The stored record: a manifest uploaded before keeps its first name.
        let stored = self.db.record(&record.id).await?.unwrap_or(record);
        Ok(shown(&stored))
    }

    /// A page of records ascending by id, after cursor `after` (an id), of `game` when given:
    /// at most `list_page_max`, and the cursor of the next page when there may be one.
    ///
    /// # Errors
    /// The store failing; a cursor that is not an id is an empty page.
    pub(crate) async fn list(
        &self,
        game: Option<&str>,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<(Vec<Value>, Option<String>), StoreError> {
        let limit = limit.unwrap_or(self.page_max).clamp(1, self.page_max);
        let after = match after {
            Some(a) => Some(key_of(a).ok_or_else(|| StoreError::Corrupt("bad cursor".into()))?),
            None => None,
        };
        let page = self.db.list(game, after.as_ref(), limit).await?;
        let next = (page.len() == limit).then(|| page.last().map(|r| crate::hex(&r.id))).flatten();
        Ok((page.iter().map(shown).collect(), next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> BlobKey {
        [n; 32]
    }

    #[test]
    fn the_cache_stays_within_its_bytes_and_drops_the_least_recently_used() {
        let mut c = Cache::new(10);
        c.put(id(1), Arc::new(vec![0; 4]));
        c.put(id(2), Arc::new(vec![0; 4]));
        assert!(c.get(&id(1)).is_some(), "touched: now id 2 is the oldest");
        c.put(id(3), Arc::new(vec![0; 4]));
        assert!(c.get(&id(2)).is_none() && c.get(&id(1)).is_some() && c.get(&id(3)).is_some());
        assert_eq!(c.used, 8);
        c.put(id(4), Arc::new(vec![0; 11]));
        assert!(c.get(&id(4)).is_none(), "larger than the whole cache: never kept");
        assert_eq!(c.used, 8, "and it evicted nothing");
        c.put(id(5), Arc::new(vec![0; 10]));
        assert_eq!((c.held.len(), c.used), (1, 10), "one big entry pushes the rest out");
    }

    #[test]
    fn an_id_is_64_lowercase_hex_digits() {
        let k = id(0xab);
        assert_eq!(key_of(&crate::hex(&k)), Some(k));
        for bad in ["", "ab", &"AB".repeat(32), &"zz".repeat(32), &"ab".repeat(33)] {
            assert_eq!(key_of(bad), None, "{bad}");
        }
    }

    fn manifests(limits: &config::Limits) -> Manifests {
        Manifests::new(Db::Mem(Arc::new(MemLogStore::new())), limits)
    }

    #[tokio::test]
    async fn a_manifest_is_stored_once_read_through_the_cache_and_listed_in_pages() {
        let limits = config::Limits { list_page_max: 2, ..config::Limits::DESIGN };
        let m = manifests(&limits);
        let found = |game: &str| json!({"game": game, "version": "1", "items": 2, "locations": 3});
        let mut ids = Vec::new();
        for i in 0..5 {
            let body = format!("manifest {i}");
            let game = if i < 3 { "Fixture" } else { "Other" };
            let r = m.put(&format!("m{i}.json"), body.as_bytes(), &found(game)).await.unwrap();
            assert_eq!((&r["game"], &r["bytes"]), (&json!(game), &json!(body.len())));
            ids.push(r["id"].as_str().unwrap().to_owned());
        }
        let again = m.put("renamed.json", b"manifest 0", &found("Fixture")).await.unwrap();
        assert_eq!(again["name"], "m0.json", "the first upload's record stays");
        assert_eq!(m.record(&ids[1]).await.unwrap().unwrap()["name"], "m1.json");
        assert_eq!(m.record(&"0".repeat(64)).await.unwrap(), None);
        assert_eq!(m.record("../x").await.unwrap(), None);
        assert_eq!(m.bytes(&ids[1]).await.unwrap().as_deref(), Some(&b"manifest 1".to_vec()));
        assert_eq!(m.cache.lock().unwrap().held.len(), 1, "the read filled the cache");

        // Pages are capped at the limit whatever is asked, and walk every record once.
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let (page, next) = m.list(None, after.as_deref(), Some(100)).await.unwrap();
            assert!(page.len() <= 2);
            seen.extend(page.iter().map(|r| r["id"].as_str().unwrap().to_owned()));
            match next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        let mut want = ids.clone();
        want.sort();
        assert_eq!(seen, want);
        let (only, _) = m.list(Some("Other"), None, None).await.unwrap();
        assert_eq!(only.len(), 2);
        assert!(m.list(None, Some("nope"), None).await.is_err());
    }
}
