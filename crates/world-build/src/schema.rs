//! The settings schema, `settings.json` (design doc 20 section 5): what a settings file is
//! checked against, so the checker and the web screen never run the world compiler. Options
//! in declaration order (the order a screen shows them), then the names a setting may hold.
//!
//! Versioning: a reader of `schema/1` ignores keys it does not know, so a key added later
//! stays `schema/1`; a key whose meaning changes makes `schema/2`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::lower::OptTy;
use crate::world::{Model, NOTHING};

/// The schema's `format` value.
pub(crate) const FORMAT: &str = "schema/1";

/// The one language labels are written in today.
const LANG: &str = "en";

/// The schema of the world `manifest` and `model` were built from, as canonical JSON.
pub(crate) fn of(
    model: &Model,
    manifest: &Value,
    meta: &crate::meta::Meta,
) -> Result<String, String> {
    // Labels are string keys into `strings`, which holds the text per language.
    let mut strings: BTreeMap<String, Value> = BTreeMap::new();
    let options: Vec<Value> = model
        .options
        .iter()
        .map(|o| {
            let mut v = match &o.ty {
                OptTy::Toggle => json!({"kind": "toggle", "default": o.default != 0}),
                OptTy::Choice(values) => json!({
                    "kind": "choice", "values": values,
                    "default": usize::try_from(o.default).ok().and_then(|d| values.get(d)),
                }),
                OptTy::Range(lo, hi) => {
                    json!({"kind": "range", "min": lo, "max": hi, "default": o.default})
                }
            };
            v["key"] = json!(o.key);
            v["scope"] = json!(o.scope);
            if let Some(l) = &o.label {
                let key = format!("opt.{}.label", o.key);
                v["label"] = json!(key);
                strings.insert(key, json!({LANG: l}));
            }
            v
        })
        .collect();
    // `nothing` is the engine's own filler, never a name an author declared. Copies that read
    // options have no one number, so they are left out.
    let items: Vec<Value> = model
        .items
        .iter()
        .filter(|(k, _, _)| k != NOTHING)
        .map(|(k, _, n)| {
            if model.copies.contains_key(k) {
                json!({"name": k})
            } else {
                json!({"name": k, "copies": n})
            }
        })
        .collect();
    // Every list holds objects, so an entry can gain keys within `schema/1`.
    let named = |n: &Value| json!({"name": n});
    let places: Vec<Value> =
        manifest["locations"].as_array().into_iter().flatten().map(|x| named(&x["key"])).collect();
    let regions = manifest["regions"].as_array().into_iter().flatten();
    let doors: BTreeSet<&str> = regions
        .flat_map(|r| r["exits"].as_array().into_iter().flatten())
        .filter_map(|e| e["door"].as_str())
        .collect();
    crate::canon::to_string(&json!({
        "format": FORMAT,
        "world": meta.name,
        "version": meta.version,
        "options": options,
        "items": items,
        "places": places,
        "strings": strings,
        "doors": doors.into_iter().map(|d| named(&json!(d))).collect::<Vec<_>>(),
    }))
}
