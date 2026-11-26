//! A list the fill takes entries out of: a taken entry leaves a tombstone instead of shifting
//! everything after it, and a cursor skips the dead prefix. A scan sees the live entries in the
//! order they had, so the first match is the one `position` plus `Vec::remove` would take.
//! Tombstones are dropped once they outnumber the live entries, so a scan never walks more
//! dead slots than live ones (a live entry nobody takes cannot pin a long dead run behind it),
//! and dropping them costs O(1) per take on average.

/// Entries in order; `None` marks a taken one.
#[derive(Debug, Clone)]
pub(crate) struct Live<T> {
    items: Vec<Option<T>>,
    /// No live entry sits before this index.
    first: usize,
    /// Tombstones in `items`.
    dead: usize,
}

impl<T: Copy> Live<T> {
    pub(crate) fn new(items: Vec<T>) -> Self {
        Live { items: items.into_iter().map(Some).collect(), first: 0, dead: 0 }
    }

    /// Take out the first live entry `ok` accepts.
    pub(crate) fn take_first(&mut self, mut ok: impl FnMut(T) -> bool) -> Option<T> {
        let at = self.items[self.first..].iter().position(|x| x.is_some_and(&mut ok))?;
        let x = self.items[self.first + at].take();
        self.dead += 1;
        if self.dead * 2 > self.items.len() {
            self.retain(|_| true);
        }
        while self.items.get(self.first).is_some_and(Option::is_none) {
            self.first += 1;
        }
        x
    }

    /// Keep only the live entries `keep` accepts, in order (and drop the tombstones).
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(T) -> bool) {
        self.items.retain(|x| x.is_some_and(&mut keep));
        (self.first, self.dead) = (0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::Live;

    /// The first entry is never taken, so the cursor cannot move: the slots a scan walks still
    /// stay within twice the live entries.
    #[test]
    fn dead_slots_never_outnumber_the_live_entries() {
        let mut live = Live::new((0..1000u32).collect());
        for left in (1..1000usize).rev() {
            assert!(live.take_first(|x| x != 0).is_some());
            assert!(live.items.len() <= 2 * left, "{} slots for {left} entries", live.items.len());
        }
        assert_eq!(live.take_first(|_| true), Some(0));
        assert_eq!(live.take_first(|_| true), None);
    }

    /// Against `position` plus `remove` on a plain list, the reference it replaces.
    #[test]
    fn takes_what_position_and_remove_would_in_the_same_order() {
        let mut rng = spec_rng::SpecRng::from_u64(3);
        for n in [0usize, 1, 2, 7, 40] {
            let mut plain: Vec<u32> = (0..u32::try_from(n).unwrap()).collect();
            rng.shuffle(&mut plain);
            let mut live = Live::new(plain.clone());
            for step in 0..3 * n {
                let m = u32::try_from(step % 5 + 1).unwrap();
                let ok = |x: u32| x.is_multiple_of(m);
                if step == n {
                    plain.retain(|x| x % 3 != 1);
                    live.retain(|x| x % 3 != 1);
                }
                let want = plain.iter().position(|x| ok(*x)).map(|i| plain.remove(i));
                assert_eq!(live.take_first(ok), want, "n {n}, step {step}");
            }
            assert_eq!(live.take_first(|_| true), plain.first().copied(), "n {n}: what is left");
        }
    }
}
