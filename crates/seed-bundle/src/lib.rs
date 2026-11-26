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
pub const MAX_ID: i64 = (1 << 53) - 1;

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
        let v = serde_json::to_value(self).map_err(|e| e.to_string())?;
        Ok(Sha256::digest(package::canonical(v)).into())
    }

    /// [`SeedBundle::hash`] with the canonical JSON of the bundle's `games` given (`games`: the
    /// whole map, as [`package::canonical`] writes it), so the names and ids of every game,
    /// which are the same for every seed of a world, are hashed from bytes made once. This
    /// bundle's own `games` are not read: take them out first and nothing of them is written.
    ///
    /// # Errors
    /// Never in practice (a bundle always has a JSON form).
    pub fn hash_with_games(&self, games: &[u8]) -> Result<[u8; 32], String> {
        let mut v = serde_json::to_value(self).map_err(|e| e.to_string())?;
        v.sort_all_objects();
        let mut h = Sha256::new();
        for (n, (key, value)) in v.as_object().into_iter().flatten().enumerate() {
            h.update(if n == 0 { b"{" } else { b"," });
            h.update(serde_json::Value::from(key.as_str()).to_string());
            h.update(b":");
            if key == "games" {
                h.update(games);
            } else {
                h.update(value.to_string());
            }
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
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for s in &self.slots {
            ensure!(s.id >= 1, "slot id 0 is reserved for the server");
            ensure!(ids.insert(s.id), "slot id {} repeats", s.id);
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
        let ids_of =
            |pick: fn(&GameData) -> &BTreeMap<String, i64>| -> BTreeMap<&str, BTreeSet<i64>> {
                self.games
                    .iter()
                    .map(|(n, g)| (n.as_str(), pick(g).values().copied().collect()))
                    .collect()
            };
        let items = ids_of(|g| &g.item_name_to_id);
        let locations = ids_of(|g| &g.location_name_to_id);
        for s in &self.slots {
            for m in &s.group_members {
                ensure!(ids.contains(m), "group {} names missing slot {m}", s.id);
            }
            self.validate_locations(s, &items, &locations)?;
            let own = items.get(s.game.as_str());
            if let Some(i) = s.start_inventory.iter().find(|i| !own.is_some_and(|g| g.contains(i)))
            {
                return Err(BundleError::Invalid(format!(
                    "slot {} starts with item {i}, which game {:?} lacks",
                    s.id, s.game
                )));
            }
        }
        for h in &self.start_hints {
            let world = self.slot(h.finding).map(|s| &s.locations);
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
                self.slot(*slot).is_some_and(|s| s.team == *team),
                "connect name {name:?} points at no slot"
            );
        }
        Ok(())
    }

    fn validate_locations(
        &self,
        s: &Slot,
        items: &BTreeMap<&str, BTreeSet<i64>>,
        locations: &BTreeMap<&str, BTreeSet<i64>>,
    ) -> Result<(), BundleError> {
        let none = BTreeSet::new();
        let own = locations.get(s.game.as_str()).unwrap_or(&none);
        let mut seen = BTreeSet::new();
        for l in s.entrances.keys() {
            ensure!(
                s.locations.iter().any(|p| p.location == *l),
                "slot {} entrance for location {l}, which its world lacks",
                s.id
            );
        }
        for p in &s.locations {
            ensure!(seen.insert(p.location), "slot {} location {} repeats", s.id, p.location);
            ensure!(
                own.contains(&p.location),
                "slot {} location {} is not in game {:?}",
                s.id,
                p.location,
                s.game
            );
            ensure!(
                p.flags <= 0b111,
                "slot {} location {} has flags {}",
                s.id,
                p.location,
                p.flags
            );
            let receiver = self.slot(p.receiver);
            ensure!(
                receiver.is_some(),
                "slot {} location {} sends to missing slot {}",
                s.id,
                p.location,
                p.receiver
            );
            let known = receiver
                .and_then(|r| items.get(r.game.as_str()))
                .is_some_and(|set| set.contains(&p.item));
            ensure!(
                known,
                "slot {} location {} places item {} that slot {}'s game lacks",
                s.id,
                p.location,
                p.item,
                p.receiver
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
