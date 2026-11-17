//! CWP v1 on the wire (design doc 01 D1.10, proposal D-123).
//!
//! One envelope with a fixed set of frames, every feature a registered type inside `cmd` and
//! `event`, durable or ephemeral delivery declared per type, and room state as paths read with
//! `get` and followed with `watch`.
//!
//! These are wire types. The room's own `Command` and `Event` (crate root) stay the internal
//! model; an edge translates between the two.
mod frame;
mod names;
mod registry;
#[cfg(test)]
mod tests;

pub use frame::{
    Ack, ClientFrame, Cmd, CmdResult, ErrorBody, EventFrame, Get, Hello, Limits, Major, Outcome,
    Resume, Role, ServerFrame, Want, WantItems, Watch, Welcome,
};
pub use names::{
    CMD_ID_MAX, CmdId, ExtId, NameError, STATE_PATH_MAX, StatePath, TYPE_NAME_MAX, TypeName,
};
pub use registry::{
    CORE_TYPES, Check, CoreCmd, CoreEvent, DecodeError, Delivery, Goal, ItemGrant, Kind,
    LocationChecked, Scope, SlotGoal, Stability, StateChanged, TypeInfo, lookup,
};
