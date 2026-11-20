//! Experiment: fill and reachability for a data-only world (doc 07), no world code at
//! generation time. See `docs/data-only-world-experiment.md` for the format extensions it reads.

pub mod check;
pub mod decode;
pub mod fairness;
pub mod fill;
mod fold;
pub mod par;
pub mod parity_rng;
pub mod reach;
pub mod room;
pub mod section;
pub mod seed;
pub mod spheres;
pub mod world;

pub use fill::{FillError, Filled, Instance, Plan, fill, fill_spheres, validate};
pub use reach::{Reach, Stats};
pub use room::{Given, RoomFilled, Seat, fill_room, fill_room_spheres};
pub use world::World;

/// The fill as seed-bundle placements for one slot (receiver 1, single player).
#[must_use]
pub fn placements(w: &World, filled: &Filled, progression: &[bool]) -> Vec<seed_bundle::Placed> {
    filled
        .placed
        .iter()
        .enumerate()
        .filter_map(|(l, i)| {
            let i = (*i)? as usize;
            Some(seed_bundle::Placed {
                location: w.loc_id(u32::try_from(l).unwrap_or(0)),
                item: w.item_id(u16::try_from(i).unwrap_or(0)),
                receiver: 1,
                flags: u8::from(progression.get(i).copied().unwrap_or(false)),
                // The fill does not record spheres in its placements (`fill_spheres` reports them).
                sphere: None,
            })
        })
        .collect()
}
