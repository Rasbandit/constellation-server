//! Parity RNG: a bit-exact port of CPython's `random.Random` (MT19937 plus the sampling methods
//! the reference generator uses), so a seed draws exactly what the legacy oracle draws.
//!
//! This is not the spec RNG (`spec-rng`, `ChaCha8`); it exists only to reproduce legacy seeds.
//! Pinned by `tests/parity_rng.rs` against vectors written by CPython itself.

const N: usize = 624;
const M: usize = 397;

/// CPython-compatible Mersenne Twister with CPython's sampling on top.
#[derive(Debug, Clone)]
pub struct ParityRng {
    mt: Box<[u32; N]>,
    i: usize,
}

impl ParityRng {
    /// `random.Random(seed)` for a non-negative integer seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self::from_key(&[(seed & 0xFFFF_FFFF) as u32, (seed >> 32) as u32])
    }

    /// `random.Random(n)` where `key` is `abs(n)` as little-endian 32-bit words (any length;
    /// high zero words are ignored, as CPython keys on the significant words only).
    #[must_use]
    pub fn from_key(key: &[u32]) -> Self {
        let used = key.iter().rposition(|w| *w != 0).map_or(1, |p| p + 1);
        let key: &[u32] = if key.is_empty() { &[0] } else { &key[..used] };
        let mut r = Self::genrand(19_650_218);
        let mt = &mut r.mt;
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..N.max(key.len()) {
            let prev = mt[i - 1] ^ (mt[i - 1] >> 30);
            mt[i] = (mt[i] ^ prev.wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(u32::try_from(j).unwrap_or(0));
            i += 1;
            j += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..N - 1 {
            let prev = mt[i - 1] ^ (mt[i - 1] >> 30);
            mt[i] = (mt[i] ^ prev.wrapping_mul(1_566_083_941))
                .wrapping_sub(u32::try_from(i).unwrap_or(0));
            i += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
        }
        mt[0] = 0x8000_0000;
        r
    }

    fn genrand(s: u32) -> Self {
        let mut mt = Box::new([0u32; N]);
        mt[0] = s;
        for i in 1..N {
            mt[i] = 1_812_433_253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(u32::try_from(i).unwrap_or(0));
        }
        Self { mt, i: N }
    }

    /// The next raw 32-bit output (`getrandbits(32)`).
    pub fn next_u32(&mut self) -> u32 {
        if self.i >= N {
            let mt = &mut self.mt;
            for k in 0..N {
                let y = (mt[k] & 0x8000_0000) | (mt[(k + 1) % N] & 0x7FFF_FFFF);
                let mag = if y & 1 == 0 { 0 } else { 0x9908_B0DF };
                mt[k] = mt[(k + M) % N] ^ (y >> 1) ^ mag;
            }
            self.i = 0;
        }
        let mut y = self.mt[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9D2C_5680;
        y ^= (y << 15) & 0xEFC6_0000;
        y ^ (y >> 18)
    }

    /// `getrandbits(k)` as little-endian 32-bit words (empty for `k == 0`).
    pub fn getrandbits(&mut self, k: u32) -> Vec<u32> {
        let mut out = Vec::with_capacity(k.div_ceil(32) as usize);
        let mut left = k;
        while left > 0 {
            let r = self.next_u32();
            out.push(if left < 32 { r >> (32 - left) } else { r });
            left = left.saturating_sub(32);
        }
        out
    }

    /// `getrandbits(k)` for `k <= 64`.
    pub fn bits64(&mut self, k: u32) -> u64 {
        let w = self.getrandbits(k.min(64));
        w.iter().rev().fold(0u64, |acc, x| (acc << 32) | u64::from(*x))
    }

    /// `random()`: a float in `[0, 1)` with 53 random bits.
    pub fn random(&mut self) -> f64 {
        let a = f64::from(self.next_u32() >> 5);
        let b = f64::from(self.next_u32() >> 6);
        (a * 67_108_864.0 + b) * (1.0 / 9_007_199_254_740_992.0)
    }

    /// `_randbelow(n)`: uniform in `[0, n)` by rejection on `n.bit_length()` bits. `n == 0`
    /// returns 0 without drawing (CPython never calls it with 0).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        let k = 64 - n.leading_zeros();
        loop {
            let r = self.bits64(k);
            if r < n {
                return r;
            }
        }
    }

    fn below_usize(&mut self, n: usize) -> usize {
        usize::try_from(self.below(n as u64)).unwrap_or(0)
    }

    /// `randrange(start, stop)` with step 1. An empty range returns `start` without drawing
    /// (CPython raises there).
    pub fn randrange(&mut self, start: i64, stop: i64) -> i64 {
        let width = i128::from(stop) - i128::from(start);
        if width <= 0 {
            return start;
        }
        let r = self.below(u64::try_from(width).unwrap_or(u64::MAX));
        i64::try_from(i128::from(start) + i128::from(r)).unwrap_or(start)
    }

    /// `randint(a, b)`, both ends included.
    pub fn randint(&mut self, a: i64, b: i64) -> i64 {
        self.randrange(a, b.saturating_add(1))
    }

    /// Index `choice(seq)` picks for a sequence of length `n`.
    pub fn choice_index(&mut self, n: usize) -> usize {
        self.below_usize(n)
    }

    /// `shuffle(x)` in place.
    pub fn shuffle<T>(&mut self, x: &mut [T]) {
        for i in (1..x.len()).rev() {
            let j = self.below_usize(i + 1);
            x.swap(i, j);
        }
    }

    /// `sample(population, k)`, including CPython's switch between its pool and set methods.
    /// `k` above the population size is clamped (CPython raises).
    pub fn sample<T: Clone>(&mut self, population: &[T], k: usize) -> Vec<T> {
        let n = population.len();
        let k = k.min(n);
        let mut setsize = 21usize;
        if k > 5 {
            #[allow(clippy::cast_precision_loss)] // k * 3 is far below 2^53
            let e = ((k * 3) as f64).ln() / 4f64.ln();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let e = e.ceil() as u32;
            setsize += 4usize.saturating_pow(e);
        }
        let mut out = Vec::with_capacity(k);
        if n <= setsize {
            let mut pool: Vec<T> = population.to_vec();
            for i in 0..k {
                let j = self.below_usize(n - i);
                out.push(pool[j].clone());
                pool[j] = pool[n - i - 1].clone();
            }
        } else {
            let mut selected = std::collections::HashSet::with_capacity(k);
            for _ in 0..k {
                let mut j = self.below_usize(n);
                while selected.contains(&j) {
                    j = self.below_usize(n);
                }
                selected.insert(j);
                out.push(population[j].clone());
            }
        }
        out
    }

    /// Index one `choices(population, weights)` draw picks, for a population of length `n`
    /// (`weights` `None` means equal weights).
    pub fn choices_index(&mut self, n: usize, weights: Option<&[i64]>) -> usize {
        #[allow(clippy::cast_precision_loss)]
        let Some(w) = weights else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            return (self.random() * n as f64).floor() as usize;
        };
        let mut cum = Vec::with_capacity(w.len());
        let mut acc = 0i64;
        for x in w {
            acc += x;
            cum.push(acc);
        }
        #[allow(clippy::cast_precision_loss)] // weights are small integers
        let total = acc as f64;
        let x = self.random() * total;
        let hi = n.saturating_sub(1);
        // bisect_right over cum[0..hi]
        #[allow(clippy::cast_precision_loss)]
        let pos = cum[..hi].partition_point(|c| (*c as f64) <= x);
        pos
    }
}
