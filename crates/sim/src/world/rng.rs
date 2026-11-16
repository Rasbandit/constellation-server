//! Seeded choices for the schedule, on top of the pinned `spec-rng` stream.
use spec_rng::SpecRng;

#[derive(Debug)]
pub(super) struct Rng(SpecRng);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        let bytes = seed.to_le_bytes();
        Self(SpecRng::from_seed(core::array::from_fn(|i| bytes.get(i).copied().unwrap_or(0))))
    }

    /// Uniform in `[0, n)`; 0 when `n == 0`.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        u64::try_from(n)
            .ok()
            .and_then(|n| self.0.below(n))
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0)
    }

    pub(crate) fn below_u64(&mut self, n: u64) -> u64 {
        self.0.below(n).unwrap_or(0)
    }

    pub(crate) fn below_u32(&mut self, n: u32) -> u32 {
        u32::try_from(self.below_u64(u64::from(n))).unwrap_or(0)
    }

    pub(crate) fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub(crate) fn weighted(&mut self, weights: &[u64]) -> usize {
        self.0.weighted_choice(weights).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_choices_and_zero_is_safe() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        assert_eq!(
            (0..20).map(|_| a.below(1000)).collect::<Vec<_>>(),
            (0..20).map(|_| b.below(1000)).collect::<Vec<_>>()
        );
        assert_eq!(a.below(0), 0);
        assert!(!a.chance(0));
        assert!(a.chance(100));
        assert_eq!(a.weighted(&[]), 0);
    }
}
