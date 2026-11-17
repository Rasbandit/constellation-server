//! The type registry (doc 01 D1.10 rule 7): every core command and event, with its delivery
//! class, who may send or see it, its stability and its display template. The committed schema
//! (`schema/cwp-v1.json`) is generated from this table and the payload types below.
use super::frame::{Cmd, EventFrame};
use super::names::{StatePath, TypeName};
use crate::{ItemSeq, Seq, SlotNo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether a type travels client to server (`cmd`) or server to client (`event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Sent by a client in a `cmd` frame.
    Cmd,
    /// Sent by the server in an `event` frame.
    Event,
}

/// Delivery class of an event type (doc 01 D1.10 rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Logged, carries `seq`, replayed on resume.
    Durable,
    /// Live only, no `seq`, may be dropped for a full queue.
    Ephemeral,
}

/// Token scopes (doc 01 D1.2). For a command: who may send it. For an event: who may see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// A playing slot.
    Play,
    /// A read-only session.
    Observe,
}

/// Lifecycle of a type (doc 01 D1.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    /// May change without notice.
    Experimental,
    /// No breaking change within the major.
    Stable,
    /// Still works; removal announced.
    Deprecated,
}

/// One registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TypeInfo {
    /// The wire name.
    pub name: &'static str,
    /// Command or event.
    pub kind: Kind,
    /// Events only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    /// Who may send it (commands) or see it (events).
    pub scopes: &'static [Scope],
    /// Lifecycle.
    pub stability: Stability,
    /// Display text for people, with `{field}` placeholders from `data` (and `{slot}` for the
    /// frame's slot). Clients render it; servers never send text (doc 01 D1.10 rule 7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<&'static str>,
    /// One line for the docs.
    pub summary: &'static str,
}

const PLAY: &[Scope] = &[Scope::Play];
const ALL: &[Scope] = &[Scope::Play, Scope::Observe];

/// Every core type of CWP v1. Extension types live in their extension's own registry.
pub const CORE_TYPES: &[TypeInfo] = &[
    TypeInfo {
        name: "check",
        kind: Kind::Cmd,
        delivery: None,
        scopes: PLAY,
        stability: Stability::Experimental,
        template: None,
        summary: "Report a checked location in the slot's own world.",
    },
    TypeInfo {
        name: "goal",
        kind: Kind::Cmd,
        delivery: None,
        scopes: PLAY,
        stability: Stability::Experimental,
        template: None,
        summary: "The slot reached its goal. Repeats are answered from the first.",
    },
    TypeInfo {
        name: "location.checked",
        kind: Kind::Event,
        delivery: Some(Delivery::Durable),
        scopes: ALL,
        stability: Stability::Experimental,
        template: None,
        summary: "A slot checked a location.",
    },
    TypeInfo {
        name: "item.grant",
        kind: Kind::Event,
        delivery: Some(Delivery::Durable),
        scopes: ALL,
        stability: Stability::Experimental,
        template: Some("{from} sent {item} to {slot}"),
        summary: "An item was delivered to the event's slot, with its per-slot `item_seq`.",
    },
    TypeInfo {
        name: "slot.goal",
        kind: Kind::Event,
        delivery: Some(Delivery::Durable),
        scopes: ALL,
        stability: Stability::Experimental,
        template: Some("{slot} reached their goal"),
        summary: "The event's slot reached its goal.",
    },
    TypeInfo {
        name: "state.changed",
        kind: Kind::Event,
        delivery: Some(Delivery::Ephemeral),
        scopes: ALL,
        stability: Stability::Experimental,
        template: None,
        summary: "A watched state path changed; after a reconnect, `get` it again.",
    },
];

/// The registry entry for a core type.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static TypeInfo> {
    CORE_TYPES.iter().find(|t| t.name == name)
}

/// `check` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Check {
    /// Location id in the slot's own world.
    pub location: i64,
}

/// `goal` payload (empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Goal {}

/// `location.checked` payload; the frame's `slot` is the checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct LocationChecked {
    /// Location id in the checker's world.
    pub location: i64,
}

/// `item.grant` payload; the frame's `slot` is the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ItemGrant {
    /// The slot whose location held the item.
    pub from: SlotNo,
    /// That location, in `from`'s world.
    pub location: i64,
    /// Item id in the receiver's world.
    pub item: i64,
    /// The receiver's delivery index, contiguous from 1; what `ack` confirms.
    pub item_seq: ItemSeq,
}

/// `slot.goal` payload (empty); the frame's `slot` reached its goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SlotGoal {}

/// `state.changed` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StateChanged {
    /// The watched path.
    pub path: StatePath,
    /// Its new value.
    pub value: Value,
    /// The `seq` that last changed it.
    pub ver: Seq,
}

/// Why a `cmd` or `event` could not be read as a core type.
#[derive(Debug)]
pub enum DecodeError {
    /// An extension type: relay it or hand it to the extension, per its registry.
    Extension,
    /// A core-looking name that is not registered for this direction.
    Unknown,
    /// Registered, but `data` does not match its schema.
    Data(serde_json::Error),
}

/// A decoded core command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreCmd {
    /// `check`
    Check(Check),
    /// `goal`
    Goal(Goal),
}

impl CoreCmd {
    /// Reads `cmd.data` as the type `cmd.ty` names.
    ///
    /// # Errors
    /// [`DecodeError`] for extension types, unregistered names and bad payloads.
    pub fn decode(cmd: &Cmd) -> Result<Self, DecodeError> {
        let data = || cmd.data.clone();
        match core_name(&cmd.ty)? {
            "check" => serde_json::from_value(data()).map(Self::Check),
            "goal" => serde_json::from_value(data()).map(Self::Goal),
            _ => return Err(DecodeError::Unknown),
        }
        .map_err(DecodeError::Data)
    }
}

/// A decoded core event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    /// `location.checked`
    LocationChecked(LocationChecked),
    /// `item.grant`
    ItemGrant(ItemGrant),
    /// `slot.goal`
    SlotGoal(SlotGoal),
    /// `state.changed`
    StateChanged(StateChanged),
}

impl CoreEvent {
    /// Reads `ev.data` as the type `ev.ty` names.
    ///
    /// # Errors
    /// [`DecodeError`] for extension types, unregistered names and bad payloads.
    pub fn decode(ev: &EventFrame) -> Result<Self, DecodeError> {
        let data = || ev.data.clone();
        match core_name(&ev.ty)? {
            "location.checked" => serde_json::from_value(data()).map(Self::LocationChecked),
            "item.grant" => serde_json::from_value(data()).map(Self::ItemGrant),
            "slot.goal" => serde_json::from_value(data()).map(Self::SlotGoal),
            "state.changed" => serde_json::from_value(data()).map(Self::StateChanged),
            _ => return Err(DecodeError::Unknown),
        }
        .map_err(DecodeError::Data)
    }

    /// The wire name of this event's type.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::LocationChecked(_) => "location.checked",
            Self::ItemGrant(_) => "item.grant",
            Self::SlotGoal(_) => "slot.goal",
            Self::StateChanged(_) => "state.changed",
        }
    }
}

fn core_name(ty: &TypeName) -> Result<&str, DecodeError> {
    if ty.extension().is_some() {
        return Err(DecodeError::Extension);
    }
    Ok(ty.as_str())
}
