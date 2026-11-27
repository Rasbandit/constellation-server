//! How deep in its own world an item first matters: the order the exact fill places a
//! player's progression in (`room.rs`).
//!
//! An assumed fill places an item against a state that holds everything not yet placed. What
//! is placed can be lost again when an item it lies behind is taken out, and with it what
//! lies behind that, in other worlds too. So the items a world needs first are placed last:
//! while a player's round item is one first needed at depth `d`, every item of theirs needed
//! before `d` is still held, nothing placed can take those away, and the player still reaches
//! all of their world that those items open, however many players the room has.
//!
//! The depth is read off one forward solve of the world alone ([`waves`]): step by step, every
//! copy that would open something right now (an exit, an event, a check, a half-day) is added.
//! The step a copy is added in is its depth. Copies that never open anything are the deepest.

use crate::fill::Plan;
use crate::reach::Stats;
use crate::room::Given;
use crate::world::World;

/// Per world item: the step of each progression copy of `plan` that opens something, in the
/// order the copies do (so ascending). A copy beyond the list never opened anything.
/// Confined items are held from the start: they are placed before the rounds. The solve's
/// work is added to `stats`.
pub(crate) fn waves(w: &World, plan: &Plan, stats: &mut Stats) -> Vec<Vec<u32>> {
    let mut left = vec![0u32; w.items()];
    let mut confined = Vec::new();
    for i in &plan.progression {
        if plan.confined(*i) {
            confined.push(*i);
        } else {
            left[usize::from(*i)] += 1;
        }
    }
    let kinds: Vec<u16> =
        (0..w.items()).filter(|i| left[*i] > 0).filter_map(|i| u16::try_from(i).ok()).collect();
    let mut out = vec![Vec::new(); w.items()];
    let mut r = plan.state(w, &confined, &plan.locked_placed(w));
    for step in 0u32.. {
        let has = |i: &u16| left[usize::from(*i)] > 0;
        let mut now: Vec<u16> = Vec::new();
        for i in kinds.iter().copied().filter(has) {
            if r.opens(i, true) {
                now.push(i);
            }
        }
        if now.is_empty() {
            // Nothing opens alone: items a rule needs together go in together.
            now = kinds.iter().copied().filter(has).filter(|i| r.wants(*i)).collect();
        }
        if now.is_empty() {
            break;
        }
        for i in now {
            out[usize::from(i)].push(step);
            left[usize::from(i)] -= 1;
            r.add(i, 1);
        }
        r.run();
    }
    stats.absorb(&r.stats);
    out
}

/// Order `prog` so that each player's deepest copies are last (`waves[player]` as [`waves`]
/// gives them). Copies of one depth keep the order they were drawn in. A round takes the last
/// item of every player's pile, so the deepest are placed first.
pub(crate) fn deepest_last(prog: &mut [Given], waves: &[Vec<Vec<u32>>]) {
    let mut used: Vec<Vec<usize>> = waves.iter().map(|w| vec![0; w.len()]).collect();
    let mut keyed: Vec<(u32, Given)> = prog
        .iter()
        .map(|g| {
            let (p, i) = (usize::from(g.to), usize::from(g.item));
            used[p][i] += 1;
            (waves[p][i].get(used[p][i] - 1).copied().unwrap_or(u32::MAX), *g)
        })
        .collect();
    keyed.sort_by_key(|x| x.0);
    for (slot, (_, g)) in prog.iter_mut().zip(keyed) {
        *slot = g;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{Hostile, Shape, room};

    /// In a chain world key `k` opens region `k + 1`: its depth is `k`, whatever the room.
    #[test]
    fn a_chain_key_is_as_deep_as_the_region_it_opens() {
        let c = room(&Shape { checks: 60, ..Shape::hostile(Hostile::Chain) }, 1, 2, 1).unwrap();
        for s in c.seats() {
            let waves = waves(s.w, s.plan, &mut Stats::default());
            for k in 0..6u32 {
                let item = s.w.item(&format!("k{k}")).unwrap();
                assert_eq!(waves[usize::from(item)], [k]);
            }
        }
    }

    /// Four copies of a kind that single checks read: the first copy opens them, the others
    /// open nothing and are the deepest. A key in front of most of the world is the shallowest.
    #[test]
    fn copies_that_open_nothing_are_the_deepest() {
        let c = room(&Shape { checks: 60, ..Shape::hostile(Hostile::Gated) }, 1, 1, 1).unwrap();
        let s = c.seats()[0];
        let waves = waves(s.w, s.plan, &mut Stats::default());
        assert_eq!(waves[usize::from(s.w.item("k0").unwrap())], [0]);
        let loose = waves[usize::from(s.w.item("p0").unwrap())].len();
        assert!(loose <= 1, "{loose} copies of p0 open something");
    }

    /// The deepest go last, per player; copies of one depth stay in the order drawn; a copy
    /// without a depth is deeper than all.
    #[test]
    fn the_deepest_copies_are_last_and_ties_keep_their_order() {
        let g = |to, item| Given { to, item };
        let waves = vec![vec![vec![2], vec![0, 5], vec![]], vec![vec![1], vec![1], vec![0]]];
        let mut prog = vec![g(0, 2), g(1, 1), g(0, 1), g(0, 0), g(1, 0), g(0, 1), g(1, 2), g(0, 2)];
        deepest_last(&mut prog, &waves);
        let of = |p| prog.iter().filter(|x| x.to == p).map(|x| x.item).collect::<Vec<_>>();
        assert_eq!(of(0), [1, 0, 1, 2, 2]);
        assert_eq!(of(1), [2, 1, 0]);
    }
}
