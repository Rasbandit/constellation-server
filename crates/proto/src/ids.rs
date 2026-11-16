//! Position and slot identifiers.
use serde::{Deserialize, Serialize};

/// Room-wide event log position, gapless.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Seq(pub u64);

/// Per-slot delivery index (doc 05 7.1, D-02).
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct ItemSeq(pub u64);

/// Slot number within a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlotNo(pub u32);

/// Client message id, the idempotency key, unique per slot (doc 05 7.1).
/// The doc sketches a string of at most 64 bytes; a plain integer is used until the wire codec owns the encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Cid(pub u64);

/// Room identifier (doc 05 7.2). The doc's real id is longer; a plain integer until the registry owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RoomId(pub u64);

/// Owner epoch, the fence token bumped on every room move (doc 02 D5).
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Epoch(pub u64);

/// Seconds since the Unix epoch. Supplied by the runtime; core crates never read a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp(pub u64);
