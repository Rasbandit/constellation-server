//! Serde helpers for the wire: the integer range (doc 01 D1.10 rule 15) and "present, even if
//! null" fields.
//!
//! Every CWP integer is within plus or minus 2^53 - 1, the range every JSON parser holds exactly.
//! The room's own types (`Seq`, `ItemSeq`, `Epoch`, ids) are wider; these helpers enforce the
//! wire range where those types cross the wire, in both directions.
use crate::{Epoch, ItemSeq, Seq};
use serde::{Deserialize, Deserializer, Serializer, de, ser};
use serde_json::Value;

/// The largest integer CWP carries: 2^53 - 1.
pub const MAX_SAFE: u64 = 9_007_199_254_740_991;
/// The same bound for signed ids.
pub const MAX_SAFE_I64: i64 = 9_007_199_254_740_991;

/// An unsigned wire integer.
pub(super) trait Unsigned: Copy {
    fn get(self) -> u64;
    fn make(v: u64) -> Self;
}

impl Unsigned for u64 {
    fn get(self) -> u64 {
        self
    }
    fn make(v: u64) -> Self {
        v
    }
}

macro_rules! unsigned_newtype {
    ($t:ty) => {
        impl Unsigned for $t {
            fn get(self) -> u64 {
                self.0
            }
            fn make(v: u64) -> Self {
                Self(v)
            }
        }
    };
}
unsigned_newtype!(Seq);
unsigned_newtype!(ItemSeq);
unsigned_newtype!(Epoch);

const RANGE: &str = "integer outside the CWP range of plus or minus 2^53 - 1";

pub(super) fn de_u<'de, D: Deserializer<'de>, T: Unsigned>(d: D) -> Result<T, D::Error> {
    let v = u64::deserialize(d)?;
    if v > MAX_SAFE {
        return Err(de::Error::custom(RANGE));
    }
    Ok(T::make(v))
}

pub(super) fn ser_u<S: Serializer, T: Unsigned>(v: &T, s: S) -> Result<S::Ok, S::Error> {
    let v = v.get();
    if v > MAX_SAFE {
        return Err(ser::Error::custom(RANGE));
    }
    s.serialize_u64(v)
}

pub(super) fn de_opt_u<'de, D: Deserializer<'de>, T: Unsigned>(
    d: D,
) -> Result<Option<T>, D::Error> {
    match Option::<u64>::deserialize(d)? {
        None => Ok(None),
        Some(v) if v > MAX_SAFE => Err(de::Error::custom(RANGE)),
        Some(v) => Ok(Some(T::make(v))),
    }
}

#[expect(clippy::ref_option, reason = "serde's serialize_with passes a reference to the field")]
pub(super) fn ser_opt_u<S: Serializer, T: Unsigned>(
    v: &Option<T>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        None => s.serialize_none(),
        Some(v) => ser_u(v, s),
    }
}

pub(super) fn de_i<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let v = i64::deserialize(d)?;
    if !(-MAX_SAFE_I64..=MAX_SAFE_I64).contains(&v) {
        return Err(de::Error::custom(RANGE));
    }
    Ok(v)
}

#[expect(clippy::trivially_copy_pass_by_ref, reason = "serde's serialize_with passes a reference")]
pub(super) fn ser_i<S: Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
    if !(-MAX_SAFE_I64..=MAX_SAFE_I64).contains(v) {
        return Err(ser::Error::custom(RANGE));
    }
    s.serialize_i64(*v)
}

/// For `Option<Value>` fields where `null` is a real value: missing gives `None` (with
/// `#[serde(default)]`), `null` gives `Some(Value::Null)`.
pub(super) fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// `skip_serializing_if` for plain `bool` fields that default to false.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
pub(super) const fn is_false(b: &bool) -> bool {
    !*b
}

#[expect(clippy::ref_option, reason = "serde's serialize_with passes a reference to the field")]
pub(super) fn ser_opt_i<S: Serializer>(v: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        None => s.serialize_none(),
        Some(v) => ser_i(v, s),
    }
}

pub(super) fn de_opt_i<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    match Option::<i64>::deserialize(d)? {
        Some(v) if !(-MAX_SAFE_I64..=MAX_SAFE_I64).contains(&v) => Err(de::Error::custom(RANGE)),
        v => Ok(v),
    }
}

/// Whether every number in `v`, however deep (free-form `data`, `opts`, `ok`, `value`), is within
/// plus or minus 2^53 - 1 (rule 15). Fractions are allowed; magnitudes above the bound are not.
#[must_use]
pub fn in_range(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.as_u64().is_none_or(|u| u <= MAX_SAFE)
                && n.as_i64().is_none_or(|i| i >= -MAX_SAFE_I64)
                && n.as_f64().is_none_or(|f| f.abs() <= 9_007_199_254_740_991.0)
        }
        Value::Array(a) => a.iter().all(in_range),
        Value::Object(m) => m.values().all(in_range),
        Value::Null | Value::Bool(_) | Value::String(_) => true,
    }
}
