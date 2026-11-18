//! Game data packages, stored once per content hash and shared by every room in the process.
//!
//! A package is one game's names and ids (`GameData`) plus the game's name. Its content is
//! the canonical JSON of `{"data": <GameData>, "game": <name>}` (keys sorted, no whitespace),
//! its key the SHA-256 of that content. Its file `<hex key>.pkg` in a seed's `packages` folder
//! is sectioned so other per-game data can join it later: a table of contents, then each
//! section (layout in `docs/seed-bundle-v2.md`). Section [`SECTION_GAME`] holds the content as
//! raw deflate (RFC 1951); [`SECTION_RULES`] is reserved for a rules and regions section.
//! In memory a package keeps only what rooms use: the sorted id tables that map ids to dense
//! indexes, and the JSON clients are sent, serialized once.

use crate::{BundleError, GameData, id_ok};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// A package's content address: SHA-256 of its content.
pub type Key = [u8; 32];

/// The first four bytes of a package file.
pub const MAGIC: [u8; 4] = *b"CSPK";
/// The package file version this crate writes and reads.
pub const VERSION: u16 = 1;
/// Section: the game's names and ids (the content the key hashes).
pub const SECTION_GAME: u16 = 1;
/// Section, reserved: the game's rules and regions (not written yet).
pub const SECTION_RULES: u16 = 2;
/// Bytes of one table of contents entry.
const ENTRY: usize = 48;
/// Section flag: the bytes are raw deflate.
const DEFLATED: u16 = 1;

/// One section of a package file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// What it holds ([`SECTION_GAME`], [`SECTION_RULES`], ...).
    pub id: u16,
    /// Its bytes, inflated.
    pub bytes: Vec<u8>,
}

/// A package file of these sections (each deflated, with the SHA-256 of its inflated bytes in
/// the table of contents).
///
/// # Errors
/// Never in practice (writes go to memory); a section over 4 GiB.
pub fn file_bytes(sections: &[Section]) -> std::io::Result<Vec<u8>> {
    let big = || std::io::Error::other("a package section is too large");
    let stored: Vec<Vec<u8>> =
        sections.iter().map(|s| deflate(&s.bytes)).collect::<Result<_, _>>()?;
    let count = u16::try_from(sections.len()).map_err(|_| big())?;
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    let mut at = 8 + ENTRY * sections.len();
    for (s, b) in sections.iter().zip(&stored) {
        out.extend_from_slice(&s.id.to_le_bytes());
        out.extend_from_slice(&DEFLATED.to_le_bytes());
        for n in [at, b.len(), s.bytes.len()] {
            out.extend_from_slice(&u32::try_from(n).map_err(|_| big())?.to_le_bytes());
        }
        out.extend_from_slice(&Sha256::digest(&s.bytes));
        at += b.len();
    }
    for b in stored {
        out.extend_from_slice(&b);
    }
    Ok(out)
}

/// Every section of a package file, each checked against its hash. Sections this crate does
/// not know are returned too; a reader skips what it does not use.
///
/// # Errors
/// A malformed file, a section over `max` bytes, or one that does not match its hash.
pub fn sections(file: &[u8], max: u64) -> Result<Vec<Section>, BundleError> {
    let bad = |why: &str| BundleError::Invalid(format!("package file: {why}"));
    let u16_at = |at: usize| file.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| {
        file.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    if !file.starts_with(&MAGIC) {
        return Err(bad("not a package file"));
    }
    if u16_at(4) != Some(VERSION) {
        return Err(bad("unknown version"));
    }
    let count = usize::from(u16_at(6).ok_or_else(|| bad("cut short"))?);
    (0..count)
        .map(|k| {
            let e = 8 + ENTRY * k;
            let (id, flags) = (u16_at(e), u16_at(e + 2));
            let (at, len, raw) = (u32_at(e + 4), u32_at(e + 8), u32_at(e + 12));
            let hash = file.get(e + 16..e + ENTRY);
            let (Some(id), Some(flags), Some(at), Some(len), Some(raw), Some(hash)) =
                (id, flags, at, len, raw, hash)
            else {
                return Err(bad("table of contents cut short"));
            };
            let stored = at.checked_add(len).and_then(|end| file.get(at..end));
            let stored = stored.ok_or_else(|| bad("section past the end"))?;
            let cap = max.min(raw as u64);
            let bytes = if flags & DEFLATED != 0 { inflate(stored, cap)? } else { stored.to_vec() };
            if bytes.len() != raw || Sha256::digest(&bytes)[..] != *hash {
                return Err(bad("a section does not match its hash"));
            }
            Ok(Section { id, bytes })
        })
        .collect()
}

/// One game's package as rooms use it.
#[derive(Debug, PartialEq, Eq)]
pub struct GamePackage {
    key: Key,
    name: Box<str>,
    checksum: Box<str>,
    /// Every location id, ascending; a location's index is its position.
    locations: Box<[i64]>,
    /// Every item id, ascending.
    items: Box<[i64]>,
    /// The package clients get in `DataPackage`: names to ids and the checksum, as JSON.
    data_package: Box<str>,
    item_groups: Box<str>,
    location_groups: Box<str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    data: GameData,
    game: String,
}

/// The canonical JSON of `v`: object keys sorted, no whitespace (the form `bundle_hash`
/// hashes).
#[must_use]
pub fn canonical(mut v: serde_json::Value) -> Vec<u8> {
    v.sort_all_objects();
    v.to_string().into_bytes()
}

fn sorted(ids: impl Iterator<Item = i64>) -> Box<[i64]> {
    let mut v: Vec<i64> = ids.collect();
    v.sort_unstable();
    v.dedup();
    v.into_boxed_slice()
}

impl GamePackage {
    /// The content of the package for game `name`.
    #[must_use]
    pub fn content(name: &str, g: &GameData) -> Vec<u8> {
        canonical(serde_json::json!({"data": g, "game": name}))
    }

    /// The key of `content`.
    #[must_use]
    pub fn key_of(content: &[u8]) -> Key {
        Sha256::digest(content).into()
    }

    /// The package of game `name`, keyed by `key` (the key of its content).
    #[must_use]
    pub fn new(key: Key, name: &str, g: &GameData) -> Self {
        // Name groups are not part of the package clients get; they read them through `Get`
        // (reference server: the reduced package, `MultiServer.py:624-641`).
        let reduced = serde_json::json!({
            "item_name_to_id": g.item_name_to_id,
            "location_name_to_id": g.location_name_to_id,
            "checksum": g.checksum,
        });
        Self {
            key,
            name: name.into(),
            checksum: g.checksum.as_str().into(),
            locations: sorted(g.location_name_to_id.values().copied()),
            items: sorted(g.item_name_to_id.values().copied()),
            data_package: reduced.to_string().into(),
            item_groups: serde_json::json!(g.item_name_groups).to_string().into(),
            location_groups: serde_json::json!(g.location_name_groups).to_string().into(),
        }
    }

    /// Parse and check package content: the game's name and data.
    ///
    /// # Errors
    /// When it is not a package or an id is out of range.
    pub fn parse(content: &[u8]) -> Result<(String, GameData), BundleError> {
        let f: File =
            serde_json::from_slice(content).map_err(|e| BundleError::Parse(e.to_string()))?;
        let ids = f.data.item_name_to_id.values().chain(f.data.location_name_to_id.values());
        if !ids.copied().all(id_ok) {
            return Err(BundleError::Invalid(format!(
                "package {:?} has an id outside +-(2^53 - 1)",
                f.game
            )));
        }
        Ok((f.game, f.data))
    }

    /// The content address.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// The game's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The generator's checksum of this package.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    /// Every location id, ascending.
    #[must_use]
    pub fn locations(&self) -> &[i64] {
        &self.locations
    }

    /// Every item id, ascending.
    #[must_use]
    pub fn items(&self) -> &[i64] {
        &self.items
    }

    /// The index of location id `id`.
    #[must_use]
    pub fn location_index(&self, id: i64) -> Option<usize> {
        self.locations.binary_search(&id).ok()
    }

    /// The index of item id `id`.
    #[must_use]
    pub fn item_index(&self, id: i64) -> Option<usize> {
        self.items.binary_search(&id).ok()
    }

    /// The JSON object clients get for this game in `DataPackage`.
    #[must_use]
    pub fn data_package(&self) -> &str {
        &self.data_package
    }

    /// Item group name to item names, as JSON.
    #[must_use]
    pub fn item_groups(&self) -> &str {
        &self.item_groups
    }

    /// Location group name to location names, as JSON.
    #[must_use]
    pub fn location_groups(&self) -> &str {
        &self.location_groups
    }
}

/// The file of package `key` in folder `dir`.
#[must_use]
pub fn file(dir: &Path, key: &Key) -> PathBuf {
    use core::fmt::Write as _;
    let hex = key.iter().fold(String::with_capacity(64), |mut h, b| {
        // Writing to a String cannot fail.
        let _ = write!(h, "{b:02x}");
        h
    });
    dir.join(format!("{hex}.pkg"))
}

/// Raw deflate of `bytes`.
///
/// # Errors
/// Never in practice (writes go to memory).
pub fn deflate(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(bytes)?;
    e.finish()
}

/// Inflate raw deflate `bytes`, refusing output over `max` bytes (a deflate bomb).
///
/// # Errors
/// Bad deflate data, or output over `max`.
pub fn inflate(bytes: &[u8], max: u64) -> Result<Vec<u8>, BundleError> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(bytes)
        .take(max.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| BundleError::Parse(format!("deflate: {e}")))?;
    if u64::try_from(out.len()).unwrap_or(u64::MAX) > max {
        return Err(BundleError::Invalid(format!("inflates to more than {max} bytes")));
    }
    Ok(out)
}

/// Write the package with this content (its [`SECTION_GAME`]) into `dir` (created if missing)
/// unless it is there. Atomic: a reader never sees half a file.
///
/// # Errors
/// When the folder or file cannot be written.
pub fn write(dir: &Path, content: &[u8]) -> std::io::Result<Key> {
    let key = GamePackage::key_of(content);
    let path = file(dir, &key);
    if !path.exists() {
        std::fs::create_dir_all(dir)?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let bytes = file_bytes(&[Section { id: SECTION_GAME, bytes: content.to_vec() }])?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(key)
}

/// Read package `key` from `dir`: its [`SECTION_GAME`], checked to be the content the key
/// names.
///
/// # Errors
/// A missing or unreadable file, a malformed one, more than `max` bytes, or content that does
/// not hash to `key`.
pub fn read(dir: &Path, key: &Key, max: u64) -> Result<Vec<u8>, BundleError> {
    let path = file(dir, key);
    let bytes =
        std::fs::read(&path).map_err(|e| BundleError::Parse(format!("{}: {e}", path.display())))?;
    let game = sections(&bytes, max)?.into_iter().find(|s| s.id == SECTION_GAME);
    let content = game
        .ok_or_else(|| BundleError::Invalid(format!("{}: no game section", path.display())))?
        .bytes;
    if GamePackage::key_of(&content) != *key {
        return Err(BundleError::Invalid(format!("{} does not match its key", path.display())));
    }
    Ok(content)
}

/// Packages in use in this process, by key. Entries are weak: a package is dropped once no
/// room holds it, and loaded again on next use.
static SHARED: Mutex<BTreeMap<Key, Weak<GamePackage>>> = Mutex::new(BTreeMap::new());

/// The process's copy of package `key`, made by `load` when no room holds one.
///
/// # Errors
/// What `load` returns.
pub fn shared(
    key: &Key,
    load: impl FnOnce() -> Result<GamePackage, BundleError>,
) -> Result<Arc<GamePackage>, BundleError> {
    let lock = || SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(p) = lock().get(key).and_then(Weak::upgrade) {
        return Ok(p);
    }
    // Loaded outside the lock: another room may race to load the same package, and the first
    // to insert wins; both are the same content.
    let p = Arc::new(load()?);
    if p.key() != key {
        return Err(BundleError::Invalid("a package does not match its key".to_owned()));
    }
    let mut map = lock();
    map.retain(|_, w| w.strong_count() > 0);
    if let Some(existing) = map.get(key).and_then(Weak::upgrade) {
        return Ok(existing);
    }
    map.insert(*key, Arc::downgrade(&p));
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> GameData {
        GameData {
            item_name_to_id: BTreeMap::from([("b".into(), 9), ("a".into(), -1)]),
            location_name_to_id: BTreeMap::from([("x".into(), 5), ("y".into(), 2)]),
            checksum: "c0ffee".into(),
            item_name_groups: BTreeMap::from([("all".into(), vec!["a".into(), "b".into()])]),
            location_name_groups: BTreeMap::new(),
        }
    }

    #[test]
    fn ids_map_to_dense_indexes_in_id_order() {
        let g = game();
        let p = GamePackage::new([0; 32], "G", &g);
        assert_eq!(p.locations(), [2, 5]);
        assert_eq!(p.items(), [-1, 9]);
        assert_eq!((p.location_index(5), p.location_index(3)), (Some(1), None));
        assert_eq!(p.item_index(-1), Some(0));
        // Exactly what the edge sent before packages were shared (sorted keys, compact).
        assert_eq!(
            p.data_package(),
            r#"{"checksum":"c0ffee","item_name_to_id":{"a":-1,"b":9},"location_name_to_id":{"x":5,"y":2}}"#
        );
        assert_eq!(p.item_groups(), r#"{"all":["a","b"]}"#);
    }

    #[test]
    fn a_written_package_reads_back_and_a_tampered_one_does_not() {
        let dir = std::env::temp_dir().join(format!("pkg-test-{}", std::process::id()));
        let content = GamePackage::content("G", &game());
        let key = write(&dir, &content).unwrap();
        assert_eq!(key, GamePackage::key_of(&content));
        assert_eq!(write(&dir, &content).unwrap(), key, "a second write is a no-op");
        assert_eq!(read(&dir, &key, 1 << 20).unwrap(), content);
        assert_eq!(GamePackage::parse(&content).unwrap(), ("G".to_owned(), game()));
        // Over the cap: refused, not inflated without bound.
        assert!(read(&dir, &key, 10).is_err());
        let other = file_bytes(&[Section { id: SECTION_GAME, bytes: b"{}".to_vec() }]).unwrap();
        std::fs::write(file(&dir, &key), other).unwrap();
        assert!(read(&dir, &key, 1 << 20).is_err(), "content that is not the key's");
        assert!(read(&dir, &[1; 32], 1 << 20).is_err(), "missing");
        std::fs::write(file(&dir, &key), b"not deflate").unwrap();
        assert!(read(&dir, &key, 1 << 20).is_err());
    }

    #[test]
    fn sections_round_trip_and_unknown_or_broken_ones_are_handled() {
        let all = [
            Section { id: SECTION_GAME, bytes: b"game".to_vec() },
            Section { id: 99, bytes: vec![7; 300] },
        ];
        let f = file_bytes(&all).unwrap();
        assert_eq!(sections(&f, 1 << 20).unwrap(), all, "an unknown section is kept, not refused");
        assert!(sections(&f, 10).is_err(), "over the cap");
        for cut in [0, 4, 7, 20, f.len() - 1] {
            assert!(sections(&f[..cut], 1 << 20).is_err(), "cut at {cut}");
        }
        let mut wrong = f.clone();
        wrong[8 + 16] ^= 1; // the first section's hash
        assert!(sections(&wrong, 1 << 20).is_err());
        let mut v2 = f;
        v2[4] = 2;
        assert!(sections(&v2, 1 << 20).is_err());
    }

    #[test]
    fn a_package_is_shared_while_held_and_loaded_again_after() {
        let key = [7; 32];
        let make = || Ok(GamePackage::new(key, "Shared", &game()));
        let a = shared(&key, make).unwrap();
        let b = shared(&key, || panic!("must not load again while held")).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        drop((a, b));
        let mut loaded = false;
        shared(&key, || {
            loaded = true;
            make()
        })
        .unwrap();
        assert!(loaded);
        let wrong = shared(&[8; 32], make);
        assert!(wrong.is_err(), "a package under another key is refused");
    }

    #[test]
    fn an_out_of_range_id_in_a_package_is_refused() {
        let mut g = game();
        g.item_name_to_id.insert("big".into(), i64::MAX);
        assert!(GamePackage::parse(&GamePackage::content("G", &g)).is_err());
        assert!(GamePackage::parse(b"[]").is_err());
    }
}
