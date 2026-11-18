//! The bit and package reading this crate needs, under the names `crates/bits` and the
//! seed-bundle package module use, so moving onto them is a change to this file only.
//! Layout: `width`-bit fields, least significant bit first (bit `n` is bit `n % 8` of byte
//! `n / 8`); a set's words are little-endian `u64`, so set bit `i` is field `i` of width 1.

/// Bits that hold every value in `0..=max` (`bits::width`).
#[must_use]
pub const fn width(max: u64) -> u8 {
    // At most 64, so the cast is exact.
    (u64::BITS - max.leading_zeros()) as u8
}

/// The `width`-bit field (at most 57 bits) at bit `bit`; bits past the end read as 0
/// (`bits::read`).
#[must_use]
pub fn read(bytes: &[u8], bit: usize, width: u8) -> u64 {
    if width == 0 {
        return 0;
    }
    let tail = bytes.get(bit / 8..).unwrap_or_default();
    // Fast path (one unaligned load) whenever eight bytes remain; same result as the copy.
    let word = if let Some(w) = tail.first_chunk::<8>() {
        *w
    } else {
        let mut word = [0_u8; 8];
        word[..tail.len()].copy_from_slice(tail);
        word
    };
    let mask = if width >= 64 { u64::MAX } else { (1_u64 << width) - 1 };
    (u64::from_le_bytes(word) >> (bit % 8)) & mask
}

/// Bytes that `count` fields of `width` bits take (`bits::bytes_for`).
#[must_use]
pub const fn bytes_for(count: usize, width: usize) -> usize {
    (count * width).div_ceil(8)
}

/// Appends `width`-bit fields (`bits::Packer`).
#[derive(Debug, Clone, Default)]
pub struct Packer {
    bytes: Vec<u8>,
    bit: usize,
}

impl Packer {
    /// Append the low `width` bits of `value`.
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

/// A read-only packed list over borrowed bytes (`bits::PackedRef`).
#[derive(Debug, Clone, Copy)]
pub struct PackedRef<'a> {
    bytes: &'a [u8],
    width: u8,
    len: usize,
}

impl<'a> PackedRef<'a> {
    /// `len` values of `width` bits packed from bit 0; `None` when the bytes are too short.
    #[must_use]
    pub fn from_bytes(bytes: &'a [u8], width: u8, len: usize) -> Option<Self> {
        let width = width.min(32);
        let need = len.checked_mul(usize::from(width))?.div_ceil(8);
        (bytes.len() >= need).then_some(Self { bytes, width, len })
    }

    /// The value at `i`.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<u32> {
        // A width of at most 32 bits always fits a u32.
        #[allow(clippy::cast_possible_truncation)]
        (i < self.len).then(|| read(self.bytes, i * usize::from(self.width), self.width) as u32)
    }
}

/// A set of indexes `0..len`, one bit each (`bits::BitSet`, only the methods used here).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitSet {
    words: Vec<u64>,
    len: usize,
}

impl BitSet {
    /// An empty set over `0..len`.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self { words: vec![0; len.div_ceil(64)], len }
    }

    /// Whether `i` is in the set (false past the range).
    #[must_use]
    pub fn contains(&self, i: usize) -> bool {
        self.words.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
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

    /// Remove `i` (nothing past the range).
    pub fn remove(&mut self, i: usize) {
        if let Some(w) = self.words.get_mut(i / 64) {
            *w &= !(1 << (i % 64));
        }
    }

    /// Remove every index.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }
}

/// Package stand-in: `u16` section count, then per section `u16 id, u32 offset, u32 length`,
/// then the bytes. The v2 package (`seed_bundle::package::file_bytes`) adds a magic, a version
/// and a hash per section; nothing here depends on those.
#[must_use]
pub fn file_bytes(sections: &[(u16, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let n = u16::try_from(sections.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&n.to_le_bytes());
    let mut at = 2 + 10 * sections.len();
    for (id, b) in sections {
        out.extend_from_slice(&id.to_le_bytes());
        for v in [at, b.len()] {
            out.extend_from_slice(&u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes());
        }
        at += b.len();
    }
    for (_, b) in sections {
        out.extend_from_slice(b);
    }
    out
}

/// Section `id` of a package file, read in place (`seed_bundle::package::stored`).
///
/// # Errors
/// A table of contents that points outside the file.
pub fn stored(file: &[u8], id: u16) -> Result<Option<&[u8]>, String> {
    let bad = || "package: malformed table of contents".to_owned();
    let n = usize::from(u16::from_le_bytes(
        file.get(..2).ok_or_else(bad)?.try_into().map_err(|_| bad())?,
    ));
    for k in 0..n {
        let e = file.get(2 + 10 * k..12 + 10 * k).ok_or_else(bad)?;
        if u16::from_le_bytes([e[0], e[1]]) != id {
            continue;
        }
        let at = u32::from_le_bytes([e[2], e[3], e[4], e[5]]) as usize;
        let len = u32::from_le_bytes([e[6], e[7], e[8], e[9]]) as usize;
        return file.get(at..at.checked_add(len).ok_or_else(bad)?).map(Some).ok_or_else(bad);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_and_sets_share_one_bit_order() {
        let mut p = Packer::default();
        for (v, w) in [(5, 3), (1023, 10), (1, 1), (0x1_2345, 17)] {
            p.push(v, w);
        }
        let b = p.finish();
        assert_eq!(
            (read(&b, 0, 3), read(&b, 3, 10), read(&b, 13, 1), read(&b, 14, 17)),
            (5, 1023, 1, 0x1_2345)
        );
        let list = PackedRef::from_bytes(&b, 3, 2).unwrap();
        assert_eq!(list.get(0), Some(5));
        assert!(PackedRef::from_bytes(&b, 32, 2).is_none(), "too short");
        let mut s = BitSet::new(70);
        assert!(s.insert(69) && !s.insert(70));
        let bytes: Vec<u8> = s.words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(read(&bytes, 69, 1), 1);
        s.clear();
        assert!(!s.contains(69));
    }

    #[test]
    fn a_package_returns_its_sections_in_place() {
        let f = file_bytes(&[(1, b"game"), (2, b"rules!")]);
        assert_eq!(stored(&f, 2).unwrap(), Some(&b"rules!"[..]));
        assert_eq!(stored(&f, 9).unwrap(), None);
        assert!(stored(&f[..f.len() - 1], 2).is_err());
    }
}
