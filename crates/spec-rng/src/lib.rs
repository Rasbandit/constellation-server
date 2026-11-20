//! The one pseudo-random generator for fills and seeds (doc 01 D3.1 and D3.2).
//!
//! Same seed, same numbers, in every language and forever. The stream is `ChaCha8` (64-bit block
//! counter, zero stream id, little-endian words, `u64` = low word then high word). Sampling is
//! defined here and pinned by `tests/vectors.rs`, whose expected values come from an independent
//! Python implementation, not from this crate.
//!
//! No clock, no OS randomness, no IO.
#![no_std]

use hkdf::Hkdf;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use sha2::Sha256;

/// HKDF salt, versioned so a future derivation change cannot collide with this one.
/// PROVISIONAL: doc 01 D3.1 names HKDF but not the salt or info layout; confirm in the spec.
const DERIVE_SALT: &[u8] = b"cwp-rng-v1";

/// A deterministic random stream.
#[derive(Debug, Clone)]
pub struct SpecRng(ChaCha8Rng);

impl SpecRng {
    /// Start a stream from a 32-byte seed.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(ChaCha8Rng::from_seed(seed))
    }

    /// Start a stream from a number: its 8 little-endian bytes, then zeros.
    #[must_use]
    pub fn from_u64(seed: u64) -> Self {
        Self::from_u64_tagged(seed, &[0; 8], 0)
    }

    /// [`SpecRng::from_u64`] in a named stream: `tag` at bytes 8 to 16 and `index` (little
    /// endian) at 16 to 24, so fills, baselines and per-player draws of one seed never share
    /// draws.
    #[must_use]
    pub fn from_u64_tagged(seed: u64, tag: &[u8; 8], index: u64) -> Self {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&seed.to_le_bytes());
        b[8..16].copy_from_slice(tag);
        b[16..24].copy_from_slice(&index.to_le_bytes());
        Self::from_seed(b)
    }

    /// Next 64 bits of the stream.
    pub fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    /// Uniform integer in `[0, n)`, or `None` when `n == 0`.
    ///
    /// Rejection sampling: draws below `2^64 mod n` are discarded, so there is no modulo bias.
    /// The number of draws used is part of the spec (it moves the stream).
    pub fn below(&mut self, n: u64) -> Option<u64> {
        // (2^64 - n) mod n == 2^64 mod n; `checked_rem` is None only for n == 0.
        let threshold = n.wrapping_neg().checked_rem(n)?;
        loop {
            let x = self.next_u64();
            if x >= threshold {
                return x.checked_rem(n);
            }
        }
    }

    /// Fisher-Yates shuffle, walking from the end.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let bound = u64::try_from(i).map_or(u64::MAX, |v| v.saturating_add(1));
            let j = self.below(bound).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
            items.swap(i, j);
        }
    }

    /// Index chosen by integer weights, or `None` if the weights are empty, sum to zero or
    /// overflow `u64`. A zero weight is never chosen.
    pub fn weighted_choice(&mut self, weights: &[u64]) -> Option<usize> {
        let total = weights.iter().try_fold(0_u64, |acc, w| acc.checked_add(*w))?;
        let roll = self.below(total)?;
        let mut cumulative = 0_u64;
        for (index, weight) in weights.iter().enumerate() {
            cumulative = cumulative.checked_add(*weight)?;
            if cumulative > roll {
                return Some(index);
            }
        }
        None
    }
}

/// Child seed for a named stream: `HKDF-SHA256(salt = "cwp-rng-v1", ikm = parent,
/// info = label || 0x00 || index as 4 bytes big-endian)`, 32 bytes.
///
/// Use `("slot", slot_index)` from the root seed, then `("hook", n)` from a slot seed.
/// Callers must keep labels short (they are concatenated into a fixed 64-byte buffer);
/// a longer label returns `None`.
#[must_use]
pub fn derive(parent: &[u8; 32], label: &[u8], index: u32) -> Option<[u8; 32]> {
    let mut info = [0_u8; 64];
    let label_end = label.len();
    let tail_end = label_end.checked_add(5)?;
    let dest = info.get_mut(..tail_end)?;
    let (head, tail) = dest.split_at_mut(label_end);
    head.copy_from_slice(label);
    let (sep, idx) = tail.split_at_mut(1);
    sep.fill(0);
    idx.copy_from_slice(&index.to_be_bytes());
    let mut out = [0_u8; 32];
    Hkdf::<Sha256>::new(Some(DERIVE_SALT), parent).expand(info.get(..tail_end)?, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod tests;
