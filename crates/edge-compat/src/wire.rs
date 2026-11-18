//! Packets of the Archipelago network protocol, as serde types.
//!
//! Written in our own words from the protocol document (`docs/network protocol.md` in the
//! Archipelago repository, read at the 0.6.8 checkout pinned by the owner's other project);
//! section names are cited per type.
//!
//! Every frame is a JSON list of objects, each with a `cmd` key. Client packets are parsed per
//! command so a bad packet gets the right `InvalidPacket` type; unknown fields are ignored, as
//! the document allows fields to be added.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// `NetworkVersion`. The `class` key lets the other side compare versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetVersion {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Build.
    pub build: u32,
    /// Always `Version` when we send it; ignored when we read it.
    #[serde(default)]
    pub class: String,
}

impl NetVersion {
    /// A version we send.
    #[must_use]
    pub fn new(major: u32, minor: u32, build: u32) -> Self {
        Self { major, minor, build, class: "Version".to_owned() }
    }

    /// For ordering comparisons.
    #[must_use]
    pub const fn tuple(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.build)
    }
}

/// `NetworkItem`. `player` is the finding slot, except in `LocationInfo` where it is the
/// receiving slot.
///
/// Sent with `"class": "NetworkItem"`, like `NetworkPlayer` and `NetworkSlot`: the reference
/// server tags these types (`NetUtils.py:98-107`) and its client library only turns tagged
/// objects into them (`NetUtils.py:147-170`). The protocol document does not mention the tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class")]
pub struct NetworkItem {
    /// Item id.
    pub item: i64,
    /// Location id.
    pub location: i64,
    /// Slot (see the type docs).
    pub player: u32,
    /// 1 progression, 2 useful, 4 trap.
    pub flags: u8,
}

/// `NetworkPlayer`, tagged with its class (see [`NetworkItem`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "class")]
pub struct NetworkPlayer {
    /// Team, from 0.
    pub team: u32,
    /// Slot, from 1.
    pub slot: u32,
    /// Current display name.
    pub alias: String,
    /// Name as generated.
    pub name: String,
}

/// `NetworkSlot`, tagged with its class (see [`NetworkItem`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "class")]
pub struct NetworkSlot {
    /// Player name.
    pub name: String,
    /// Game name.
    pub game: String,
    /// `SlotType`: 0 spectator, 1 player, 2 group.
    #[serde(rename = "type")]
    pub kind: u8,
    /// Members of a group slot.
    pub group_members: Vec<u32>,
}

/// `Permission` values for release, collect and remaining.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Permissions {
    /// Release.
    pub release: u8,
    /// Collect.
    pub collect: u8,
    /// Remaining.
    pub remaining: u8,
}

/// `JSONMessagePart`, the subset we send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextPart {
    /// `None` is plain text.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    /// Text, or the id as a decimal string for the id types.
    pub text: String,
    /// The slot an item or location id belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<u32>,
    /// Item flags, for `item_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u8>,
}

impl TextPart {
    /// Plain text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self { kind: None, text: text.into(), player: None, flags: None }
    }

    /// A slot reference, rendered by the client with the player's name.
    #[must_use]
    pub fn player(slot: u32) -> Self {
        Self { kind: Some("player_id"), text: slot.to_string(), player: None, flags: None }
    }

    /// An item of `receiver`'s world.
    #[must_use]
    pub fn item(item: i64, receiver: u32, flags: u8) -> Self {
        Self {
            kind: Some("item_id"),
            text: item.to_string(),
            player: Some(receiver),
            flags: Some(flags),
        }
    }

    /// A location of `owner`'s world.
    #[must_use]
    pub fn location(location: i64, owner: u32) -> Self {
        Self {
            kind: Some("location_id"),
            text: location.to_string(),
            player: Some(owner),
            flags: None,
        }
    }
}

/// `PrintJSON`. Only `data` is needed to show it; the rest depends on `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrintJson {
    /// `PrintJsonType`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The message.
    pub data: Value,
    /// `ItemSend`: the receiving slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiving: Option<u32>,
    /// `ItemSend`: the item, `player` being the finder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<NetworkItem>,
    /// `Join`, `Part`, `Chat`, `Goal`: the player's team.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<u32>,
    /// `Join`, `Part`, `Chat`, `Goal`: the player's slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<u32>,
    /// `Chat`: the text without the sender prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `Join`: the player's tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Server to client packets (section "(Server -> Client)"). `DataPackage` and `Bounced` are
/// assembled elsewhere: one from cached JSON, the other passes the sender's fields through.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "cmd")]
pub enum ServerPacket {
    /// `RoomInfo`.
    RoomInfo {
        /// Protocol level this server speaks.
        version: NetVersion,
        /// Version that generated the seed.
        generator_version: NetVersion,
        /// Server capability tags.
        tags: Vec<String>,
        /// A password is required.
        password: bool,
        /// Release, collect and remaining permissions.
        permissions: Permissions,
        /// Hint cost, percent.
        hint_cost: u8,
        /// Hint points per check.
        location_check_points: u32,
        /// Games in the room.
        games: Vec<String>,
        /// Data package checksum per game.
        datapackage_checksums: BTreeMap<String, String>,
        /// Seed name.
        seed_name: String,
        /// Unix time now, seconds.
        time: f64,
    },
    /// `ConnectionRefused`.
    ConnectionRefused {
        /// Error names from the document's list.
        errors: Vec<&'static str>,
    },
    /// `Connected`.
    Connected {
        /// Team.
        team: u32,
        /// Slot.
        slot: u32,
        /// Every player.
        players: Vec<NetworkPlayer>,
        /// The slot's unchecked locations.
        missing_locations: Vec<i64>,
        /// The slot's checked locations.
        checked_locations: Vec<i64>,
        /// Absent when the client asked for none.
        #[serde(skip_serializing_if = "Option::is_none")]
        slot_data: Option<Value>,
        /// Slot number (as a string key) to slot info.
        slot_info: BTreeMap<String, NetworkSlot>,
        /// Hint points.
        hint_points: i64,
    },
    /// `ReceivedItems`.
    ReceivedItems {
        /// Position of the first item in the client's item list.
        index: usize,
        /// The items.
        items: Vec<NetworkItem>,
    },
    /// `LocationInfo`.
    LocationInfo {
        /// Scouted items, `player` being the receiver.
        locations: Vec<NetworkItem>,
    },
    /// `RoomUpdate`, only the fields we change.
    RoomUpdate {
        /// Newly checked locations.
        checked_locations: Vec<i64>,
        /// Hint points after the change.
        hint_points: i64,
    },
    /// `PrintJSON`.
    #[serde(rename = "PrintJSON")]
    PrintJson(PrintJson),
    /// `InvalidPacket`.
    InvalidPacket {
        /// `PacketProblemType`: `cmd` or `arguments`.
        #[serde(rename = "type")]
        kind: &'static str,
        /// The faulty packet's `cmd`, when it had one.
        original_cmd: Option<String>,
        /// What was wrong.
        text: String,
    },
    /// `SetReply`: the body is built by the storage module (it passes request fields through).
    SetReply(Map<String, Value>),
    /// `Retrieved`, with the `Get` packet's extra fields passed through.
    Retrieved {
        /// Requested key to value (`null` when absent).
        keys: Map<String, Value>,
        /// Extra fields from the request.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
}

/// Section `Connect`.
#[derive(Debug, Clone, Deserialize)]
pub struct Connect {
    /// Room password.
    #[serde(default)]
    pub password: Option<String>,
    /// Game name; may be empty for trackers and text clients.
    #[serde(default)]
    pub game: String,
    /// Slot name.
    pub name: String,
    /// Client version.
    pub version: NetVersion,
    /// Flag bits, checked by the session (a bad value is a refusal, not a parse error).
    pub items_handling: Value,
    /// Client tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Send `slot_data` in `Connected`.
    #[serde(default = "yes")]
    pub slot_data: bool,
}

const fn yes() -> bool {
    true
}

/// Section `ConnectUpdate`.
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectUpdate {
    /// New flag bits.
    #[serde(default)]
    pub items_handling: Option<Value>,
    /// New tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Sections `LocationChecks` and `LocationScouts`.
#[derive(Debug, Clone, Deserialize)]
pub struct Locations {
    /// Location ids in the sender's world.
    pub locations: Vec<i64>,
    /// `LocationScouts` only: create hints (not supported yet, see `docs/compat-status.md`).
    #[serde(default)]
    pub create_as_hint: i64,
}

/// Section `StatusUpdate`.
#[derive(Debug, Clone, Deserialize)]
pub struct StatusUpdate {
    /// `ClientStatus`; 30 is the goal.
    pub status: i64,
}

/// Section `Say`.
#[derive(Debug, Clone, Deserialize)]
pub struct Say {
    /// Chat text.
    pub text: String,
}

/// Section `GetDataPackage`.
#[derive(Debug, Clone, Deserialize)]
pub struct GetDataPackage {
    /// Only these games; all when absent.
    #[serde(default)]
    pub games: Option<Vec<String>>,
}

/// Section `Bounce`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bounce {
    /// Target teams; the sender's team when absent.
    #[serde(default)]
    pub teams: Option<Vec<u32>>,
    /// Target games.
    #[serde(default)]
    pub games: Option<Vec<String>>,
    /// Target slots.
    #[serde(default)]
    pub slots: Option<Vec<u32>>,
    /// Target tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// `or`, `and` or `legacy` (the default).
    #[serde(default)]
    pub operator: Option<String>,
}

/// Section `Get`.
#[derive(Debug, Clone, Deserialize)]
pub struct Get {
    /// Keys to read.
    pub keys: Vec<String>,
}

/// `ClientStatus` goal value.
pub const CLIENT_GOAL: i64 = 30;

/// Encode packets as one frame (a JSON list).
#[must_use]
pub fn frame(packets: &[ServerPacket]) -> String {
    // Serializing these plain structs cannot fail (no maps with non-string keys, no floats
    // that are NaN: `time` comes from the clock).
    serde_json::to_string(packets).unwrap_or_else(|_| "[]".to_owned())
}
