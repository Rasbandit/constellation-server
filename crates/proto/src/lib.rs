//! CWP wire types shared by server, SDK and tools. Leaf crate: no IO, no clock, no rng.
#![no_std]

mod error;
mod ids;

pub use error::{ErrorCode, Retry};
pub use ids::{ItemSeq, Seq, SlotNo};
