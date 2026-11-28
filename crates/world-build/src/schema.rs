//! The settings schema, `settings.json` (design doc 20 section 5): what a settings file is
//! checked against, so the checker and the web screen never run the world compiler. Options
//! in declaration order (the order a screen shows them), then the names a setting may hold.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::lower::OptTy;
use crate::world::{Model, NOTHING};

/// The schema's `format` value.
pub(crate) const FORMAT: &str = "schema/1";

/// The schema of the world `manifest` and `model` were built from, as canonical JSON.
pub(crate) fn of(
    model: &Model,
    manifest: &Value,
    meta: &crate::meta::Meta,
) -> Result<String, String> {
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
                v["label"] = json!(l);
            }
            v
        })
        .collect();
    // `nothing` is the engine's own filler, never a name an author declared.
    let keys = |list: &Value| -> Vec<Value> {
        let all = list.as_array().into_iter().flatten().map(|x| x["key"].clone());
        all.filter(|k| k != NOTHING).collect()
    };
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
        "items": keys(&manifest["items"]),
        "places": keys(&manifest["locations"]),
        "doors": doors,
    }))
}
