//! The CWP v1 frames (doc 01 D1.10): a fixed set, each with one shape. Features are types
//! carried inside `cmd` and `event`, never new frames.
use super::names::{CmdId, ExtId, StatePath, TypeName};
use crate::{Epoch, ErrorCode, ItemSeq, Retry, Seq, SlotNo};
use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Protocol major versions are small integers (doc 01 D1.8).
pub type Major = u16;

/// The empty JSON object, the default `data` of a command or event.
fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Everything a client may send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum ClientFrame {
    /// First frame of a session.
    Hello(Box<Hello>),
    /// A command; answered by exactly one `result`.
    Cmd(Cmd),
    /// Read one state path; answered by a `result`.
    Get(Get),
    /// Follow state paths; answered by a `result`, then `state.changed` events.
    Watch(Watch),
    /// The game has applied every item up to `applied`.
    Ack(Ack),
    /// Liveness check; answered by `pong`.
    Ping,
    /// Answer to a server `ping`.
    Pong,
}

/// Everything a server may send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum ServerFrame {
    /// Answer to `hello`.
    Welcome(Box<Welcome>),
    /// Answer to one `cmd`, `get` or `watch`.
    Result(CmdResult),
    /// Something happened in the room.
    Event(EventFrame),
    /// A problem with a frame that has no usable `cid` (it did not parse, or came before `hello`).
    Error(ErrorBody),
    /// Liveness check; answered by `pong`.
    Ping,
    /// Answer to a client `ping`.
    Pong,
}

/// What the client wants, sent once in `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Hello {
    /// Protocol majors the client speaks, for example `[1]`.
    pub proto: Vec<Major>,
    /// The slot token (`cst1_...`, doc 01 D1.2). Never in the URL.
    pub token: String,
    /// Extensions the client enables, each with the majors it accepts.
    #[serde(default)]
    pub exts: BTreeMap<ExtId, Vec<Major>>,
    /// Which deliveries the session wants.
    #[serde(default)]
    pub want: Want,
    /// Where to resume the room log; absent on a first connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Resume>,
}

/// Typed wants (doc 01 D1.10 rule 6); replaces bit fields and behaviour-changing tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Want {
    /// Which item grants to deliver.
    pub items: WantItems,
    /// Include the slot's generator data in `welcome`.
    pub slot_data: bool,
}

/// Which item grants a session receives. All on by default: a new client gets every item it is
/// owed unless it opts out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct WantItems {
    /// Items found in other slots' worlds.
    pub from_others: bool,
    /// Items the slot found in its own world.
    pub own: bool,
    /// The slot's starting inventory.
    pub starting: bool,
}

impl Default for WantItems {
    fn default() -> Self {
        Self { from_others: true, own: true, starting: true }
    }
}

/// Resume cursor (doc 01 D1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Resume {
    /// Last room `seq` the client saw.
    pub after: Seq,
    /// Room epoch of that cursor, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<Epoch>,
}

/// A command (doc 01 D1.10 rule 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Cmd {
    /// Idempotency key, echoed in the `result`.
    pub cid: CmdId,
    /// Registered type (`check`, `org.cwp.deathlink/death`).
    #[serde(rename = "type")]
    pub ty: TypeName,
    /// Payload, checked against the type's schema.
    #[serde(default = "empty_object")]
    pub data: Value,
}

/// Read one state path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Get {
    /// Echoed in the `result`.
    pub cid: CmdId,
    /// The path (`slot/3/status`).
    pub path: StatePath,
}

/// Follow state paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Watch {
    /// Echoed in the `result`.
    pub cid: CmdId,
    /// Paths to follow.
    pub paths: Vec<StatePath>,
}

/// Cumulative item ack (doc 01 D1.4, D-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Ack {
    /// Highest `item_seq` the game has applied.
    pub applied: ItemSeq,
}

/// The server's answer to `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Welcome {
    /// The protocol major chosen.
    pub proto: Major,
    /// Enabled extensions, each with the major chosen. Anything absent is off.
    #[serde(default)]
    pub exts: BTreeMap<ExtId, Major>,
    /// Limits the client must respect.
    pub limits: Limits,
    /// Current room epoch.
    pub epoch: Epoch,
    /// Room `seq` of the last durable event.
    pub head: Seq,
    /// The session's slot.
    pub slot: SlotNo,
    /// What the token allows.
    pub role: Role,
    /// Name-table checksum per game in the room; tables are served at
    /// `/games/{game}/names/{checksum}.json` (doc 01 D1.10 rule 5).
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    /// The slot's generator data, when `want.slot_data` asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_data: Option<Value>,
}

/// Server limits announced in `welcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Limits {
    /// Ping interval in milliseconds.
    pub hb_ms: u32,
    /// Largest frame the server accepts, in bytes.
    pub max_frame: u32,
}

/// The session's role, from the token's scope (doc 01 D1.2), never from a client claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum Role {
    /// Sends checks, receives items.
    Play,
    /// Reads the player-visible log only.
    Observe,
}

/// The answer to one `cmd`, `get` or `watch`: `{"cid": ..., "ok": ...}` or
/// `{"cid": ..., "error": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CmdResult {
    /// The `cid` of the frame this answers.
    pub cid: CmdId,
    /// Success or failure.
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Success or failure of a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum Outcome {
    /// Done; the value depends on the command (`{}` when there is nothing to say).
    Ok(Value),
    /// Refused.
    Error(ErrorBody),
}

/// One event (doc 01 D1.10 rule 3): durable ones carry `seq`, ephemeral ones do not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct EventFrame {
    /// Room-wide position, present only for durable events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<Seq>,
    /// Registered type.
    #[serde(rename = "type")]
    pub ty: TypeName,
    /// The slot the event is about, or absent for room-wide events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<SlotNo>,
    /// Payload, shaped by the type's schema.
    #[serde(default = "empty_object")]
    pub data: Value,
}

/// The error shape of doc 01 D1.7.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ErrorBody {
    /// Registered numeric code.
    pub code: u16,
    /// Registered dotted key; clients localize from it.
    pub key: String,
    /// Whether and how to retry.
    pub retry: Retry,
    /// Wait this long first, when `retry` is `after`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub after_ms: u64,
    /// Key-specific details.
    #[serde(default = "empty_object")]
    pub detail: Value,
}

impl From<ErrorCode> for ErrorBody {
    fn from(e: ErrorCode) -> Self {
        Self {
            code: e.code(),
            key: e.key().into(),
            retry: e.retry(),
            after_ms: 0,
            detail: empty_object(),
        }
    }
}
