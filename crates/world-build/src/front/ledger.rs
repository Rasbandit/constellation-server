//! The id ledger (design doc 18 section 2.11): `ledger.json`, written only by the tool,
//! append-only. Items and checks are numbered separately, each keyed by a stable key (the
//! name, or a written `key "..."`), never by position. A `was` moves an old key's id to its
//! new name. A key no longer declared is retired, and its id is never given again. A build to
//! publish marks every declared key published; a published key may be renamed with `was` but
//! never dropped, since seeds and clients already carry its id.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Value, json};

use super::{Files, Ident, LEDGER};
use crate::model::{At, World};
use crate::world::Errs;

/// Largest id: inside 2^53 - 1, the range world ids keep to.
const MAX_ID: i64 = config::ID_MAX;

/// One entry of a ledger table.
#[derive(Clone, Copy)]
struct Entry {
    id: i64,
    published: bool,
}

/// One table of the ledger: key to its entry (retired or not is recomputed each build).
type Table = BTreeMap<String, Entry>;

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
        let published = match e.get("published") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                errs.whole(
                    format!("{LEDGER}: {what} {k:?} has `published` {other}, not true or false"),
                    "the ledger is written by the tool; restore it from version control",
                );
                // Kept as published, the safe reading, though the build already fails.
                true
            }
        };
        out.insert(k.clone(), Entry { id, published });
    }
    out
}

/// Give every declared item and check of `w` its id from the ledger, numbering new keys after
/// the largest id ever given; returns the ledger to write back. With `publish`, every declared
/// key is marked published.
pub(crate) fn assign(
    w: &mut World,
    idents: &[Ident],
    files: &dyn Files,
    publish: bool,
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
        let mut next = table.values().map(|x| x.id).max().unwrap_or(0) + 1;
        // What is declared after overlays: a removed check or item is retired.
        let present: HashSet<&str> = if check {
            w.regions.iter().flat_map(|r| &r.checks).map(|c| c.key.v.as_str()).collect()
        } else {
            w.items.iter().map(|i| i.key.v.as_str()).collect()
        };
        let mine = one_per_check(
            idents.iter().filter(|i| i.check == check && present.contains(i.name.as_str())),
            check,
            errs,
        );
        let mine: Vec<&Ident> = mine.iter().collect();
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
            let entry = match (
                table.get(&key).copied(),
                i.was.as_ref().and_then(|o| table.get(o).copied().map(|e| (o, e))),
            ) {
                (Some(e), _) => e,
                (None, Some((o, e))) => {
                    let o = o.clone();
                    table.remove(&o);
                    e
                }
                (None, None) => {
                    next += 1;
                    Entry { id: next - 1, published: false }
                }
            };
            let id = entry.id;
            if id > MAX_ID {
                errs.err(i.pos, "the ledger has no ids left", "");
            }
            table.insert(key, Entry { id, published: entry.published || publish });
            ids.insert(i.name.clone(), id);
        }
        let mut written = serde_json::Map::new();
        let what = &field[..field.len() - 1];
        for (k, e) in &table {
            let retired = !used.contains(k);
            if retired && e.published {
                errs.whole(
                    format!("published {what} `{k}` (id {}) is no longer declared", e.id),
                    "seeds and clients already carry its id: declare it again, or rename it with `was`",
                );
            }
            let mut entry = json!({"id": e.id, "retired": retired});
            // Written only when set, so a ledger never published reads as before.
            if e.published {
                entry["published"] = json!(true);
            }
            written.insert(k.clone(), entry);
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

/// The idents of one table, a check declared in several regions joined into one: its `key` and
/// `was` come from the one declaration that gives each (a second is an error), so the check
/// keeps one id.
fn one_per_check<'i>(
    idents: impl Iterator<Item = &'i Ident>,
    check: bool,
    errs: &mut Errs,
) -> Vec<Ident> {
    let mut out: Vec<Ident> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for i in idents {
        let Some(o) = at.get(&i.name).filter(|_| check).map(|k| &mut out[*k]) else {
            at.insert(i.name.clone(), out.len());
            out.push(i.clone());
            continue;
        };
        for (field, mine, theirs) in [("key", &mut o.key, &i.key), ("was", &mut o.was, &i.was)] {
            match (mine.is_some(), theirs) {
                (true, Some(_)) => errs.err(
                    i.pos,
                    format!("`{field}` of check `{}` is also given in another declaration", i.name),
                    "a check in several regions gives each modifier in one declaration only",
                ),
                (false, Some(t)) => *mine = Some(t.clone()),
                _ => {}
            }
        }
    }
    out
}

/// Whether some declaration of this table writes `key` as its `key "..."`.
fn mine_has_key(idents: &[Ident], check: bool, key: &str) -> bool {
    idents.iter().any(|i| i.check == check && i.key.as_deref() == Some(key))
}
