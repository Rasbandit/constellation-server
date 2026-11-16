//! Position and slot identifiers.
use serde::{Deserialize, Serialize};

/// Room-wide event log position, gapless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Seq(pub u64);

/// Per-slot delivery index (doc 05 7.1, D-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ItemSeq(pub u64);

/// Slot number within a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlotNo(pub u32);
