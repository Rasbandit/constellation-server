//! The `none` reference draw: an assignment of things to slots under a permission table.

use spec_rng::SpecRng;

/// An index below `n` from the stream; `None` when `n` is 0.
pub(super) fn pick(rng: &mut SpecRng, n: usize) -> Option<usize> {
    rng.below(u64::try_from(n).ok()?).and_then(|k| usize::try_from(k).ok())
}

/// The `none` reference draw (doc 17 section 4.5, without pins, counts and weights, which this
/// slice does not have): visit the slots in a seeded order, most restricted first, and give
/// each one allowed thing that is still free, drawn evenly. A slot with none left frees one by
/// moving earlier slots to other allowed things (the augmenting step of bipartite matching),
/// so the draw fails only when no assignment exists. `allows(slot, thing)` is the table.
pub(super) fn assign(
    slots: usize,
    things: usize,
    allows: &dyn Fn(usize, usize) -> bool,
    rng: &mut SpecRng,
) -> Option<Vec<usize>> {
    let mut order: Vec<usize> = (0..slots).collect();
    rng.shuffle(&mut order);
    let choices: Vec<usize> =
        (0..slots).map(|slot| (0..things).filter(|t| allows(slot, *t)).count()).collect();
    order.sort_by_key(|slot| choices[*slot]);
    let mut owner: Vec<Option<usize>> = vec![None; things];
    for slot in order {
        let free: Vec<usize> =
            (0..things).filter(|t| allows(slot, *t) && owner[*t].is_none()).collect();
        match pick(rng, free.len()) {
            Some(k) => owner[free[k]] = Some(slot),
            None => {
                if !augment(slot, allows, &mut owner, &mut vec![false; things]) {
                    return None;
                }
            }
        }
    }
    let mut got = vec![usize::MAX; slots];
    owner.iter().enumerate().for_each(|(t, o)| o.iter().for_each(|slot| got[*slot] = t));
    Some(got)
}

/// Give `slot` a thing, moving the slots that hold its allowed things on to others. Nests at
/// most one level per slot ([`rando_limits::SHUFFLE_MAX_SLOTS`]).
fn augment(
    slot: usize,
    allows: &dyn Fn(usize, usize) -> bool,
    owner: &mut [Option<usize>],
    seen: &mut [bool],
) -> bool {
    for t in 0..owner.len() {
        if seen[t] || !allows(slot, t) {
            continue;
        }
        seen[t] = true;
        if owner[t].is_none_or(|o| augment(o, allows, owner, seen)) {
            owner[t] = Some(slot);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid assignment for every seed, also where a slot must push an earlier one on; and
    /// none when the table has no assignment.
    #[test]
    fn the_none_draw_finds_an_assignment_whenever_one_exists() {
        // Slots 0 and 1 take things 0 and 1, slot 2 takes 0 or 2, slot 3 takes 2 or 3.
        let rows = [
            [true, true, false, false],
            [true, true, false, false],
            [true, false, true, false],
            [false, false, true, true],
        ];
        let table = |slot: usize, t: usize| rows[slot][t];
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..200 {
            let got = assign(4, 4, &table, &mut SpecRng::from_u64(seed)).unwrap();
            assert!(got.iter().enumerate().all(|(slot, t)| rows[slot][*t]), "seed {seed}");
            let mut things = got.clone();
            things.sort_unstable();
            assert_eq!(things, [0, 1, 2, 3], "seed {seed}");
            assert_eq!(got, assign(4, 4, &table, &mut SpecRng::from_u64(seed)).unwrap());
            seen.insert(got);
        }
        assert_eq!(seen.len(), 2, "both assignments the table has: {seen:?}");
        // Three slots that all need thing 0 or 1.
        let tight = |_: usize, t: usize| t < 2;
        assert!(assign(3, 3, &tight, &mut SpecRng::from_u64(1)).is_none());
        // More things than slots leaves things over; more slots than things has no assignment.
        let any = |_: usize, _: usize| true;
        assert_eq!(assign(1, 3, &any, &mut SpecRng::from_u64(1)).map(|g| g.len()), Some(1));
        assert!(assign(2, 1, &any, &mut SpecRng::from_u64(1)).is_none());
    }
}
