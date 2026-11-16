//! CWP wire types shared by server, SDK and tools. Leaf crate: no IO, no clock, no rng.
#![no_std]

mod error;
mod ids;
mod msg;

pub use error::{ErrorCode, Retry};
pub use ids::{Cid, Epoch, ItemSeq, RoomId, Seq, SlotNo, Timestamp};
pub use msg::{Command, Event, EventKind};
