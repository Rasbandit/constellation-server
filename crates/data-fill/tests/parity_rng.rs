//! The parity RNG against vectors written by CPython's `random.Random`
//! (`tools/data-only/parity_rng_vectors.py`).

use data_fill::parity_rng::ParityRng;
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("parity_rng_vectors.json")).expect("vectors parse")
}

/// Decimal string to little-endian `u32` words of its absolute value.
fn words(dec: &str) -> Vec<u32> {
    let mut w: Vec<u32> = vec![0];
    for c in dec.trim_start_matches('-').bytes() {
        let mut carry = u64::from(c - b'0');
        for x in &mut w {
            let v = u64::from(*x) * 10 + carry;
            *x = (v & 0xFFFF_FFFF) as u32;
            carry = v >> 32;
        }
        if carry > 0 {
            w.push(carry as u32);
        }
    }
    w
}

/// Little-endian words to a decimal string.
fn decimal(mut w: Vec<u32>) -> String {
    let mut digits = Vec::new();
    while w.iter().any(|x| *x != 0) {
        let mut rem = 0u64;
        for x in w.iter_mut().rev() {
            let v = (rem << 32) | u64::from(*x);
            *x = (v / 10) as u32;
            rem = v % 10;
        }
        digits.push(b'0' + rem as u8);
    }
    if digits.is_empty() {
        return "0".into();
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}

fn rng(case: &Value) -> ParityRng {
    ParityRng::from_key(&words(case["seed"].as_str().unwrap_or("0")))
}

fn ints(v: &Value) -> Vec<i64> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default()
}

#[test]
fn bigint_helpers_round_trip() {
    for s in [
        "0",
        "1",
        "4294967296",
        "18446744073709551621",
        "515377520732011331036461129765621272702107522001",
    ] {
        assert_eq!(decimal(words(s)), s);
    }
}

#[test]
fn seed_and_getrandbits_match_cpython() {
    for case in vectors()["cases"].as_array().into_iter().flatten() {
        let seed = case["seed"].as_str().unwrap_or("");
        let mut r = rng(case);
        let want: Vec<u32> = ints(&case["u32"]).into_iter().map(|x| x as u32).collect();
        let got: Vec<u32> = (0..want.len()).map(|_| r.next_u32()).collect();
        assert_eq!(got, want, "u32 stream, seed {seed}");

        let mut r = rng(case);
        for pair in case["bits"].as_array().into_iter().flatten() {
            let k = pair[0].as_u64().unwrap_or(0) as u32;
            let want = pair[1].as_str().unwrap_or("");
            assert_eq!(decimal(r.getrandbits(k)), want, "getrandbits({k}), seed {seed}");
        }
    }
}

#[test]
fn u64_seed_matches_key_seed() {
    for s in [0u64, 1, 42, u64::MAX, 1 << 32] {
        let mut a = ParityRng::new(s);
        let mut b = ParityRng::from_key(&words(&s.to_string()));
        assert_eq!(a.next_u32(), b.next_u32(), "seed {s}");
    }
}

#[test]
fn random_float_matches_cpython() {
    for case in vectors()["cases"].as_array().into_iter().flatten() {
        let mut r = rng(case);
        for want in case["random"].as_array().into_iter().flatten() {
            let want: u64 = want.as_str().unwrap_or("").parse().unwrap_or(0);
            assert_eq!(r.random().to_bits(), want, "random(), seed {}", case["seed"]);
        }
    }
}

#[test]
fn ranges_and_choice_match_cpython() {
    for case in vectors()["cases"].as_array().into_iter().flatten() {
        let seed = &case["seed"];
        let mut r = rng(case);
        for t in case["randrange"].as_array().into_iter().flatten() {
            let (a, b, want) =
                (t[0].as_i64().unwrap_or(0), t[1].as_i64().unwrap_or(0), t[2].as_i64());
            assert_eq!(Some(r.randrange(a, b)), want, "randrange({a}, {b}), seed {seed}");
        }
        let mut r = rng(case);
        for t in case["randint"].as_array().into_iter().flatten() {
            let (a, b, want) =
                (t[0].as_i64().unwrap_or(0), t[1].as_i64().unwrap_or(0), t[2].as_i64());
            assert_eq!(Some(r.randint(a, b)), want, "randint({a}, {b}), seed {seed}");
        }
        let mut r = rng(case);
        for t in case["choice"].as_array().into_iter().flatten() {
            let n = t[0].as_u64().unwrap_or(0) as usize;
            assert_eq!(
                Some(r.choice_index(n) as u64),
                t[1].as_u64(),
                "choice(range({n})), seed {seed}"
            );
        }
    }
}

#[test]
fn shuffle_sample_choices_match_cpython() {
    for case in vectors()["cases"].as_array().into_iter().flatten() {
        let seed = &case["seed"];
        for want in case["shuffle"].as_array().into_iter().flatten() {
            let want = ints(want);
            let mut xs: Vec<i64> = (0..want.len() as i64).collect();
            rng(case).shuffle(&mut xs);
            assert_eq!(xs, want, "shuffle of {}, seed {seed}", want.len());
        }
        for t in case["sample"].as_array().into_iter().flatten() {
            let (n, k) = (t[0].as_u64().unwrap_or(0) as usize, t[1].as_u64().unwrap_or(0) as usize);
            let pop: Vec<i64> = (0..n as i64).collect();
            assert_eq!(
                rng(case).sample(&pop, k),
                ints(&t[2]),
                "sample(range({n}), {k}), seed {seed}"
            );
        }
        for t in case["choices"].as_array().into_iter().flatten() {
            let weights = ints(&t[0]);
            let k = t[1].as_u64().unwrap_or(0) as usize;
            let want = ints(&t[2]);
            let n = if weights.is_empty() { 7 } else { weights.len() };
            let w = (!weights.is_empty()).then_some(weights.as_slice());
            let mut r = rng(case);
            let got: Vec<i64> = (0..k).map(|_| r.choices_index(n, w) as i64).collect();
            assert_eq!(got, want, "choices weights {weights:?}, seed {seed}");
        }
    }
}

#[test]
fn mixed_stream_and_child_seed_match_cpython() {
    for case in vectors()["cases"].as_array().into_iter().flatten() {
        let seed = &case["seed"];
        let mut r = rng(case);
        let m = case["mixed"].as_array().cloned().unwrap_or_default();
        for chunk in m.chunks(4) {
            assert_eq!(Some(r.randint(0, 200)), chunk[0].as_i64(), "mixed randint, seed {seed}");
            assert_eq!(Some(u64::from(r.next_u32())), chunk[1].as_u64(), "mixed bits, seed {seed}");
            assert_eq!(
                Some(r.choice_index(1) as u64),
                chunk[2].as_u64(),
                "mixed choice, seed {seed}"
            );
            let mut xs: Vec<i64> = (0..9).collect();
            r.shuffle(&mut xs);
            assert_eq!(xs, ints(&chunk[3]), "mixed shuffle, seed {seed}");
        }
        let mut parent = rng(case);
        let mut child = ParityRng::new(parent.bits64(64));
        let got: Vec<i64> = (0..4).map(|_| i64::from(child.next_u32())).collect();
        assert_eq!(got, ints(&case["child_u32"]), "child stream, seed {seed}");
    }
}
