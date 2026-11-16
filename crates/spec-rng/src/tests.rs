// Golden vectors. Expected values were produced by an independent Python implementation of
// `ChaCha8`, the sampling rules and HKDF-SHA256 (`ref.py`, standard library only). The zero-seed
// first word also matches the well-known `ChaCha8` zero-key keystream (bytes 3e 00 ef 2f 89 5f 40 d6).
use super::{SpecRng, derive};
use std::{format, string::String, vec::Vec};

const ZERO: [u8; 32] = [0; 32];

fn inc() -> [u8; 32] {
    core::array::from_fn(|i| u8::try_from(i).unwrap())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().fold(String::new(), |acc, b| acc + &format!("{b:02x}"))
}

#[test]
fn zero_seed_stream() {
    let mut r = SpecRng::from_seed(ZERO);
    let got: Vec<u64> = (0..4).map(|_| r.next_u64()).collect();
    assert_eq!(
        got,
        [
            0xd640_5f89_2fef_003e,
            0xa1a5_091f_e8b8_5b7f,
            0x3b7f_9ace_c30e_842c,
            0x1e1a_71ef_88e1_1b18
        ]
    );
}

#[test]
fn increasing_seed_stream_and_block_boundary() {
    let mut r = SpecRng::from_seed(inc());
    let got: Vec<u64> = (0..4).map(|_| r.next_u64()).collect();
    assert_eq!(
        got,
        [
            0x6aab_126e_8fb2_1540,
            0x3312_c531_7b66_e8d9,
            0x4fd9_b290_2717_8ff7,
            0xcbbe_bcff_d72e_6b32
        ]
    );
    // index 39 is 80 words in, past the 16-word block and the 64-word buffer
    let mut r = SpecRng::from_seed(inc());
    assert_eq!((0..40).map(|_| r.next_u64()).last().unwrap(), 0x20b5_07f4_0393_8aa0);
}

#[test]
fn below_is_pinned() {
    let mut r = SpecRng::from_seed(inc());
    let got: Vec<u64> = (0..8).map(|_| r.below(10).unwrap()).collect();
    assert_eq!(got, [6, 9, 5, 4, 7, 9, 7, 9]);
    // a bound that makes the rejection threshold large
    let mut r = SpecRng::from_seed(inc());
    let got: Vec<u64> = (0..4).map(|_| r.below((1 << 63) + 5).unwrap()).collect();
    assert_eq!(
        got,
        [
            5_458_007_605_432_445_741,
            9_047_904_780_383_764_586,
            1_432_057_575_928_333_194,
            6_221_338_298_857_404_846
        ]
    );
}

#[test]
fn below_zero_is_none_and_one_is_zero() {
    let mut r = SpecRng::from_seed(ZERO);
    assert_eq!(r.below(0), None);
    assert_eq!(r.below(1), Some(0));
}

#[test]
fn shuffle_is_pinned_and_a_permutation() {
    let mut a: Vec<u32> = (0..10).collect();
    SpecRng::from_seed(inc()).shuffle(&mut a);
    assert_eq!(a, [2, 0, 1, 9, 4, 3, 5, 7, 8, 6]);
    let mut empty: [u32; 0] = [];
    SpecRng::from_seed(inc()).shuffle(&mut empty);
    let mut one = [7_u32];
    SpecRng::from_seed(inc()).shuffle(&mut one);
    assert_eq!(one, [7]);
}

#[test]
fn weighted_choice_is_pinned_and_edge_cases() {
    let mut r = SpecRng::from_seed(ZERO);
    let got: Vec<usize> = (0..8).map(|_| r.weighted_choice(&[1, 0, 3, 6]).unwrap()).collect();
    assert_eq!(got, [3, 3, 3, 0, 3, 3, 2, 2]);
    let mut r = SpecRng::from_seed(ZERO);
    assert_eq!(r.weighted_choice(&[]), None);
    assert_eq!(r.weighted_choice(&[0, 0]), None);
    assert_eq!(r.weighted_choice(&[u64::MAX, 1]), None);
}

#[test]
fn derive_is_pinned() {
    let slot1 = derive(&inc(), b"slot", 1).unwrap();
    assert_eq!(
        hex(&derive(&inc(), b"slot", 0).unwrap()),
        "506056a3f74b93f032b5e1c4d12bfd512fc66e27be4e660ff6ef7a361bca0dc7"
    );
    assert_eq!(hex(&slot1), "fba988a78bb6fecc7f23bd6863c23ab229e2004506ae7796756e5291f93faae7");
    assert_eq!(
        hex(&derive(&slot1, b"hook", 7).unwrap()),
        "02d720938b37f21538643bab7ba61ce9ac2e89d640958b142cb4d84b8dc94626"
    );
    assert!(derive(&inc(), &[b'x'; 60], 0).is_none());
}

mod properties {
    use super::{SpecRng, derive};
    use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest};
    use std::vec::Vec;

    proptest! {
        #[test]
        fn below_stays_in_range(seed in any::<[u8; 32]>(), n in 1_u64..) {
            let mut r = SpecRng::from_seed(seed);
            for _ in 0..8 {
                prop_assert!(r.below(n).is_some_and(|v| v < n));
            }
        }

        #[test]
        fn the_same_seed_gives_the_same_stream(seed in any::<[u8; 32]>()) {
            let (mut a, mut b) = (SpecRng::from_seed(seed), SpecRng::from_seed(seed));
            for _ in 0..16 {
                prop_assert_eq!(a.next_u64(), b.next_u64());
            }
        }

        #[test]
        fn shuffle_keeps_every_item(seed in any::<[u8; 32]>(), mut items in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut expected = items.clone();
            SpecRng::from_seed(seed).shuffle(&mut items);
            items.sort_unstable();
            expected.sort_unstable();
            prop_assert_eq!(items, expected);
        }

        #[test]
        fn weighted_choice_never_picks_a_zero_weight(
            seed in any::<[u8; 32]>(),
            weights in proptest::collection::vec(0_u32..1000, 1..16),
        ) {
            let weights: Vec<u64> = weights.into_iter().map(u64::from).collect();
            let picked = SpecRng::from_seed(seed).weighted_choice(&weights);
            if weights.iter().sum::<u64>() == 0 {
                prop_assert_eq!(picked, None);
            } else {
                prop_assert!(picked.and_then(|i| weights.get(i)).is_some_and(|w| *w > 0));
            }
        }

        #[test]
        fn derive_separates_streams(parent in any::<[u8; 32]>(), a in any::<u32>(), b in any::<u32>()) {
            let (x, y) = (derive(&parent, b"slot", a), derive(&parent, b"slot", b));
            prop_assert_eq!(x == y, a == b);
        }
    }
}
