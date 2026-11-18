//! Sphere-by-sphere collection over a filled seed: which locations become reachable in each
//! round when everything reached so far is collected. Only progression items count as held,
//! as in the legacy generator's own sweeps.

use crate::fill::Plan;
use crate::reach::Reach;
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

/// Collect `placed` round by round. `progression[item]` says whether an item counts as held.
#[must_use]
pub fn spheres(w: &World, plan: &Plan, placed: &[Option<u16>], progression: &[bool]) -> Spheres {
    collect(w, plan, &mut plan.base(w, &[]), placed, progression)
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
    let out = collect(w, plan, r, placed, progression);
    r.release();
    out
}

fn collect(
    w: &World,
    plan: &Plan,
    r: &mut Reach<'_>,
    placed: &[Option<u16>],
    progression: &[bool],
) -> Spheres {
    let filled: Vec<u32> =
        plan.included.iter().copied().filter(|l| placed[*l as usize].is_some()).collect();
    let mut taken = vec![false; w.locations()];
    let mut out = Vec::new();
    loop {
        r.run();
        let mut new: Vec<u32> =
            filled.iter().copied().filter(|l| !taken[*l as usize] && r.reached(*l)).collect();
        if new.is_empty() {
            break;
        }
        new.sort_unstable();
        for l in &new {
            taken[*l as usize] = true;
            if let Some(i) = placed[*l as usize]
                && progression.get(i as usize).copied().unwrap_or(false)
            {
                r.add(i, 1);
            }
        }
        out.push(new);
    }
    let unreachable = filled.into_iter().filter(|l| !taken[*l as usize]).collect();
    Spheres { spheres: out, beatable: r.goal(), unreachable }
}
