// Expected values are hand calculations of Python's operators, the semantics the reference
// server evaluates with (`MultiServer.py:174-197`), each written down below; the property tests
// compare against an independent oracle (`oracle`) written for these tests on `i128` and std
// float math.
use super::{KvRefusal, evaluate};
use proto::KvOp::{self, *};
use serde_json::{Value, json};
use std::{vec, vec::Vec};

const CAP: usize = 64 * 1024;

fn run(start: &Value, op: KvOp, v: &Value) -> Result<Value, KvRefusal> {
    evaluate(start.clone(), &[(op, v.clone())], CAP, f64::powf)
}

/// `(start, op, operand, expected)`; `Err` names the refusal.
type Case = (Value, KvOp, Value, Result<Value, KvRefusal>);

#[allow(clippy::unnecessary_wraps, reason = "reads as the expected `Ok` in the case table")]
fn ok(v: Value) -> Result<Value, KvRefusal> {
    Ok(v)
}

#[test]
fn the_protocol_documents_example_adds_twelve() {
    // Protocol doc, "DataStorageOperation": `{"operation": "add", "value": 12}` on a key with
    // no value and no default starts from 0 (reference `MultiServer.py:2292`).
    assert_eq!(run(&json!(0), Add, &json!(12)), Ok(json!(12)));
}

fn check_all(cases: Vec<Case>) {
    for (start, op, v, want) in cases {
        assert_eq!(run(&start, op, &v), want, "{start} {op:?} {v}");
    }
}

#[test]
fn arithmetic_matches_the_reference_on_its_table() {
    use KvRefusal::{Overflow, Type, ZeroDivision};
    check_all(vec![
        // replace and default
        (json!(5), Replace, json!("x"), ok(json!("x"))),
        (json!(7), Default, json!(1), ok(json!(7))),
        // add: int + float is float, bools are 0 and 1, strings and lists concatenate
        (json!(1), Add, json!(2.5), ok(json!(3.5))),
        (json!(true), Add, json!(true), ok(json!(2))),
        (json!("ab"), Add, json!("c"), ok(json!("abc"))),
        (json!([1]), Add, json!([2, 3]), ok(json!([1, 2, 3]))),
        (json!({}), Add, json!({}), Err(Type(Add))),
        (json!(1), Add, json!("a"), Err(Type(Add))),
        (json!(null), Add, json!(1), Err(Type(Add))),
        (json!(i64::MAX), Add, json!(1), Err(Overflow(Add))),
        // mul: repetition either way round, a count of zero or less is empty
        (json!(3), Mul, json!(4), ok(json!(12))),
        (json!(1.5), Mul, json!(2), ok(json!(3.0))),
        (json!("ab"), Mul, json!(3), ok(json!("ababab"))),
        (json!(2), Mul, json!("ab"), ok(json!("abab"))),
        (json!([1, 2]), Mul, json!(2), ok(json!([1, 2, 1, 2]))),
        (json!([1]), Mul, json!(-1), ok(json!([]))),
        (json!([]), Mul, json!(1_000_000_000), ok(json!([]))),
        (json!("a"), Mul, json!(true), ok(json!("a"))),
        (json!("a"), Mul, json!(2.0), Err(Type(Mul))),
        (json!("a"), Mul, json!("b"), Err(Type(Mul))),
        (json!([1]), Mul, json!(1_000_000_000), Err(KvRefusal::ValueTooLarge)),
        // pow: an int to a negative int is a float; 0 to a negative power divides by zero
        (json!(2), Pow, json!(10), ok(json!(1024))),
        (json!(2), Pow, json!(62), ok(json!(4_611_686_018_427_387_904_i64))),
        (json!(2), Pow, json!(63), Err(Overflow(Pow))),
        (json!(2), Pow, json!(-1), ok(json!(0.5))),
        (json!(-1), Pow, json!(4_294_967_297_i64), ok(json!(-1))),
        (json!(1), Pow, json!(i64::MAX), ok(json!(1))),
        (json!(0), Pow, json!(-1), Err(ZeroDivision(Pow))),
        (json!(0.0), Pow, json!(-2.5), Err(ZeroDivision(Pow))),
        (json!(4.0), Pow, json!(0.5), ok(json!(2.0))),
        (json!(5.5), Pow, json!(0), ok(json!(1.0))),
        (json!(-8), Pow, json!(0.5), Err(Type(Pow))),
        (json!(-8.0), Pow, json!(3), ok(json!(-512.0))),
        (json!(10.0), Pow, json!(400), Err(Overflow(Pow))),
        // mod: the result takes the divisor's sign (Python), not the dividend's (Rust `%`)
        (json!(7), Mod, json!(3), ok(json!(1))),
        (json!(-7), Mod, json!(3), ok(json!(2))),
        (json!(7), Mod, json!(-3), ok(json!(-2))),
        (json!(-7), Mod, json!(-3), ok(json!(-1))),
        (json!(7.5), Mod, json!(2), ok(json!(1.5))),
        (json!(-7.5), Mod, json!(2), ok(json!(0.5))),
        (json!(6.0), Mod, json!(-3), ok(json!(-0.0))),
        (json!(i64::MIN), Mod, json!(-1), ok(json!(0))),
        (json!(5), Mod, json!(0), Err(ZeroDivision(Mod))),
        (json!(5), Mod, json!(false), Err(ZeroDivision(Mod))),
        (json!(5.0), Mod, json!(0.0), Err(ZeroDivision(Mod))),
        (json!("%d"), Mod, json!(1), Err(Type(Mod))),
        // floor and ceil give integers and ignore the operand
        (json!(2.7), Floor, json!(null), ok(json!(2))),
        (json!(-2.1), Floor, json!(null), ok(json!(-3))),
        (json!(5), Floor, json!("ignored"), ok(json!(5))),
        (json!(true), Floor, json!(null), ok(json!(1))),
        (json!(2.1), Ceil, json!(null), ok(json!(3))),
        (json!(-2.7), Ceil, json!(null), ok(json!(-2))),
        (json!(-0.5), Ceil, json!(null), ok(json!(0))),
        (json!(1e300), Floor, json!(null), Err(Overflow(Floor))),
        (json!("a"), Ceil, json!(null), Err(Type(Ceil))),
        // an integer above i64 (Python would hold it; we refuse, docs/compat-status.md)
        (json!(u64::MAX), Add, json!(0), Err(Overflow(Add))),
    ]);
}

#[test]
fn ordering_and_bit_operations_match_the_reference_on_their_table() {
    use KvRefusal::{Overflow, Range, Type};
    check_all(vec![
        // max and min: a tie keeps the current value (Python returns its first argument)
        (json!(1), Max, json!(2), ok(json!(2))),
        (json!(2), Max, json!(1), ok(json!(2))),
        (json!(1), Max, json!(1.0), ok(json!(1))),
        (json!(1.0), Min, json!(1), ok(json!(1.0))),
        (json!(true), Max, json!(1), ok(json!(true))),
        (json!("a"), Max, json!("b"), ok(json!("b"))),
        (json!([1, 2]), Max, json!([1, 3]), ok(json!([1, 3]))),
        (json!([1]), Max, json!([1, 0]), ok(json!([1, 0]))),
        (json!([1, "a"]), Min, json!([0, 5]), ok(json!([0, 5]))),
        (json!(1), Max, json!("a"), Err(Type(Max))),
        (json!(null), Max, json!(null), Err(Type(Max))),
        (json!({}), Min, json!({}), Err(Type(Min))),
        (json!([1, "a"]), Max, json!([1, 2]), Err(Type(Max))),
        // and, or, xor: integers bitwise, two booleans stay boolean, `or` merges objects
        (json!(12), And, json!(10), ok(json!(8))),
        (json!(12), Or, json!(10), ok(json!(14))),
        (json!(12), Xor, json!(10), ok(json!(6))),
        (json!(true), And, json!(false), ok(json!(false))),
        (json!(true), Xor, json!(true), ok(json!(false))),
        (json!(true), Or, json!(2), ok(json!(3))),
        (json!(-1), And, json!(255), ok(json!(255))),
        (json!(1.5), And, json!(1), Err(Type(And))),
        (json!({"a": 1}), Or, json!({"a": 2, "b": 3}), ok(json!({"a": 2, "b": 3}))),
        (json!({}), And, json!({}), Err(Type(And))),
        (json!({}), Xor, json!({}), Err(Type(Xor))),
        // shifts: arithmetic right shift, a negative count is refused
        (json!(1), LeftShift, json!(4), ok(json!(16))),
        (json!(-8), RightShift, json!(1), ok(json!(-4))),
        (json!(-1), RightShift, json!(100), ok(json!(-1))),
        (json!(5), RightShift, json!(100), ok(json!(0))),
        (json!(1), LeftShift, json!(63), Err(Overflow(LeftShift))),
        (json!(-1), LeftShift, json!(63), ok(json!(i64::MIN))),
        (json!(0), LeftShift, json!(1000), ok(json!(0))),
        (json!(1), LeftShift, json!(-1), Err(Range(LeftShift))),
        (json!(1), RightShift, json!(-1), Err(Range(RightShift))),
        (json!(1.0), LeftShift, json!(1), Err(Type(LeftShift))),
    ]);
}

#[test]
fn list_and_object_operations_match_the_reference_on_their_table() {
    use KvRefusal::{Range, Type};
    check_all(vec![
        // remove: the first equal element, Python equality (1 == 1.0 == true)
        (json!([1, 2, 1]), Remove, json!(1), ok(json!([2, 1]))),
        (json!([1.0, 2]), Remove, json!(true), ok(json!([2]))),
        (json!([[1], [1.0]]), Remove, json!([1.0]), ok(json!([[1.0]]))),
        (json!([1]), Remove, json!(5), ok(json!([1]))),
        (json!("abc"), Remove, json!("a"), Err(Type(Remove))),
        (json!({"a": 1}), Remove, json!("a"), Err(Type(Remove))),
        // pop: list index (negative from the end, past the end a no-op) or object key
        (json!([1, 2, 3]), Pop, json!(0), ok(json!([2, 3]))),
        (json!([1, 2, 3]), Pop, json!(-1), ok(json!([1, 2]))),
        (json!([1, 2, 3]), Pop, json!(3), ok(json!([1, 2, 3]))),
        (json!([1, 2, 3]), Pop, json!(-4), Err(Range(Pop))),
        (json!([1, 2, 3]), Pop, json!(true), ok(json!([1, 3]))),
        (json!([1, 2, 3]), Pop, json!(1.0), Err(Type(Pop))),
        (json!({"a": 1, "b": 2}), Pop, json!("a"), ok(json!({"b": 2}))),
        (json!({"a": 1}), Pop, json!("z"), ok(json!({"a": 1}))),
        (json!({"a": 1}), Pop, json!(1), ok(json!({"a": 1}))),
        (json!({"a": 1}), Pop, json!([1]), Err(Type(Pop))),
        (json!("s"), Pop, json!(0), Err(Type(Pop))),
        // update: a list gains missing entries (judged against the list as it was), an object
        // merges; a list iterates an object's keys and a string's characters
        (json!([1, 2]), Update, json!([2, 3, 3]), ok(json!([1, 2, 3, 3]))),
        (json!([1]), Update, json!({"a": 0, "b": 0}), ok(json!([1, "a", "b"]))),
        (json!(["a"]), Update, json!("abca"), ok(json!(["a", "b", "c"]))),
        (json!([1]), Update, json!([1.0, true]), ok(json!([1]))),
        (json!([1]), Update, json!(5), Err(Type(Update))),
        (json!({"a": 1}), Update, json!({"a": 2, "b": 3}), ok(json!({"a": 2, "b": 3}))),
        (json!({"a": 1}), Update, json!([["b", 2]]), ok(json!({"a": 1, "b": 2}))),
        (json!({"a": 1}), Update, json!([[1, 2]]), Err(Type(Update))),
        (json!({"a": 1}), Update, json!([["b", 2, 3]]), Err(Type(Update))),
        (json!({}), Update, json!(""), ok(json!({}))),
        (json!({}), Update, json!("ab"), Err(Type(Update))),
        (json!(3), Update, json!([]), Err(Type(Update))),
    ]);
}

#[test]
fn operations_run_in_order_and_one_refusal_refuses_the_whole_list() {
    let ops = [(Add, json!(2)), (Mul, json!(10)), (Mod, json!(7))];
    assert_eq!(evaluate(json!(1), &ops, CAP, f64::powf), Ok(json!(2)));
    let bad = [(Add, json!(2)), (Mod, json!(0)), (Replace, json!(9))];
    assert_eq!(evaluate(json!(1), &bad, CAP, f64::powf), Err(KvRefusal::ZeroDivision(Mod)));
    assert_eq!(evaluate(json!("x"), &[], CAP, f64::powf), Ok(json!("x")));
}

#[test]
fn ints_compare_exactly_with_floats_beyond_two_to_the_53() {
    // 2^53 + 1 is not a float; Python still orders it above 2^53 as a float.
    let big = (1_i64 << 53) + 1;
    assert_eq!(run(&json!(9_007_199_254_740_992.0), Max, &json!(big)), Ok(json!(big)));
    assert_eq!(
        run(&json!(big), Min, &json!(9_007_199_254_740_992.0)),
        Ok(json!(9_007_199_254_740_992.0))
    );
    assert_eq!(run(&json!(i64::MAX), Max, &json!(9.3e18)), Ok(json!(9.3e18)));
    assert_eq!(run(&json!(i64::MIN), Min, &json!(-9.3e18)), Ok(json!(-9.3e18)));
    assert_eq!(run(&json!(3), Max, &json!(3.5)), Ok(json!(3.5)));
    assert_eq!(run(&json!(-3), Min, &json!(-3.5)), Ok(json!(-3.5)));
    assert_eq!(run(&json!([1]), Remove, &json!(1.5)), Ok(json!([1])));
}

/// The independent oracle: Python's operators on unbounded integers (`i128` is enough for the
/// generated ranges) and std float math, then our `i64` cap applied to the result.
#[allow(
    clippy::many_single_char_names,
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::excessive_precision,
    clippy::manual_range_contains,
    clippy::unreachable,
    clippy::type_complexity,
    reason = "a test oracle: Python's math restated plainly on small generated ranges"
)]
mod oracle {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    pub(super) enum N {
        I(i128),
        F(f64),
    }

    pub(super) fn of(v: &Value) -> Option<N> {
        match v {
            Value::Bool(b) => Some(N::I(i128::from(*b))),
            Value::Number(n) => {
                n.as_i64().map(|i| N::I(i128::from(i))).or_else(|| n.as_f64().map(N::F))
            }
            _ => None,
        }
    }

    fn f(n: N) -> f64 {
        match n {
            N::I(i) => i as f64,
            N::F(x) => x,
        }
    }

    fn int_out(i: i128, op: KvOp) -> Result<Value, KvRefusal> {
        i64::try_from(i).map(Value::from).map_err(|_| KvRefusal::Overflow(op))
    }

    fn float_out(x: f64, op: KvOp) -> Result<Value, KvRefusal> {
        if x.is_finite() { Ok(json!(x)) } else { Err(KvRefusal::Overflow(op)) }
    }

    /// Python's `a <op> b` for two numbers (booleans as 0 and 1).
    pub(super) fn numeric(a: &Value, op: KvOp, b: &Value) -> Result<Value, KvRefusal> {
        let (Some(x), Some(y)) = (of(a), of(b)) else { return Err(KvRefusal::Type(op)) };
        let both_bool = a.is_boolean() && b.is_boolean();
        match (op, x, y) {
            (Add, N::I(p), N::I(q)) => int_out(p + q, op),
            (Mul, N::I(p), N::I(q)) => int_out(p * q, op),
            (Add, ..) => float_out(f(x) + f(y), op),
            (Mul, ..) => float_out(f(x) * f(y), op),
            (Mod, _, N::I(0)) => Err(KvRefusal::ZeroDivision(op)),
            (Mod, N::I(p), N::I(q)) => int_out(((p % q) + q) % q, op),
            (Mod, ..) if f(y) == 0.0 => Err(KvRefusal::ZeroDivision(op)),
            (Mod, ..) => {
                // Python: the remainder of a - b * floor(a / b), with the divisor's sign.
                let (p, q) = (f(x), f(y));
                let r = p % q;
                let r = if r != 0.0 && (r < 0.0) != (q < 0.0) { r + q } else { r };
                float_out(if r == 0.0 { 0.0_f64.copysign(q) } else { r }, op)
            }
            (Max | Min, ..) => {
                let (p, q) = (f(x), f(y));
                let take = if op == Max { q > p } else { q < p };
                Ok(if take { b.clone() } else { a.clone() })
            }
            (And | Or | Xor, N::I(p), N::I(q)) => {
                let r = match op {
                    And => p & q,
                    Or => p | q,
                    _ => p ^ q,
                };
                if both_bool { Ok(json!(r != 0)) } else { int_out(r, op) }
            }
            (LeftShift | RightShift, N::I(_), N::I(q)) if q < 0 => Err(KvRefusal::Range(op)),
            (LeftShift, N::I(0), N::I(_)) => Ok(json!(0)),
            (LeftShift, N::I(p), N::I(q)) => {
                if q >= 64 {
                    Err(KvRefusal::Overflow(op))
                } else {
                    int_out(p << q, op)
                }
            }
            (RightShift, N::I(p), N::I(q)) => int_out(p >> q.min(127), op),
            (Floor | Ceil, N::I(p), _) => int_out(p, op),
            (Floor | Ceil, N::F(p), _) => {
                let r = if op == Floor { p.floor() } else { p.ceil() };
                if r >= -9.223_372_036_854_775_808e18 && r < 9.223_372_036_854_775_808e18 {
                    Ok(json!(r as i64))
                } else {
                    Err(KvRefusal::Overflow(op))
                }
            }
            (Pow, N::I(p), N::I(q)) if q >= 0 => p
                .checked_pow(u32::try_from(q).unwrap_or(u32::MAX))
                .map_or(Err(KvRefusal::Overflow(op)), |r| int_out(r, op)),
            (Pow, ..) if f(y) == 0.0 => Ok(json!(1.0)),
            (Pow, ..) if f(x) == 0.0 && f(y) < 0.0 => Err(KvRefusal::ZeroDivision(op)),
            (Pow, ..) if f(x) < 0.0 && f(y).fract() != 0.0 => Err(KvRefusal::Type(op)),
            (Pow, ..) => float_out(f(x).powf(f(y)), op),
            _ => Err(KvRefusal::Type(op)),
        }
    }

    /// Python's list operations on lists of small integers.
    pub(super) fn list(a: &[i64], op: KvOp, b: &Value) -> Result<Value, KvRefusal> {
        let mut out: Vec<i64> = a.to_vec();
        match op {
            Remove => {
                let target = of(b);
                if let Some(i) = out.iter().position(|x| match target {
                    Some(N::I(t)) => i128::from(*x) == t,
                    Some(N::F(t)) => *x as f64 == t,
                    None => false,
                }) {
                    out.remove(i);
                }
            }
            Pop => {
                let Some(N::I(i)) = of(b) else { return Err(KvRefusal::Type(op)) };
                let len = out.len() as i128;
                if i < len {
                    let at = if i < 0 { len + i } else { i };
                    if at < 0 {
                        return Err(KvRefusal::Range(op));
                    }
                    out.remove(at as usize);
                }
            }
            Update => {
                let Value::Array(entries) = b else { return Err(KvRefusal::Type(op)) };
                let before: std::collections::BTreeSet<i64> = a.iter().copied().collect();
                for e in entries {
                    let e = e.as_i64().unwrap();
                    if !before.contains(&e) {
                        out.push(e);
                    }
                }
            }
            Add => {
                let Value::Array(more) = b else { return Err(KvRefusal::Type(op)) };
                out.extend(more.iter().map(|v| v.as_i64().unwrap()));
            }
            Mul => {
                let Some(N::I(n)) = of(b) else { return Err(KvRefusal::Type(op)) };
                out = a
                    .iter()
                    .copied()
                    .cycle()
                    .take(a.len() * usize::try_from(n).unwrap_or(0))
                    .collect();
            }
            _ => unreachable!("not a list op in these tests"),
        }
        Ok(json!(out))
    }
}

mod properties {
    use super::*;
    use proptest::prelude::{Just, Strategy, any, prop_assert_eq, proptest};

    /// Integers small enough that every one is exactly a float, so the oracle's float
    /// comparisons are exact; the edges of i64 are covered by the table.
    fn number() -> impl Strategy<Value = Value> {
        proptest::prop_oneof![
            4 => (-(1_i64 << 40)..(1_i64 << 40)).prop_map(Value::from),
            2 => (-20_i64..20).prop_map(Value::from),
            2 => (-1e6_f64..1e6).prop_map(|x| json!(x)),
            1 => (-8_i32..8).prop_map(|x| json!(f64::from(x) / 4.0)),
            1 => any::<bool>().prop_map(Value::from),
        ]
    }

    fn numeric_op() -> impl Strategy<Value = KvOp> {
        proptest::prop_oneof![
            Just(Add),
            Just(Mul),
            Just(Mod),
            Just(Max),
            Just(Min),
            Just(And),
            Just(Or),
            Just(Xor),
            Just(LeftShift),
            Just(RightShift),
            Just(Floor),
            Just(Ceil),
            Just(Pow),
        ]
    }

    fn small_list() -> impl Strategy<Value = Vec<i64>> {
        proptest::collection::vec(-3_i64..4, 0..6)
    }

    fn list_case() -> impl Strategy<Value = (KvOp, Value)> {
        proptest::prop_oneof![
            (-4_i64..4).prop_map(|v| (Remove, json!(v))),
            (-8_i64..8).prop_map(|v| (Pop, json!(v))),
            small_list().prop_map(|v| (Update, json!(v))),
            small_list().prop_map(|v| (Add, json!(v))),
            (-2_i64..4).prop_map(|v| (Mul, json!(v))),
        ]
    }

    proptest! {
        #[test]
        fn numeric_operations_match_the_oracle(a in number(), op in numeric_op(), b in number()) {
            prop_assert_eq!(run(&a, op, &b), oracle::numeric(&a, op, &b), "{} {:?} {}", a, op, b);
        }

        #[test]
        fn list_operations_match_the_oracle(a in small_list(), (op, b) in list_case()) {
            prop_assert_eq!(run(&json!(a), op, &b), oracle::list(&a, op, &b));
        }

        #[test]
        fn a_refused_operation_anywhere_refuses_the_whole_write(
            ops in proptest::collection::vec((numeric_op(), number()), 0..8),
            start in number(),
        ) {
            // The result equals folding each step; a refusal at any step is the result.
            let mut want = Ok(start.clone());
            for (op, v) in &ops {
                want = want.and_then(|cur| run(&cur, *op, v));
            }
            prop_assert_eq!(evaluate(start, &ops, CAP, f64::powf), want);
        }
    }
}
