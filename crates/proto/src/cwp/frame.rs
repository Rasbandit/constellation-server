//! The CWP v1 frames (doc 01 D1.10): a fixed set, each with one shape. Features are types
//! carried inside `cmd` and `event`, never new frames.
use super::names::{Checksum, CmdId, ExtId, GameId, Secret, StatePath, StreamName, TypeName};
use super::registry::EpochChange;
use super::serde_util::{de_opt_u, de_u, is_false, present, ser_opt_u, ser_u};
use crate::{Epoch, ErrorCode, ItemSeq, Retry, Seq, SlotNo};
use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};

/// Protocol and extension major versions are small integers (doc 01 D1.8).
pub type Major = u16;
/// Protocol and extension minor versions.
pub type Minor = u16;

/// The empty JSON object, the default `data` of a command or event.
pub(super) fn empty_object() -> Value {
    Value::Object(Map::new())
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
    /// Follow state paths; answered by a `result` with their values, then `state.changed` events.
    Watch(Watch),
    /// Stop following state paths; answered by a `result`.
    Unwatch(Watch),
    /// The game has applied every item up to `applied`.
    Ack(Ack),
    /// Heartbeat, sent every `limits.hb_ms` (doc 09 section 5.5); answered by `pong`.
    Ping(Beat),
    /// An `op` this version does not know. A server answers it with `op.unknown` (rule 1).
    #[serde(other)]
    Unknown,
}

/// Everything a server may send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum ServerFrame {
    /// Answer to `hello`.
    Welcome(Box<Welcome>),
    /// Answer to one `cmd`, `get`, `watch` or `unwatch`.
    Result(CmdResult),
    /// Something happened in the room.
    Event(EventFrame),
    /// A problem not tied to a `cid` (rule 13 reasons, a bad frame, a bad `ack`).
    Error(ErrorBody),
    /// Answer to a client `ping`, echoing its nonce.
    Pong(Beat),
    /// An `op` this version does not know. Clients ignore it (rule 1).
    #[serde(other)]
    Unknown,
}

macro_rules! frame_from {
    ($frame:ident :: $variant:ident ($t:ty)) => {
        impl From<$t> for $frame {
            fn from(v: $t) -> Self {
                Self::$variant(v.into())
            }
        }
    };
}
frame_from!(ClientFrame::Hello(Hello));
frame_from!(ClientFrame::Cmd(Cmd));
frame_from!(ClientFrame::Get(Get));
frame_from!(ClientFrame::Ack(Ack));
frame_from!(ServerFrame::Welcome(Welcome));
frame_from!(ServerFrame::Result(CmdResult));
frame_from!(ServerFrame::Event(EventFrame));
frame_from!(ServerFrame::Error(ErrorBody));

/// `ping` (client) and `pong` (server), doc 09 section 5.5: an optional nonce, which `pong`
/// echoes, and on `ping` an optional presence state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Beat {
    /// Nonce; a `pong` echoes the `ping`'s.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_u",
        serialize_with = "ser_opt_u"
    )]
    #[cfg_attr(test, schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub n: Option<u64>,
    /// Presence state (`menu`, `loading`, `playing`), on a client's `ping`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<StreamName>,
}

/// What the client is and wants, sent once (doc 01 D1.10 rule 9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Hello {
    /// Protocol majors the client speaks, for example `[1]`.
    pub proto: Vec<Major>,
    /// A credential: slot token or integration credential (doc 01 D1.2). Never in a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Secret>,
    /// A connect ticket instead of a token (doc 08 section 3.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<Secret>,
    /// What kind of client this is.
    pub client: ClientInfo,
    /// The game the client holds, for game clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<GameInfo>,
    /// Hash of the name tables the client holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_checksum: Option<Checksum>,
    /// Extensions the client enables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exts: BTreeMap<ExtId, ExtOffer>,
    /// Which deliveries the session wants.
    #[serde(default)]
    pub want: Want,
    /// Where to resume the room log; absent on a first connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Resume>,
    /// BCP 47 language tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Client wall clock, for skew warnings only.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_u",
        serialize_with = "ser_opt_u"
    )]
    #[cfg_attr(test, schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub client_time_ms: Option<u64>,
    /// Replace an existing session for this seat, after the person confirmed it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub takeover: bool,
}

/// The credential a `hello` presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential<'a> {
    /// A slot token or integration credential.
    Token(&'a Secret),
    /// A connect ticket.
    Ticket(&'a Secret),
}

impl Hello {
    /// The credential, when exactly one of `token` and `ticket` is present. A server refuses a
    /// `hello` for which this is `None` with `auth.token_unknown`.
    #[must_use]
    pub const fn credential(&self) -> Option<Credential<'_>> {
        match (&self.token, &self.ticket) {
            (Some(t), None) => Some(Credential::Token(t)),
            (None, Some(t)) => Some(Credential::Ticket(t)),
            _ => None,
        }
    }
}

/// `hello.client` (doc 09 section 4.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ClientInfo {
    /// What kind of client.
    pub kind: ClientKind,
    /// Free text, length-limited by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// SDK name and version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk: Option<String>,
}

/// `hello.client.kind` (doc 09 section 4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum ClientKind {
    /// A game or mod.
    Game,
    /// A browser page.
    Browser,
    /// A console or other device.
    Device,
    /// A bot with an integration credential.
    Bot,
    /// A tracker.
    Tracker,
    /// A spectator view.
    Spectator,
    /// The host console.
    Console,
    /// Synthesized by the compat edge for an existing client.
    Legacy,
    /// A kind this version does not know.
    #[serde(other)]
    Other,
}

/// `hello.game`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct GameInfo {
    /// The game id.
    pub id: GameId,
    /// The game's version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Hash of the world the client holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_hash: Option<Checksum>,
}

/// One extension the client offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ExtOffer {
    /// Majors the client accepts.
    pub v: Vec<Major>,
    /// The extension's own hello block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opts: Option<Value>,
}

/// Typed wants; replaces bit fields and behaviour-changing tags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Want {
    /// Which item grants to deliver.
    pub items: WantItems,
    /// Include the slot's generator data in `welcome`.
    pub slot_data: bool,
    /// Optional event streams declared by enabled extensions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<StreamName>,
}

/// Which item grants a session receives. All on by default.
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
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub after: Seq,
    /// Room epoch of that cursor, if known.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_u",
        serialize_with = "ser_opt_u"
    )]
    pub epoch: Option<Epoch>,
}

/// A command (doc 01 D1.10 rule 3).
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

/// Read one state path, or one page of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Get {
    /// Echoed in the `result`.
    pub cid: CmdId,
    /// The path (`slot/3/status`).
    pub path: StatePath,
    /// Page cursor from a previous result's `next`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Page size the client prefers; the server may return fewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Follow or stop following state paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Watch {
    /// Echoed in the `result`.
    pub cid: CmdId,
    /// The paths.
    pub paths: Vec<StatePath>,
}

/// Cumulative item ack (doc 01 D1.4, D-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Ack {
    /// Highest `item_seq` the game has applied.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub applied: ItemSeq,
}

/// The server's answer to `hello` (doc 01 D1.10 rule 9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Welcome {
    /// The protocol version in use.
    pub proto: ProtoVersion,
    /// Session id; a bearer secret on HTTP transports.
    pub session: Secret,
    /// Who the session acts as.
    pub caller: Caller,
    /// The caller's slot, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<SlotInfo>,
    /// Enabled extensions; anything absent is off.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exts: BTreeMap<ExtId, ExtChoice>,
    /// Limits the client must respect.
    pub limits: Limits,
    /// Current room epoch.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub epoch: Epoch,
    /// Room-wide `seq` of the last durable event.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub head: Seq,
    /// The slot's stored applied cursor.
    #[serde(default, deserialize_with = "de_u", serialize_with = "ser_u")]
    pub applied: ItemSeq,
    /// The durable range about to be replayed; absent when there is nothing to replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<Replay>,
    /// What became of `hello.resume`.
    pub resume: ResumeOutcome,
    /// Name-table checksum per game id in the room (rule 8).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<GameId, Checksum>,
    /// The slot's generator data, when asked for and small enough.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub slot_data: Option<Value>,
    /// Server clock.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    #[cfg_attr(test, schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub server_time_ms: u64,
    /// Room block of doc 09 section 4.6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<Value>,
    /// Seat readiness block of doc 09 section 4.6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_state: Option<Value>,
}

/// `welcome.proto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProtoVersion {
    /// Major in use.
    pub major: Major,
    /// Minor in use; a type whose `since` is above it does not exist on this server.
    pub minor: Minor,
}

/// `welcome.caller` (doc 01 D1.10 rule 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Caller {
    /// What the credential is.
    pub kind: CallerKind,
    /// The slot, for `slot` callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<SlotNo>,
    /// The token's scopes.
    #[serde(default)]
    pub scopes: Vec<Scope>,
    /// The account's room roles (doc 10 section 2.3).
    #[serde(default)]
    pub roles: Vec<RoomRole>,
}

/// `welcome.caller.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum CallerKind {
    /// A slot token.
    Slot,
    /// An observe-only spectator link.
    Spectator,
    /// A bot's integration credential.
    Integration,
    /// An account (the host console).
    Account,
    /// A value this version does not know; it grants nothing.
    #[serde(other)]
    Other,
}

/// Token scopes (doc 01 D1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum Scope {
    /// Sends checks, receives items.
    Play,
    /// Reads the player-visible log only.
    Observe,
    /// A value this version does not know; it grants nothing.
    #[serde(other)]
    Other,
}

/// Room roles (doc 10 section 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum RoomRole {
    /// Owns the room.
    Owner,
    /// Hosts with the owner.
    CoHost,
    /// Moderates chat and players.
    Moderator,
    /// A value this version does not know; it grants nothing.
    #[serde(other)]
    Other,
}

/// `welcome.slot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SlotInfo {
    /// Slot number.
    pub index: SlotNo,
    /// Slot name.
    pub name: String,
    /// The slot's game.
    pub game: GameId,
}

/// One enabled extension in `welcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ExtChoice {
    /// The major chosen.
    pub v: Major,
    /// The minor the server implements.
    #[serde(default)]
    pub minor: Minor,
    /// The extension's own welcome block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opts: Option<Value>,
}

/// Limits announced in `welcome` (doc 09 section 4.6 names).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Limits {
    /// Largest message the server accepts, in bytes.
    pub max_frame_bytes: u32,
    /// Ping interval in milliseconds.
    pub hb_ms: u32,
    /// Missed-heartbeat tolerance, as a multiple of `hb_ms`.
    pub hb_slack: f64,
    /// Commands per second before `rate.limited`.
    pub cmds_per_sec: u32,
    /// Watched paths per session.
    pub max_watches: u32,
    /// Paths per `watch` frame.
    pub max_paths_per_watch: u32,
}

/// `welcome.replay`: the durable range, both ends inclusive (rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Replay {
    /// First `seq` that may be replayed.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub from: Seq,
    /// Replay is complete once the event with this `seq` arrived.
    #[serde(deserialize_with = "de_u", serialize_with = "ser_u")]
    pub to: Seq,
}

/// `welcome.resume` (rule 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ResumeOutcome {
    /// What happened to the cursor.
    pub status: ResumeStatus,
    /// For `new_timeline`: the change, so the client can replay its journal after
    /// `recovered_head` (doc 06 section 6.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch_change: Option<EpochChange>,
}

/// `welcome.resume.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum ResumeStatus {
    /// Replay follows (also for a first connect).
    Ok,
    /// The cursor is older than the retained window: read the inventory in pages.
    TooOld,
    /// The room was restored or failed over to another history (doc 01 D5.3 rule 2);
    /// `epoch_change` says where the new history starts.
    NewTimeline,
    /// A status this version does not know; treat it as `new_timeline`.
    #[serde(other)]
    Other,
}

/// The answer to one `cmd`, `get`, `watch` or `unwatch`: `{"cid", "seq"?, "epoch"?, "ok"}` or
/// `{"cid", "error"}`. Exactly one of `ok` and `error` must be present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CmdResult {
    /// The `cid` of the frame this answers.
    pub cid: CmdId,
    /// Last durable `seq` the command committed (rule 4).
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_u")]
    pub seq: Option<Seq>,
    /// Epoch of that commit.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_u")]
    pub epoch: Option<Epoch>,
    /// The last durable `seq` sent to this session when the result was produced (required);
    /// over HTTP the client applies the result once its stream has reached it (rule 4).
    #[serde(serialize_with = "ser_u")]
    pub upto: Seq,
    /// Success or failure.
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Success or failure of a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum Outcome {
    /// Done; the value's schema is the command's registered result schema.
    Ok(Value),
    /// Refused.
    Error(ErrorBody),
}

/// The wire form of [`CmdResult`] before the exactly-one rule is checked.
#[derive(Deserialize)]
struct RawResult {
    cid: CmdId,
    #[serde(default, deserialize_with = "de_opt_u")]
    seq: Option<Seq>,
    #[serde(default, deserialize_with = "de_opt_u")]
    epoch: Option<Epoch>,
    #[serde(deserialize_with = "de_u")]
    upto: Seq,
    #[serde(default, deserialize_with = "present")]
    ok: Option<Value>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

impl<'de> Deserialize<'de> for CmdResult {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = RawResult::deserialize(d)?;
        let outcome = match (raw.ok, raw.error) {
            (Some(v), None) => Outcome::Ok(v),
            (None, Some(e)) => Outcome::Error(e),
            (Some(_), Some(_)) => return Err(de::Error::custom("a result has both ok and error")),
            (None, None) => return Err(de::Error::custom("a result has neither ok nor error")),
        };
        Ok(Self { cid: raw.cid, seq: raw.seq, epoch: raw.epoch, upto: raw.upto, outcome })
    }
}

impl CmdResult {
    /// A success with no commit; `upto` is the last durable `seq` sent to the session.
    #[must_use]
    pub const fn ok(cid: CmdId, upto: Seq, value: Value) -> Self {
        Self { cid, seq: None, epoch: None, upto, outcome: Outcome::Ok(value) }
    }

    /// A refusal; `upto` is the last durable `seq` sent to the session.
    #[must_use]
    pub fn error(cid: CmdId, upto: Seq, e: impl Into<ErrorBody>) -> Self {
        Self { cid, seq: None, epoch: None, upto, outcome: Outcome::Error(e.into()) }
    }
}

/// One event (rule 5): durable ones carry `seq`, ephemeral ones `eseq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct EventFrame {
    /// Room-wide position, durable events only.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_u",
        serialize_with = "ser_opt_u"
    )]
    pub seq: Option<Seq>,
    /// Per-session counter, ephemeral events only.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_u",
        serialize_with = "ser_opt_u"
    )]
    #[cfg_attr(test, schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub eseq: Option<u64>,
    /// Registered type.
    #[serde(rename = "type")]
    pub ty: TypeName,
    /// The slot the event is about (rule 6), stamped by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<SlotNo>,
    /// Server time in milliseconds, informational.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_u",
        serialize_with = "ser_opt_u"
    )]
    #[cfg_attr(test, schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub t: Option<u64>,
    /// Hash of the previous event, tournament rooms only (D-31).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<Checksum>,
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
    /// Registered dotted key; clients localize from it. Kept as a string so a client accepts
    /// keys registered after it was built.
    pub key: String,
    /// Whether and how to retry.
    pub retry: Retry,
    /// Wait this long first, when `retry` is `after`.
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "de_u",
        serialize_with = "ser_u"
    )]
    #[cfg_attr(test, schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub after_ms: u64,
    /// The frame this answers when there is no `cid`: its `op`, or the `cid` of a frame that
    /// could not be decoded.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Key-specific details.
    #[serde(default = "empty_object")]
    pub detail: Value,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl From<ErrorCode> for ErrorBody {
    fn from(e: ErrorCode) -> Self {
        Self {
            code: e.code(),
            key: e.key().into(),
            retry: e.retry(),
            after_ms: 0,
            reference: None,
            detail: empty_object(),
        }
    }
}

impl ErrorBody {
    /// The registered code, when this side knows the key.
    #[must_use]
    pub fn registered(&self) -> Option<ErrorCode> {
        ErrorCode::from_key(&self.key)
    }

    /// Adds details.
    #[must_use]
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }

    /// Adds the reference to the frame it answers.
    #[must_use]
    pub fn with_ref(mut self, r: impl Into<String>) -> Self {
        self.reference = Some(r.into());
        self
    }
}
