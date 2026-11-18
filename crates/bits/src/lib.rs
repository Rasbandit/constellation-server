//! Compact index types, the one home for them: [`BitSet`] (a set of dense indexes, one bit
//! each), [`Packed`] (a list of small integers, each in the fewest bits), and the raw bit
//! stream both write ([`Packer`], [`read`]): fields of `width` bits, least significant bit
//! first (bit `n` is bit `n % 8` of byte `n / 8`), the layout of the v2 seed file
//! (`docs/seed-bundle-v2.md`). No dependencies, `no_std` with `alloc`.
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;

/// Bits that hold every value in `0..=max`: `ceil(log2(max + 1))`, so 0 for `max == 0`.
#[must_use]
pub const fn width(max: u64) -> u8 {
    // At most 64, so the cast is exact.
    (u64::BITS - max.leading_zeros()) as u8
}

/// The `width`-bit field (at most 57 bits) that starts at bit `bit` of `bytes`. Bits past the
/// end read as 0, so a reader never panics on a short slice.
#[must_use]
pub fn read(bytes: &[u8], bit: usize, width: u8) -> u64 {
    if width == 0 {
        return 0;
    }
    let mut word = [0_u8; 8];
    let tail = bytes.get(bit / 8..).unwrap_or_default();
    let n = tail.len().min(8);
    word[..n].copy_from_slice(&tail[..n]);
    let mask = if width >= 64 { u64::MAX } else { (1_u64 << width) - 1 };
    (u64::from_le_bytes(word) >> (bit % 8)) & mask
}

/// Bytes that `count` fields of `width` bits take.
#[must_use]
pub const fn bytes_for(count: usize, width: usize) -> usize {
    (count * width).div_ceil(8)
}

/// Appends `width`-bit fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Packer {
    bytes: Vec<u8>,
    bit: usize,
}

impl Packer {
    /// Append the low `width` bits of `value` (`width` at most 64).
    pub fn push(&mut self, value: u64, width: u8) {
        for k in 0..width {
            if self.bit.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> k) & 1 == 1
                && let Some(last) = self.bytes.last_mut()
            {
                *last |= 1 << (self.bit % 8);
            }
            self.bit += 1;
        }
    }

    /// The packed bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// A set of indexes `0..len`, one bit each, stored as little-endian `u64` words.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct BitSet {
    words: Vec<u64>,
    len: usize,
}

impl BitSet {
    /// An empty set over `0..len`.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self { words: alloc::vec![0; len.div_ceil(64)], len }
    }

    /// A set over `0..len` from its words; `None` when the word count is wrong or a bit at or
    /// past `len` is set.
    #[must_use]
    pub fn from_words(words: Vec<u64>, len: usize) -> Option<Self> {
        let s = Self { words, len };
        (s.words.len() == len.div_ceil(64) && s.ones().all(|i| i < len)).then_some(s)
    }

    /// The words, low indexes first.
    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// The index range this set covers.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the range is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether `i` is in the set (false past the range).
    #[must_use]
    pub fn contains(&self, i: usize) -> bool {
        contains(&self.words, i)
    }

    /// Add `i`; false when `i` is past the range.
    pub fn insert(&mut self, i: usize) -> bool {
        if i >= self.len {
            return false;
        }
        if let Some(w) = self.words.get_mut(i / 64) {
            *w |= 1 << (i % 64);
        }
        true
    }

    /// How many indexes are in the set.
    #[must_use]
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The indexes in the set, ascending.
    pub fn ones(&self) -> impl Iterator<Item = usize> + '_ {
        ones(&self.words)
    }
}

/// Whether bit `i` of `words` is set (false past the end).
#[must_use]
pub fn contains(words: &[u64], i: usize) -> bool {
    words.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
}

/// Set bit indexes of `words`, ascending.
pub fn ones(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
    words.iter().enumerate().flat_map(|(k, &w)| {
        let mut w = w;
        core::iter::from_fn(move || {
            (w != 0).then(|| {
                let b = w.trailing_zeros() as usize;
                w &= w - 1;
                k * 64 + b
            })
        })
    })
}

/// A list of integers, each in `width` bits (at most 32): a compact order list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Packed {
    bits: Packer,
    width: u8,
    len: usize,
}

impl Packed {
    /// An empty list of `width`-bit values (at most 32).
    #[must_use]
    pub fn new(width: u8) -> Self {
        Self { bits: Packer::default(), width: width.min(32), len: 0 }
    }

    /// Append `v`; false (nothing appended) when it does not fit the width.
    pub fn push(&mut self, v: u32) -> bool {
        if self.width < 32 && u64::from(v) >> self.width != 0 {
            return false;
        }
        self.bits.push(u64::from(v), self.width);
        self.len += 1;
        true
    }

    /// The value at `i`.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<u32> {
        // A width of at most 32 bits always fits a u32.
        (i < self.len)
            .then(|| read(&self.bits.bytes, i * usize::from(self.width), self.width) as u32)
    }

    /// How many values.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the list is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Every value, in order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.len).filter_map(|i| self.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[test]
    fn width_is_the_bits_of_the_largest_value() {
        // Hand values: 0 needs no bit, 1 one, 2..=3 two, 236 (MM's last item index) eight.
        for (max, w) in [(0, 0), (1, 1), (2, 2), (3, 2), (4, 3), (7, 3), (236, 8), (255, 8)] {
            assert_eq!(width(max), w, "{max}");
        }
        assert_eq!(width(256), 9);
        assert_eq!(width(u64::MAX), 64);
    }

    #[test]
    fn fields_read_back_across_byte_edges_and_at_the_end() {
        let fields: [(u64, u8); 6] =
            [(5, 3), (0, 0), (1023, 10), (1, 1), (0x1_2345, 17), (0xffff_ffff, 32)];
        let mut p = Packer::default();
        for (v, w) in fields {
            p.push(v, w);
        }
        let bytes = p.finish();
        assert_eq!(bytes.len(), bytes_for(1, 3 + 10 + 1 + 17 + 32));
        let mut bit = 0;
        for (v, w) in fields {
            assert_eq!(read(&bytes, bit, w), v);
            bit += usize::from(w);
        }
        // Past the end: zeros, never a panic.
        assert_eq!(read(&bytes, bit + 100, 8), 0);
        assert_eq!(read(&[], 0, 32), 0);
    }

    #[test]
    fn a_bitset_holds_only_its_range() {
        let mut s = BitSet::new(130);
        assert!(!s.is_empty() && s.count() == 0);
        for i in [0, 63, 64, 129] {
            assert!(s.insert(i));
        }
        assert!(!s.insert(130), "past the range");
        assert!(s.contains(129) && !s.contains(1) && !s.contains(1000));
        assert_eq!(s.ones().collect::<Vec<_>>(), [0, 63, 64, 129]);
        assert_eq!(s.count(), 4);
        let back = BitSet::from_words(s.words().to_vec(), 130).unwrap();
        assert_eq!(back, s);
        assert_eq!(BitSet::from_words(s.words().to_vec(), 129), None, "bit 129 is past 129");
        assert_eq!(BitSet::from_words(alloc::vec![0], 130), None, "too few words");
        assert_eq!(BitSet::new(0).words().len(), 0);
    }

    #[test]
    fn a_packed_list_keeps_values_in_their_width() {
        let mut p = Packed::new(11);
        for v in [0, 2047, 5, 1024] {
            assert!(p.push(v));
        }
        assert!(!p.push(2048), "needs 12 bits");
        assert_eq!(p.iter().collect::<Vec<_>>(), [0, 2047, 5, 1024]);
        assert_eq!((p.get(4), p.len()), (None, 4));
        let mut wide = Packed::new(40);
        assert!(wide.push(u32::MAX));
        assert_eq!(wide.get(0), Some(u32::MAX));
        let mut zero = Packed::new(0);
        assert!(zero.push(0) && !zero.push(1));
        assert_eq!(zero.get(0), Some(0));
    }

    #[test]
    fn the_layout_is_lsb_first() {
        // 0b101 then 0b11: byte 0 = 0b0001_1101 by hand.
        let mut p = Packer::default();
        p.push(0b101, 3);
        p.push(0b11, 2);
        assert_eq!(p.finish(), [0b0001_1101]);
    }
}
