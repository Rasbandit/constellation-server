//! Canonical JSON (design doc 16 section 3.5 rule 1): object keys sorted by their UTF-8
//! bytes, integers only, no whitespace, one newline at the end. Written here rather than left
//! to `serde_json`'s map order, which a `preserve_order` feature anywhere in a build would
//! change.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::Value;

/// `v` as canonical JSON text.
///
/// # Errors
/// A number that is not an integer (a manifest never holds one).
pub fn to_string(v: &Value) -> Result<String, String> {
    let mut out = String::new();
    write(v, &mut out)?;
    out.push('\n');
    Ok(out)
}

fn write(v: &Value, out: &mut String) -> Result<(), String> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => out.push_str(&n.to_string()),
        Value::Number(n) => return Err(format!("{n} is not an integer")),
        Value::Array(a) => {
            out.push('[');
            for (k, x) in a.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                write(x, out)?;
            }
            out.push(']');
        }
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push('{');
            for (k, key) in keys.into_iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write(&o[key], out)?;
            }
            out.push('}');
        }
        // Null, booleans and strings: serde_json's own escaping.
        x => out.push_str(&x.to_string()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_by_bytes_with_no_whitespace() {
        let v = json!({"b": [1, {"z": null, "a": true}], "a": "x\"y", "B": -3});
        assert_eq!(
            to_string(&v).unwrap(),
            "{\"B\":-3,\"a\":\"x\\\"y\",\"b\":[1,{\"a\":true,\"z\":null}]}\n"
        );
    }

    #[test]
    fn a_fraction_is_refused() {
        assert!(to_string(&json!({"a": 1.5})).is_err());
    }
}
