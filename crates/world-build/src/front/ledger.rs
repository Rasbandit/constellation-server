//! The id ledger (design doc 18 section 2.11): `ledger.json`, written only by the tool,
//! append-only. Items and checks are numbered separately, each keyed by a stable key (the
//! name, or a written `key "..."`), never by position. A `was` moves an old key's id to its
//! new name. A key no longer declared is retired, and its id is never given again.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use super::{Files, Ident, LEDGER};
use crate::model::{At, World};
use crate::world::Errs;

/// Largest id: inside 2^53 - 1, the range world ids keep to.
const MAX_ID: i64 = config::ID_MAX;

/// One table of the ledger: key to (id, retired).
type Table = BTreeMap<String, (i64, bool)>;

fn read_table(v: &Value, what: &str, errs: &mut Errs) -> Table {
    let mut out = Table::new();
    let mut ids = HashSet::new();
    for (k, e) in v.as_object().into_iter().flatten() {
        let id = e["id"].as_i64().unwrap_or(0);
        if !(1..=MAX_ID).contains(&id) {
            errs.whole(
                format!("{LEDGER}: {what} {k:?} has id {}, outside 1 to 2^53 - 1", e["id"]),
                "the ledger is written by the tool; restore it from version control",
            );
            continue;
        }
        if !ids.insert(id) {
            errs.whole(
                format!("{LEDGER}: two {what}s have id {id}"),
                "the ledger is written by the tool; restore it from version control",
            );
            continue;
        }
        out.insert(k.clone(), (id, e["retired"].as_bool().unwrap_or(false)));
    }
    out
}

/// Give every declared item and check of `w` its id from the ledger, numbering new keys after
/// the largest id ever given; returns the ledger to write back.
pub(crate) fn assign(
    w: &mut World,
    idents: &[Ident],
    files: &dyn Files,
    errs: &mut Errs,
) -> String {
    let game = w.game.as_ref().map(|g| g.v.clone()).unwrap_or_default();
    let old: Value = match files.read(LEDGER) {
        None => json!({}),
        Some(Err(e)) => {
            errs.whole(e, "");
            json!({})
        }
        Some(Ok(text)) => serde_json::from_str(&text).unwrap_or_else(|e| {
            errs.whole(
                format!("{LEDGER} is not valid JSON: {e}"),
                "the ledger is written by the tool; restore it from version control",
            );
            json!({})
        }),
    };
    if let Some(other) = old["world"].as_str()
        && other != game
    {
        errs.whole(
            format!("{LEDGER} belongs to world `{other}`, not `{game}`"),
            "a ledger is never shared between worlds",
        );
    }
    let mut out = json!({"world": game});
    for (check, field) in [(false, "items"), (true, "checks")] {
        let mut table = read_table(&old[field], &field[..field.len() - 1], errs);
        let mut next = table.values().map(|x| x.0).max().unwrap_or(0) + 1;
        // What is declared after overlays: a removed check or item is retired.
        let present: HashSet<&str> = if check {
            w.regions.iter().flat_map(|r| &r.checks).map(|c| c.key.v.as_str()).collect()
        } else {
            w.items.iter().map(|i| i.key.v.as_str()).collect()
        };
        let mine: Vec<&Ident> = idents
            .iter()
            .filter(|i| i.check == check && present.contains(i.name.as_str()))
            .collect();
        let key_of = |i: &Ident| i.key.clone().unwrap_or_else(|| i.name.clone());
        let declared: HashSet<String> = mine.iter().map(|i| key_of(i)).collect();
        let mut used: HashSet<String> = HashSet::new();
        let mut moved: HashSet<String> = HashSet::new();
        let mut ids: BTreeMap<String, i64> = BTreeMap::new();
        for i in mine {
            let key = key_of(i);
            if !used.insert(key.clone()) {
                // Two declarations of one name are the checks' error, reported there; a key
                // that meets another's key or name is this one's.
                if i.key.is_some() || mine_has_key(idents, check, &key) {
                    errs.err(
                        i.pos,
                        format!("ledger key `{key}` is used twice"),
                        "a key names one item or check forever; give each its own",
                    );
                }
                continue;
            }
            if let Some(o) = &i.was
                && declared.contains(o)
            {
                errs.err(i.pos, format!("`was {o}` names a key that is still declared"), "a rename moves an id from a name that is gone; drop the `was`, or the old declaration");
                continue;
            }
            if let Some(o) = &i.was
                && !table.contains_key(&key)
                && !moved.insert(o.clone())
            {
                errs.err(
                    i.pos,
                    format!("`was {o}` is also written on another declaration"),
                    "an id moves to one new name; drop one of the `was`",
                );
                continue;
            }
            let id = match (
                table.get(&key).copied(),
                i.was.as_ref().and_then(|o| table.get(o).copied().map(|e| (o, e))),
            ) {
                (Some((id, _)), _) => id,
                (None, Some((o, (id, _)))) => {
                    let o = o.clone();
                    table.remove(&o);
                    id
                }
                (None, None) => {
                    next += 1;
                    next - 1
                }
            };
            if id > MAX_ID {
                errs.err(i.pos, "the ledger has no ids left", "");
            }
            table.insert(key, (id, false));
            ids.insert(i.name.clone(), id);
        }
        let mut written = serde_json::Map::new();
        for (k, (id, _)) in &table {
            written.insert(k.clone(), json!({"id": id, "retired": !used.contains(k)}));
        }
        out[field] = Value::Object(written);
        if check {
            for c in w.regions.iter_mut().flat_map(|r| &mut r.checks) {
                c.id = ids.get(&c.key.v).map(|id| At::new(*id, c.key.pos));
            }
        } else {
            for it in &mut w.items {
                it.id = ids.get(&it.key.v).map(|id| At::new(*id, it.key.pos));
            }
        }
    }
    crate::canon::to_string(&out).unwrap_or_else(|e| {
        errs.whole(format!("the ledger is not canonical: {e}"), "");
        String::new()
    })
}

/// Whether some declaration of this table writes `key` as its `key "..."`.
fn mine_has_key(idents: &[Ident], check: bool, key: &str) -> bool {
    idents.iter().any(|i| i.check == check && i.key.as_deref() == Some(key))
}
