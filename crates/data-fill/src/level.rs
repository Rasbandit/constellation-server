//! Level fill: the counting and picking behind [`crate::room::Algorithm::Level`].
//!
//! The progression items are drawn in an order (the order a player finds them). One forward
//! solve that adds the order's items one at a time gives every location a level: the number of
//! order items held when it is first reached. Item `k` of the order (from 1) may sit only at a
//! location of level at most `k - 1`, so a player collects item 1, then 2, and so on: the seed
//! is beatable without a reachability check per item.
//!
//! Every order item has the same placement rules (confined items are placed before, and no
//! progression goes on an excluded location), so placing all of them is possible exactly when,
//! for every `k`, at least `k` locations have a level below `k` (the counting condition). Items
//! are placed from the last to the first, each among the levels that leave the condition true
//! for the items before it, so no pick can strand an earlier item.
//!
//! This module knows nothing about worlds: it works on locations bucketed by level. The solve
//! that labels them is the room's (`room::Linked::levels`).

use spec_rng::SpecRng;

/// Whether the counting condition holds: for every `k` from 1 to the number of items
/// (`levels.len()`), at least `k` locations have a level below `k`. `levels[v]` are the free
/// locations of level `v`.
pub(crate) fn feasible<L>(levels: &[Vec<L>]) -> bool {
    let mut open = 0;
    levels.iter().enumerate().all(|(v, at)| {
        open += at.len();
        open > v
    })
}

/// Reorder `order` so that, as far as the last solve can tell, the counting condition holds:
/// whenever the locations open so far are all needed by the items so far, the next item is the
/// first one left that opened a location in that solve (`levels[j + 1]` is what the item at
/// index `j` opened), moved forward; otherwise the next one left. Draws nothing. `false` when
/// no such item is left: the caller gives up on this order.
///
/// What an item opens depends on the items before it, so the new order is a guess that the
/// next solve confirms or not.
pub(crate) fn repair<T: Copy, L>(order: &mut [T], levels: &[Vec<L>]) -> bool {
    let items = order.len();
    let opens = |j: usize| levels.get(j + 1).map_or(0, Vec::len);
    let mut open = levels.first().map_or(0, Vec::len);
    let mut taken = vec![false; items];
    let (mut next, mut opener) = (0, 0);
    let mut out = Vec::with_capacity(items);
    while out.len() < items {
        if open <= out.len() {
            return false;
        }
        while taken[next] {
            next += 1;
        }
        let mut j = next;
        // Nothing to spare, and an item follows this one: this one has to open something.
        if open == out.len() + 1 && out.len() + 1 < items {
            opener = opener.max(next);
            while opener < items && (taken[opener] || opens(opener) == 0) {
                opener += 1;
            }
            if opener == items {
                return false;
            }
            j = opener;
        }
        taken[j] = true;
        open += opens(j);
        out.push(order[j]);
    }
    order.copy_from_slice(&out);
    true
}

/// Free locations per level as a binary tree over the levels: a node holds how many locations
/// its levels have, and the lowest running sum of `locations - 1` over them, from its first
/// level. The running sum through level `v` is the locations of level at most `v` less the
/// `v + 1` items that need them: 0 means every one of them is spoken for.
#[derive(Debug, Clone)]
struct Tree {
    /// Leaves (a power of two, at least the number of levels); level `v` is node `size + v`.
    size: usize,
    count: Vec<u32>,
    low: Vec<i64>,
}

impl Tree {
    fn new(per_level: impl ExactSizeIterator<Item = usize>) -> Self {
        let size = per_level.len().next_power_of_two();
        let mut t = Tree { size, count: vec![0; 2 * size], low: vec![-1; 2 * size] };
        for (v, n) in per_level.enumerate() {
            t.count[size + v] = u32::try_from(n).unwrap_or(u32::MAX);
            t.low[size + v] = i64::from(t.count[size + v]) - 1;
        }
        for i in (1..size).rev() {
            t.pull(i);
        }
        t
    }

    /// Levels under node `i`.
    fn width(&self, i: usize) -> usize {
        self.size >> i.ilog2()
    }

    /// Sum of `locations - 1` over node `i`'s levels.
    fn sum(&self, i: usize) -> i64 {
        i64::from(self.count[i]) - i64::try_from(self.width(i)).unwrap_or(i64::MAX)
    }

    fn pull(&mut self, i: usize) {
        self.count[i] = self.count[2 * i] + self.count[2 * i + 1];
        self.low[i] = self.low[2 * i].min(self.sum(2 * i) + self.low[2 * i + 1]);
    }

    /// One location of level `v` is gone.
    fn take(&mut self, v: usize) {
        let mut i = self.size + v;
        self.count[i] -= 1;
        self.low[i] -= 1;
        while i > 1 {
            i /= 2;
            self.pull(i);
        }
    }

    /// Locations of a level below `v`.
    fn below(&self, v: usize) -> u32 {
        if v >= self.size {
            return self.count[1];
        }
        let (mut i, mut n) = (self.size + v, 0);
        while i > 1 {
            if i % 2 == 1 {
                n += self.count[i - 1];
            }
            i /= 2;
        }
        n
    }

    /// The location number `k` (from 0) in level order: its level and its place in the level.
    fn nth(&self, mut k: u32) -> (usize, usize) {
        let mut i = 1;
        while i < self.size {
            i *= 2;
            if k >= self.count[i] {
                k -= self.count[i];
                i += 1;
            }
        }
        (i - self.size, k as usize)
    }

    /// The highest level at most `limit` whose running sum is 0, under node `i` (which starts
    /// at level `start`, with running sum `before` ahead of it). The sums up to `limit` are
    /// never negative (the counting condition), so a node whose lowest is above 0 has none.
    fn tight(&self, i: usize, start: usize, limit: usize, before: i64) -> Option<usize> {
        if start > limit || before + self.low[i] > 0 {
            return None;
        }
        if i >= self.size {
            return Some(start);
        }
        let half = self.width(i) / 2;
        self.tight(2 * i + 1, start + half, limit, before + self.sum(2 * i))
            .or_else(|| self.tight(2 * i, start, limit, before))
    }
}

/// Draws a pick makes beyond its first for the depth dial (`config::FILL_DEPTH_PERCENT`):
/// one per full 100 of `depth`, either sign, and one more with the remainder as a percent
/// chance. 0 draws nothing from `rng`.
pub(crate) fn draws(rng: &mut SpecRng, depth: i32) -> Option<u32> {
    let depth = depth.unsigned_abs();
    let chance = u64::from(depth % 100);
    Some(depth / 100 + u32::from(chance > 0 && rng.below(100)? < chance))
}

/// The free locations by level, while the order's items take them, last item first.
#[derive(Debug, Clone)]
pub(crate) struct Picker<L> {
    tree: Tree,
    levels: Vec<Vec<L>>,
}

impl<L: Copy> Picker<L> {
    /// `levels[v]`: the free locations of level `v`, for `v` below the number of items. The
    /// caller has checked [`feasible`].
    pub(crate) fn new(levels: Vec<Vec<L>>) -> Self {
        Picker { tree: Tree::new(levels.iter().map(Vec::len)), levels }
    }

    /// The lowest level item `k` (from 1) may take and leave the counting condition true for
    /// the items before it: one above the highest level below `k - 1` whose locations, with
    /// all those under it, are all needed by the items up to it.
    fn floor(&self, k: usize) -> usize {
        match k.checked_sub(2) {
            Some(limit) => self.tree.tight(1, 0, limit, 0).map_or(0, |v| v + 1),
            None => 0,
        }
    }

    /// Take a location for item `k` (from 1; call with the last item first): its level and the
    /// location. Uniform over the locations of a level from [`Picker::floor`] to `k - 1` when
    /// `depth` is 0 (one draw). Otherwise [`draws`] more are made and the deepest level drawn
    /// wins (`depth` above 0) or the shallowest (below 0). `None` when no location is left for
    /// it, which the counting condition rules out.
    pub(crate) fn take(&mut self, k: usize, rng: &mut SpecRng, depth: i32) -> Option<(usize, L)> {
        let (from, to) = (self.tree.below(self.floor(k)), self.tree.below(k));
        let n = u64::from(to.checked_sub(from).filter(|n| *n > 0)?);
        let mut best: Option<(usize, usize)> = None;
        for _ in 0..=draws(rng, depth)? {
            let drawn = self.tree.nth(from + u32::try_from(rng.below(n)?).ok()?);
            if best.is_none_or(|b| if depth > 0 { drawn.0 > b.0 } else { drawn.0 < b.0 }) {
                best = Some(drawn);
            }
        }
        let (v, at) = best?;
        self.tree.take(v);
        Some((v, self.levels[v].swap_remove(at)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `per_level[v]` locations of level `v`, each `(level, number)`.
    fn levels(per_level: &[usize]) -> Vec<Vec<(usize, usize)>> {
        per_level.iter().enumerate().map(|(v, n)| (0..*n).map(|i| (v, i)).collect()).collect()
    }

    /// The counting condition for the first `items` items, counted from the locations left.
    fn holds(p: &Picker<(usize, usize)>, items: usize) -> bool {
        feasible(&p.levels[..items])
    }

    #[test]
    fn the_counting_condition_needs_k_locations_below_level_k() {
        assert!(feasible(&levels(&[1, 1, 1])));
        assert!(feasible(&levels(&[3, 0, 0])));
        assert!(feasible(&levels(&[2, 0, 1])));
        assert!(!feasible(&levels(&[0, 3, 3])), "the first item has nowhere to go");
        assert!(!feasible(&levels(&[1, 0, 5])), "two items, one location below level 2");
        assert!(feasible::<u8>(&[]), "no items");
    }

    /// The level rule exactly: with one location per level, item `k` has to take the location
    /// of level `k - 1`. A rule one off in either direction (level `k` allowed, or level
    /// `k - 1` refused) cannot produce this.
    #[test]
    fn item_k_takes_a_level_below_k_and_never_level_k() {
        for items in [1usize, 2, 3, 8, 9, 33] {
            let mut p = Picker::new(levels(&vec![1; items]));
            let mut rng = SpecRng::from_u64(5);
            for k in (1..=items).rev() {
                assert_eq!(p.take(k, &mut rng, 250), Some((k - 1, (k - 1, 0))), "{items} items");
            }
        }
        // Level 1 is open to item 2 and not to item 1, whatever is drawn.
        for seed in 1..=50 {
            let mut p = Picker::new(levels(&[1, 4]));
            let mut rng = SpecRng::from_u64(seed);
            assert_eq!(p.take(2, &mut rng, 0).map(|x| x.0), Some(1), "level 0 is kept for item 1");
            assert_eq!(p.take(1, &mut rng, 0).map(|x| x.0), Some(0));
        }
    }

    #[test]
    fn every_pick_leaves_the_counting_condition_true_for_the_items_before() {
        let mut shapes = SpecRng::from_u64(9);
        let (mut reserved, mut deep) = (0, 0);
        for round in 0..400u64 {
            let items = 1 + usize::try_from(shapes.below(40).unwrap()).unwrap();
            let per: Vec<usize> =
                (0..items).map(|_| usize::try_from(shapes.below(4).unwrap()).unwrap()).collect();
            let all = levels(&per);
            if !feasible(&all) {
                continue;
            }
            let depth = [0, 50, 100, 300][usize::try_from(round % 4).unwrap()];
            let mut p = Picker::new(all);
            let mut rng = SpecRng::from_u64(round);
            let mut seen = std::collections::BTreeSet::new();
            for k in (1..=items).rev() {
                let floor = p.floor(k);
                reserved += usize::from(floor > 0);
                let (v, loc) = p.take(k, &mut rng, depth).expect("a location is left");
                assert_eq!(v, loc.0, "the level reported is the location's");
                assert!(v < k, "round {round}: item {k} at level {v}");
                assert!(v >= floor, "round {round}: item {k} below its floor");
                assert!(seen.insert(loc), "round {round}: {loc:?} taken twice");
                assert!(holds(&p, k - 1), "round {round}: items before {k} are stranded");
                deep += usize::from(v + 1 == k);
            }
        }
        assert!(reserved > 0 && deep > 0, "floors and deepest levels were exercised");
    }

    /// Without the floor, taking any level below `k` strands an earlier item: the reserve is
    /// what keeps the fill from a dead end.
    #[test]
    fn a_pick_below_the_floor_would_strand_an_earlier_item() {
        let p = Picker::new(levels(&[1, 1, 3]));
        assert_eq!(p.floor(3), 2, "levels 0 and 1 are needed by items 1 and 2");
        let mut q = p.clone();
        q.tree.take(0);
        q.levels[0].pop();
        assert!(!holds(&q, 2));
    }

    #[test]
    fn deeper_draws_favour_deeper_levels() {
        let mean = |depth| {
            let mut sum = 0;
            for seed in 0..400 {
                let mut p = Picker::new(levels(&[5, 5, 5, 5, 5]));
                sum += p.take(5, &mut SpecRng::from_u64(seed), depth).unwrap().0;
            }
            sum
        };
        let (flat, half, twice) = (mean(0), mean(50), mean(200));
        assert!(flat < half && half < twice, "{flat} {half} {twice}");
        assert!(mean(-200) < mean(-50) && mean(-50) < flat, "below 0 favours shallow levels");
        assert!((700..900).contains(&flat), "uniform over five levels: mean level 2, got {flat}");
    }

    #[test]
    fn repair_moves_an_opening_item_forward_and_draws_nothing() {
        // Order a, b, c, d: one location at the start, and only c (index 2) opens more.
        let mut order = ['a', 'b', 'c', 'd'];
        let found = levels(&[1, 0, 0, 5]);
        assert!(!feasible(&found));
        assert!(repair(&mut order, &found));
        assert_eq!(order, ['c', 'a', 'b', 'd']);
        // Nothing opens anything: no order can work.
        let mut stuck = ['a', 'b', 'c'];
        assert!(!repair(&mut stuck, &levels(&[1, 0, 0])));
        assert_eq!(stuck, ['a', 'b', 'c'], "a failed repair leaves the order alone");
        assert!(!repair(&mut stuck, &levels(&[0, 3, 3])), "nowhere to start");
        // A feasible order is kept as it is.
        let mut fine = ['a', 'b', 'c'];
        assert!(repair(&mut fine, &levels(&[2, 0, 1])));
        assert_eq!(fine, ['a', 'b', 'c']);
        // The last item needs nothing to follow it.
        let mut last = ['a', 'b'];
        assert!(repair(&mut last, &levels(&[2, 0])));
        assert_eq!(last, ['a', 'b']);
    }
}
