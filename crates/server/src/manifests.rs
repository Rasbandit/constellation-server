//! Uploaded data-only world manifests: stored in the database once per content hash (never as
//! files, so every server instance sees them), with the listing columns beside them so a listing
//! never reads a manifest, and each one's prepared world (`data_fill::prepared`) as a blob found
//! by manifest and engine (the file layout and the server build). A size-bounded in-memory cache is in front of the bytes a
//! generation reads.

use crate::cache::Cache;
use crate::store::Db;
use log::{BlobKey, ManifestRecord, StoreError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, PoisonError};

/// The manifest store the admin API uses.
#[derive(Debug)]
pub(crate) struct Manifests {
    db: Db,
    cache: Mutex<Cache<Vec<u8>>>,
    page_max: usize,
    /// The engine prepared worlds are stored and found under ([`engine_key`] of this build).
    pub(crate) engine: String,
}

/// A prepared world's file and the key it is stored under (the hash of the file), which names
/// the world to a fill worker that may already hold it loaded ([`crate::pool`]). Reads as the
/// file's bytes.
#[derive(Debug, Clone)]
pub(crate) struct PreparedWorld {
    pub(crate) key: BlobKey,
    pub(crate) file: Arc<Vec<u8>>,
}

impl core::ops::Deref for PreparedWorld {
    type Target = Vec<u8>;

    fn deref(&self) -> &Vec<u8> {
        &self.file
    }
}

/// What a prepared world is stored and found under beside its manifest: the layout of the file
/// (`data_fill::prepared::layout`) and the server build that made it (`build`,
/// [`crate::build_version`]). The compiled form is kept between generations, so a fix to the
/// compiler would otherwise never reach a stored world: with the build in the key, each deploy
/// prepares each world again on its first generation (a few milliseconds, once).
pub(crate) fn engine_key(build: &str) -> String {
    format!("{}@{build}", data_fill::prepared::layout())
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
            engine: engine_key(&crate::build_version()),
        }
    }

    /// The record of manifest `id`, if stored.
    pub(crate) async fn record(&self, id: &str) -> Result<Option<Value>, StoreError> {
        let Some(key) = key_of(id) else { return Ok(None) };
        Ok(self.db.manifest_record(&key).await?.as_ref().map(shown))
    }

    /// The manifest's bytes: from the cache, else from the database (and then cached).
    pub(crate) async fn bytes(&self, id: &str) -> Result<Option<Arc<Vec<u8>>>, StoreError> {
        let Some(key) = key_of(id) else { return Ok(None) };
        if let Some(hit) = self.cached(&key) {
            return Ok(Some(hit));
        }
        let Some(bytes) = self.db.manifest_bytes(&key).await? else { return Ok(None) };
        Ok(Some(self.cache(key, bytes)))
    }

    fn cached(&self, key: &BlobKey) -> Option<Arc<Vec<u8>>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).get(key)
    }

    fn cache(&self, key: BlobKey, bytes: Vec<u8>) -> Arc<Vec<u8>> {
        let bytes = Arc::new(bytes);
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).put(key, Arc::clone(&bytes));
        bytes
    }

    /// Manifest `id`'s prepared world, from the cache or the blob store. `None` when it must be
    /// prepared (again): never prepared by this engine, or its blob is missing or torn.
    pub(crate) async fn prepared(&self, id: &str) -> Result<Option<PreparedWorld>, StoreError> {
        let Some(key) = key_of(id) else { return Ok(None) };
        let Some(package) = self.db.prepared_key(&key, &self.engine).await? else {
            return Ok(None);
        };
        if let Some(file) = self.cached(&package) {
            return Ok(Some(PreparedWorld { key: package, file }));
        }
        match self.db.blob_get(&package).await {
            Ok(Some(file)) => {
                Ok(Some(PreparedWorld { key: package, file: self.cache(package, file) }))
            }
            Ok(None) | Err(StoreError::Corrupt(_)) => {
                tracing::warn!(id, "a prepared world's blob is missing or torn");
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Store manifest `id`'s prepared world `file` as a blob, found under this engine.
    pub(crate) async fn put_prepared(
        &self,
        id: &str,
        file: Vec<u8>,
    ) -> Result<PreparedWorld, StoreError> {
        let key = key_of(id).ok_or_else(|| StoreError::Corrupt("not a manifest id".into()))?;
        let package = log::blob_key(&file);
        self.db.blob_put(&package, &file).await?;
        self.db.prepared_put(&key, &self.engine, &package).await?;
        Ok(PreparedWorld { key: package, file: self.cache(package, file) })
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
        self.db.manifest_put(&record, bytes).await?;
        // The stored record: a manifest uploaded before keeps its first name.
        let stored = self.db.manifest_record(&record.id).await?.unwrap_or(record);
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
        let page = self.db.manifest_list(game, after.as_ref(), limit).await?;
        let next = (page.len() == limit).then(|| page.last().map(|r| crate::hex(&r.id))).flatten();
        Ok((page.iter().map(shown).collect(), next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::MemLogStore;

    fn id(n: u8) -> BlobKey {
        [n; 32]
    }

    #[test]
    fn an_id_is_64_lowercase_hex_digits() {
        let k = id(0xab);
        assert_eq!(key_of(&crate::hex(&k)), Some(k));
        for bad in ["", "ab", &"AB".repeat(32), &"zz".repeat(32), &"ab".repeat(33)] {
            assert_eq!(key_of(bad), None, "{bad}");
        }
    }

    /// Fails without the build in the key: worlds are stored under this build's version and
    /// the file layout, and another build or another layout is another key.
    #[test]
    fn prepared_worlds_are_keyed_by_the_file_layout_and_the_build() {
        let here = manifests(&config::Limits::DESIGN).engine;
        assert_eq!(here, engine_key(&crate::build_version()));
        assert!(here.starts_with(&format!("{}@", data_fill::prepared::layout())), "{here}");
        assert!(here.ends_with(env!("CARGO_PKG_VERSION")) || here.contains('+'), "{here}");
        assert_ne!(engine_key("1.0.0+aaaaaaa"), engine_key("1.0.0+bbbbbbb"));
        let layout = data_fill::prepared::layout();
        let parts: Vec<&str> = layout.split('.').collect();
        assert_eq!(parts.len(), 3, "rules section, package file, prepared layout: {layout}");
        assert_eq!(parts[1], seed_bundle::package::VERSION.to_string());
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
