//! Game data packages, stored once per content hash and shared by every room in the process.
//!
//! A package is one game's names and ids (`GameData`) plus the game's name, as a sectioned
//! file: a table of contents, then each section (layout in `docs/seed-bundle-v2.md`). Section
//! [`SECTION_GAME`] holds the canonical JSON of `{"data": <GameData>, "game": <name>}` as raw
//! deflate (RFC 1951); [`SECTION_RULES`] is reserved for a rules and regions section. The key
//! is the SHA-256 of the whole file, so a blob store can check every read against it. This
//! crate does no IO: the server keeps package files in its blob store (`log::BlobStore`).
//! In memory a package keeps only what rooms use: the sorted id tables that map ids to dense
//! indexes, names by index, and the JSON clients are sent, serialized once.

use crate::{BundleError, GameData, id_ok};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// A package's content address: the SHA-256 of its file ([`key_of`]).
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
    /// Stored as raw deflate (smaller); otherwise stored as is, so [`stored`] can read it in
    /// place.
    pub deflate: bool,
}

/// A package file of these sections, with the SHA-256 of each one's inflated bytes in the
/// table of contents.
///
/// # Errors
/// Never in practice (writes go to memory); a section over 4 GiB.
pub fn file_bytes(sections: &[Section]) -> std::io::Result<Vec<u8>> {
    let big = || std::io::Error::other("a package section is too large");
    let stored: Vec<Vec<u8>> = sections
        .iter()
        .map(|s| if s.deflate { deflate(&s.bytes) } else { Ok(s.bytes.clone()) })
        .collect::<Result<_, _>>()?;
    let count = u16::try_from(sections.len()).map_err(|_| big())?;
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    let mut at = 8 + ENTRY * sections.len();
    for (s, b) in sections.iter().zip(&stored) {
        out.extend_from_slice(&s.id.to_le_bytes());
        out.extend_from_slice(&(if s.deflate { DEFLATED } else { 0 }).to_le_bytes());
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

/// One table of contents entry, its stored bytes borrowed from the file.
struct Entry<'a> {
    id: u16,
    deflate: bool,
    stored: &'a [u8],
    raw: usize,
    hash: &'a [u8],
}

fn toc(file: &[u8]) -> Result<Vec<Entry<'_>>, BundleError> {
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
            Ok((at, Entry { id, deflate: flags & DEFLATED != 0, stored, raw, hash }))
        })
        .collect::<Result<Vec<_>, _>>()
        .and_then(|mut all| {
            // One entry per id, and no two over the same bytes: a small file must not stand
            // for many large sections.
            all.sort_by_key(|(at, _)| *at);
            let overlap = all.windows(2).any(|w| w[0].0 + w[0].1.stored.len() > w[1].0);
            let mut ids: Vec<u16> = all.iter().map(|(_, e)| e.id).collect();
            ids.sort_unstable();
            if overlap || ids.windows(2).any(|w| w[0] == w[1]) {
                return Err(bad("sections repeat an id or overlap"));
            }
            Ok(all.into_iter().map(|(_, e)| e).collect())
        })
}

/// Inflate (or copy) one entry, capped at `max`, and check its hash.
fn open_entry(e: &Entry<'_>, max: u64) -> Result<Vec<u8>, BundleError> {
    if e.raw as u64 > max {
        return Err(BundleError::Invalid(format!("package section over {max} bytes")));
    }
    let bytes = if e.deflate { inflate(e.stored, e.raw as u64)? } else { e.stored.to_vec() };
    checked(e, &bytes)?;
    Ok(bytes)
}

/// Section `id` of a package file, inflated and checked; no other section is inflated.
///
/// # Errors
/// A malformed file, a section over `max` bytes, or one that does not match its hash.
pub fn section_of(file: &[u8], id: u16, max: u64) -> Result<Option<Vec<u8>>, BundleError> {
    toc(file)?.iter().find(|e| e.id == id).map(|e| open_entry(e, max)).transpose()
}

fn checked(e: &Entry<'_>, bytes: &[u8]) -> Result<(), BundleError> {
    if bytes.len() != e.raw || Sha256::digest(bytes)[..] != *e.hash {
        return Err(BundleError::Invalid("package file: a section does not match its hash".into()));
    }
    Ok(())
}

/// Every section of a package file, each checked against its hash: for tools that copy or
/// rewrite a whole package. Readers that want one section use [`section_of`]. `max` caps all
/// of them together.
///
/// # Errors
/// A malformed file, sections over `max` bytes in all, or one that does not match its hash.
pub fn sections(file: &[u8], max: u64) -> Result<Vec<Section>, BundleError> {
    let entries = toc(file)?;
    let total = entries.iter().map(|e| e.raw as u64).fold(0, u64::saturating_add);
    if total > max {
        return Err(BundleError::Invalid(format!("package sections over {max} bytes")));
    }
    entries
        .iter()
        .map(|e| Ok(Section { id: e.id, bytes: open_entry(e, max)?, deflate: e.deflate }))
        .collect()
}

/// Section `id` of a package file, read in place (no copy), checked against its hash; `None`
/// when the file has no such section.
///
/// # Errors
/// A malformed file, a hash mismatch, or a section stored deflated (read it with
/// [`sections`] or [`section`], which inflate).
pub fn stored(file: &[u8], id: u16) -> Result<Option<&[u8]>, BundleError> {
    let Some(e) = toc(file)?.into_iter().find(|e| e.id == id) else { return Ok(None) };
    if e.deflate {
        return Err(BundleError::Invalid(format!("package section {id} is deflated")));
    }
    checked(&e, e.stored)?;
    Ok(Some(e.stored))
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
    /// Names by index and by name, and the name groups: what the host's room view shows and
    /// `!hint` looks up, once per process, not per room.
    location_names: Names,
    item_names: Names,
    /// The package clients get in `DataPackage`: names to ids and the checksum, as JSON.
    data_package: Box<str>,
    item_groups: Box<str>,
    location_groups: Box<str>,
    /// Names `!hint` refuses, sorted.
    hint_blacklist: Box<[Box<str>]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    data: GameData,
    game: String,
}

/// One kind of name of a game (items or locations): names by index, indexes in name order (a
/// binary search finds a name, 4 bytes a name), and the name groups as index lists in name
/// order. Built once per package, so a lookup never scans every name.
#[derive(Debug, PartialEq, Eq)]
pub struct Names {
    by_index: Box<[Box<str>]>,
    by_name: Box<[u32]>,
    groups: Box<[Group]>,
}

/// A name group: its name and its member indexes.
type Group = (Box<str>, Box<[u32]>);

impl Names {
    /// The names `by_index` (one per index) and their `groups`; a member that is not a name
    /// of the game is left out of its group.
    fn new(by_index: Box<[Box<str>]>, groups: &BTreeMap<String, Vec<String>>) -> Self {
        let mut by_name: Vec<u32> =
            (0..by_index.len()).filter_map(|i| u32::try_from(i).ok()).collect();
        by_name.sort_unstable_by(|&a, &b| by_index[a as usize].cmp(&by_index[b as usize]));
        let mut names = Self { by_index, by_name: by_name.into(), groups: Box::new([]) };
        names.groups = groups
            .iter()
            .map(|(g, members)| {
                let ids = members.iter().filter_map(|m| names.index(m));
                (g.as_str().into(), ids.filter_map(|i| u32::try_from(i).ok()).collect())
            })
            .collect();
        names
    }

    /// The name at index `i`.
    #[must_use]
    pub fn name(&self, i: usize) -> Option<&str> {
        self.by_index.get(i).map(|n| &**n)
    }

    /// The index of `name`.
    #[must_use]
    pub fn index(&self, name: &str) -> Option<usize> {
        let at = |&i: &u32| self.by_index.get(i as usize).map_or("", |n| &**n);
        let k = self.by_name.binary_search_by(|i| at(i).cmp(name)).ok()?;
        self.by_name.get(k).map(|&i| i as usize)
    }

    /// The member indexes of group `name`.
    #[must_use]
    pub fn group(&self, name: &str) -> Option<&[u32]> {
        let k = self.groups.binary_search_by(|g| (*g.0).cmp(name)).ok()?;
        self.groups.get(k).map(|g| &*g.1)
    }

    /// `name` as stored here: a group of that name (a group wins, as in the reference), else
    /// the item or location of that name.
    #[must_use]
    pub fn stored(&self, name: &str) -> Option<&str> {
        let group = self.groups.binary_search_by(|g| (*g.0).cmp(name)).ok();
        let group = group.and_then(|k| self.groups.get(k)).map(|g| &*g.0);
        group.or_else(|| self.name(self.index(name)?))
    }

    /// Every name, then every group name.
    pub fn all(&self) -> impl Iterator<Item = &str> + Clone {
        self.by_index.iter().chain(self.groups.iter().map(|g| &g.0)).map(|n| &**n)
    }
}

/// The canonical JSON of `v`: object keys sorted, no whitespace (the form `bundle_hash`
/// hashes).
#[must_use]
pub fn canonical(mut v: serde_json::Value) -> Vec<u8> {
    v.sort_all_objects();
    v.to_string().into_bytes()
}

/// Ids ascending (each once) and the name of each, in the same order. An id two names share
/// keeps the first name in name order.
fn sorted(names: &BTreeMap<String, i64>) -> (Box<[i64]>, Box<[Box<str>]>) {
    let mut v: Vec<(i64, &str)> = names.iter().map(|(n, &id)| (id, n.as_str())).collect();
    v.sort_by_key(|p| p.0);
    v.dedup_by_key(|p| p.0);
    (v.iter().map(|p| p.0).collect(), v.iter().map(|p| p.1.into()).collect())
}

impl GamePackage {
    /// The content of the package for game `name`.
    #[must_use]
    pub fn content(name: &str, g: &GameData) -> Vec<u8> {
        canonical(serde_json::json!({"data": g, "game": name}))
    }

    /// The package file of game `name`: one section, the game's content, deflated. Its
    /// SHA-256 ([`key_of`]) is the package's key.
    ///
    /// # Errors
    /// Never in practice (writes go to memory).
    pub fn file(name: &str, g: &GameData) -> std::io::Result<Vec<u8>> {
        let game = Section { id: SECTION_GAME, bytes: Self::content(name, g), deflate: true };
        file_bytes(&[game])
    }

    /// The package of a package `file` stored under `key` (checked against it where it was
    /// read: `log::BlobStore::get`), from its game section.
    ///
    /// # Errors
    /// A malformed file, no game section, or one over `max` bytes.
    pub fn from_file(key: Key, file: &[u8], max: u64) -> Result<Self, BundleError> {
        let (name, g) = game_of(file, max)?;
        Ok(Self::new(key, &name, &g))
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
        let (locations, location_names) = sorted(&g.location_name_to_id);
        let (items, item_names) = sorted(&g.item_name_to_id);
        Self {
            key,
            name: name.into(),
            checksum: g.checksum.as_str().into(),
            locations,
            items,
            location_names: Names::new(location_names, &g.location_name_groups),
            item_names: Names::new(item_names, &g.item_name_groups),
            data_package: reduced.to_string().into(),
            item_groups: serde_json::json!(g.item_name_groups).to_string().into(),
            location_groups: serde_json::json!(g.location_name_groups).to_string().into(),
            hint_blacklist: g.hint_blacklist.iter().map(|n| n.as_str().into()).collect(),
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

    /// The name of location id `id`.
    #[must_use]
    pub fn location_name(&self, id: i64) -> Option<&str> {
        self.location_names.name(self.location_index(id)?)
    }

    /// The name of item id `id`.
    #[must_use]
    pub fn item_name(&self, id: i64) -> Option<&str> {
        self.item_names.name(self.item_index(id)?)
    }

    /// Location names and location groups.
    #[must_use]
    pub const fn location_names(&self) -> &Names {
        &self.location_names
    }

    /// Item names and item groups.
    #[must_use]
    pub const fn item_names(&self) -> &Names {
        &self.item_names
    }

    /// Whether the world forbids hinting the item or location called `name`.
    #[must_use]
    pub fn hint_blacklisted(&self, name: &str) -> bool {
        self.hint_blacklist.binary_search_by(|n| (**n).cmp(name)).is_ok()
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

/// The key of a package file: the blob store's content address (one function for both).
pub use log::blob_key as key_of;

/// The game's name and data in a package file (its game section).
///
/// # Errors
/// A malformed file, no game section, or one over `max` bytes.
pub fn game_of(file: &[u8], max: u64) -> Result<(String, GameData), BundleError> {
    let content = section_of(file, SECTION_GAME, max)?
        .ok_or_else(|| BundleError::Invalid("package file: no game section".to_owned()))?;
    GamePackage::parse(&content)
}

/// Packages in use in this process, by key. Entries are weak: a package is dropped once no
/// room holds it, and loaded again on next use.
static SHARED: Mutex<BTreeMap<Key, Weak<GamePackage>>> = Mutex::new(BTreeMap::new());

/// The process's copy of package `key`, if a room holds one (what a loader need not fetch).
#[must_use]
pub fn cached(key: &Key) -> Option<Arc<GamePackage>> {
    SHARED.lock().unwrap_or_else(PoisonError::into_inner).get(key).and_then(Weak::upgrade)
}

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
            ..GameData::default()
        }
    }

    #[test]
    fn names_and_groups_are_found_by_binary_search() {
        let mut g = game();
        g.item_name_groups.insert("odd".into(), vec!["b".into(), "nope".into()]);
        let p = GamePackage::new([0; 32], "G", &g);
        let n = p.item_names();
        // Items by id: a (-1) is index 0, b (9) index 1.
        assert_eq!((n.index("a"), n.index("b"), n.index("c")), (Some(0), Some(1), None));
        assert_eq!(n.group("all"), Some(&[0, 1][..]));
        assert_eq!(n.group("odd"), Some(&[1][..]), "an unknown member is left out");
        assert_eq!(n.group("none"), None);
        assert_eq!(n.all().collect::<Vec<_>>(), ["a", "b", "all", "odd"]);
    }

    #[test]
    fn ids_map_to_dense_indexes_in_id_order() {
        let g = game();
        let p = GamePackage::new([0; 32], "G", &g);
        assert_eq!(p.locations(), [2, 5]);
        assert_eq!(p.items(), [-1, 9]);
        assert_eq!((p.location_index(5), p.location_index(3)), (Some(1), None));
        assert_eq!(p.item_index(-1), Some(0));
        assert_eq!(
            (p.item_name(9), p.location_name(2), p.item_name(3)),
            (Some("b"), Some("y"), None)
        );
        // Exactly what the edge sent before packages were shared (sorted keys, compact).
        assert_eq!(
            p.data_package(),
            r#"{"checksum":"c0ffee","item_name_to_id":{"a":-1,"b":9},"location_name_to_id":{"x":5,"y":2}}"#
        );
        assert_eq!(p.item_groups(), r#"{"all":["a","b"]}"#);
    }

    #[test]
    fn a_package_file_reads_back_and_its_key_is_its_hash() {
        let f = GamePackage::file("G", &game()).unwrap();
        assert_eq!(key_of(&f), <[u8; 32]>::from(Sha256::digest(&f)));
        assert_eq!(game_of(&f, 1 << 20).unwrap(), ("G".to_owned(), game()));
        let p = GamePackage::from_file(key_of(&f), &f, 1 << 20).unwrap();
        assert_eq!((p.name(), p.key()), ("G", &key_of(&f)));
        assert!(game_of(&f, 10).is_err(), "over the cap");
        assert!(game_of(b"not a package", 1 << 20).is_err());
        let rules_only =
            file_bytes(&[Section { id: SECTION_RULES, bytes: b"r".to_vec(), deflate: true }]);
        assert!(game_of(&rules_only.unwrap(), 1 << 20).is_err(), "no game section");
    }

    #[test]
    fn sections_round_trip_and_unknown_or_broken_ones_are_handled() {
        let all = [
            Section { id: SECTION_GAME, bytes: b"game".to_vec(), deflate: true },
            Section { id: 99, bytes: vec![7; 300], deflate: false },
        ];
        let f = file_bytes(&all).unwrap();
        assert_eq!(sections(&f, 1 << 20).unwrap(), all, "an unknown section is kept, not refused");
        assert!(sections(&f, 10).is_err(), "over the cap");
        assert_eq!(sections(&f, 304).unwrap(), all, "4 + 300 bytes: exactly at the cap");
        assert!(sections(&f, 303).is_err(), "one byte over the cap");
        for cut in [0, 4, 7, 20, f.len() - 1] {
            assert!(sections(&f[..cut], 1 << 20).is_err(), "cut at {cut}");
        }
        let mut wrong = f.clone();
        wrong[8 + 16] ^= 1; // the first section's hash
        assert!(sections(&wrong, 1 << 20).is_err());
        // A section stored as is reads in place; a deflated one says so.
        assert_eq!(stored(&f, 99).unwrap(), Some(&[7_u8; 300][..]));
        assert!(stored(&f, SECTION_GAME).is_err());
        assert_eq!(stored(&f, SECTION_RULES).unwrap(), None);
        let mut bent = f.clone();
        let last = bent.len() - 1;
        bent[last] ^= 1;
        assert!(stored(&bent, 99).is_err(), "in place, still checked against its hash");
        let mut v2 = f;
        v2[4] = 2;
        assert!(sections(&v2, 1 << 20).is_err());
    }

    /// Review finding: readers inflated every section to pick one, so a small file with big
    /// sections held them all. A reader of the game section never inflates another, and
    /// repeated or overlapping sections are refused.
    #[test]
    fn reading_one_section_never_inflates_another() {
        let content = GamePackage::content("G", &game());
        let big = Section { id: SECTION_RULES, bytes: vec![0; 2 << 20], deflate: true };
        let game_section = Section { id: SECTION_GAME, bytes: content.clone(), deflate: true };
        let f = file_bytes(&[game_section.clone(), big]).unwrap();
        assert!(f.len() < 10_000, "2 MiB of zeros deflate small: {}", f.len());
        // Capped at 1 MiB: the 2 MiB rules section is never opened by the game reader.
        assert_eq!(game_of(&f, 1 << 20).unwrap(), ("G".to_owned(), game()));
        assert!(section_of(&f, SECTION_RULES, 1 << 20).is_err());
        assert!(sections(&f, 1 << 20).is_err(), "all of them together are over the cap");
        let twice = file_bytes(&[game_section.clone(), game_section]).unwrap();
        assert!(section_of(&twice, SECTION_GAME, 1 << 20).is_err(), "a repeated id");
        // Two entries over the same bytes (the second entry's offset set to the first's).
        let two = file_bytes(&[
            Section { id: 7, bytes: vec![1; 64], deflate: false },
            Section { id: 8, bytes: vec![1; 64], deflate: false },
        ])
        .unwrap();
        let mut overlap = two.clone();
        let first_at = overlap[8 + 4..8 + 8].to_vec();
        overlap[8 + ENTRY + 4..8 + ENTRY + 8].copy_from_slice(&first_at);
        assert!(section_of(&overlap, 7, 1 << 20).is_err());
        assert_eq!(section_of(&two, 8, 1 << 20).unwrap(), Some(vec![1; 64]));
    }

    /// Indexes are the position in the sorted ids of the game section, which the key hashes:
    /// the same key always gives the same indexes, whatever order the names came in.
    #[test]
    fn indexes_are_fixed_by_the_package_key() {
        let mut reversed = game();
        reversed.item_name_to_id = game().item_name_to_id.into_iter().rev().collect();
        let (a, b) =
            (GamePackage::file("G", &game()).unwrap(), GamePackage::file("G", &reversed).unwrap());
        assert_eq!(key_of(&a), key_of(&b), "the same file, so the same key");
        let (name, parsed) = game_of(&a, 1 << 20).unwrap();
        let (p, q) =
            (GamePackage::new([0; 32], "G", &game()), GamePackage::new([0; 32], &name, &parsed));
        assert_eq!((p.items(), p.locations()), (q.items(), q.locations()));
        assert_eq!(p.items(), [-1, 9], "ascending ids");
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
