//! The type registry (doc 01 D1.10 rule 14): every core command and event with its delivery,
//! audience, permissions, result and error keys, limits, lifecycle and display templates. The
//! committed schema (`schema/cwp-v1.json`) is generated from this table and the payload types.
use super::frame::{
    Caller, Cmd, EventFrame, ExtChoice, ExtOffer, Minor, RoomRole, Scope, Want, empty_object,
};
use super::names::{CmdId, ExtId, ItemUid, StatePath, TypeName};
use super::serde_util::{de_i, de_opt_i, de_u, in_range, present, ser_i, ser_opt_i, ser_u};
use crate::{Epoch, ErrorCode, ItemSeq, Seq, SlotNo};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::fmt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
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

/// Delivery class of an event type (rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Logged, carries `seq`, replayed on resume.
    Durable,
    /// Live only, carries `eseq`.
    Ephemeral,
}

/// Who may see an event type (rule 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// Every session in the room.
    Room,
    /// Sessions of the event's slot, plus room roles.
    Subject,
    /// Sessions with the `play` scope.
    Players,
    /// Room roles only.
    Roles,
    /// As the read rule of the path the event names (`state.changed`).
    Path,
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

/// Who may send a command (rule 10): a caller qualifies with any listed scope or any listed
/// room role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Allow {
    /// Token scopes that may send it.
    pub scopes: &'static [Scope],
    /// Room roles that may send it.
    pub roles: &'static [RoomRole],
    /// Whether a fresh confirmation is needed (doc 10).
    pub step_up: bool,
}

impl Allow {
    /// Whether `caller` may send a command with this rule.
    #[must_use]
    pub fn permits(&self, caller: &Caller) -> bool {
        caller.scopes.iter().any(|s| self.scopes.contains(s))
            || caller.roles.iter().any(|r| self.roles.contains(r))
    }
}

/// A display template variant (rule 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Template {
    /// The condition: `a=b` compares two fields (`slot` is the frame's slot), `a:v` compares a
    /// field with a literal value; `None` always applies. The first variant whose condition
    /// holds is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<&'static str>,
    /// English text with typed placeholders: `{field:kind}` or `{field:kind@slotfield}`.
    pub text: &'static str,
}

/// One registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TypeInfo {
    /// The wire name.
    #[serde(rename = "type")]
    pub name: &'static str,
    /// Command or event.
    pub kind: Kind,
    /// Events: the delivery class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    /// Events: who may see it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Audience>,
    /// Events: hidden from `observe` sessions unless the room's spoiler mode allows it.
    pub spoiler: bool,
    /// Events: an optional stream (`want.streams`) whose sessions also receive every event of
    /// this type, beyond the audience; for example `room.items` for item grants (rule 6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<&'static str>,
    /// Commands: who may send it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Allow>,
    /// Commands: the error keys it can return beyond [`COMMON_CMD_ERRORS`].
    pub errors: &'static [ErrorCode],
    /// Largest `data`, in bytes.
    pub max_bytes: u32,
    /// Commands: per-session rate limit per second; 0 means the session's `cmds_per_sec`.
    pub rate_per_sec: u16,
    /// Commands: the room states in which it is accepted (`staging`, `live`); empty means any.
    /// Outside them the answer is `room.not_live` or `room.paused`.
    pub states: &'static [&'static str],
    /// Lifecycle.
    #[serde(rename = "status")]
    pub stability: Stability,
    /// The protocol minor that added it.
    pub since: Minor,
    /// Display text for people; clients render it into typed parts.
    #[serde(skip_serializing_if = "<[Template]>::is_empty")]
    pub templates: &'static [Template],
    /// One line for the docs.
    pub summary: &'static str,
}

/// Errors any command can return, whatever its type (rules 2, 3 and 10).
pub const COMMON_CMD_ERRORS: &[ErrorCode] = &[
    ErrorCode::TypeUnknown,
    ErrorCode::DataInvalid,
    ErrorCode::ExtNotEnabled,
    ErrorCode::CidConflict,
    ErrorCode::AuthScopeInsufficient,
    ErrorCode::RateLimited,
    ErrorCode::FrameTooLarge,
];

const PLAY: Allow = Allow { scopes: &[Scope::Play], roles: &[], step_up: false };
const ANYONE: Allow = Allow {
    scopes: &[Scope::Play, Scope::Observe],
    roles: &[RoomRole::Owner, RoomRole::CoHost, RoomRole::Moderator],
    step_up: false,
};

const fn cmd(
    name: &'static str,
    allow: Allow,
    errors: &'static [ErrorCode],
    max_bytes: u32,
    summary: &'static str,
) -> TypeInfo {
    TypeInfo {
        name,
        kind: Kind::Cmd,
        delivery: None,
        audience: None,
        spoiler: false,
        stream: None,
        allow: Some(allow),
        errors,
        max_bytes,
        rate_per_sec: 0,
        states: &[],
        stability: Stability::Experimental,
        since: 0,
        templates: &[],
        summary,
    }
}

const fn event(
    name: &'static str,
    delivery: Delivery,
    audience: Audience,
    templates: &'static [Template],
    summary: &'static str,
) -> TypeInfo {
    TypeInfo {
        name,
        kind: Kind::Event,
        delivery: Some(delivery),
        audience: Some(audience),
        spoiler: false,
        stream: None,
        allow: None,
        errors: &[],
        max_bytes: 4096,
        rate_per_sec: 0,
        states: &[],
        stability: Stability::Experimental,
        since: 0,
        templates,
        summary,
    }
}

/// Every core type of CWP v1. Extension types live in their extension's own registry.
pub const CORE_TYPES: &[TypeInfo] = &[
    cmd(
        "check",
        PLAY,
        &[],
        256,
        "Report a checked location in the slot's own world; the result lists the items it held.",
    ),
    cmd("goal", PLAY, &[], 64, "The slot reached its goal. Repeats are answered from the first."),
    cmd(
        "status.set",
        PLAY,
        &[],
        128,
        "The client's own state claim (ready, playing); lobby readiness is separate.",
    ),
    cmd(
        "session.update",
        ANYONE,
        &[ErrorCode::CapabilityMissing],
        4096,
        "Change wants and enable or disable extensions mid-session.",
    ),
    event("location.checked", Delivery::Durable, Audience::Room, &[], "A slot checked a location."),
    TypeInfo {
        stream: Some("room.items"),
        ..event(
            "item.grant",
            Delivery::Durable,
            Audience::Subject,
            &[
                Template {
                    when: Some("source:start"),
                    text: "{slot:slot} started with {item:item@slot}",
                },
                Template {
                    when: Some("source:admin"),
                    text: "{slot:slot} was given {item:item@slot}",
                },
                Template {
                    when: Some("from=slot"),
                    text: "{slot:slot} found their {item:item@slot}",
                },
                Template { when: None, text: "{from:slot} sent {item:item@slot} to {slot:slot}" },
            ],
            "An item was delivered to the event's slot, with its per-slot item_seq. Sessions of \
             the room.items stream see every grant.",
        )
    },
    event(
        "slot.goal",
        Delivery::Durable,
        Audience::Room,
        &[Template { when: None, text: "{slot:slot} reached their goal" }],
        "The event's slot reached its goal.",
    ),
    event(
        "room.epoch_change",
        Delivery::Durable,
        Audience::Room,
        &[],
        "The room moved to another history (failover or restore), rule 12.",
    ),
    event(
        "state.changed",
        Delivery::Ephemeral,
        Audience::Path,
        &[],
        "A watched state path changed; after a gap in eseq, get it again.",
    ),
];

/// The registry entry for a core type.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static TypeInfo> {
    CORE_TYPES.iter().find(|t| t.name == name)
}

// ---- payloads ----------------------------------------------------------------------------

/// `check` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Check {
    /// Location id in the slot's own world.
    #[serde(deserialize_with = "de_i", serialize_with = "ser_i")]
    #[cfg_attr(test, schemars(range(min = -9_007_199_254_740_991_i64, max = 9_007_199_254_740_991_i64)))]
    pub location: i64,
}

/// `check` result: what the location held.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CheckOk {
    /// The items found; empty for a repeat or an empty location.
    #[serde(default)]
    pub items: Vec<SentItem>,
}

/// One item a check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SentItem {
    /// The receiving slot.
    pub to: SlotNo,
    /// Item id in the receiver's world.
    #[serde(deserialize_with = "de_i", serialize_with = "ser_i")]
    #[cfg_attr(test, schemars(range(min = -9_007_199_254_740_991_i64, max = 9_007_199_254_740_991_i64)))]
    pub item: i64,
    /// Item classification.
    #[serde(default)]
    pub flags: Vec<ItemFlag>,
    /// The grant's stable identity, as the receiver will see it in `item.grant`.
    pub item_uid: ItemUid,
}

/// `goal` payload (empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Goal {}

/// `status.set` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StatusSet {
    /// The client's own claim.
    pub client_state: ClientState,
}

/// `slot/{n}/status.client_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum ClientState {
    /// The client says it is ready.
    Ready,
    /// The client says it is playing.
    Playing,
    /// A state this version does not know.
    #[serde(other)]
    Other,
}

/// `session.update` payload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SessionUpdate {
    /// New wants, replacing the old ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub want: Option<Want>,
    /// Extensions to have enabled afterwards; an absent id is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exts: Option<BTreeMap<ExtId, ExtOffer>>,
}

/// `session.update` result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SessionUpdateOk {
    /// The extensions now enabled.
    #[serde(default)]
    pub exts: BTreeMap<ExtId, ExtChoice>,
}

/// `location.checked` payload; the frame's `slot` is the checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct LocationChecked {
    /// Location id in the checker's world.
    #[serde(deserialize_with = "de_i", serialize_with = "ser_i")]
    #[cfg_attr(test, schemars(range(min = -9_007_199_254_740_991_i64, max = 9_007_199_254_740_991_i64)))]
    pub location: i64,
}

/// Where an item grant came from (rule 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum Source {
    /// A check of a location.
    Check,
    /// The finder released its remaining locations.
    Release,
    /// The receiver collected its remaining items.
    Collect,
    /// A copy for an item link group.
    Link,
    /// Starting inventory.
    Start,
    /// Given by a room role.
    Admin,
    /// A source this version does not know; its `from` and `location` are not checked.
    #[serde(other)]
    Other,
}

impl Source {
    /// Whether grants from this source name a finder and a location.
    #[must_use]
    pub const fn has_location(self) -> bool {
        matches!(self, Self::Check | Self::Release | Self::Collect | Self::Link)
    }
}

/// Item classification. Unknown flags from newer servers read as `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum ItemFlag {
    /// Can unlock logical progress.
    Progression,
    /// Especially useful.
    Useful,
    /// A trap.
    Trap,
    /// A flag this version does not know.
    #[serde(other)]
    Other,
}

/// `item.grant` payload; the frame's `slot` is the receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ItemGrant {
    /// Where it came from.
    pub source: Source,
    /// Item id in the receiver's world.
    #[serde(deserialize_with = "de_i", serialize_with = "ser_i")]
    #[cfg_attr(test, schemars(range(min = -9_007_199_254_740_991_i64, max = 9_007_199_254_740_991_i64)))]
    pub item: i64,
    /// The receiver's delivery index, contiguous from 1; what `ack` confirms.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub item_seq: ItemSeq,
    /// Stable identity across epochs; clients dedupe by it.
    pub item_uid: ItemUid,
    /// Classification.
    #[serde(default)]
    pub flags: Vec<ItemFlag>,
    /// The finding slot, for location-based sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<SlotNo>,
    /// The location in `from`'s world, for location-based sources.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_i",
        serialize_with = "ser_opt_i"
    )]
    #[cfg_attr(test, schemars(range(min = -9_007_199_254_740_991_i64, max = 9_007_199_254_740_991_i64)))]
    pub location: Option<i64>,
}

impl ItemGrant {
    /// Whether `from` and `location` are present exactly when `source` names a location (not
    /// checked for a source this version does not know).
    #[must_use]
    pub const fn is_consistent(&self) -> bool {
        let named = self.from.is_some() && self.location.is_some();
        let absent = self.from.is_none() && self.location.is_none();
        match self.source {
            Source::Other => true,
            s if s.has_location() => named,
            _ => absent,
        }
    }
}

/// `slot.goal` payload (empty); the frame's `slot` reached its goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SlotGoal {}

/// `room.epoch_change` payload (rule 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct EpochChange {
    /// The epoch before.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub from: Epoch,
    /// The epoch now.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub to: Epoch,
    /// The last `seq` the new history kept; the client replays its journal after it.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub recovered_head: Seq,
    /// Estimated seconds of acknowledged activity lost (doc 06 section 6.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approx_lost_seconds: Option<u32>,
}

/// `state.changed` payload, also one entry of a `watch` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StateChanged {
    /// The path.
    pub path: StatePath,
    /// Its value.
    pub value: Value,
    /// Its version (rule 7).
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub ver: Seq,
    /// `kv` paths: the value before the write (`null` when it was unset).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub prev: Option<Value>,
    /// `kv` paths: the writing slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<SlotNo>,
}

/// `get` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct GetOk {
    /// The path read.
    pub path: StatePath,
    /// Its value, or one page of it.
    pub value: Value,
    /// Its version.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub ver: Seq,
    /// Cursor for the next page; absent on the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// `watch` result: the current value of each path, atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct WatchOk {
    /// One entry per watched path, in request order.
    pub paths: Vec<StateChanged>,
}

// ---- decoding and encoding ---------------------------------------------------------------

/// Why a `cmd` or `event` could not be read as a core type.
#[derive(Debug)]
pub enum DecodeError {
    /// An extension type: relay it or hand it to the extension, per its registry.
    Extension,
    /// A core-looking name that is not registered for this direction.
    Unknown,
    /// `data` is not a JSON object.
    NotAnObject,
    /// Registered, but `data` does not match its schema.
    Data(serde_json::Error),
    /// Registered, but a frame field breaks the type's rules (`seq` on an ephemeral event, a
    /// missing `slot`, an inconsistent `source`).
    Frame(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extension => f.write_str("an extension type"),
            Self::Unknown => f.write_str("not a registered core type"),
            Self::NotAnObject => f.write_str("data is not an object"),
            Self::Data(e) => write!(f, "data does not match the schema: {e}"),
            Self::Frame(why) => f.write_str(why),
        }
    }
}

impl core::error::Error for DecodeError {}

impl DecodeError {
    /// The error a server answers a command with, or `None` for an extension type (which the
    /// server hands to the extension, or answers with `ext.not_enabled`).
    #[must_use]
    pub const fn error_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Extension => None,
            Self::Unknown => Some(ErrorCode::TypeUnknown),
            Self::NotAnObject | Self::Data(_) | Self::Frame(_) => Some(ErrorCode::DataInvalid),
        }
    }
}

fn core_name(ty: &TypeName) -> Result<&str, DecodeError> {
    if ty.extension().is_some() {
        return Err(DecodeError::Extension);
    }
    Ok(ty.as_str())
}

fn payload<T: DeserializeOwned>(data: &Value) -> Result<T, DecodeError> {
    if !data.is_object() {
        return Err(DecodeError::NotAnObject);
    }
    T::deserialize(data).map_err(DecodeError::Data)
}

fn type_name(name: &'static str) -> Result<TypeName, serde_json::Error> {
    TypeName::new(name).map_err(serde::ser::Error::custom)
}

/// A decoded core command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreCmd {
    /// `check`
    Check(Check),
    /// `goal`
    Goal(Goal),
    /// `status.set`
    StatusSet(StatusSet),
    /// `session.update`
    SessionUpdate(SessionUpdate),
}

impl CoreCmd {
    /// Reads `cmd.data` as the type `cmd.ty` names.
    ///
    /// # Errors
    /// [`DecodeError`] for extension types, unregistered names, non-object payloads and payloads
    /// that do not match the schema.
    pub fn decode(cmd: &Cmd) -> Result<Self, DecodeError> {
        let d = &cmd.data;
        Ok(match core_name(&cmd.ty)? {
            "check" => Self::Check(payload(d)?),
            "goal" => Self::Goal(payload(d)?),
            "status.set" => Self::StatusSet(payload(d)?),
            "session.update" => Self::SessionUpdate(payload(d)?),
            _ => return Err(DecodeError::Unknown),
        })
    }

    /// The wire name of this command's type.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Check(_) => "check",
            Self::Goal(_) => "goal",
            Self::StatusSet(_) => "status.set",
            Self::SessionUpdate(_) => "session.update",
        }
    }

    /// The `cmd` frame for this command, checked by [`Self::decode`].
    ///
    /// # Errors
    /// When a payload value is outside the CWP integer range.
    pub fn to_cmd(&self, cid: CmdId) -> Result<Cmd, serde_json::Error> {
        let data = match self {
            Self::Check(p) => serde_json::to_value(p),
            Self::Goal(p) => serde_json::to_value(p),
            Self::StatusSet(p) => serde_json::to_value(p),
            Self::SessionUpdate(p) => serde_json::to_value(p),
        }?;
        let cmd = Cmd { cid, ty: type_name(self.type_name())?, data };
        Self::decode(&cmd).map_err(serde::ser::Error::custom)?;
        Ok(cmd)
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
    /// `room.epoch_change`
    EpochChange(EpochChange),
    /// `state.changed`
    StateChanged(StateChanged),
}

/// How an event is delivered: its `seq` or its `eseq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivered {
    /// A durable event at this room `seq`.
    Durable(Seq),
    /// An ephemeral event with this per-session counter.
    Ephemeral(u64),
}

impl CoreEvent {
    /// Reads `ev.data` as the type `ev.ty` names, and checks the frame against the type's
    /// rules: `seq` for durable types, `eseq` for ephemeral ones, a `slot` where the type is
    /// about one, and a consistent `source` on item grants.
    ///
    /// # Errors
    /// [`DecodeError`] for extension types, unregistered names and bad payloads or frames.
    pub fn decode(ev: &EventFrame) -> Result<Self, DecodeError> {
        let d = &ev.data;
        let decoded = match core_name(&ev.ty)? {
            "location.checked" => Self::LocationChecked(payload(d)?),
            "item.grant" => Self::ItemGrant(payload(d)?),
            "slot.goal" => Self::SlotGoal(payload(d)?),
            "room.epoch_change" => Self::EpochChange(payload(d)?),
            "state.changed" => Self::StateChanged(payload(d)?),
            _ => return Err(DecodeError::Unknown),
        };
        let delivery = lookup(decoded.type_name()).and_then(|t| t.delivery);
        match (delivery, ev.seq, ev.eseq) {
            (Some(Delivery::Durable), Some(_), None)
            | (Some(Delivery::Ephemeral), None, Some(_)) => {}
            _ => return Err(DecodeError::Frame("seq or eseq does not match the delivery class")),
        }
        if decoded.needs_slot() && ev.slot.is_none() {
            return Err(DecodeError::Frame("this event type is about a slot"));
        }
        if let Self::ItemGrant(g) = &decoded
            && !g.is_consistent()
        {
            return Err(DecodeError::Frame(
                "from and location must be present exactly for location sources",
            ));
        }
        Ok(decoded)
    }

    const fn needs_slot(&self) -> bool {
        matches!(self, Self::LocationChecked(_) | Self::ItemGrant(_) | Self::SlotGoal(_))
    }

    /// The wire name of this event's type.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::LocationChecked(_) => "location.checked",
            Self::ItemGrant(_) => "item.grant",
            Self::SlotGoal(_) => "slot.goal",
            Self::EpochChange(_) => "room.epoch_change",
            Self::StateChanged(_) => "state.changed",
        }
    }

    /// The `event` frame for this event, checked by [`Self::decode`], so it never produces a
    /// frame a receiver would refuse.
    ///
    /// # Errors
    /// When a value is outside the CWP integer range, or the frame breaks the type's rules
    /// (durable or ephemeral, a missing `slot`, an inconsistent `source`).
    pub fn to_frame(
        &self,
        at: Delivered,
        slot: Option<SlotNo>,
        t: Option<u64>,
    ) -> Result<EventFrame, serde_json::Error> {
        let data = match self {
            Self::LocationChecked(p) => serde_json::to_value(p),
            Self::ItemGrant(p) => serde_json::to_value(p),
            Self::SlotGoal(p) => serde_json::to_value(p),
            Self::EpochChange(p) => serde_json::to_value(p),
            Self::StateChanged(p) => serde_json::to_value(p),
        }?;
        let (seq, eseq) = match at {
            Delivered::Durable(s) => (Some(s), None),
            Delivered::Ephemeral(e) => (None, Some(e)),
        };
        let data = if data.is_null() { empty_object() } else { data };
        let frame = EventFrame {
            seq,
            eseq,
            ty: type_name(self.type_name())?,
            slot,
            t,
            prev_hash: None,
            data,
        };
        if !in_range(&frame.data) {
            return Err(serde::ser::Error::custom("integer outside the CWP range"));
        }
        Self::decode(&frame).map_err(serde::ser::Error::custom)?;
        Ok(frame)
    }
}
