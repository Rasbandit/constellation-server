//! CWP v1 on the wire (design doc 01 D1.10, proposal D-123).
//!
//! One envelope with a fixed set of frames, every feature a registered type inside `cmd` and
//! `event`, durable or ephemeral delivery and an audience declared per type, and room state as
//! paths read with `get` and followed with `watch`.
//!
//! These are wire types. The room's own `Command` and `Event` (crate root) stay the internal
//! model; an edge translates between the two.
mod codec;
mod frame;
mod names;
mod path;
mod registry;
mod serde_util;
#[cfg(test)]
mod tests;

pub use codec::{BadFrame, decode_client, decode_server};
pub use frame::{
    Ack, Beat, Caller, CallerKind, ClientFrame, ClientInfo, ClientKind, Cmd, CmdResult, Credential,
    ErrorBody, EventFrame, ExtChoice, ExtOffer, GameInfo, Get, Hello, Limits, Major, Minor,
    Outcome, ProtoVersion, Replay, Resume, ResumeOutcome, ResumeStatus, RoomRole, Scope,
    ServerFrame, SlotInfo, Want, WantItems, Watch, Welcome,
};
pub use names::{
    CMD_ID_MAX, Checksum, CmdId, ExtId, GameId, ItemUid, NAME_MAX, NameError, SECRET_MAX,
    STATE_PATH_MAX, Secret, StatePath, StreamName, TypeName,
};
pub use path::{CORE_PATHS, CorePath, GamePart, KvScope, PathInfo, Read, SlotPart, UnknownPath};
pub use registry::{
    Allow, Audience, COMMON_CMD_ERRORS, CORE_TYPES, Check, CheckOk, ClientState, CoreCmd,
    CoreEvent, DecodeError, Delivered, Delivery, EpochChange, GetOk, Goal, ItemFlag, ItemGrant,
    Kind, LocationChecked, SentItem, SessionUpdate, SessionUpdateOk, SlotGoal, Source, Stability,
    StateChanged, StatusSet, Template, TypeInfo, WatchOk, lookup,
};
pub use serde_util::{MAX_SAFE, MAX_SAFE_I64};
