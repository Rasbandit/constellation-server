//! Layering (design doc 20 section 2.6), the checks that need every layer, canonical JSON and
//! the settings hash (section 6).
//!
//! Values apply in this order, later winning: the world's defaults, each `preset` line in file
//! order (a preset's own preset before it), then the file. Blocks then run in layer order and
//! file order (2.12). Canonical JSON folds every block up to the first one that depends on a
//! draw into `options`, and keeps that one and every later one in `rules`.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use config::Limits;
use rando_syntax::{Diagnostic, Pos, suggest};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::ast::{Header, Node, Op, PresetRef, parse};
use crate::check::{Block, Change, Layer, Val, check};
use crate::diag;
use crate::fmt::{hex, token_hash};
use crate::schema::{Kind, Schema, Scope};

/// Canonical JSON's `format`.
pub const FORMAT: &str = "settings/1";

/// Where presets come from: the world's folder or package, and the registry. Each returns the
/// preset's text and the name to show it by in messages.
pub trait Presets {
    /// The world preset `name`.
    fn world(&self, name: &str) -> Option<(String, String)>;
    /// The community preset `name`, pinned by `sha256`.
    fn community(&self, name: &str, sha256: &str) -> Option<(String, String)>;
}

/// No presets at all.
pub struct NoPresets;

impl Presets for NoPresets {
    fn world(&self, _: &str) -> Option<(String, String)> {
        None
    }
    fn community(&self, _: &str, _: &str) -> Option<(String, String)> {
        None
    }
}

/// A file layered and settled.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    /// Canonical JSON as a value.
    pub json: Value,
    /// Canonical JSON as text.
    pub canonical: String,
    /// The settings hash, SHA-256 in hex.
    pub hash: String,
}

struct Load<'a> {
    s: &'a Schema,
    presets: &'a dyn Presets,
    l: &'a Limits,
    errs: Vec<Diagnostic>,
    /// The layers in order, each with its token hash (presets only).
    layers: Vec<(Layer, Option<String>)>,
    count: usize,
}

impl Load<'_> {
    /// Preset `p`, named in `file`, and the preset it names, as layers.
    fn preset(&mut self, p: &Node<PresetRef>, file: &str, only_community: bool) {
        self.count += 1;
        if self.count > self.l.settings_max_presets {
            let msg = format!(
                "At most {} presets take part in a file's layering, counting the presets they name.",
                self.l.settings_max_presets
            );
            self.errs.push(diag(
                file,
                "settings.preset",
                Some(p.pos),
                msg,
                Some("remove a `preset` line".into()),
            ));
            return;
        }
        let err = |msg: String, help: Option<String>| {
            diag(file, "settings.preset", Some(p.pos), msg, help)
        };
        let (found, pin) = match &p.v {
            PresetRef::World(_) if only_community => {
                let msg = "A community preset names only other community presets, pinned by hash."
                    .to_owned();
                self.errs.push(err(msg, None));
                return;
            }
            PresetRef::World(name) => {
                let Some(found) = self.presets.world(name) else {
                    let help = suggest::nearest(name, self.s.presets.keys().map(String::as_str))
                        .map(|n| format!("did you mean `{n}`?"));
                    self.errs
                        .push(err(format!("{} has no preset '{name}'.", self.s.title()), help));
                    return;
                };
                (found, self.s.presets.get(name).cloned().flatten())
            }
            PresetRef::Community { name, sha256 } => {
                let Some(found) = self.presets.community(name, sha256) else {
                    let msg =
                        format!("The community preset '{name}' pinned by {sha256} was not found.");
                    self.errs
                        .push(err(msg, Some("community presets come from the registry".into())));
                    return;
                };
                (found, Some(sha256.clone()))
            }
        };
        let (text, shown) = found;
        let (f, e) = parse(&text, &shown, self.l);
        self.errs.extend(e);
        let hash = token_hash(&f);
        if let Some(pin) = pin
            && pin != hash
        {
            let msg =
                format!("The preset {shown} does not match its pin: its token hash is {hash}.");
            self.errs.push(err(
                msg,
                Some("ask for the preset that matches, or pin the new hash".into()),
            ));
            return;
        }
        if !matches!(f.header.as_ref().map(|h| &h.v), Some(Header::Preset(_))) {
            self.errs.push(err(
                format!("{shown} is not a preset file (`preset NAME for WORLD`)."),
                None,
            ));
            return;
        }
        if f.presets.len() > 1 {
            let at = f.presets[1].pos;
            self.errs.push(diag(
                &shown,
                "settings.preset",
                Some(at),
                "A preset names at most one preset.".into(),
                None,
            ));
        }
        let community = matches!(p.v, PresetRef::Community { .. });
        if let Some(inner) = f.presets.first() {
            self.preset(inner, &shown, community);
        }
        let (layer, e) = check(&f, self.s, &shown, self.l);
        self.errs.extend(e);
        self.layers.push((layer, Some(hash)));
    }
}

/// `src`, the settings file `file`, parsed, checked against `s`, layered with its presets, as
/// canonical JSON and its hash.
///
/// # Errors
/// Every error in the file and its presets.
pub fn settle(
    src: &str,
    file: &str,
    s: &Schema,
    presets: &dyn Presets,
    l: &Limits,
) -> Result<Settled, Vec<Diagnostic>> {
    let (f, errs) = parse(src, file, l);
    let mut load = Load { s, presets, l, errs, layers: Vec::new(), count: 0 };
    for p in &f.presets {
        load.preset(p, file, false);
    }
    let (top, e) = check(&f, s, file, l);
    load.errs.extend(e);
    load.layers.push((top, None));
    let Load { mut errs, layers, .. } = load;
    let json = settle_layers(s, &layers, &mut errs);
    if !errs.is_empty() {
        return Err(errs);
    }
    let canonical = rando_syntax::canon::to_string(&json)
        .map_err(|e| Vec::from([diag(file, "settings.syntax", None, e, None)]))?;
    let hash = hash(&json, s.sidecar);
    Ok(Settled { json, canonical, hash })
}

/// The settings hash of canonical JSON `v` (section 6): SHA-256 of it with `player`, `presets`
/// and `looks` removed, and `legacy` removed unless the world runs in the sidecar.
#[must_use]
pub fn hash(v: &Value, sidecar: bool) -> String {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        for k in ["player", "presets", "looks"] {
            o.remove(k);
        }
        if !sidecar {
            o.remove("legacy");
        }
    }
    let text = rando_syntax::canon::to_string(&v).unwrap_or_default();
    hex(&Sha256::digest(text.as_bytes()))
}

/// A list or counts value with `delta` added or removed (2.12, `+=` and `-=`).
fn apply(cur: &Value, op: Op, delta: &Value) -> Value {
    match (cur, delta) {
        (Value::Array(a), Value::Array(d)) => {
            // Names, or `[name, count]` pairs.
            let key = |x: &Value| {
                x.as_str().map_or_else(|| x[0].as_str().unwrap_or("").to_owned(), ToOwned::to_owned)
            };
            let mut m: BTreeMap<String, Value> = a.iter().map(|x| (key(x), x.clone())).collect();
            for x in d {
                let k = key(x);
                match op {
                    Op::Remove => {
                        m.remove(&k);
                    }
                    _ => {
                        let next = match (m.get(&k), x) {
                            (Some(Value::Array(old)), Value::Array(new)) => {
                                json!([
                                    k,
                                    old[1].as_i64().unwrap_or(0) + new[1].as_i64().unwrap_or(0)
                                ])
                            }
                            _ => x.clone(),
                        };
                        m.insert(k, next);
                    }
                }
            }
            Value::Array(m.into_values().collect())
        }
        (Value::Object(o), Value::Object(d)) => {
            let mut o = o.clone();
            for (k, n) in d {
                let old = o.get(k).and_then(Value::as_i64).unwrap_or(0);
                let n = n.as_i64().unwrap_or(0);
                o.insert(k.clone(), json!(if op == Op::Remove { old - n } else { old + n }));
            }
            Value::Object(o)
        }
        _ => cur.clone(),
    }
}

fn val_json(v: &Val) -> &Value {
    match v {
        Val::Plain(x) | Val::Surprise(x) => x,
    }
}

fn rule(b: &Block, changes: &[&Change]) -> Value {
    let mut when: Vec<Value> = b.when.iter().map(|(k, v)| json!([k, v])).collect();
    when.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
    let mut maps = [Map::new(), Map::new(), Map::new()];
    for c in changes {
        let i = match c.op {
            Op::Set => 0,
            Op::Add => 1,
            Op::Remove => 2,
        };
        maps[i].insert(c.key.clone(), val_json(&c.val).clone());
    }
    let [set, add, remove] = maps;
    json!({"when": when, "chance": b.chance.unwrap_or(100), "set": set, "add": add, "remove": remove})
}

/// The layers' values in canonical JSON (doc 20 section 6), with the checks that need every
/// layer pushed onto `errs`.
fn settle_layers(
    s: &Schema,
    layers: &[(Layer, Option<String>)],
    errs: &mut Vec<Diagnostic>,
) -> Value {
    let mut eff: BTreeMap<String, Val> = BTreeMap::new();
    for (k, o) in &s.options {
        if o.kind != Kind::Room && (!o.needs_offer || s.offers.contains(k)) {
            eff.insert(k.clone(), Val::Plain(o.default.clone()));
        }
    }
    // The last layer that sets each key, and where.
    let mut set_by: BTreeMap<&str, (usize, Pos)> = BTreeMap::new();
    for (i, (layer, _)) in layers.iter().enumerate() {
        for (k, v) in &layer.values {
            eff.insert(k.clone(), v.val.clone());
            set_by.insert(k, (i, v.pos));
        }
    }
    let mut tested: BTreeMap<&str, (Pos, &str)> = BTreeMap::new();
    let mut maybe_surprise: BTreeSet<String> =
        eff.iter().filter(|(_, v)| matches!(v, Val::Surprise(_))).map(|(k, _)| k.clone()).collect();
    let (mut rules, mut looks_rules) = (Vec::new(), Vec::new());
    let mut folding = true;
    for (i, (layer, _)) in layers.iter().enumerate() {
        let file = layer.file.as_str();
        for b in &layer.blocks {
            let mut ok = true;
            for c in &b.changes {
                let label = s.label(&c.key);
                if let Some((at, f)) = tested.get(c.key.as_str()) {
                    let whose = if *f == file {
                        format!("line {}", at.line)
                    } else {
                        format!("line {} of {f}", at.line)
                    };
                    let msg = format!(
                        "This block changes '{label}', which the block on {whose} already used to decide."
                    );
                    let help = format!("move this change into the block on {whose}, or remove it");
                    let pos = Pos { line: c.pos.line, col: c.pos.col, file: c.pos.file };
                    errs.push(diag(file, "settings.block", Some(pos), msg, Some(help)));
                    ok = false;
                }
                if c.op != Op::Set && maybe_surprise.contains(&c.key) {
                    let msg = format!(
                        "'{label}' may hold a surprise here, so there is no list yet to add to or remove from."
                    );
                    errs.push(diag(
                        file,
                        "settings.block_on_surprise",
                        Some(c.pos),
                        msg,
                        Some(format!("write `{} = ...` with the whole new value", c.key)),
                    ));
                    ok = false;
                }
            }
            for c in &b.changes {
                if matches!(c.val, Val::Surprise(_)) {
                    maybe_surprise.insert(c.key.clone());
                }
            }
            for (k, _) in &b.when {
                tested.entry(k).or_insert((b.pos, file));
            }
            if !ok {
                continue;
            }
            // A later layer's line for a key overrides this block's change to it (2.12, rule 5).
            let changes: Vec<&Change> = b
                .changes
                .iter()
                .filter(|c| set_by.get(c.key.as_str()).is_none_or(|(j, _)| *j <= i))
                .collect();
            if changes.is_empty() {
                continue;
            }
            if b.cosmetic {
                looks_rules.push(rule(b, &changes));
                continue;
            }
            let draws = b.with
                || b.chance.is_some()
                || b.when.iter().any(|(k, _)| matches!(eff.get(k), Some(Val::Surprise(_))));
            if folding && !draws {
                if b.when.iter().all(|(k, v)| eff.get(k) == Some(&Val::Plain(v.clone()))) {
                    for c in &changes {
                        let next = match (c.op, eff.get(&c.key), &c.val) {
                            (Op::Set, _, v) => v.clone(),
                            (op, Some(Val::Plain(cur)), Val::Plain(d)) => {
                                Val::Plain(apply(cur, op, d))
                            }
                            (_, _, v) => v.clone(),
                        };
                        eff.insert(c.key.clone(), next);
                    }
                }
                continue;
            }
            folding = false;
            rules.push(rule(b, &changes));
        }
    }
    places(s, layers, &eff, &set_by, errs);
    let (mut options, mut surprise, mut looks_o, mut looks_s) =
        (Map::new(), Map::new(), Map::new(), Map::new());
    for (k, v) in &eff {
        let cosmetic = s.options[k].scope == Scope::Cosmetic;
        let (to, x) = match (v, cosmetic) {
            (Val::Plain(x), false) => (&mut options, x),
            (Val::Surprise(x), false) => (&mut surprise, x),
            (Val::Plain(x), true) => (&mut looks_o, x),
            (Val::Surprise(x), true) => (&mut looks_s, x),
        };
        to.insert(k.clone(), x.clone());
    }
    let mut legacy = Map::new();
    let (mut place, mut connect, mut pin) = (Vec::new(), Vec::new(), Vec::new());
    for (layer, _) in layers {
        legacy.extend(layer.legacy.clone());
        place.extend(layer.place.iter().map(|p| p.json.clone()));
        connect.extend(layer.connect.iter().map(|c| c.0.clone()));
        pin.extend(layer.pin.iter().map(|p| p.0.clone()));
    }
    let presets: Vec<&String> = layers.iter().filter_map(|l| l.1.as_ref()).collect();
    let mut out = json!({
        "format": FORMAT,
        "world": s.world,
        "version": s.version,
        "presets": presets,
        "options": options,
        "surprise": surprise,
        "rules": rules,
        "place": place,
        "connect": connect,
        "pin": pin,
        "looks": {"options": looks_o, "surprise": looks_s, "rules": looks_rules},
        "legacy": legacy,
    });
    if let Some(p) = layers.last().and_then(|l| l.0.player.as_ref()) {
        out["player"] = json!(p);
    }
    out
}

/// The `place`, `connect` and `pin` checks that need every layer (2.11).
fn places(
    s: &Schema,
    layers: &[(Layer, Option<String>)],
    eff: &BTreeMap<String, Val>,
    set_by: &BTreeMap<&str, (usize, Pos)>,
    errs: &mut Vec<Diagnostic>,
) {
    let names = |k: &str| -> BTreeSet<String> {
        match eff.get(k) {
            Some(Val::Plain(Value::Array(a))) => {
                a.iter().filter_map(|x| x.as_str().map(ToOwned::to_owned)).collect()
            }
            _ => BTreeSet::new(),
        }
    };
    let (away, no, yes) = (names("send_away"), names("no_important_at"), names("important_at"));
    if let Some(both) = no.intersection(&yes).next()
        && let Some((i, pos)) = set_by.get("important_at").max(set_by.get("no_important_at"))
    {
        let msg = format!("'{both}' is in both `no_important_at` and `important_at`.");
        errs.push(diag(
            &layers[*i].0.file,
            "settings.placement",
            Some(*pos),
            msg,
            Some("keep it in one of them".into()),
        ));
    }
    let mut taken: BTreeMap<String, i64> = BTreeMap::new();
    if let Some(Val::Plain(sw)) = eff.get("start_with") {
        for pair in sw["from_shuffle"].as_array().into_iter().flatten() {
            *taken.entry(pair[0].as_str().unwrap_or("").to_owned()).or_default() +=
                pair[1].as_i64().unwrap_or(0);
        }
    }
    let mut single: BTreeMap<&str, ()> = BTreeMap::new();
    let mut all: BTreeSet<&str> = BTreeSet::new();
    let mut doors: BTreeSet<&str> = BTreeSet::new();
    let mut slots: BTreeSet<(String, &str)> = BTreeSet::new();
    for (layer, _) in layers {
        let file = layer.file.as_str();
        let mut err = |pos: Pos, msg: String| {
            errs.push(diag(file, "settings.placement", Some(pos), msg, None))
        };
        for p in &layer.place {
            for (item, n) in &p.items {
                if away.contains(item) {
                    err(
                        p.pos,
                        format!(
                            "'{item}' is in `send_away`, so it cannot be placed in your own game."
                        ),
                    );
                }
                if p.copy {
                    continue;
                }
                let have = s.items.get(item).and_then(|i| i.copies).map(|c| c as i64);
                let used = taken.entry(item.clone()).or_default();
                match n {
                    None => {
                        if *used > 0 || !all.insert(item) {
                            err(
                                p.pos,
                                format!(
                                    "`{item} x all` takes every copy, and another line takes '{item}' too."
                                ),
                            );
                        }
                    }
                    Some(n) => {
                        *used += n;
                        if all.contains(item.as_str()) {
                            err(
                                p.pos,
                                format!(
                                    "`{item} x all` takes every copy, and this line takes '{item}' too."
                                ),
                            );
                        } else if let Some(have) = have
                            && *used > have
                        {
                            err(
                                p.pos,
                                format!(
                                    "The shuffle holds {have} of '{item}', and this file takes {used}."
                                ),
                            );
                        }
                    }
                }
            }
            if p.strict
                && let Some(place) = &p.single
                && single.insert(place, ()).is_some()
            {
                err(p.pos, format!("Two placements must both land on '{place}'."));
            }
        }
        for (_, ds, pos) in &layer.connect {
            for d in ds {
                if !doors.insert(d) {
                    err(*pos, format!("The door '{d}' is connected twice."));
                }
            }
        }
        for (j, slot, pos) in &layer.pin {
            let shuffle = j["shuffle"].as_str().unwrap_or("").to_owned();
            if !slots.insert((shuffle, slot)) {
                err(*pos, format!("The slot '{slot}' is pinned twice."));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::tests::lanternfall;
    use crate::fmt::token_hash;
    use alloc::borrow::ToOwned;
    use alloc::collections::BTreeMap;
    use alloc::string::ToString;
    use alloc::vec;
    use serde_json::json;

    const L: Limits = Limits::DESIGN;

    /// Presets by name (world) or by name and hash (community).
    struct Map(BTreeMap<String, String>);

    impl Presets for Map {
        fn world(&self, name: &str) -> Option<(String, String)> {
            self.0.get(name).map(|t| (t.clone(), alloc::format!("presets/{name}.randoset")))
        }
        fn community(&self, name: &str, sha256: &str) -> Option<(String, String)> {
            self.0
                .get(&alloc::format!("{name}@{sha256}"))
                .map(|t| (t.clone(), alloc::format!("{name}.randoset")))
        }
    }

    const RELAXED: &str = "preset relaxed for lanternfall\ntraps = a_few\nhint_count = 8\n";

    fn presets() -> Map {
        let mut m = BTreeMap::new();
        m.insert("relaxed".to_owned(), RELAXED.to_owned());
        m.insert(
            "standard".to_owned(),
            "preset standard for lanternfall\nhint_count = 3\ngoal = any\n".to_owned(),
        );
        m.insert(
            "chained".to_owned(),
            "preset chained for lanternfall\npreset relaxed\nbombs = 10\n".to_owned(),
        );
        Map(m)
    }

    fn settle_ok(src: &str, p: &dyn Presets) -> Settled {
        settle(src, "mira.randoset", &lanternfall(), p, &L).unwrap_or_else(|e| panic!("{e:#?}"))
    }

    fn errs(src: &str, p: &dyn Presets) -> Vec<String> {
        settle(src, "mira.randoset", &lanternfall(), p, &L)
            .map(|_| ())
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn keys(src: &str) -> Vec<String> {
        settle(src, "mira.randoset", &lanternfall(), &presets(), &L)
            .err()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|d| d.key)
            .collect()
    }

    #[test]
    fn the_beginner_file_settles_to_pinned_canonical_json_and_hash() {
        let s = settle_ok(crate::ast::tests::BEGINNER, &presets());
        let o = &s.json["options"];
        assert_eq!(o["goal"], json!("find_the_crown"));
        assert_eq!(o["hint_count"], json!(8));
        assert_eq!(o["start_with"], json!({"from_shuffle": [], "items": [["lantern", 1]]}));
        assert_eq!(o["shared_deaths"], json!(false));
        assert_eq!(o["keep_moving"], json!("gentle"));
        assert!(o.get("traps").is_none() && o.get("lamp_color").is_none());
        assert_eq!(s.json["surprise"], json!({"traps": {"one_of": ["a_few", "none"]}}));
        assert_eq!(s.json["looks"]["options"]["lamp_color"], json!("amber"));
        assert_eq!(s.json["player"], json!("Mira"));
        let relaxed = token_hash(&crate::ast::parse(RELAXED, "p", &L).0);
        assert_eq!(s.json["presets"], json!([relaxed]));
        assert_eq!(
            (s.json["format"].as_str(), s.json["version"].as_str(), s.json["world"].as_str()),
            (Some("settings/1"), Some("0.1.3"), Some("lanternfall"))
        );
        assert_eq!(s.canonical, rando_syntax::canon::to_string(&s.json).unwrap());
        // Golden: these change only when the canonical form or the test world changes.
        assert_eq!(s.hash, GOLDEN_HASH, "{}", s.canonical);
    }

    const GOLDEN_HASH: &str = "38b130063b07fff59a4961820b97ff3031b3002745adf6063063f3e57c010faf";

    #[test]
    fn later_layers_win() {
        let s =
            settle_ok("settings for lanternfall\npreset relaxed\npreset standard\n", &presets());
        assert_eq!(s.json["options"]["hint_count"], json!(3));
        assert_eq!(s.json["options"]["traps"], json!("a_few"));
        assert_eq!(s.json["surprise"]["goal"], json!({"any": {}}));
        let s = settle_ok(
            "settings for lanternfall\npreset standard\npreset relaxed\nhint_count = 1\n",
            &presets(),
        );
        assert_eq!(s.json["options"]["hint_count"], json!(1));
        assert_eq!(s.json["options"].get("goal"), None);
        // A preset's own preset comes before it.
        let s = settle_ok("settings for lanternfall\npreset chained\n", &presets());
        assert_eq!(
            (s.json["options"]["bombs"].clone(), s.json["options"]["hint_count"].clone()),
            (json!(10), json!(8))
        );
        assert_eq!(s.json["presets"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn preset_rules_are_held() {
        assert_eq!(
            keys("settings for lanternfall\npreset chained\npreset standard\n"),
            ["settings.preset"]
        );
        assert_eq!(
            keys("settings for lanternfall\npreset relaxed\npreset standard\npreset chained\n"),
            ["settings.preset"]
        );
        assert_eq!(keys("settings for lanternfall\npreset missing\n"), ["settings.preset"]);
        assert_eq!(
            keys("settings for lanternfall\npreset \"fast\" sha256 \"00\"\n"),
            ["settings.preset"]
        );
        let text = "preset fast for lanternfall\nbombs = 3\n";
        let sha = token_hash(&crate::ast::parse(text, "p", &L).0);
        let mut m = presets().0;
        m.insert(alloc::format!("fast@{sha}"), text.to_owned());
        m.insert("fast@00".to_owned(), text.to_owned());
        let p = Map(m);
        let s = settle_ok(
            &alloc::format!("settings for lanternfall\npreset \"fast\" sha256 \"{sha}\"\n"),
            &p,
        );
        assert_eq!(s.json["options"]["bombs"], json!(3));
        let e = errs("settings for lanternfall\npreset \"fast\" sha256 \"00\"\n", &p);
        assert!(e[0].contains("does not match"), "{e:?}");
        // A community preset names only community presets.
        let bad = "preset c for lanternfall\npreset relaxed\n";
        let sha = token_hash(&crate::ast::parse(bad, "p", &L).0);
        let mut m = presets().0;
        m.insert(alloc::format!("c@{sha}"), bad.to_owned());
        let e = errs(
            &alloc::format!("settings for lanternfall\npreset \"c\" sha256 \"{sha}\"\n"),
            &Map(m),
        );
        assert!(e.iter().any(|x| x.contains("community preset")), "{e:?}");
        // A preset's own errors carry its file name.
        let mut m = presets().0;
        m.insert("broken".to_owned(), "preset broken for lanternfall\nbombs = lots\n".to_owned());
        let e = errs("settings for lanternfall\npreset broken\n", &Map(m));
        assert!(e[0].starts_with("presets/broken.randoset:2:9:"), "{e:?}");
    }

    #[test]
    fn a_preset_and_the_same_options_by_hand_hash_the_same() {
        let a =
            settle_ok("settings for lanternfall\nplayer \"Mira\"\npreset relaxed\n", &presets());
        let b = settle_ok(
            "settings for lanternfall\nplayer \"Ana\"\ntraps = a_few\nhint_count = 8\nlamp_color = teal\n",
            &presets(),
        );
        assert_ne!(a.canonical, b.canonical);
        assert_eq!(a.hash, b.hash);
        let c = settle_ok("settings for lanternfall\ntraps = a_few\nhint_count = 9\n", &presets());
        assert_ne!(a.hash, c.hash);
        // Named numbers hash as their integers.
        let n = settle_ok("settings for lanternfall\nbombs = normal\n", &presets());
        let i = settle_ok("settings for lanternfall\nbombs = 50\n", &presets());
        assert_eq!(n.hash, i.hash);
    }

    #[test]
    fn legacy_is_hashed_only_for_sidecar_worlds() {
        let v = json!({"format": "settings/1", "legacy": {"a": 1}, "player": "x", "presets": ["p"], "looks": {}});
        let w = json!({"format": "settings/1", "legacy": {"a": 2}});
        assert_eq!(hash(&v, false), hash(&w, false));
        assert_ne!(hash(&v, true), hash(&w, true));
        assert_eq!(hash(&v, false), hash(&json!({"format": "settings/1"}), false));
    }

    #[test]
    fn draw_free_blocks_fold_and_the_rest_stay_rules() {
        let s = settle_ok(
            "settings for lanternfall\ngoal = light_every_lamp\nwhen goal = light_every_lamp {\n  hint_count = 10\n  keep_home += [lamp_oil, rope]\n}\nwhen goal = find_the_crown {\n  hint_count = 1\n}\nwith 30% {\n  bombs = 5\n}\nwhen goal = light_every_lamp {\n  keep_home -= [rope]\n}\n",
            &presets(),
        );
        assert_eq!(s.json["options"]["hint_count"], json!(10));
        assert_eq!(s.json["options"]["keep_home"], json!(["lamp_oil", "rope"]));
        assert_eq!(
            s.json["rules"],
            json!([
                {"when": [], "chance": 30, "set": {"bombs": 5}, "add": {}, "remove": {}},
                {"when": [["goal", "light_every_lamp"]], "chance": 100, "set": {}, "add": {}, "remove": {"keep_home": ["rope"]}}
            ])
        );
        // A block that tests a surprise depends on a draw.
        let s = settle_ok(
            "settings for lanternfall\ngoal = any\nwhen traps = none and goal = light_every_lamp {\n  hint_count = 10\n}\n",
            &presets(),
        );
        assert_eq!(
            s.json["rules"][0]["when"],
            json!([["goal", "light_every_lamp"], ["traps", "none"]])
        );
        assert_eq!(s.json["options"]["hint_count"], json!(5));
    }

    #[test]
    fn a_later_layers_plain_line_wins_over_an_earlier_layers_block() {
        let mut m = presets().0;
        m.insert(
            "blocky".to_owned(),
            "preset blocky for lanternfall\nwith 50% {\n  traps = lots\n  bombs = 7\n}\n"
                .to_owned(),
        );
        let s = settle_ok("settings for lanternfall\npreset blocky\ntraps = none\n", &Map(m));
        assert_eq!(s.json["options"]["traps"], json!("none"));
        assert_eq!(
            s.json["rules"],
            json!([{"when": [], "chance": 50, "set": {"bombs": 7}, "add": {}, "remove": {}}])
        );
    }

    #[test]
    fn cross_block_rules_are_held() {
        let k = |body: &str| keys(&alloc::format!("settings for lanternfall\n{body}"));
        assert_eq!(
            k(
                "when goal = light_every_lamp {\n  traps = lots\n}\nwhen traps = lots {\n  goal = find_the_crown\n}\n"
            ),
            ["settings.block"]
        );
        assert_eq!(
            k(
                "extra_tools = some_of(1, rope, boots)\nwhen goal = light_every_lamp {\n  extra_tools += [lamp_oil]\n}\n"
            ),
            ["settings.block_on_surprise"]
        );
        assert_eq!(
            k("with 50% {\n  extra_tools = any\n}\nwith 50% {\n  extra_tools += [rope]\n}\n"),
            ["settings.block_on_surprise"]
        );
        let e = errs(
            "settings for lanternfall\nwhen goal = light_every_lamp {\n  traps = lots\n}\nwhen traps = lots {\n  hint_count = 1\n}\nwith 5% {\n  goal = find_the_crown\n}\n",
            &presets(),
        );
        assert_eq!(
            e,
            [
                "mira.randoset:9:3: This block changes 'Goal', which the block on line 2 already used to decide.\n  help: move this change into the block on line 2, or remove it"
            ]
        );
    }

    #[test]
    fn cosmetic_blocks_and_options_go_to_looks() {
        let s = settle_ok(
            "settings for lanternfall\nmusic = any\nwhen lamp_color = amber {\n  music = epic\n}\n",
            &presets(),
        );
        assert_eq!(s.json["looks"]["surprise"]["music"], json!({"any": {}}));
        assert_eq!(s.json["looks"]["rules"].as_array().unwrap().len(), 1);
        assert_eq!(s.json["rules"], json!([]));
        let plain = settle_ok("settings for lanternfall\n", &presets());
        assert_eq!(s.hash, plain.hash);
    }

    #[test]
    fn placements_across_the_file_are_checked() {
        let k = |body: &str| keys(&alloc::format!("settings for lanternfall\n{body}"));
        assert_eq!(k("send_away = [rope]\nplace rope at p1\n"), ["settings.placement"]);
        assert_eq!(k("no_important_at = [p1]\nimportant_at = [p1, p2]\n"), ["settings.placement"]);
        assert_eq!(k("place rope at p1\nplace boots at p1\n"), ["settings.placement"]);
        assert_eq!(k("place rope at p1\nplace boots at p1 if_possible\n"), Vec::<String>::new());
        assert_eq!(
            k("place potion x 2 at [p1, p2]\nplace potion x 2 at [p3, p4]\n"),
            ["settings.placement"]
        );
        assert_eq!(
            k("place potion x 2 at [p1, p2]\nplace potion x 2 at [p3, p4] copy\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            k("start_with = { from_shuffle = [potion x 2] }\nplace potion x 2 at [p1, p2]\n"),
            ["settings.placement"]
        );
        assert_eq!(
            k("place potion x all at every_place\nplace potion at p1\n"),
            ["settings.placement"]
        );
        assert_eq!(
            k("connect cave_mouth to mill_door\nconnect well_door to mill_door\n"),
            ["settings.placement"]
        );
        assert_eq!(
            k("pin stone_golem at mill_arena in bosses\npin ash_wyrm at mill_arena in bosses\n"),
            ["settings.placement"]
        );
        let s = settle_ok(
            "settings for lanternfall\nplace pickaxe at mine_cart\nconnect cave_mouth to mill_door one_way\npin stone_golem at mill_arena in bosses\n",
            &presets(),
        );
        assert_eq!(s.json["place"][0]["places"], json!(["mine_cart"]));
        assert_eq!(s.json["connect"][0]["dir"], json!("one_way"));
        assert_eq!(s.json["pin"][0]["slot"], json!("mill_arena"));
        let _ = vec![0];
    }
}
