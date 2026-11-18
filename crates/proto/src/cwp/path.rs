//! The core state paths (doc 01 D1.10 rule 7): which paths exist, who may read them, and a
//! parser that works on segments, so permissions never depend on string prefixes.
use super::names::{ExtId, GameId, StatePath};
use crate::SlotNo;
use alloc::string::String;
use serde::Serialize;

/// Who may read a path, in the terms of rule 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Read {
    /// Every session in the room.
    Room,
    /// The path's slot and room roles.
    Subject,
    /// The path's slot and room roles; others only when the room's spoiler mode allows.
    SubjectOrSpoiler,
    /// The path's slot and room roles; others only when the seed marks it public.
    SubjectOrPublic,
    /// As the owning extension's spec says.
    Extension,
}

/// One registered path pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PathInfo {
    /// The pattern, with `{n}` for a slot number, `{id}` for a game id, `{scope}`, `{key}` and
    /// `{ext}` for an extension id.
    pub pattern: &'static str,
    /// Who may read it.
    pub read: Read,
    /// Whether the value comes in pages (`get` with `after` and `limit`).
    pub paged: bool,
    /// One line for the docs.
    pub summary: &'static str,
}

/// Every core path of CWP v1.
pub const CORE_PATHS: &[PathInfo] = &[
    PathInfo {
        pattern: "room/info",
        read: Read::Room,
        paged: false,
        summary: "Seed name, room state, spoiler and race mode, hint settings, release and collect modes, server time.",
    },
    PathInfo {
        pattern: "room/slots",
        read: Read::Room,
        paged: false,
        summary: "Every slot: number, name, alias, game id, kind, group members.",
    },
    PathInfo {
        pattern: "slot/{n}/status",
        read: Read::Room,
        paged: false,
        summary: "Connection, the client's own state claim, goal.",
    },
    PathInfo {
        pattern: "slot/{n}/locations",
        read: Read::Room,
        paged: false,
        summary: "Checked and missing location ids.",
    },
    PathInfo {
        pattern: "slot/{n}/inventory",
        read: Read::Subject,
        paged: true,
        summary: "Items granted, in item_seq order.",
    },
    PathInfo {
        pattern: "slot/{n}/slot_data",
        read: Read::SubjectOrPublic,
        paged: true,
        summary: "The slot's generator data.",
    },
    PathInfo {
        pattern: "slot/{n}/hints",
        read: Read::Subject,
        paged: false,
        summary: "Hints where the slot finds or receives the item.",
    },
    PathInfo {
        pattern: "game/{id}/names",
        read: Read::Room,
        paged: true,
        summary: "Name tables; fallback for the HTTP form.",
    },
    PathInfo {
        pattern: "game/{id}/groups",
        read: Read::Room,
        paged: false,
        summary: "Item and location name groups.",
    },
    PathInfo {
        pattern: "kv/{scope}/{key}",
        read: Read::Extension,
        paged: false,
        summary: "Shared storage of org.cwp.kv.",
    },
    PathInfo {
        pattern: "ext/{ext}/...",
        read: Read::Extension,
        paged: false,
        summary: "State an extension declares.",
    },
];

/// The part of a slot a `slot/{n}/...` path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotPart {
    /// `status`
    Status,
    /// `locations`
    Locations,
    /// `inventory`
    Inventory,
    /// `slot_data`
    SlotData,
    /// `hints`
    Hints,
}

/// The part of a game a `game/{id}/...` path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamePart {
    /// `names`
    Names,
    /// `groups`
    Groups,
}

/// A `kv` scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvScope {
    /// Shared by the room.
    Room,
    /// One slot's.
    Slot(SlotNo),
    /// Keys written by the compat edge for existing clients.
    Legacy,
}

/// A parsed core path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorePath {
    /// `room/info`
    RoomInfo,
    /// `room/slots`
    RoomSlots,
    /// `slot/{n}/...`
    Slot(SlotNo, SlotPart),
    /// `game/{id}/...`
    Game(GameId, GamePart),
    /// `kv/{scope}/{key}`; the key is still percent-encoded.
    Kv(KvScope, String),
    /// `ext/{ext}/...`; the rest is still percent-encoded and belongs to the extension.
    Ext(ExtId, String),
}

/// A path that is well formed but not registered (`path.unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownPath;

/// Canonical decimal: `0`, or digits with no leading zero, that fits a slot number.
fn slot_number(s: &str) -> Option<SlotNo> {
    let canonical =
        s == "0" || (!s.starts_with('0') && !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()));
    if !canonical {
        return None;
    }
    s.parse().ok().map(SlotNo)
}

impl CorePath {
    /// Parses a path against the registered patterns.
    ///
    /// # Errors
    /// [`UnknownPath`] when no pattern matches.
    pub fn parse(path: &StatePath) -> Result<Self, UnknownPath> {
        let mut seg = path.segments();
        let first = seg.next().ok_or(UnknownPath)?;
        let rest: alloc::vec::Vec<&str> = seg.collect();
        let parsed = match (first, rest.as_slice()) {
            ("room", ["info"]) => Self::RoomInfo,
            ("room", ["slots"]) => Self::RoomSlots,
            ("slot", [n, part]) => {
                let n = slot_number(n).ok_or(UnknownPath)?;
                let part = match *part {
                    "status" => SlotPart::Status,
                    "locations" => SlotPart::Locations,
                    "inventory" => SlotPart::Inventory,
                    "slot_data" => SlotPart::SlotData,
                    "hints" => SlotPart::Hints,
                    _ => return Err(UnknownPath),
                };
                Self::Slot(n, part)
            }
            ("game", [id, part]) => {
                let id = GameId::new(*id).map_err(|_| UnknownPath)?;
                let part = match *part {
                    "names" => GamePart::Names,
                    "groups" => GamePart::Groups,
                    _ => return Err(UnknownPath),
                };
                Self::Game(id, part)
            }
            ("kv", [scope, key]) => {
                let scope = match *scope {
                    "room" => KvScope::Room,
                    "legacy" => KvScope::Legacy,
                    s => KvScope::Slot(
                        s.strip_prefix("slot.").and_then(slot_number).ok_or(UnknownPath)?,
                    ),
                };
                Self::Kv(scope, String::from(*key))
            }
            ("ext", [ext, more @ ..]) if !more.is_empty() => {
                let ext = ExtId::new(*ext).map_err(|_| UnknownPath)?;
                Self::Ext(ext, more.join("/"))
            }
            _ => return Err(UnknownPath),
        };
        Ok(parsed)
    }

    /// The registry entry this path matches (always `Some`; the test
    /// `every_parsed_path_has_a_registry_row` checks it).
    #[must_use]
    pub fn info(&self) -> Option<&'static PathInfo> {
        let pattern = match self {
            Self::RoomInfo => "room/info",
            Self::RoomSlots => "room/slots",
            Self::Slot(_, SlotPart::Status) => "slot/{n}/status",
            Self::Slot(_, SlotPart::Locations) => "slot/{n}/locations",
            Self::Slot(_, SlotPart::Inventory) => "slot/{n}/inventory",
            Self::Slot(_, SlotPart::SlotData) => "slot/{n}/slot_data",
            Self::Slot(_, SlotPart::Hints) => "slot/{n}/hints",
            Self::Game(_, GamePart::Names) => "game/{id}/names",
            Self::Game(_, GamePart::Groups) => "game/{id}/groups",
            Self::Kv(..) => "kv/{scope}/{key}",
            Self::Ext(..) => "ext/{ext}/...",
        };
        CORE_PATHS.iter().find(|p| p.pattern == pattern)
    }

    /// The slot a `slot/{n}/...` or `kv/slot.{n}/...` path is about.
    #[must_use]
    pub const fn subject(&self) -> Option<SlotNo> {
        match self {
            Self::Slot(n, _) | Self::Kv(KvScope::Slot(n), _) => Some(*n),
            _ => None,
        }
    }
}
