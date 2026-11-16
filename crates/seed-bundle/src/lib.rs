//! The seed bundle: one room's fixed data, as written by `tools/ap-import` from a seed made by
//! Archipelago's generator (the legacy sidecar of D-04, D-100). One source of truth for the
//! schema; [`SeedBundle::from_json`] is the only way in and validates everything a room trusts.
//!
//! The per-game data package, `slot_data` and the server options are kept because a client
//! compatible with Archipelago asks for them; only `edge-compat` interprets them.

use proto::SlotNo;
use room_core::Placement;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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
}

/// One game's names and ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameData {
    /// Item name to id.
    pub item_name_to_id: BTreeMap<String, i64>,
    /// Location name to id.
    pub location_name_to_id: BTreeMap<String, i64>,
    /// The generator's checksum of this data package.
    pub checksum: String,
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

    fn validate(&self) -> Result<(), BundleError> {
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
