//! Experiment: fill and reachability for a data-only world (doc 07), no world code at
//! generation time. See `docs/data-only-world-experiment.md` for the format extensions it reads.

mod bitrules;
pub mod capacity;
pub mod check;
pub mod decode;
mod depth;
pub mod fairness;
pub mod fill;
mod fold;
pub mod job;
mod live;
pub mod par;
pub mod parity_rng;
pub mod prepared;
pub mod reach;
pub mod room;
pub mod section;
pub mod seed;
pub mod shuffle;
pub mod spheres;
pub mod synth;
pub mod world;

/// Whether this build keeps the measurements only the bench and the tests read (bytecode nodes
/// read, wall time and reached sets of a fill's rounds): with feature `memory-harness`, and in
/// this crate's own tests. A fill in the product pays for none of them.
pub(crate) const COUNTED: bool = cfg!(any(test, feature = "memory-harness"));

pub use bitrules::CLASSES as RULE_CLASSES;
pub use fill::{FillError, Instance, Plan};
pub use reach::{Reach, Stats};
pub use room::{Given, RoomFilled, Seat, fill_room, fill_room_spheres};
pub use world::World;
