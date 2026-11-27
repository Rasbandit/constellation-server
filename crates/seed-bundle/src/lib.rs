//! The seed bundle: one room's fixed data, as written by `tools/ap-import` from a seed made by
//! Archipelago's generator (the legacy sidecar of D-04, D-100). One source of truth for the
//! schema; [`SeedBundle::from_json`] is the only way in and validates everything a room trusts.
//!
//! The per-game data package, `slot_data` and the server options are kept because a client
//! compatible with Archipelago asks for them; only `edge-compat` interprets them.
//!
//! [`SeedBundle`] is the v1 schema (JSON, what `tools/ap-import` writes). Rooms run on the v2
//! form, [`Seed`]: a compact binary file read in place ([`v2`]), with each game's data package
//! stored once per content hash and shared across rooms ([`package`]). [`import_v1`] turns a
//! v1 file into both. No IO here: the server keeps seeds and packages in its store.

pub mod package;
pub mod v2;

pub use package::GamePackage;
pub use v2::{Seed, SlotView};

use proto::SlotNo;
use room_core::{CoreError, Placement, RoomState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

/// The bundle format this crate reads.
pub const FORMAT: u32 = 1;

/// Largest id magnitude a JSON number carries exactly (2^53 - 1); the protocol document's range.
pub const MAX_ID: i64 = config::ID_MAX;

/// One room's seed data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedBundle {
    /// Always [`FORMAT`].
    pub format: u32,
    /// The generator's name for this seed.
    pub seed_name: String,
    /// The generator's version.
    pub generator_version: Version,
    /// Server options chosen when the seed was made.
    pub server_options: ServerOptions,
    /// Every slot, in slot order.
    pub slots: Vec<Slot>,
    /// Names a client may connect with, to (team, slot).
    pub connect_names: BTreeMap<String, (u32, u32)>,
    /// Data package per game name.
    pub games: BTreeMap<String, GameData>,
    /// The generator's race mode flag (0 or 1); clients read it as `_read_race_mode`.
    #[serde(default)]
    pub race_mode: u8,
    /// Hints every room of this seed starts with (the generator's start hints and start
    /// location hints), once each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub start_hints: Vec<StartHint>,
}

/// A hint the seed starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartHint {
    /// The slot whose world holds the location.
    pub finding: u32,
    /// The location.
    pub location: i64,
    /// Its status (protocol doc, "`HintStatus`").
    pub status: u8,
}

/// A three-part version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Build.
    pub build: u32,
}

/// When a player may release or collect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Never.
    Disabled,
    /// At any time.
    Enabled,
    /// Only after the player's goal.
    Goal,
    /// Automatically at the player's goal.
    Auto,
    /// Automatically at the goal, and by hand at any time.
    AutoEnabled,
}

/// Server options carried from the seed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerOptions {
    /// Room password; `None` means open.
    pub password: Option<String>,
    /// Percent of a slot's locations a hint costs, 0 to 100.
    pub hint_cost: u8,
    /// Hint points earned per checked location.
    pub location_check_points: u32,
    /// Release permission.
    pub release_mode: Mode,
    /// Collect permission.
    pub collect_mode: Mode,
    /// Remaining-items query permission (`auto` is not valid here).
    pub remaining_mode: Mode,
}

/// What kind of slot this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    /// Watches only.
    Spectator,
    /// Plays a world.
    Player,
    /// An item group of several players.
    Group,
}

/// One slot and its world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    /// Slot number, from 1, unique in the room.
    pub id: u32,
    /// Team, from 0.
    pub team: u32,
    /// Player name as generated.
    pub name: String,
    /// Game name; a key of [`SeedBundle::games`].
    pub game: String,
    /// Slot kind.
    pub kind: SlotKind,
    /// Oldest client version the world accepts.
    pub minimum_client_version: Version,
    /// Group members (slot ids) for a group slot.
    #[serde(default)]
    pub group_members: Vec<u32>,
    /// Every location in this slot's world and what is placed there.
    pub locations: Vec<Placed>,
    /// Game-defined data handed to the client on connect (a JSON object).
    pub slot_data: serde_json::Value,
    /// Starting inventory: item ids of this slot's game, in the generator's order, repeats
    /// allowed. Sent only to clients that ask for it (`items_handling` bit 2), before every
    /// received item, as from location -2 and player 0 (protocol doc, "Data Package Contents").
    #[serde(default)]
    pub start_inventory: Vec<i64>,
    /// Location id to the entrance a hint names for it (worlds with shuffled entrances). The
    /// names are per seed, so the v2 seed keeps them in its own string table.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entrances: BTreeMap<i64, String>,
}

/// An item placed at a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placed {
    /// Location id in the owning slot's world.
    pub location: i64,
    /// Item id in the receiving slot's world.
    pub item: i64,
    /// Receiving slot.
    pub receiver: u32,
    /// Item classification bits: 1 progression, 2 useful, 4 trap.
    pub flags: u8,
    /// The generator's sphere of this location, from 0 (`!hint` prefers early ones); `None`
    /// when the seed records none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sphere: Option<u32>,
}

/// One game's names and ids.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameData {
    /// Item name to id.
    pub item_name_to_id: BTreeMap<String, i64>,
    /// Location name to id.
    pub location_name_to_id: BTreeMap<String, i64>,
    /// The generator's checksum of this data package.
    pub checksum: String,
    /// Item group name to item names, read by clients as `_read_item_name_groups_<game>`. Not
    /// part of the data package sent to clients.
    #[serde(default)]
    pub item_name_groups: BTreeMap<String, Vec<String>>,
    /// Location group name to location names (`_read_location_name_groups_<game>`).
    #[serde(default)]
    pub location_name_groups: BTreeMap<String, Vec<String>>,
    /// Item and location names the world forbids hinting (the same for every seed of the
    /// game, so it lives in the package).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub hint_blacklist: BTreeSet<String>,
}

impl GameData {
    /// Bytes this data holds, itself and on the heap: every name's bytes, and its maps' nodes
    /// as an upper bound (`package::map_bytes`). What a cache that keeps it counts.
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        let ids = |m: &BTreeMap<String, i64>| package::map_bytes(m, |k, _| k.capacity());
        let groups = |m: &BTreeMap<String, Vec<String>>| {
            package::map_bytes(m, |k, v| {
                k.capacity()
                    + v.capacity() * size_of::<String>()
                    + v.iter().map(String::capacity).sum::<usize>()
            })
        };
        size_of::<Self>()
            + ids(&self.item_name_to_id)
            + ids(&self.location_name_to_id)
            + self.checksum.capacity()
            + groups(&self.item_name_groups)
            + groups(&self.location_name_groups)
            + self
                .hint_blacklist
                .iter()
                .map(|n| n.capacity() + 2 * size_of::<String>())
                .sum::<usize>()
    }
}

/// Why a bundle was refused. Names the first rule that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    /// Not valid JSON, or not the schema.
    Parse(String),
    /// Valid JSON that breaks a rule.
    Invalid(String),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(m) => write!(f, "seed bundle does not parse: {m}"),
            Self::Invalid(m) => write!(f, "seed bundle is invalid: {m}"),
        }
    }
}

impl std::error::Error for BundleError {}

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(BundleError::Invalid(format!($($arg)+)));
        }
    };
}

/// SHA-256 of a bundle file's canonical JSON.
///
/// Canonical: parsed as plain JSON, every object's keys sorted, no whitespace. It depends on the
/// file's content only, never on the `SeedBundle` type, so a new defaulted field in the type
/// does not change a stored room's hash. A v2 seed carries the hash of the v1 file it was made
/// from.
///
/// # Errors
/// When the text is not JSON.
pub fn bundle_hash(text: &str) -> Result<Vec<u8>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(Sha256::digest(package::canonical(v)).to_vec())
}

/// Import a v1 bundle file (JSON): validate it and encode it to v2, on this process's shared
/// packages. Returns the seed (its hash is the v1 file's [`bundle_hash`]) and its package
/// files, for the caller to store. `max_bytes` caps the seed.
///
/// # Errors
/// A message naming the first problem.
pub fn import_v1(bytes: &[u8], max_bytes: u64) -> Result<(Seed, Vec<Vec<u8>>), String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("seed bundle is not UTF-8: {e}"))?;
    let bundle = SeedBundle::from_json(text).map_err(|e| e.to_string())?;
    let hash: [u8; 32] = bundle_hash(text)?.try_into().map_err(|_| "hash length")?;
    Seed::from_v1(&bundle, &hash, max_bytes).map_err(|e| e.to_string())
}

/// A room's state before any event, on its seed: the placement, the hint economy and the
/// start hints. The one place a room is built from a seed.
///
/// # Errors
/// What room-core refuses (`Seed::open` rules most of it out).
pub fn room_state(seed: &Arc<Seed>) -> Result<RoomState, CoreError> {
    let world: Arc<dyn room_core::World> = Arc::clone(seed) as _;
    RoomState::from_world(world)?.with_hints(seed.hint_rules(), seed.start_hints())
}

fn id_ok(id: i64) -> bool {
    (-MAX_ID..=MAX_ID).contains(&id)
}

/// Write `key` (a JSON key with what stands before it) and the canonical JSON of `value` into
/// the hash. For the small parts of a bundle: the value is built as a JSON tree.
fn member(h: &mut Sha256, key: &str, value: &impl Serialize) -> Result<(), String> {
    h.update(key);
    h.update(package::canonical(serde_json::to_value(value).map_err(|e| e.to_string())?));
    Ok(())
}

/// A [`Placed`] as its canonical JSON has it: the keys in sorted order.
#[derive(Serialize)]
struct CanonicalPlaced {
    flags: u8,
    item: i64,
    location: i64,
    receiver: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    sphere: Option<u32>,
}

impl Slot {
    /// This slot's canonical JSON into the hash. `text` is scratch space, kept between slots.
    fn hash_into(&self, h: &mut Sha256, text: &mut Vec<u8>) -> Result<(), String> {
        // Every field is named, so a new one does not compile until it is hashed here.
        let Self {
            id,
            team,
            name,
            game,
            kind,
            minimum_client_version,
            group_members,
            locations,
            slot_data,
            start_inventory,
            entrances,
        } = self;
        if entrances.is_empty() {
            h.update(b"{");
        } else {
            member(h, "{\"entrances\":", entrances)?;
            h.update(b",");
        }
        member(h, "\"game\":", game)?;
        member(h, ",\"group_members\":", group_members)?;
        member(h, ",\"id\":", id)?;
        member(h, ",\"kind\":", kind)?;
        // The placements, written as text with no tree: numbers only, so nothing to sort.
        text.clear();
        text.extend_from_slice(b",\"locations\":[");
        for (k, &Placed { location, item, receiver, flags, sphere }) in locations.iter().enumerate()
        {
            if k > 0 {
                text.push(b',');
            }
            let p = CanonicalPlaced { flags, item, location, receiver, sphere };
            serde_json::to_writer(&mut *text, &p).map_err(|e| e.to_string())?;
        }
        text.push(b']');
        h.update(&*text);
        member(h, ",\"minimum_client_version\":", minimum_client_version)?;
        member(h, ",\"name\":", name)?;
        member(h, ",\"slot_data\":", slot_data)?;
        member(h, ",\"start_inventory\":", start_inventory)?;
        member(h, ",\"team\":", team)?;
        h.update(b"}");
        Ok(())
    }
}

impl SeedBundle {
    /// Parse and validate.
    ///
    /// # Errors
    /// [`BundleError`] for bad JSON or the first broken rule.
    pub fn from_json(text: &str) -> Result<Self, BundleError> {
        let b: Self = serde_json::from_str(text).map_err(|e| BundleError::Parse(e.to_string()))?;
        b.validate()?;
        Ok(b)
    }

    /// The slot with this id.
    #[must_use]
    pub fn slot(&self, id: u32) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }

    /// The placement table room-core runs on.
    #[must_use]
    pub fn placement(&self) -> Placement {
        self.slots
            .iter()
            .map(|s| {
                let world = s
                    .locations
                    .iter()
                    .map(|p| (p.location, (SlotNo(p.receiver), p.item)))
                    .collect();
                (SlotNo(s.id), world)
            })
            .collect()
    }

    /// Every slot number.
    pub fn slot_nos(&self) -> impl Iterator<Item = SlotNo> + '_ {
        self.slots.iter().map(|s| SlotNo(s.id))
    }

    /// The [`bundle_hash`] of this bundle's JSON text, from the bundle itself: the canonical
    /// form is built from its values, so no text is written and parsed back.
    ///
    /// # Errors
    /// Never in practice (a bundle always has a JSON form).
    pub fn hash(&self) -> Result<[u8; 32], String> {
        let games = serde_json::to_value(&self.games).map_err(|e| e.to_string())?;
        self.hash_with_games(&package::canonical(games))
    }

    /// [`SeedBundle::hash`] with the canonical JSON of the bundle's `games` given (`games`: the
    /// whole map, as [`package::canonical`] writes it), so the names and ids of every game,
    /// which are the same for every seed of a world, are hashed from bytes made once. This
    /// bundle's own `games` are not read: take them out first and nothing of them is written.
    ///
    /// The canonical text goes into the hasher a member at a time, keys in sorted order, and
    /// no JSON tree is built of the placements (most of a seed).
    ///
    /// # Errors
    /// Never in practice (a bundle always has a JSON form).
    pub fn hash_with_games(&self, games: &[u8]) -> Result<[u8; 32], String> {
        // Every field is named, so a new one does not compile until it is hashed here.
        let Self {
            format,
            seed_name,
            generator_version,
            server_options,
            slots,
            connect_names,
            games: _,
            race_mode,
            start_hints,
        } = self;
        let mut h = Sha256::new();
        member(&mut h, "{\"connect_names\":", connect_names)?;
        member(&mut h, ",\"format\":", format)?;
        h.update(b",\"games\":");
        h.update(games);
        member(&mut h, ",\"generator_version\":", generator_version)?;
        member(&mut h, ",\"race_mode\":", race_mode)?;
        member(&mut h, ",\"seed_name\":", seed_name)?;
        member(&mut h, ",\"server_options\":", server_options)?;
        let mut text = Vec::new();
        h.update(b",\"slots\":[");
        for (k, s) in slots.iter().enumerate() {
            if k > 0 {
                h.update(b",");
            }
            s.hash_into(&mut h, &mut text)?;
        }
        h.update(b"]");
        if !start_hints.is_empty() {
            member(&mut h, ",\"start_hints\":", start_hints)?;
        }
        h.update(b"}");
        Ok(h.finalize().into())
    }

    /// Check every rule a room trusts (what [`SeedBundle::from_json`] checks after parsing): a
    /// bundle built in this process is checked with this before it is encoded.
    ///
    /// # Errors
    /// [`BundleError::Invalid`] naming the first broken rule.
    pub fn validate(&self) -> Result<(), BundleError> {
        ensure!(self.format == FORMAT, "format {} is not {FORMAT}", self.format);
        ensure!(!self.slots.is_empty(), "no slots");
        ensure!(self.server_options.hint_cost <= 100, "hint_cost above 100 percent");
        ensure!(self.server_options.remaining_mode != Mode::Auto, "remaining_mode cannot be auto");
        ensure!(
            self.server_options.remaining_mode != Mode::AutoEnabled,
            "remaining_mode cannot be auto-enabled"
        );
        // Every slot by id, so nothing below scans the slots for one.
        let mut by_id = BTreeMap::new();
        let mut names = BTreeSet::new();
        for s in &self.slots {
            ensure!(s.id >= 1, "slot id 0 is reserved for the server");
            ensure!(by_id.insert(s.id, s).is_none(), "slot id {} repeats", s.id);
            ensure!(names.insert(s.name.as_str()), "slot name {:?} repeats", s.name);
            ensure!(
                self.games.contains_key(&s.game),
                "slot {} plays unknown game {:?}",
                s.id,
                s.game
            );
            ensure!(s.slot_data.is_object(), "slot {} slot_data is not an object", s.id);
            ensure!(
                s.kind != SlotKind::Group,
                "slot {} is a group slot (item link), which is not supported yet",
                s.id
            );
        }
        for (game, g) in &self.games {
            let all = g.item_name_to_id.values().chain(g.location_name_to_id.values());
            ensure!(all.copied().all(id_ok), "game {game:?} has an id outside +-(2^53 - 1)");
        }
        // Each game's ids, sorted to be searched.
        let ids_of = |pick: fn(&GameData) -> &BTreeMap<String, i64>| -> BTreeMap<&str, Vec<i64>> {
            let sorted = |g: &GameData| {
                let mut ids: Vec<i64> = pick(g).values().copied().collect();
                ids.sort_unstable();
                ids
            };
            self.games.iter().map(|(n, g)| (n.as_str(), sorted(g))).collect()
        };
        let items = ids_of(|g| &g.item_name_to_id);
        let locations = ids_of(|g| &g.location_name_to_id);
        // Each slot's game is resolved once: slot id to the item ids that slot can receive.
        let receives: BTreeMap<u32, &[i64]> =
            self.slots.iter().map(|s| (s.id, ids_in(&items, s))).collect();
        for s in &self.slots {
            for m in &s.group_members {
                ensure!(by_id.contains_key(m), "group {} names missing slot {m}", s.id);
            }
            validate_locations(s, &receives, ids_in(&locations, s))?;
            let own = ids_in(&items, s);
            if let Some(i) = s.start_inventory.iter().find(|i| own.binary_search(i).is_err()) {
                return Err(BundleError::Invalid(format!(
                    "slot {} starts with item {i}, which game {:?} lacks",
                    s.id, s.game
                )));
            }
        }
        for h in &self.start_hints {
            let world = by_id.get(&h.finding).map(|s| &s.locations);
            ensure!(
                world.is_some_and(|w| w.iter().any(|p| p.location == h.location)),
                "start hint for slot {} location {} is outside that world",
                h.finding,
                h.location
            );
            ensure!(room_core::valid_hint_status(h.status), "start hint has status {}", h.status);
        }
        for (name, (team, slot)) in &self.connect_names {
            ensure!(
                by_id.get(slot).is_some_and(|s| s.team == *team),
                "connect name {name:?} points at no slot"
            );
        }
        Ok(())
    }
}

/// The sorted ids of `s`'s game in `of` (none for a game the bundle lacks).
fn ids_in<'a>(of: &'a BTreeMap<&str, Vec<i64>>, s: &Slot) -> &'a [i64] {
    of.get(s.game.as_str()).map_or(&[], Vec::as_slice)
}

/// The rules of one slot's locations: `receives` is each slot's receivable item ids and `own`
/// the location ids of this slot's game, both sorted.
fn validate_locations(
    s: &Slot,
    receives: &BTreeMap<u32, &[i64]>,
    own: &[i64],
) -> Result<(), BundleError> {
    let mut placed: Vec<i64> = s.locations.iter().map(|p| p.location).collect();
    placed.sort_unstable();
    for l in s.entrances.keys() {
        ensure!(
            placed.binary_search(l).is_ok(),
            "slot {} entrance for location {l}, which its world lacks",
            s.id
        );
    }
    // A repeat shows as neighbours once sorted; only a slot that has one is walked with a set,
    // to name the first repeat in file order.
    let repeats = placed.windows(2).any(|w| w.first() == w.last());
    let mut seen = BTreeSet::new();
    for p in &s.locations {
        ensure!(
            !repeats || seen.insert(p.location),
            "slot {} location {} repeats",
            s.id,
            p.location
        );
        ensure!(
            own.binary_search(&p.location).is_ok(),
            "slot {} location {} is not in game {:?}",
            s.id,
            p.location,
            s.game
        );
        ensure!(p.flags <= 0b111, "slot {} location {} has flags {}", s.id, p.location, p.flags);
        let receiver = receives.get(&p.receiver);
        ensure!(
            receiver.is_some(),
            "slot {} location {} sends to missing slot {}",
            s.id,
            p.location,
            p.receiver
        );
        ensure!(
            receiver.is_some_and(|items| items.binary_search(&p.item).is_ok()),
            "slot {} location {} places item {} that slot {}'s game lacks",
            s.id,
            p.location,
            p.item,
            p.receiver
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
