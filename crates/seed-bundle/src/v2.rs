//! Seed file v2: one room's seed as compact binary, read in place (`docs/seed-bundle-v2.md`).
//!
//! The game data packages are not in the file; it names them by key ([`crate::package`]).
//! Everything is at a fixed or stated offset, so the file works from an `mmap` with no
//! parsing: [`Seed::open`] only checks it, and every read is a bounds-checked look at the
//! bytes. Placements are bit-packed: per slot a presence bitset over its game's location
//! indexes, then one record per set bit (item index in the receiver's game, receiver slot
//! position, flags), each field in the fewest bits this seed needs.

use crate::package::{self, GamePackage, Key};
use crate::{BundleError, GameData, Mode, Placed, SeedBundle, ServerOptions, Slot, SlotKind};
use crate::{FORMAT, Version};
use bits::{self, Packer};
use proto::SlotNo;
use room_core::World;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// The first four bytes of a v2 seed file.
pub const MAGIC: [u8; 4] = *b"CSB2";
/// The version this crate writes and reads.
pub const VERSION: u16 = 2;

const HEADER: usize = 96;
const ROOM: usize = 36;
const GAME: usize = 32;
const SLOT: usize = 72;
const CONNECT: usize = 16;
/// A string reference length that means "no string" (a room with no password).
const NONE: u32 = u32::MAX;
/// Widest fields the reader accepts (it reads at most 57 bits at once).
const MAX_ITEM_BITS: u8 = 32;
const MAX_RECV_BITS: u8 = 16;
const MAX_FLAG_BITS: u8 = 3;

fn bad(why: impl Into<String>) -> BundleError {
    BundleError::Invalid(why.into())
}

const fn mode_byte(m: Mode) -> u8 {
    match m {
        Mode::Disabled => 0,
        Mode::Enabled => 1,
        Mode::Goal => 2,
        Mode::Auto => 3,
        Mode::AutoEnabled => 4,
    }
}

const fn byte_mode(b: u8) -> Option<Mode> {
    Some(match b {
        0 => Mode::Disabled,
        1 => Mode::Enabled,
        2 => Mode::Goal,
        3 => Mode::Auto,
        4 => Mode::AutoEnabled,
        _ => return None,
    })
}

const fn kind_byte(k: SlotKind) -> u8 {
    match k {
        SlotKind::Spectator => 0,
        SlotKind::Player => 1,
        SlotKind::Group => 2,
    }
}

const fn byte_kind(b: u8) -> Option<SlotKind> {
    Some(match b {
        0 => SlotKind::Spectator,
        1 => SlotKind::Player,
        2 => SlotKind::Group,
        _ => return None,
    })
}

/// Bits per start inventory entry: the item width, but at least 1, so every entry costs file
/// bytes and a hostile count is bounded by the file's length.
const fn start_width(w_item: u8) -> u8 {
    if w_item == 0 { 1 } else { w_item }
}

fn u32_of(n: usize) -> Result<u32, BundleError> {
    u32::try_from(n).map_err(|_| bad("too large for a v2 seed file"))
}

fn u16_of(n: usize) -> Result<u16, BundleError> {
    u16::try_from(n).map_err(|_| bad("too many slots, games or names for a v2 seed file"))
}

/// A v2 seed and the packages it names, ready to write.
#[derive(Debug)]
pub struct Encoded {
    /// The seed file.
    pub seed: Vec<u8>,
    /// Each game's package content, in the file's game order.
    pub packages: Vec<Vec<u8>>,
}

/// Interned strings: each distinct string once.
#[derive(Default)]
struct Strings {
    bytes: Vec<u8>,
    at: BTreeMap<String, (u32, u32)>,
}

impl Strings {
    fn add(&mut self, s: &str) -> Result<(u32, u32), BundleError> {
        if let Some(&r) = self.at.get(s) {
            return Ok(r);
        }
        let r = (u32_of(self.bytes.len())?, u32_of(s.len())?);
        if r.1 == NONE {
            return Err(bad("a string is too long"));
        }
        self.bytes.extend_from_slice(s.as_bytes());
        self.at.insert(s.to_owned(), r);
        Ok(r)
    }
}

/// Little-endian appends.
trait Put {
    fn u16(&mut self, v: u16);
    fn u32(&mut self, v: u32);
    fn r#ref(&mut self, r: (u32, u32));
}

impl Put for Vec<u8> {
    fn u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn r#ref(&mut self, r: (u32, u32)) {
        self.u32(r.0);
        self.u32(r.1);
    }
}

/// One slot's placements as indexes.
struct Indexed {
    game: usize,
    /// (location index, item index, receiver position, flags), by location index.
    placed: Vec<(usize, usize, usize, u8)>,
    start: Vec<usize>,
}

/// Encode a validated bundle whose v1 canonical hash is `hash` (`crate::bundle_hash`).
///
/// Slots are written in id order and locations in id order, the order `tools/ap-import`
/// writes; any other order in `b` is normalized.
///
/// The seed is sized before anything is allocated for it and refused when over `max_bytes`
/// (the config's `bundle_max_bytes`): a bundle is untrusted (generation runs uploaded code),
/// and a small one can name a huge game.
///
/// # Errors
/// When the bundle is too large for the format's fields or for `max_bytes`.
pub fn encode(b: &SeedBundle, hash: &[u8; 32], max_bytes: u64) -> Result<Encoded, BundleError> {
    let games: Vec<(&String, &GameData)> = b.games.iter().collect();
    let packages: Vec<Vec<u8>> =
        games.iter().map(|(name, g)| GamePackage::content(name, g)).collect();
    let tables: Vec<GamePackage> = games
        .iter()
        .zip(&packages)
        .map(|((name, g), c)| GamePackage::new(GamePackage::key_of(c), name, g))
        .collect();
    let game_of = |name: &str| games.iter().position(|(n, _)| n.as_str() == name);
    let mut slots: Vec<&Slot> = b.slots.iter().collect();
    slots.sort_by_key(|s| s.id);
    let pos_of = |id: u32| slots.iter().position(|s| s.id == id);
    let missing = || bad("the bundle names something it lacks (validate it first)");

    let mut indexed = Vec::with_capacity(slots.len());
    for s in &slots {
        let g = game_of(&s.game).ok_or_else(missing)?;
        let own = tables.get(g).ok_or_else(missing)?;
        let mut placed = Vec::with_capacity(s.locations.len());
        for p in &s.locations {
            let to = pos_of(p.receiver).ok_or_else(missing)?;
            let rg = slots.get(to).and_then(|r| game_of(&r.game)).ok_or_else(missing)?;
            let item = tables.get(rg).and_then(|t| t.item_index(p.item)).ok_or_else(missing)?;
            let at = own.location_index(p.location).ok_or_else(missing)?;
            placed.push((at, item, to, p.flags));
        }
        placed.sort_unstable_by_key(|p| p.0);
        let start: Result<Vec<usize>, _> =
            s.start_inventory.iter().map(|&i| own.item_index(i).ok_or_else(missing)).collect();
        indexed.push(Indexed { game: g, placed, start: start? });
    }
    let max = |f: fn(&(usize, usize, usize, u8)) -> u64| {
        indexed.iter().flat_map(|x| x.placed.iter().map(f)).max().unwrap_or(0)
    };
    let start_max = indexed.iter().flat_map(|x| x.start.iter()).max().copied().unwrap_or(0);
    let w_item = bits::width(max(|p| p.1 as u64).max(start_max as u64));
    let w_recv = bits::width(max(|p| p.2 as u64));
    let w_flags = bits::width(max(|p| u64::from(p.3)));
    if w_item > MAX_ITEM_BITS || w_recv > MAX_RECV_BITS || w_flags > MAX_FLAG_BITS {
        return Err(bad("a placement field is too wide for a v2 seed file"));
    }
    let w_record = usize::from(w_item + w_recv + w_flags);
    let w_start = start_width(w_item);

    let connect: Vec<(&String, &(u32, u32))> = b.connect_names.iter().collect();
    let data_start =
        HEADER + ROOM + GAME * games.len() + SLOT * slots.len() + CONNECT * connect.len();
    let raws: Vec<Vec<u8>> = slots.iter().map(|s| s.slot_data.to_string().into_bytes()).collect();
    // An upper bound of the whole file (slot_data before compression, strings before
    // interning), from the counts alone.
    let words = |x: &Indexed| {
        if x.placed.is_empty() {
            0
        } else {
            tables.get(x.game).map_or(0, |t| t.locations().len().div_ceil(64))
        }
    };
    let strings_max = slots.iter().map(|s| s.name.len()).sum::<usize>()
        + connect.iter().map(|(n, _)| n.len()).sum::<usize>()
        + b.seed_name.len()
        + b.server_options.password.as_ref().map_or(0, String::len);
    let need =
        slots.iter().zip(&indexed).zip(&raws).fold(data_start + strings_max, |n, ((s, x), raw)| {
            n.saturating_add(words(x) * 8)
                .saturating_add(bits::bytes_for(x.placed.len(), w_record))
                .saturating_add(raw.len())
                .saturating_add(s.group_members.len() * 4)
                .saturating_add(bits::bytes_for(x.start.len(), usize::from(w_start)))
        });
    if u64::try_from(need).unwrap_or(u64::MAX) > max_bytes {
        return Err(bad(format!("the seed would take {need} bytes, over the {max_bytes} cap")));
    }

    let mut strings = Strings::default();
    let mut data = Vec::with_capacity(need - data_start);
    let at = |data: &Vec<u8>| u32_of(data_start + data.len());
    let mut slot_table = Vec::with_capacity(SLOT * slots.len());
    for ((s, x), raw) in slots.iter().zip(&indexed).zip(raws) {
        // A slot that places nothing stores no presence words.
        let mut presence = vec![0_u64; words(x)];
        for p in &x.placed {
            if let Some(w) = presence.get_mut(p.0 / 64) {
                *w |= 1 << (p.0 % 64);
            }
        }
        let presence_at = if presence.is_empty() { 0 } else { at(&data)? };
        for w in presence {
            data.extend_from_slice(&w.to_le_bytes());
        }
        let records_at = at(&data)?;
        let mut packer = Packer::default();
        for p in &x.placed {
            packer.push(p.1 as u64, w_item);
            packer.push(p.2 as u64, w_recv);
            packer.push(u64::from(p.3), w_flags);
        }
        let records = packer.finish();
        debug_assert_eq!(records.len(), bits::bytes_for(x.placed.len(), w_record));
        data.extend_from_slice(&records);
        let packed = package::deflate(&raw).map_err(|e| bad(e.to_string()))?;
        let (blob, raw_len) =
            if packed.len() < raw.len() { (packed, u32_of(raw.len())?) } else { (raw, 0) };
        let data_at = at(&data)?;
        data.extend_from_slice(&blob);
        let members_at = at(&data)?;
        for m in &s.group_members {
            data.u32(*m);
        }
        let start_at = at(&data)?;
        let mut packer = Packer::default();
        for &i in &x.start {
            packer.push(i as u64, w_start);
        }
        data.extend_from_slice(&packer.finish());

        let t = &mut slot_table;
        t.u32(s.id);
        t.u32(s.team);
        t.r#ref(strings.add(&s.name)?);
        t.u16(u16_of(x.game)?);
        t.push(kind_byte(s.kind));
        t.push(0);
        let v = s.minimum_client_version;
        for n in [v.major, v.minor, v.build, presence_at, u32_of(x.placed.len())?, records_at] {
            t.u32(n);
        }
        for n in [data_at, u32_of(blob.len())?, raw_len, members_at] {
            t.u32(n);
        }
        for n in [u32_of(s.group_members.len())?, start_at, u32_of(x.start.len())?] {
            t.u32(n);
        }
    }
    let mut connect_table = Vec::with_capacity(CONNECT * connect.len());
    for (name, (team, slot)) in &connect {
        connect_table.r#ref(strings.add(name)?);
        connect_table.u32(*team);
        connect_table.u32(*slot);
    }
    let o = &b.server_options;
    let mut room = Vec::with_capacity(ROOM);
    room.r#ref(strings.add(&b.seed_name)?);
    room.r#ref(match &o.password {
        Some(p) => strings.add(p)?,
        None => (0, NONE),
    });
    let g = b.generator_version;
    for n in [g.major, g.minor, g.build] {
        room.u32(n);
    }
    room.extend([o.hint_cost, mode_byte(o.release_mode), mode_byte(o.collect_mode)]);
    room.push(mode_byte(o.remaining_mode));
    room.u32(o.location_check_points);

    let strings_at = data_start + data.len();
    let total = strings_at + strings.bytes.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    out.u16(VERSION);
    out.extend([b.race_mode, w_item, w_recv, w_flags]);
    out.u16(u16_of(slots.len())?);
    out.u16(u16_of(games.len())?);
    out.u16(0);
    for n in [u32_of(connect.len())?, u32_of(strings_at)?, u32_of(total)?, 0] {
        out.u32(n);
    }
    out.extend_from_slice(hash);
    out.extend_from_slice(&[0; 32]);
    out.extend_from_slice(&room);
    for t in &tables {
        out.extend_from_slice(t.key());
    }
    out.extend_from_slice(&slot_table);
    out.extend_from_slice(&connect_table);
    out.extend_from_slice(&data);
    out.extend_from_slice(&strings.bytes);
    debug_assert_eq!(out.len(), total);
    let body = body_hash(&out);
    out.get_mut(64..96).ok_or_else(missing)?.copy_from_slice(&body);
    Ok(Encoded { seed: out, packages })
}

/// Decode a v2 file back to the v1 bundle, each game's full data read from the package files
/// in `packages` (the debugging view: `bundle v2-decode`).
///
/// # Errors
/// A malformed seed or a missing or bad package.
pub fn decode(bytes: Vec<u8>, packages: &Path, max_bytes: u64) -> Result<SeedBundle, BundleError> {
    let mut data = BTreeMap::new();
    let games = package_keys(&bytes)?
        .into_iter()
        .map(|key| {
            let (name, g) = GamePackage::parse(&package::read(packages, &key, max_bytes)?)?;
            let p = Arc::new(GamePackage::new(key, &name, &g));
            data.insert(key, g);
            Ok(p)
        })
        .collect::<Result<_, BundleError>>()?;
    Seed::open(bytes, games, max_bytes)?.to_v1(&data)
}

/// SHA-256 of every byte of the file except the body hash field (bytes 64 to 96).
fn body_hash(file: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(file.get(..64).unwrap_or_default());
    h.update(file.get(HEADER..).unwrap_or_default());
    h.finalize().into()
}

/// Whether `bytes` start like a v2 seed file.
#[must_use]
pub fn is_v2(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC)
}

/// The package keys a v2 file names, in game order, after checking its header.
///
/// # Errors
/// When the header or the game table is malformed.
pub fn package_keys(bytes: &[u8]) -> Result<Vec<Key>, BundleError> {
    let r = Reader(bytes);
    header(r)?;
    let games = usize::from(r.u16(12));
    (0..games)
        .map(|g| {
            let at = HEADER + ROOM + GAME * g;
            let key: Key = bytes
                .get(at..at + GAME)
                .and_then(|k| k.try_into().ok())
                .ok_or_else(|| bad("game table cut short"))?;
            Ok(key)
        })
        .collect()
}

fn header(r: Reader<'_>) -> Result<(), BundleError> {
    if !is_v2(r.0) || r.0.len() < HEADER {
        return Err(BundleError::Parse("not a v2 seed file".to_owned()));
    }
    if r.u16(4) != VERSION {
        return Err(bad(format!("seed file version {} is not {VERSION}", r.u16(4))));
    }
    if r.u32(24) as usize != r.0.len() {
        return Err(bad("seed file length differs from its header (cut short?)"));
    }
    if r.0.get(64..96) != Some(&body_hash(r.0)[..]) {
        return Err(bad("seed file is corrupt (body hash mismatch)"));
    }
    Ok(())
}

/// Bounds-checked little-endian reads; out of range reads as 0 (`Seed::open` checked every
/// range, so that never happens on an opened seed).
#[derive(Clone, Copy)]
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn u8(self, at: usize) -> u8 {
        self.0.get(at).copied().unwrap_or(0)
    }
    fn u16(self, at: usize) -> u16 {
        self.0.get(at..at + 2).and_then(|b| b.try_into().ok()).map_or(0, u16::from_le_bytes)
    }
    fn u32(self, at: usize) -> u32 {
        self.0.get(at..at + 4).and_then(|b| b.try_into().ok()).map_or(0, u32::from_le_bytes)
    }
    fn u64(self, at: usize) -> u64 {
        self.0.get(at..at + 8).and_then(|b| b.try_into().ok()).map_or(0, u64::from_le_bytes)
    }
}

/// An opened v2 seed: the file's bytes and its game packages.
pub struct Seed {
    bytes: Box<[u8]>,
    games: Box<[Arc<GamePackage>]>,
    /// Slot ids by position (for [`World::slots`]).
    ids: Box<[SlotNo]>,
    /// Placed locations before each slot position (`n + 1` entries): room-wide ordinals.
    bases: Box<[u32]>,
    /// Per slot, the set bits before each presence word, back to back; slot `pos` owns
    /// `ranks[rank_at[pos]..rank_at[pos + 1]]`. Rank and select without a scan.
    ranks: Box<[u32]>,
    rank_at: Box<[u32]>,
}

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seed")
            .field("seed_name", &self.seed_name())
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// One placed location, in protocol ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Location id in the slot's world.
    pub location: i64,
    /// Item id in the receiver's world.
    pub item: i64,
    /// Receiving slot id.
    pub receiver: u32,
    /// Item classification bits.
    pub flags: u8,
}

/// One slot of an opened seed.
#[derive(Clone, Copy)]
pub struct SlotView<'a> {
    seed: &'a Seed,
    pos: usize,
}

impl Seed {
    fn r(&self) -> Reader<'_> {
        Reader(&self.bytes)
    }

    fn str_at(&self, at: usize) -> Option<&str> {
        let r = self.r();
        let (off, len) = (r.u32(at) as usize, r.u32(at + 4));
        if len == NONE {
            return None;
        }
        let base = r.u32(20) as usize;
        let start = base.checked_add(off)?;
        std::str::from_utf8(self.bytes.get(start..start.checked_add(len as usize)?)?).ok()
    }

    fn widths(&self) -> (u8, u8, u8) {
        let r = self.r();
        (r.u8(7), r.u8(8), r.u8(9))
    }

    /// The file's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Bits per item index, receiver position and flags.
    #[must_use]
    pub fn bit_widths(&self) -> (u8, u8, u8) {
        self.widths()
    }

    /// SHA-256 of the canonical v1 JSON this seed was made from (`crate::bundle_hash`).
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        self.bytes.get(32..64).and_then(|h| h.try_into().ok()).unwrap_or([0; 32])
    }

    /// The generator's name for this seed.
    #[must_use]
    pub fn seed_name(&self) -> &str {
        self.str_at(HEADER).unwrap_or_default()
    }

    /// The room password from the seed; `None` means open.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        self.str_at(HEADER + 8)
    }

    /// The generator's version.
    #[must_use]
    pub fn generator_version(&self) -> Version {
        let r = self.r();
        Version { major: r.u32(HEADER + 16), minor: r.u32(HEADER + 20), build: r.u32(HEADER + 24) }
    }

    /// The server options.
    #[must_use]
    pub fn server_options(&self) -> ServerOptions {
        let r = self.r();
        let mode = |at| byte_mode(r.u8(HEADER + at)).unwrap_or(Mode::Disabled);
        ServerOptions {
            password: self.password().map(str::to_owned),
            hint_cost: r.u8(HEADER + 28),
            release_mode: mode(29),
            collect_mode: mode(30),
            remaining_mode: mode(31),
            location_check_points: r.u32(HEADER + 32),
        }
    }

    /// Percent of a slot's locations a hint costs.
    #[must_use]
    pub fn hint_cost(&self) -> u8 {
        self.r().u8(HEADER + 28)
    }

    /// Hint points per checked location.
    #[must_use]
    pub fn location_check_points(&self) -> u32 {
        self.r().u32(HEADER + 32)
    }

    /// The race mode flag (0 or 1).
    #[must_use]
    pub fn race_mode(&self) -> u8 {
        self.r().u8(6)
    }

    /// Every game, by name.
    #[must_use]
    pub fn games(&self) -> &[Arc<GamePackage>] {
        &self.games
    }

    /// The game called `name`.
    #[must_use]
    pub fn game(&self, name: &str) -> Option<&GamePackage> {
        self.games.iter().find(|g| g.name() == name).map(|g| &**g)
    }

    fn slots_at(&self) -> usize {
        HEADER + ROOM + GAME * self.games.len()
    }

    fn connect_at(&self) -> usize {
        self.slots_at() + SLOT * self.ids.len()
    }

    /// Every slot, in id order.
    pub fn slots(&self) -> impl ExactSizeIterator<Item = SlotView<'_>> + '_ {
        (0..self.ids.len()).map(move |pos| SlotView { seed: self, pos })
    }

    /// The slot with id `id`.
    #[must_use]
    pub fn slot(&self, id: u32) -> Option<SlotView<'_>> {
        self.pos(SlotNo(id)).map(|pos| SlotView { seed: self, pos })
    }

    /// The slot at position `pos`.
    #[must_use]
    pub fn slot_at(&self, pos: usize) -> Option<SlotView<'_>> {
        (pos < self.ids.len()).then_some(SlotView { seed: self, pos })
    }

    /// The position of slot `id`.
    #[must_use]
    pub fn pos(&self, id: SlotNo) -> Option<usize> {
        self.ids.binary_search(&id).ok()
    }

    /// Every slot number.
    pub fn slot_nos(&self) -> impl Iterator<Item = SlotNo> + '_ {
        self.ids.iter().copied()
    }

    /// The (team, slot) a client may connect to with `name`.
    #[must_use]
    pub fn connect(&self, name: &str) -> Option<(u32, u32)> {
        self.connect_names().find(|(n, _)| *n == name).map(|(_, t)| t)
    }

    /// Every connect name and its (team, slot), sorted by name.
    pub fn connect_names(&self) -> impl Iterator<Item = (&str, (u32, u32))> + '_ {
        let r = self.r();
        let n = r.u32(16) as usize;
        let base = self.connect_at();
        (0..n).map(move |k| {
            let at = base + CONNECT * k;
            (self.str_at(at).unwrap_or_default(), (r.u32(at + 8), r.u32(at + 12)))
        })
    }

    /// Check `bytes` against the format and open it on its packages (in the file's game
    /// order, as [`package_keys`] lists them). `max_bytes` caps what one `slot_data` may
    /// inflate to.
    ///
    /// # Errors
    /// The first rule the file breaks.
    pub fn open(
        bytes: Vec<u8>,
        games: Vec<Arc<GamePackage>>,
        max_bytes: u64,
    ) -> Result<Self, BundleError> {
        let keys = package_keys(&bytes)?;
        if keys.len() != games.len() || keys.iter().zip(&games).any(|(k, g)| k != g.key()) {
            return Err(bad("the packages given are not the ones the seed names"));
        }
        let r = Reader(&bytes);
        let n = usize::from(r.u16(10));
        let slots_at = HEADER + ROOM + GAME * games.len();
        let ids: Box<[SlotNo]> = (0..n).map(|p| SlotNo(r.u32(slots_at + SLOT * p))).collect();
        let mut seed = Self {
            bytes: bytes.into_boxed_slice(),
            games: games.into(),
            ids,
            bases: Box::new([]),
            ranks: Box::new([]),
            rank_at: Box::new([]),
        };
        seed.check(max_bytes)?;
        seed.index()?;
        Ok(seed)
    }

    fn check(&self, max_bytes: u64) -> Result<(), BundleError> {
        let r = self.r();
        let len = self.bytes.len();
        let n = self.ids.len();
        let strings = r.u32(20) as usize;
        let connect = r.u32(16) as usize;
        let tables = self
            .connect_at()
            .checked_add(CONNECT.checked_mul(connect).ok_or_else(|| bad("bad counts"))?);
        if tables.is_none_or(|t| t > strings) || strings > len {
            return Err(bad("seed file sections overlap or run past its end"));
        }
        let data_from = tables.unwrap_or(usize::MAX);
        // Every data area lies between the tables and the strings (an empty one anywhere).
        let in_data = |at: usize, size: usize| {
            size == 0 || (at >= data_from && at.checked_add(size).is_some_and(|e| e <= strings))
        };
        if r.u16(14) != 0 || r.u32(28) != 0 {
            return Err(bad("reserved header fields are not 0"));
        }
        let (w_item, w_recv, w_flags) = self.widths();
        if w_item > MAX_ITEM_BITS || w_recv > MAX_RECV_BITS || w_flags > MAX_FLAG_BITS {
            return Err(bad("a placement field is too wide"));
        }
        let ok_str = |at: usize| r.u32(at + 4) == NONE || self.str_at(at).is_some();
        if !ok_str(HEADER) || r.u32(HEADER + 4) == NONE || !ok_str(HEADER + 8) {
            return Err(bad("a room string is out of range or not UTF-8"));
        }
        let o = self.server_options();
        let modes = [29, 30, 31].map(|at| byte_mode(r.u8(HEADER + at)));
        if o.hint_cost > 100
            || modes.iter().any(Option::is_none)
            || matches!(o.remaining_mode, Mode::Auto | Mode::AutoEnabled)
            || r.u8(6) > 1
        {
            return Err(bad("server options out of range"));
        }
        if n == 0 || !self.ids.windows(2).all(|w| w[0] < w[1]) || self.ids[0].0 == 0 {
            return Err(bad("slots must be ids from 1, strictly ascending"));
        }
        for s in self.slots() {
            let at = s.at();
            let id = s.id();
            if !ok_str(at + 8) || self.str_at(at + 8).is_none() {
                return Err(bad(format!("slot {id} name out of range")));
            }
            let game = usize::from(r.u16(at + 16));
            let Some(g) = self.games.get(game) else {
                return Err(bad(format!("slot {id} plays a missing game")));
            };
            if byte_kind(r.u8(at + 18)).is_none_or(|k| k == SlotKind::Group) {
                return Err(bad(format!("slot {id} has a bad or unsupported kind")));
            }
            let placed = s.placed();
            let words = if placed == 0 { 0 } else { g.locations().len().div_ceil(64) };
            let presence = r.u32(at + 32) as usize;
            if !in_data(presence, words * 8) {
                return Err(bad(format!("slot {id} presence out of range")));
            }
            let set: usize =
                (0..words).map(|k| r.u64(presence + 8 * k).count_ones() as usize).sum();
            let tail = g.locations().len() % 64;
            if tail != 0 && words > 0 && r.u64(presence + 8 * (words - 1)) >> tail != 0 {
                return Err(bad(format!("slot {id} names a location its game lacks")));
            }
            let w = usize::from(w_item + w_recv + w_flags);
            if set != placed || !in_data(r.u32(at + 40) as usize, bits::bytes_for(placed, w)) {
                return Err(bad(format!("slot {id} placements out of range")));
            }
            for i in 0..placed {
                let (item, to, flags) = s.record(i);
                let items = self
                    .ids
                    .get(to)
                    .and_then(|_| self.slot_at(to))
                    .map_or(0, |rs| rs.package().items().len());
                if item >= items || flags > 0b111 {
                    return Err(bad(format!("slot {id} placement {i} is out of range")));
                }
            }
            let (data, size, raw) = (r.u32(at + 44), r.u32(at + 48), r.u32(at + 52));
            if !in_data(data as usize, size as usize) || u64::from(raw) > max_bytes {
                return Err(bad(format!("slot {id} slot_data out of range")));
            }
            let members = r.u32(at + 60) as usize;
            if !in_data(r.u32(at + 56) as usize, members.saturating_mul(4))
                || s.group_members().iter().any(|m| self.pos(SlotNo(*m)).is_none())
            {
                return Err(bad(format!("slot {id} group members out of range")));
            }
            let start = r.u32(at + 68) as usize;
            let w_start = usize::from(start_width(w_item));
            if !in_data(r.u32(at + 64) as usize, bits::bytes_for(start, w_start))
                || s.start_indexes().any(|i| i >= g.items().len())
            {
                return Err(bad(format!("slot {id} start inventory out of range")));
            }
        }
        for (k, (name, (team, slot))) in self.connect_names().enumerate() {
            let at = self.connect_at() + CONNECT * k;
            if self.str_at(at).is_none() || self.slot(slot).is_none_or(|s| s.team() != team) {
                return Err(bad(format!("connect name {name:?} points at no slot")));
            }
        }
        Ok(())
    }

    /// Build the prefix sums (`bases`) and rank tables of a checked seed.
    fn index(&mut self) -> Result<(), BundleError> {
        let (mut bases, mut ranks, mut rank_at) = (vec![0_u32], Vec::new(), vec![0_u32]);
        for s in self.slots() {
            let last = bases.last().copied().unwrap_or(0);
            bases.push(
                last.checked_add(u32_of(s.placed())?).ok_or_else(|| bad("too many placements"))?,
            );
            let p = s.presence();
            let mut before = 0_u32;
            for k in 0..p.len() / 8 {
                ranks.push(before);
                before += SlotView::word(p, k).count_ones();
            }
            rank_at.push(u32_of(ranks.len())?);
        }
        (self.bases, self.ranks, self.rank_at) = (bases.into(), ranks.into(), rank_at.into());
        Ok(())
    }

    /// The global ordinal of location `index` of slot `pos`: a dense number over every
    /// placed location of the room (a compact handle for an item). O(1).
    #[must_use]
    pub fn ordinal(&self, pos: usize, index: usize) -> Option<u32> {
        let (base, end) = (*self.bases.get(pos)?, *self.bases.get(pos + 1)?);
        let o = base.checked_add(u32::try_from(index).ok()?)?;
        (o < end).then_some(o)
    }

    /// The (slot position, location index) of an ordinal: a binary search of the prefix sums.
    #[must_use]
    pub fn from_ordinal(&self, ordinal: u32) -> Option<(usize, usize)> {
        if ordinal >= *self.bases.last()? {
            return None;
        }
        let pos = self.bases.partition_point(|&b| b <= ordinal).checked_sub(1)?;
        Some((pos, (ordinal - *self.bases.get(pos)?) as usize))
    }

    /// Back to the v1 bundle, with each game's full data from `games` (by package key).
    ///
    /// # Errors
    /// When a package's data is missing or `slot_data` does not decode.
    pub fn to_v1(&self, games: &BTreeMap<Key, GameData>) -> Result<SeedBundle, BundleError> {
        let mut out = BTreeMap::new();
        for g in self.games.iter() {
            let data = games.get(g.key()).ok_or_else(|| bad("a package's data is missing"))?;
            out.insert(g.name().to_owned(), data.clone());
        }
        let slots = self
            .slots()
            .map(|s| {
                Ok(Slot {
                    id: s.id(),
                    team: s.team(),
                    name: s.name().to_owned(),
                    game: s.package().name().to_owned(),
                    kind: s.kind(),
                    minimum_client_version: s.minimum_client_version(),
                    group_members: s.group_members(),
                    locations: s
                        .placements()
                        .map(|p| Placed {
                            location: p.location,
                            item: p.item,
                            receiver: p.receiver,
                            flags: p.flags,
                        })
                        .collect(),
                    slot_data: s.slot_data()?,
                    start_inventory: s.start_inventory().collect(),
                })
            })
            .collect::<Result<_, BundleError>>()?;
        Ok(SeedBundle {
            format: FORMAT,
            seed_name: self.seed_name().to_owned(),
            generator_version: self.generator_version(),
            server_options: self.server_options(),
            slots,
            connect_names: self.connect_names().map(|(n, t)| (n.to_owned(), t)).collect(),
            games: out,
            race_mode: self.race_mode(),
        })
    }

    /// Encode `b` (validated; `hash` is its `crate::bundle_hash`) and open it on this
    /// process's shared packages: how a v1 bundle loads.
    ///
    /// # Errors
    /// When it does not encode.
    pub fn from_v1(b: &SeedBundle, hash: &[u8; 32], max_bytes: u64) -> Result<Self, BundleError> {
        let Encoded { seed, packages } = encode(b, hash, max_bytes)?;
        let games = b
            .games
            .iter()
            .zip(&packages)
            .map(|((name, g), c)| {
                let key = GamePackage::key_of(c);
                package::shared(&key, || Ok(GamePackage::new(key, name, g)))
            })
            .collect::<Result<_, _>>()?;
        Self::open(seed, games, max_bytes)
    }

    /// Open a v2 file's `bytes`, loading its packages from `dir` (or this process's copies).
    ///
    /// # Errors
    /// A malformed seed, or a package that is missing or does not match its key.
    pub fn read(bytes: Vec<u8>, dir: &Path, max_bytes: u64) -> Result<Self, BundleError> {
        let games = package_keys(&bytes)?
            .iter()
            .map(|key| {
                package::shared(key, || {
                    let (name, g) = GamePackage::parse(&package::read(dir, key, max_bytes)?)?;
                    Ok(GamePackage::new(*key, &name, &g))
                })
            })
            .collect::<Result<_, _>>()?;
        Self::open(bytes, games, max_bytes)
    }
}

impl<'a> SlotView<'a> {
    fn at(&self) -> usize {
        self.seed.slots_at() + SLOT * self.pos
    }

    fn r(&self) -> Reader<'a> {
        Reader(&self.seed.bytes)
    }

    fn field(&self, off: usize) -> u32 {
        self.r().u32(self.at() + off)
    }

    /// Position in slot order.
    #[must_use]
    pub const fn pos(&self) -> usize {
        self.pos
    }

    /// Slot id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.field(0)
    }

    /// Team.
    #[must_use]
    pub fn team(&self) -> u32 {
        self.field(4)
    }

    /// Player name.
    #[must_use]
    pub fn name(&self) -> &'a str {
        self.seed.str_at(self.at() + 8).unwrap_or_default()
    }

    /// The slot's game.
    #[must_use]
    pub fn package(&self) -> &'a GamePackage {
        let g = usize::from(self.r().u16(self.at() + 16));
        self.seed
            .games
            .get(g)
            .or_else(|| self.seed.games.first())
            .map_or_else(|| unreachable_package(), |g| &**g)
    }

    /// The game's name.
    #[must_use]
    pub fn game(&self) -> &'a str {
        self.package().name()
    }

    /// Slot kind.
    #[must_use]
    pub fn kind(&self) -> SlotKind {
        byte_kind(self.r().u8(self.at() + 18)).unwrap_or(SlotKind::Spectator)
    }

    /// Oldest client version the world accepts.
    #[must_use]
    pub fn minimum_client_version(&self) -> Version {
        Version { major: self.field(20), minor: self.field(24), build: self.field(28) }
    }

    /// Group members (slot ids).
    #[must_use]
    pub fn group_members(&self) -> Vec<u32> {
        let (at, n) = (self.field(56) as usize, self.field(60) as usize);
        (0..n).map(|k| self.r().u32(at + 4 * k)).collect()
    }

    fn start_indexes(&self) -> impl Iterator<Item = usize> + use<'a> {
        let (at, n) = (self.field(64) as usize, self.field(68) as usize);
        let bytes = self.seed.bytes.get(at..).unwrap_or_default();
        let list = bits::PackedRef::from_bytes(bytes, start_width(self.seed.widths().0), n);
        list.into_iter().flat_map(bits::PackedRef::iter).map(|v| v as usize)
    }

    /// Starting inventory: item ids of this slot's game, in order.
    pub fn start_inventory(&self) -> impl Iterator<Item = i64> + use<'a> {
        let items = self.package().items();
        self.start_indexes().map(move |i| items.get(i).copied().unwrap_or(0))
    }

    /// `slot_data` as JSON (inflated here: it is read only on `Connect`).
    ///
    /// # Errors
    /// When the stored bytes do not decode.
    pub fn slot_data(&self) -> Result<serde_json::Value, BundleError> {
        let (at, size, raw) = (self.field(44) as usize, self.field(48) as usize, self.field(52));
        let blob = self.seed.bytes.get(at..at + size).ok_or_else(|| bad("slot_data range"))?;
        let parse =
            |b: &[u8]| serde_json::from_slice(b).map_err(|e| BundleError::Parse(e.to_string()));
        if raw == 0 { parse(blob) } else { parse(&package::inflate(blob, u64::from(raw))?) }
    }

    /// Placed locations.
    #[must_use]
    pub fn placed(&self) -> usize {
        self.field(36) as usize
    }

    fn presence(&self) -> &'a [u8] {
        if self.placed() == 0 {
            return &[];
        }
        let at = self.field(32) as usize;
        let words = self.package().locations().len().div_ceil(64);
        self.seed.bytes.get(at..at + 8 * words).unwrap_or_default()
    }

    fn word(presence: &[u8], k: usize) -> u64 {
        Reader(presence).u64(8 * k)
    }

    /// (item index, receiver position, flags) of record `i`.
    fn record(&self, i: usize) -> (usize, usize, u8) {
        let (wi, wr, wf) = self.seed.widths();
        let bytes = self.seed.bytes.get(self.field(40) as usize..).unwrap_or_default();
        let bit = i * usize::from(wi + wr + wf);
        let item = bits::read(bytes, bit, wi) as usize;
        let to = bits::read(bytes, bit + usize::from(wi), wr) as usize;
        let flags = bits::read(bytes, bit + usize::from(wi + wr), wf) as u8;
        (item, to, flags)
    }

    /// The index of location id `location`: its rank among the slot's placed locations.
    #[must_use]
    pub fn find(&self, location: i64) -> Option<usize> {
        let at = self.package().location_index(location)?;
        let p = self.presence();
        let word = Self::word(p, at / 64);
        if word >> (at % 64) & 1 == 0 {
            return None;
        }
        let below = self.ranks().get(at / 64).copied()?;
        let mask = (1_u64 << (at % 64)) - 1;
        Some((below + (word & mask).count_ones()) as usize)
    }

    /// Location ids of every placed location, ascending.
    pub fn locations(&self) -> impl Iterator<Item = i64> + use<'a> {
        let p = self.presence();
        let ids = self.package().locations();
        let words = p.len() / 8;
        (0..words).flat_map(move |k| {
            let w = Self::word(p, k);
            (0..64)
                .filter(move |b| w >> b & 1 == 1)
                .filter_map(move |b| ids.get(k * 64 + b).copied())
        })
    }

    /// The location id at index `i`: the `i`th set bit of the presence set, found by
    /// skipping whole words by their bit counts.
    #[must_use]
    pub fn location(&self, i: usize) -> Option<i64> {
        if i >= self.placed() {
            return None;
        }
        let ranks = self.ranks();
        let i = u32::try_from(i).ok()?;
        let k = ranks.partition_point(|&r| r <= i).checked_sub(1)?;
        let mut w = Self::word(self.presence(), k);
        for _ in 0..i - ranks.get(k)? {
            w &= w.wrapping_sub(1);
        }
        self.package().locations().get(k * 64 + w.trailing_zeros() as usize).copied()
    }

    /// The set bits before each presence word of this slot.
    fn ranks(&self) -> &'a [u32] {
        let s = self.seed;
        let (a, b) = (s.rank_at.get(self.pos), s.rank_at.get(self.pos + 1));
        match (a, b) {
            (Some(&a), Some(&b)) => s.ranks.get(a as usize..b as usize).unwrap_or_default(),
            _ => &[],
        }
    }

    /// The flags of placement `i`.
    #[must_use]
    pub fn flags(&self, i: usize) -> Option<u8> {
        (i < self.placed()).then(|| self.record(i).2)
    }

    /// Placement `i` in protocol ids, given its location id.
    fn resolve(&self, i: usize, location: i64) -> Placement {
        let (item, to, flags) = self.record(i);
        let receiver = self.seed.slot_at(to);
        let item = receiver.and_then(|r| r.package().items().get(item).copied()).unwrap_or(0);
        Placement { location, item, receiver: receiver.map_or(0, |r| r.id()), flags }
    }

    /// Placement `i`.
    #[must_use]
    pub fn placement(&self, i: usize) -> Option<Placement> {
        Some(self.resolve(i, self.location(i)?))
    }

    /// Every placement, by location id.
    pub fn placements(&self) -> impl Iterator<Item = Placement> + use<'a> {
        let me = *self;
        self.locations().enumerate().map(move |(i, l)| me.resolve(i, l))
    }

    /// Receiver position and item id of placement `i`.
    #[must_use]
    pub fn target(&self, i: usize) -> Option<(usize, i64)> {
        if i >= self.placed() {
            return None;
        }
        let (item, to, _) = self.record(i);
        Some((to, *self.seed.slot_at(to)?.package().items().get(item)?))
    }
}

/// `Seed::open` refuses a seed with no games, so a slot always has one.
fn unreachable_package() -> &'static GamePackage {
    static EMPTY: std::sync::OnceLock<GamePackage> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| {
        let empty = GameData {
            item_name_to_id: BTreeMap::new(),
            location_name_to_id: BTreeMap::new(),
            checksum: String::new(),
            item_name_groups: BTreeMap::new(),
            location_name_groups: BTreeMap::new(),
        };
        GamePackage::new([0; 32], "", &empty)
    })
}

impl World for Seed {
    fn slots(&self) -> &[SlotNo] {
        &self.ids
    }

    fn placed(&self, pos: usize) -> usize {
        self.slot_at(pos).map_or(0, |s| s.placed())
    }

    fn find(&self, pos: usize, location: i64) -> Option<usize> {
        self.slot_at(pos)?.find(location)
    }

    fn target(&self, pos: usize, index: usize) -> Option<(usize, i64)> {
        self.slot_at(pos)?.target(index)
    }
}

#[cfg(test)]
mod tests;
