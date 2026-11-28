//! How much room a world leaves early on for the progression still to be placed: a warning
//! about a fill that needed repairs (`seed.rs`), never an input to a fill.
//!
//! The early capacity ratio of a world as one player plays it: draw the progression pool in a
//! random order; holding the first `j - 1` items of it, count the free locations that may take
//! progression and are reached, and divide by `j`, the items that would have to fit there.
//! Mean over `config::FILL_CAPACITY_ORDERS` orders, lowest over `j`. Under 1, at some point
//! there are more items to seat than places to seat them, which is a likely cause when an
//! assumed fill of such a world needs repairs. It is a forward-fill notion (the first `j - 1`
//! items held, where the assumed fill holds everything unplaced), so worlds under 1 can fill
//! without any: the heavy synthetic profile reads 0.83 and never restarted on the bench. A
//! ratio under 1 is a signal to look at, not a verdict.

use spec_rng::SpecRng;

use crate::fill::Plan;
use crate::world::World;

/// The early capacity ratio of `w` played with `plan` (see the module text); `None` for a world
/// without progression. The orders are drawn from streams of their own: the fill's draws are
/// untouched.
#[must_use]
pub fn early(w: &World, plan: &Plan) -> Option<f64> {
    let locked = plan.locked_placed(w);
    // The usable locations as set words, laid out as a state's reached set.
    let mut usable = vec![0u64; w.locations().div_ceil(64)];
    for l in plan.included.iter().map(|l| *l as usize) {
        if locked[l].is_none() && !plan.excluded[l] {
            usable[l / 64] |= 1 << (l % 64);
        }
    }
    let mut pool = plan.progression.clone();
    let n = pool.len();
    if n == 0 {
        return None;
    }
    let mut sum = vec![0.0; n];
    for k in 0..config::FILL_CAPACITY_ORDERS {
        SpecRng::from_u64(u64::from(k)).shuffle(&mut pool);
        let mut r = plan.state(w, &[], &locked);
        for (j, item) in pool.iter().enumerate() {
            let words = r.reached_words().iter().zip(&usable);
            let reached: u32 = words.map(|(a, b)| (a & b).count_ones()).sum();
            sum[j] += f64::from(reached) / (j + 1) as f64;
            r.add(*item, 1);
            r.run();
        }
    }
    let orders = f64::from(config::FILL_CAPACITY_ORDERS);
    sum.into_iter().map(|s| s / orders).reduce(f64::min)
}

#[cfg(test)]
mod tests {
    use crate::synth::{Hostile, Shape, room};

    /// As much progression as locations that take it is under 1; an ordinary world is well
    /// over it.
    #[test]
    fn a_world_with_no_spare_locations_is_under_one_and_an_ordinary_one_is_not() {
        let ratio = |shape: &Shape| {
            let c = room(shape, 1, 1).unwrap();
            super::early(&c.worlds[0], &c.plans[0]).unwrap()
        };
        let tight = ratio(&Shape { checks: 200, ..Shape::hostile(Hostile::Tight) });
        let light = ratio(&Shape { checks: 200, ..Shape::default() });
        assert!(tight < 1.0 && light > 1.0, "tight {tight}, light {light}");
    }
}
