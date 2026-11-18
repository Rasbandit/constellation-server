//! The room's key-value store: what each data storage operation does (docs/protocol-plan.md 1.2).
//!
//! Clients compatible with Archipelago expect the operations to behave like the reference
//! server's, which evaluates them with Python operators (`MultiServer.py:174-197`, helpers at
//! `127-155`). This module implements those rules in our own code, with these deviations, each
//! listed in docs/compat-status.md:
//! - Integers are `i64` and floats `f64`; a result outside `i64`, or a float that is not finite
//!   or not real, refuses the write (Python integers are unbounded).
//! - A write is all or nothing: one refused operation stores nothing (the reference may leave a
//!   list half changed before a later operation fails).
//! - `mod` with a string is refused (Python would format the string).
//! - `update` accepts lists that hold lists or objects (Python refuses unhashable elements);
//!   `update` of an object takes another object or a list of `[key, value]` pairs with string
//!   keys only.
//!
//! Booleans count as the integers 0 and 1 wherever Python treats them so, and `and`, `or` and
//! `xor` of two booleans stay boolean.

#![allow(
    clippy::redundant_pub_crate,
    reason = "rustc's unreachable_pub (denied) needs crate visibility in this private module"
)]
#![allow(
    clippy::float_cmp,
    reason = "Python's float semantics are exact comparisons; a margin would be wrong here"
)]

use alloc::{collections::BTreeSet, string::String, vec::Vec};
use core::cmp::Ordering;
use proto::KvOp;
use serde_json::{Map, Number, Value};

/// Why a key write was refused. Nothing was stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvRefusal {
    /// Keys starting with `_read_` are read only.
    ReservedKey,
    /// The key is longer than `Limits::kv_max_key_bytes`.
    KeyTooLong,
    /// More operations than `Limits::kv_max_ops`.
    TooManyOps,
    /// A default or operand nests deeper than `Limits::kv_max_depth`.
    TooDeep,
    /// The result is larger than `Limits::kv_max_value_bytes`.
    ValueTooLarge,
    /// The room's store would pass `Limits::kv_max_room_bytes`.
    RoomFull,
    /// The operation does not apply to these value types.
    Type(KvOp),
    /// Division or modulo by zero, or zero to a negative power.
    ZeroDivision(KvOp),
    /// An integer outside `i64`, or a float result that is not finite.
    Overflow(KvOp),
    /// A negative shift count, or a list index before the list's start.
    Range(KvOp),
}

/// Numbers as Python sees them: booleans are the integers 0 and 1.
#[derive(Debug, Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

/// 2^63 as a float (bits: exponent 1086, no mantissa): the first float above every `i64`.
const TWO_63: f64 = f64::from_bits(0x43E0_0000_0000_0000);
/// 2^52: every float at least this large is an integer.
const TWO_52: f64 = 4_503_599_627_370_496.0;

/// `x` toward zero as an `i64`, saturating at the ends (exact for integers in range).
#[allow(clippy::as_conversions, reason = "a float to int cast saturates; callers range check")]
#[allow(clippy::cast_possible_truncation, reason = "see above")]
const fn trunc_i64(x: f64) -> i64 {
    x as i64
}

/// `x` as an `i64` when it is an integer inside the `i64` range.
fn exact_int(x: f64) -> Option<i64> {
    ((-TWO_63..TWO_63).contains(&x) && floor(x) == x).then(|| trunc_i64(x))
}

/// `i` as a float, rounded to nearest like Python's `float(int)`.
#[allow(clippy::as_conversions, reason = "round to nearest is the conversion Python makes")]
#[allow(clippy::cast_precision_loss, reason = "the same loss as Python's int to float")]
const fn to_float(i: i64) -> f64 {
    i as f64
}

/// Round toward minus infinity (`core` has no float rounding). Below 2^52 in size the
/// truncation to `i64` and back is exact; at or above it every float is an integer already.
fn floor(x: f64) -> f64 {
    if x.abs() >= TWO_52 {
        return x;
    }
    let t = to_float(trunc_i64(x));
    if t > x { t - 1.0 } else { t }
}

fn ceil(x: f64) -> f64 {
    -floor(-x)
}

fn num(v: &Value, op: KvOp) -> Result<Option<Num>, KvRefusal> {
    Ok(match v {
        Value::Bool(b) => Some(Num::Int(i64::from(*b))),
        Value::Number(n) => Some(match n.as_i64() {
            Some(i) => Num::Int(i),
            // A positive integer above `i64::MAX`.
            None if n.is_u64() => return Err(KvRefusal::Overflow(op)),
            None => Num::Float(n.as_f64().unwrap_or_default()),
        }),
        _ => None,
    })
}

fn int(v: &Value, op: KvOp) -> Result<Option<i64>, KvRefusal> {
    Ok(match num(v, op)? {
        Some(Num::Int(i)) => Some(i),
        _ => None,
    })
}

const fn float(n: Num) -> f64 {
    match n {
        Num::Int(i) => to_float(i),
        Num::Float(f) => f,
    }
}

fn from_float(f: f64, op: KvOp) -> Result<Value, KvRefusal> {
    Number::from_f64(f).map(Value::Number).ok_or(KvRefusal::Overflow(op))
}

/// Integer op on two integers, else the float op on both as floats.
fn arith(
    a: &Value,
    b: &Value,
    op: KvOp,
    ints: fn(i64, i64) -> Option<i64>,
    floats: fn(f64, f64) -> f64,
) -> Result<Value, KvRefusal> {
    match (num(a, op)?, num(b, op)?) {
        (Some(Num::Int(x)), Some(Num::Int(y))) => {
            ints(x, y).map(Value::from).ok_or(KvRefusal::Overflow(op))
        }
        (Some(x), Some(y)) => from_float(floats(float(x), float(y)), op),
        _ => Err(KvRefusal::Type(op)),
    }
}

/// `a` against `b` the way Python compares an int with a float: exactly.
fn cmp_int_float(a: i64, b: f64) -> Ordering {
    if b >= TWO_63 {
        return Ordering::Less;
    }
    if b < -TWO_63 {
        return Ordering::Greater;
    }
    let fl = floor(b);
    match a.cmp(&trunc_i64(fl)) {
        Ordering::Equal if b > fl => Ordering::Less,
        o => o,
    }
}

fn cmp_num(a: Num, b: Num) -> Option<Ordering> {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(&y)),
        (Num::Int(x), Num::Float(y)) => Some(cmp_int_float(x, y)),
        (Num::Float(x), Num::Int(y)) => Some(cmp_int_float(y, x).reverse()),
        (Num::Float(x), Num::Float(y)) => x.partial_cmp(&y),
    }
}

#[cfg(test)]
std::thread_local! {
    /// Element comparisons made on this thread, so tests can bound an operation's work.
    pub(crate) static COMPARISONS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

/// Count one element comparison (tests only; a no-op otherwise).
#[allow(clippy::missing_const_for_fn, reason = "not const in tests, where it counts")]
fn counted() {
    #[cfg(test)]
    COMPARISONS.with(|c| c.set(c.get().saturating_add(1)));
}

/// Python's `==` on JSON values: `1 == 1.0 == true`, lists and objects compared deeply.
pub(crate) fn py_eq(a: &Value, b: &Value) -> bool {
    counted();
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => match (num(a, KvOp::Max), num(b, KvOp::Max)) {
            (Ok(Some(x)), Ok(Some(y))) => cmp_num(x, y) == Some(Ordering::Equal),
            // Null, strings, mixed types, and integers above i64::MAX: plain equality.
            _ => a == b,
        },
    }
}

/// A value's identity under [`py_eq`], as an ordered key: numbers equal in Python share one
/// form (`1`, `1.0` and `true` are `Int(1)`; `-0.0` is `Int(0)`), so a set of these finds an
/// equal element in O(log n) instead of a scan.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Canon {
    Null,
    Int(i64),
    /// A float that is not an integer inside `i64`, by its bits.
    Float(u64),
    /// An integer above `i64::MAX`, equal only to itself (as in `py_eq`).
    Big(u64),
    Str(String),
    List(Vec<Self>),
    Obj(Vec<(String, Self)>),
}

fn canon(v: &Value) -> Canon {
    counted();
    match v {
        Value::Null => Canon::Null,
        Value::String(s) => Canon::Str(s.clone()),
        Value::Array(a) => Canon::List(a.iter().map(canon).collect()),
        Value::Object(o) => Canon::Obj(o.iter().map(|(k, x)| (k.clone(), canon(x))).collect()),
        Value::Bool(_) | Value::Number(_) => match num(v, KvOp::Update) {
            Ok(Some(Num::Int(i))) => Canon::Int(i),
            Ok(Some(Num::Float(f))) => {
                exact_int(f).map_or_else(|| Canon::Float(f.to_bits()), Canon::Int)
            }
            _ => Canon::Big(v.as_u64().unwrap_or_default()),
        },
    }
}

/// Python's ordering: numbers with numbers, strings by code point, lists element by element.
/// `None` where Python raises (other mixes, objects, `null`).
fn py_cmp(left: &Value, right: &Value, op: KvOp) -> Result<Ordering, KvRefusal> {
    match (left, right) {
        (Value::String(l), Value::String(r)) => Ok(l.cmp(r)),
        (Value::Array(l), Value::Array(r)) => match l.iter().zip(r).find(|(p, q)| !py_eq(p, q)) {
            Some((p, q)) => py_cmp(p, q, op),
            None => Ok(l.len().cmp(&r.len())),
        },
        _ => match (num(left, op)?, num(right, op)?) {
            (Some(l), Some(r)) => cmp_num(l, r).ok_or(KvRefusal::Type(op)),
            _ => Err(KvRefusal::Type(op)),
        },
    }
}

/// Nesting depth: 0 for a scalar, 1 for a flat list or object.
pub(crate) fn depth(v: &Value) -> usize {
    let inner = match v {
        Value::Array(a) => a.iter().map(depth).max().unwrap_or(0),
        Value::Object(o) => o.values().map(depth).max().unwrap_or(0),
        _ => return 0,
    };
    inner.saturating_add(1)
}

/// Bytes of `v` as compact JSON.
pub(crate) fn size(v: &Value) -> usize {
    serde_json::to_vec(v).map_or(usize::MAX, |b| b.len())
}

/// Apply `ops` to `start` in order. `max_bytes` bounds a repetition before it is built.
pub(crate) fn evaluate(
    start: Value,
    ops: &[(KvOp, Value)],
    max_bytes: usize,
    powf: fn(f64, f64) -> f64,
) -> Result<Value, KvRefusal> {
    ops.iter().try_fold(start, |cur, (op, v)| step(cur, *op, v, max_bytes, powf))
}

fn step(
    cur: Value,
    op: KvOp,
    v: &Value,
    max_bytes: usize,
    powf: fn(f64, f64) -> f64,
) -> Result<Value, KvRefusal> {
    match op {
        KvOp::Replace => Ok(v.clone()),
        KvOp::Default => Ok(cur),
        KvOp::Add => add(cur, v),
        KvOp::Mul => mul(&cur, v, max_bytes),
        KvOp::Pow => pow(&cur, v, powf),
        KvOp::Mod => modulo(&cur, v),
        KvOp::Floor | KvOp::Ceil => round(&cur, op),
        KvOp::Max | KvOp::Min => {
            // Python's max and min keep the first argument unless the second is strictly
            // larger (smaller), so a tie keeps the current value.
            let o = py_cmp(v, &cur, op)?;
            let take = if op == KvOp::Max { o == Ordering::Greater } else { o == Ordering::Less };
            Ok(if take { v.clone() } else { cur })
        }
        KvOp::And | KvOp::Or | KvOp::Xor => bits(cur, v, op),
        KvOp::LeftShift | KvOp::RightShift => shift(&cur, v, op),
        KvOp::Remove => remove(cur, v),
        KvOp::Pop => pop(cur, v),
        KvOp::Update => update(cur, v),
    }
}

fn add(cur: Value, v: &Value) -> Result<Value, KvRefusal> {
    match (cur, v) {
        (Value::String(mut a), Value::String(b)) => {
            a.push_str(b);
            Ok(Value::String(a))
        }
        (Value::Array(mut a), Value::Array(b)) => {
            a.extend(b.iter().cloned());
            Ok(Value::Array(a))
        }
        (a, b) => arith(&a, b, KvOp::Add, i64::checked_add, |x, y| x + y),
    }
}

fn mul(cur: &Value, v: &Value, max_bytes: usize) -> Result<Value, KvRefusal> {
    let op = KvOp::Mul;
    let (seq, count) = match (cur, v) {
        (s @ (Value::String(_) | Value::Array(_)), n)
        | (n, s @ (Value::String(_) | Value::Array(_))) => {
            (s, int(n, op)?.ok_or(KvRefusal::Type(op))?)
        }
        (a, b) => return arith(a, b, op, i64::checked_mul, |x, y| x * y),
    };
    // A count of zero or less is empty, as in Python.
    let n = usize::try_from(count).unwrap_or(0);
    let one = match seq {
        Value::String(s) => s.len(),
        Value::Array(a) if a.is_empty() => 0,
        _ => size(seq),
    };
    // Refuse before building: a list times 10^9 must never be allocated.
    if one.checked_mul(n).is_none_or(|total| total > max_bytes) {
        return Err(KvRefusal::ValueTooLarge);
    }
    Ok(match seq {
        Value::String(s) => Value::String(s.repeat(n)),
        Value::Array(a) => {
            Value::Array(a.iter().cycle().take(a.len().saturating_mul(n)).cloned().collect())
        }
        _ => return Err(KvRefusal::Type(op)),
    })
}

fn pow(cur: &Value, operand: &Value, powf: fn(f64, f64) -> f64) -> Result<Value, KvRefusal> {
    let op = KvOp::Pow;
    let (Some(base), Some(exp)) = (num(cur, op)?, num(operand, op)?) else {
        return Err(KvRefusal::Type(op));
    };
    if let (Num::Int(x), Num::Int(y)) = (base, exp)
        && y >= 0
    {
        let r = match (x, u32::try_from(y)) {
            (_, Ok(e)) => x.checked_pow(e),
            (0 | 1, Err(_)) => Some(x),
            (-1, Err(_)) => Some(if y & 1 == 0 { 1 } else { -1 }),
            _ => None,
        };
        return r.map(Value::from).ok_or(KvRefusal::Overflow(op));
    }
    // Python turns an integer to a negative power, and any float power, into float math.
    let (x, y) = (float(base), float(exp));
    if x == 0.0 && y < 0.0 {
        return Err(KvRefusal::ZeroDivision(op));
    }
    if y == 0.0 {
        return from_float(1.0, op);
    }
    if x < 0.0 && floor(y) != y {
        // A complex number in Python, which JSON cannot hold.
        return Err(KvRefusal::Type(op));
    }
    from_float(powf(x, y), op)
}

fn modulo(cur: &Value, v: &Value) -> Result<Value, KvRefusal> {
    let op = KvOp::Mod;
    match (num(cur, op)?, num(v, op)?) {
        (Some(_), Some(Num::Int(0))) => Err(KvRefusal::ZeroDivision(op)),
        (Some(Num::Int(a)), Some(Num::Int(b))) => {
            // `checked_rem` fails only for i64::MIN % -1, which is 0.
            let r = a.checked_rem(b).unwrap_or(0);
            // Python's result takes the divisor's sign; |r| < |b|, so the sum cannot overflow.
            Ok(Value::from(if r != 0 && r.signum() != b.signum() { r.wrapping_add(b) } else { r }))
        }
        (Some(a), Some(b)) => {
            let (a, b) = (float(a), float(b));
            if b == 0.0 {
                return Err(KvRefusal::ZeroDivision(op));
            }
            let m = a % b;
            let m = if m == 0.0 {
                0.0_f64.copysign(b)
            } else if b.is_sign_negative() != m.is_sign_negative() {
                m + b
            } else {
                m
            };
            from_float(m, op)
        }
        _ => Err(KvRefusal::Type(op)),
    }
}

fn round(cur: &Value, op: KvOp) -> Result<Value, KvRefusal> {
    match num(cur, op)? {
        Some(Num::Int(i)) => Ok(Value::from(i)),
        Some(Num::Float(f)) => {
            let r = if op == KvOp::Floor { floor(f) } else { ceil(f) };
            exact_int(r).map(Value::from).ok_or(KvRefusal::Overflow(op))
        }
        None => Err(KvRefusal::Type(op)),
    }
}

fn bits(cur: Value, operand: &Value, op: KvOp) -> Result<Value, KvRefusal> {
    let apply: fn(i64, i64) -> i64 = match op {
        KvOp::And => |l, r| l & r,
        KvOp::Or => |l, r| l | r,
        _ => |l, r| l ^ r,
    };
    match (cur, operand) {
        (Value::Bool(l), Value::Bool(r)) => {
            Ok(Value::Bool(apply(i64::from(l), i64::from(*r)) != 0))
        }
        // Python's dict union: the right side wins.
        (Value::Object(mut l), Value::Object(r)) if op == KvOp::Or => {
            l.extend(r.iter().map(|(k, x)| (k.clone(), x.clone())));
            Ok(Value::Object(l))
        }
        (l, r) => match (int(&l, op)?, int(r, op)?) {
            (Some(x), Some(y)) => Ok(Value::from(apply(x, y))),
            _ => Err(KvRefusal::Type(op)),
        },
    }
}

fn shift(cur: &Value, v: &Value, op: KvOp) -> Result<Value, KvRefusal> {
    let (Some(a), Some(n)) = (int(cur, op)?, int(v, op)?) else {
        return Err(KvRefusal::Type(op));
    };
    if n < 0 {
        return Err(KvRefusal::Range(op));
    }
    let n = u32::try_from(n).ok().filter(|n| *n < 64);
    let r = if op == KvOp::LeftShift {
        match n {
            _ if a == 0 => Some(0),
            Some(n) => {
                let r = a.wrapping_shl(n);
                // Any bit shifted out, or a sign change, means the result left i64.
                (r.wrapping_shr(n) == a).then_some(r)
            }
            None => None,
        }
    } else {
        // An arithmetic shift of 64 or more leaves only the sign.
        Some(n.map_or(if a < 0 { -1 } else { 0 }, |n| a.wrapping_shr(n)))
    };
    r.map(Value::from).ok_or(KvRefusal::Overflow(op))
}

fn remove(cur: Value, v: &Value) -> Result<Value, KvRefusal> {
    let Value::Array(mut a) = cur else { return Err(KvRefusal::Type(KvOp::Remove)) };
    if let Some(i) = a.iter().position(|x| py_eq(x, v)) {
        a.remove(i);
    }
    Ok(Value::Array(a))
}

fn pop(cur: Value, v: &Value) -> Result<Value, KvRefusal> {
    let op = KvOp::Pop;
    match cur {
        Value::Array(mut a) => {
            let i = int(v, op)?.ok_or(KvRefusal::Type(op))?;
            let len = i64::try_from(a.len()).unwrap_or(i64::MAX);
            // An index at or past the end is a no-op in the reference; before the start,
            // Python raises.
            if i >= len {
                return Ok(Value::Array(a));
            }
            let at = if i < 0 { len.checked_add(i) } else { Some(i) };
            let at = at.and_then(|x| usize::try_from(x).ok()).ok_or(KvRefusal::Range(op))?;
            a.remove(at);
            Ok(Value::Array(a))
        }
        Value::Object(mut o) => match v {
            Value::String(k) => {
                o.remove(k);
                Ok(Value::Object(o))
            }
            // Unhashable in Python.
            Value::Array(_) | Value::Object(_) => Err(KvRefusal::Type(op)),
            // Never equal to a string key.
            _ => Ok(Value::Object(o)),
        },
        _ => Err(KvRefusal::Type(op)),
    }
}

fn update(cur: Value, v: &Value) -> Result<Value, KvRefusal> {
    let op = KvOp::Update;
    match cur {
        Value::Array(mut a) => {
            let entries: Vec<Value> = match v {
                Value::Array(e) => e.clone(),
                // Python iterates an object's keys and a string's characters.
                Value::Object(o) => o.keys().cloned().map(Value::String).collect(),
                Value::String(s) => s.chars().map(|c| Value::String(String::from(c))).collect(),
                _ => return Err(KvRefusal::Type(op)),
            };
            // Membership is tested against the list as it was, so repeats inside `v` are all
            // appended; one set of the list, built once, keeps it linear (the reference does
            // the same, `MultiServer.py:147-152`).
            let before: BTreeSet<Canon> = a.iter().map(canon).collect();
            a.extend(entries.into_iter().filter(|e| !before.contains(&canon(e))));
            Ok(Value::Array(a))
        }
        Value::Object(mut o) => {
            match v {
                Value::Object(b) => o.extend(b.iter().map(|(k, x)| (k.clone(), x.clone()))),
                Value::Array(pairs) => {
                    let mut add = Map::new();
                    for p in pairs {
                        match p.as_array().map(Vec::as_slice) {
                            Some([Value::String(k), x]) => {
                                add.insert(k.clone(), x.clone());
                            }
                            _ => return Err(KvRefusal::Type(op)),
                        }
                    }
                    o.extend(add);
                }
                Value::String(s) if s.is_empty() => {}
                _ => return Err(KvRefusal::Type(op)),
            }
            Ok(Value::Object(o))
        }
        _ => Err(KvRefusal::Type(op)),
    }
}

#[cfg(test)]
mod tests;
