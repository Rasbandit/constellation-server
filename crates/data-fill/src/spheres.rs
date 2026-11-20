//! Sphere-by-sphere collection over a filled seed: which locations become reachable in each
//! round when everything reached so far is collected. Only progression items count as held,
//! as in the legacy generator's own sweeps.
//!
//! One collection serves a whole room: a single player is a room of one.

use crate::fill::Plan;
use crate::reach::Reach;
use crate::room::{Given, Seat};
use crate::world::World;

/// The spheres of one filled seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spheres {
    /// Locations (world indices, ascending) newly reached in each round.
    pub spheres: Vec<Vec<u32>>,
    /// The goal holds once everything reachable is collected.
    pub beatable: bool,
    /// Filled locations never reached.
    pub unreachable: Vec<u32>,
}

/// The spheres of a filled room: locations are `(player, location)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomSpheres {
    /// Locations (ascending) newly reached in each round, over every player's world.
    pub spheres: Vec<Vec<(u16, u32)>>,
    /// Per player: the goal holds once everything reachable is collected.
    pub beatable: Vec<bool>,
    /// Filled locations never reached.
    pub unreachable: Vec<(u16, u32)>,
}

impl RoomSpheres {
    /// Every player's goal holds.
    #[must_use]
    pub fn all_beatable(&self) -> bool {
        self.beatable.iter().all(|b| *b)
    }
}

/// A room of one player's spheres as a single player's.
fn one_player(s: RoomSpheres) -> Spheres {
    Spheres {
        spheres: s.spheres.into_iter().map(|v| v.into_iter().map(|x| x.1).collect()).collect(),
        beatable: s.beatable[0],
        unreachable: s.unreachable.into_iter().map(|x| x.1).collect(),
    }
}

/// Collect `placed` round by round. `progression[item]` says whether an item counts as held.
#[must_use]
pub fn spheres(w: &World, plan: &Plan, placed: &[Option<u16>], progression: &[bool]) -> Spheres {
    let seat = [Seat { w, plan, progression }];
    let mut r = [plan.base(w, &[])];
    let at = |_: u16, l: u32| placed[l as usize].map(|item| Given { to: 0, item });
    let s = collect(&seat, &mut r, &at);
    one_player(s)
}

/// [`spheres`] of a filled seed from the fill's own final state `r` (every placed item
/// collected): the rounds evaluate only the sites that state derived, in its buffers.
pub(crate) fn from_state(
    w: &World,
    plan: &Plan,
    r: &mut Reach<'_>,
    placed: &[Option<u16>],
    progression: &[bool],
) -> Spheres {
    r.only_what_holds();
    plan.rebase(r, &[]);
    let seat = [Seat { w, plan, progression }];
    let at = |_: u16, l: u32| placed[l as usize].map(|item| Given { to: 0, item });
    let s = collect(&seat, std::slice::from_mut(r), &at);
    r.release();
    one_player(s)
}

/// The spheres of a room's placements (`placed[player][location]`), from fresh states.
///
/// # Errors
/// [`crate::FillError::Instance`] for seats or placements that do not fit the room.
pub fn room_spheres(
    seats: &[Seat<'_>],
    placed: &[Vec<Option<Given>>],
) -> Result<RoomSpheres, crate::FillError> {
    crate::room::check_placed(seats, placed)?;
    let mut models: Vec<Reach<'_>> = seats.iter().map(|s| s.plan.base(s.w, &[])).collect();
    Ok(collect(seats, &mut models, &|p, l| placed[usize::from(p)][l as usize]))
}

/// [`room_spheres`] from the states of a finished fill (the `only_what_holds` shortcut).
pub(crate) fn room_from_states(
    seats: &[Seat<'_>],
    models: &mut [Reach<'_>],
    placed: &[Vec<Option<Given>>],
) -> RoomSpheres {
    for (s, m) in seats.iter().zip(models.iter_mut()) {
        m.only_what_holds();
        s.plan.rebase(m, &[]);
    }
    let out = collect(seats, models, &|p, l| placed[usize::from(p)][l as usize]);
    models.iter_mut().for_each(Reach::release);
    out
}

/// Rounds over every player's states: run all, take what each filled location newly reached
/// holds, give each progression item to its owner's state.
fn collect(
    seats: &[Seat<'_>],
    models: &mut [Reach<'_>],
    at: &dyn Fn(u16, u32) -> Option<Given>,
) -> RoomSpheres {
    let player = |p: usize| u16::try_from(p).unwrap_or(u16::MAX);
    let filled: Vec<(u16, u32)> = seats
        .iter()
        .enumerate()
        .flat_map(|(p, s)| s.plan.included.iter().map(move |l| (player(p), *l)))
        .filter(|(p, l)| at(*p, *l).is_some())
        .collect();
    let mut taken: Vec<Vec<bool>> = seats.iter().map(|s| vec![false; s.w.locations()]).collect();
    let mut out = Vec::new();
    loop {
        models.iter_mut().for_each(Reach::run);
        let mut new: Vec<(u16, u32)> = filled
            .iter()
            .copied()
            .filter(|(p, l)| {
                !taken[usize::from(*p)][*l as usize] && models[usize::from(*p)].reached(*l)
            })
            .collect();
        if new.is_empty() {
            break;
        }
        new.sort_unstable();
        for (p, l) in &new {
            taken[usize::from(*p)][*l as usize] = true;
            if let Some(g) = at(*p, *l)
                && seats[usize::from(g.to)]
                    .progression
                    .get(usize::from(g.item))
                    .copied()
                    .unwrap_or(false)
            {
                models[usize::from(g.to)].add(g.item, 1);
            }
        }
        out.push(new);
    }
    let unreachable =
        filled.into_iter().filter(|(p, l)| !taken[usize::from(*p)][*l as usize]).collect();
    RoomSpheres { spheres: out, beatable: models.iter().map(|m| m.goal()).collect(), unreachable }
}
