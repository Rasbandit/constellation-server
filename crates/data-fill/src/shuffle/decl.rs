//! What a world declares about its shuffles. The manifest is read once, when the world is
//! compiled ([`Decl::of`]), into the form a compiled world also stores (its shuffle section,
//! [`Decl::stored`]); a load checks that form ([`Decl::load`]) without replaying the manifest.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use spec_rng::SpecRng;

use super::draw::assign;
use super::{LAYOUT, R};
use crate::section::{arr, s};

/// Names no shuffle may take: they key the random streams of the layout solve and of a
/// world's own fill (doc 17 section 4.3).
const RESERVED: [&str; 2] = [LAYOUT, "own_fill"];

/// Top-level fields of the design this slice does not build: refused in every manifest, so a
/// world never loads with an invariant it asked for left unjudged.
const LATER: [&str; 2] = ["require", "kinds"];

fn obj<'a>(v: &'a Value, path: &str) -> R<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| format!("{path}: expected an object"))
}

/// The always-true rule, as a manifest writes it.
fn truth() -> Value {
    json!({"true": {}})
}

/// A name of a shuffle's vocabulary (slot, thing, group, exit, door), bounded.
fn key<'a>(v: &'a Value, path: &str) -> R<&'a str> {
    let k = s(v, path)?;
    let most = config::MANIFEST_NAME_MAX_BYTES;
    if k.is_empty() || k.len() > most || k.chars().any(char::is_control) {
        return Err(format!("{path}: a name is 1 to {most} bytes without control characters"));
    }
    Ok(k)
}

/// An error text cut to [`config::SHUFFLE_ERROR_MAX_BYTES`]: it may quote a name whose length
/// nothing has bounded yet.
pub(super) fn clip(mut e: String) -> String {
    let mut cut = config::SHUFFLE_ERROR_MAX_BYTES;
    if e.len() > cut {
        while !e.is_char_boundary(cut) {
            cut -= 1;
        }
        e.truncate(cut);
        e.push_str("...");
    }
    e
}

/// Every field of `o` is in `known`, or is one of `later` (fields of the design this slice
/// does not build) carrying its default value.
fn fields(o: &Map<String, Value>, path: &str, known: &[&str], later: &[(&str, Value)]) -> R<()> {
    for (k, v) in o {
        match later.iter().find(|(l, _)| l == k) {
            Some((_, default)) if v == default => {}
            Some(_) => return Err(format!("{path}: `{k}` is not supported yet")),
            None if known.contains(&k.as_str()) => {}
            None => return Err(format!("{path}: unknown field {k:?}")),
        }
    }
    Ok(())
}

/// A keyed exit of the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Exit {
    pub key: String,
    pub region: String,
    /// Position among its region's exits.
    pub index: usize,
    pub door: Option<String>,
    #[serde(skip)]
    group: Option<String>,
    /// The other side of its door.
    pub partner: Option<usize>,
}

/// One thing of a shuffle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Thing {
    pub name: String,
    #[serde(skip)]
    group: Option<String>,
    /// The rule a slot that got this thing reads (`none` shuffles; the compiler's only).
    #[serde(skip)]
    pub rule: Option<Value>,
}

/// One declared shuffle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Shuffle {
    pub name: String,
    /// Moves exits (check level `layout`); otherwise check level `none`.
    pub layout: bool,
    pub(super) coupled: bool,
    pub slots: Vec<String>,
    pub things: Vec<Thing>,
    /// The permission table as bits: a row per slot, a bit per thing.
    allowed: Vec<u64>,
    /// Per slot of a `layout` shuffle: its exit.
    pub(super) exits: Vec<usize>,
    /// Per slot of a `none` shuffle: the option that holds the index of its thing this seed,
    /// which is also the name of the def a rule-position read of the slot refers to.
    pub keys: Vec<String>,
}

/// A world's shuffle declarations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Decl {
    /// In key order.
    pub shuffles: Vec<Shuffle>,
    /// The `none` shuffles in run order: each after every shuffle its rules read, ties by key.
    pub(super) order: Vec<usize>,
    pub(super) exits: Vec<Exit>,
}

/// Every `{"at": [shuffle, slot]}` read in `v`.
fn ats<'a>(v: &'a Value, out: &mut Vec<(&'a str, &'a str)>) {
    match v {
        Value::Object(o) => match at_of(v) {
            Some([Value::String(shuffle), Value::String(slot)]) => out.push((shuffle, slot)),
            _ => o.values().for_each(|x| ats(x, out)),
        },
        Value::Array(a) => a.iter().for_each(|x| ats(x, out)),
        _ => {}
    }
}

/// The operands of an `{"at": [..]}` node, when `v` is one (a roll's `at` is a string beside
/// its `x.roll`, never alone).
fn at_of(v: &Value) -> Option<&[Value]> {
    let o = v.as_object()?;
    if o.len() != 1 {
        return None;
    }
    o.get("at")?.as_array().map(Vec::as_slice)
}

/// One side of a permission table: its members by name and by group.
struct Side<'a> {
    index: HashMap<&'a str, usize>,
    groups: HashMap<&'a str, Vec<usize>>,
    len: usize,
}

impl<'a> Side<'a> {
    fn of(members: &[(&'a str, Option<&'a str>)]) -> Self {
        let mut side = Side { index: HashMap::new(), groups: HashMap::new(), len: members.len() };
        for (k, (name, group)) in members.iter().enumerate() {
            side.index.insert(name, k);
            group.iter().for_each(|g| side.groups.entry(g).or_default().push(k));
        }
        side
    }

    /// The members a row selector names: `{"names": [...]}`, `{"groups": [...]}` or both.
    fn select(&self, v: &Value, path: &str) -> R<Vec<bool>> {
        let o = obj(v, path)?;
        fields(o, path, &["groups", "names"], &[])?;
        let list = |field: &str| match o.get(field) {
            Some(x) => arr(x, path).map(Vec::as_slice),
            None => Ok(&[][..]),
        };
        let (names, groups) = (list("names")?, list("groups")?);
        let most = config::SHUFFLE_MAX_SLOTS;
        if names.len() + groups.len() > most {
            return Err(format!("{path}: a selector lists at most {most} names and groups"));
        }
        let mut out = vec![false; self.len];
        let mut mark = |members: &[usize]| {
            members.iter().for_each(|k| out.get_mut(*k).into_iter().for_each(|x| *x = true));
        };
        for k in names {
            let k = s(k, path)?;
            mark(&[*self.index.get(k).ok_or_else(|| format!("{path}: unknown name {k:?}"))?]);
        }
        for g in groups {
            let g = s(g, path)?;
            mark(self.groups.get(g).ok_or_else(|| format!("{path}: unknown group {g:?}"))?);
        }
        Ok(out)
    }
}

/// A constant rule or a JSON boolean.
fn constant(v: &Value, path: &str) -> R<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        x if *x == truth() => Ok(true),
        x if *x == json!({"false": {}}) => Ok(false),
        _ => Err(format!("{path}: a rule here is not supported yet (true or false)")),
    }
}

/// The keyed exits of manifest `m`. Without shuffles an exit may not carry the fields a
/// shuffle reads: they would be ignored.
fn exits_of(m: &Value, shuffled: bool) -> R<Vec<Exit>> {
    fn list(v: Option<&Value>) -> &[Value] {
        v.and_then(Value::as_array).map_or(&[][..], Vec::as_slice)
    }
    let mut exits = Vec::new();
    for r in list(m.get("regions")) {
        for (index, e) in list(r.get("exits")).iter().enumerate() {
            let at = format!("regions[{}].exits[{index}]", r["key"]);
            let has = |f: &str| e.get(f).is_some_and(|x| !x.is_null());
            if !has("key") {
                if has("door") || has("group") {
                    return Err(format!("{at}: an exit with a door or a group needs a key"));
                }
                continue;
            }
            if !shuffled {
                return Err(format!(
                    "{at}: exit key {} is for a shuffle, and the world declares no shuffle",
                    e["key"]
                ));
            }
            let optional = |f: &str| match e.get(f) {
                None | Some(Value::Null) => Ok(None),
                Some(x) => key(x, &format!("{at}.{f}")).map(|k| Some(k.to_owned())),
            };
            exits.push(Exit {
                key: key(&e["key"], &format!("{at}.key"))?.to_owned(),
                region: s(&r["key"], &at)?.to_owned(),
                index,
                door: optional("door")?,
                group: optional("group")?,
                partner: None,
            });
        }
    }
    Ok(exits)
}

impl Shuffle {
    /// One shuffle at `path`; `left` is how many more slots the world may declare.
    fn parse(
        name: &str,
        v: &Value,
        path: &str,
        exits: &[Exit],
        by_key: &HashMap<&str, usize>,
        left: &mut usize,
    ) -> R<Shuffle> {
        let o = obj(v, path)?;
        let later = [
            ("after", json!([])),
            ("kind", Value::Null),
            ("pin", json!([])),
            ("pool", Value::Null),
            ("scope", json!("seed")),
            ("vanilla", Value::Null),
            ("when", truth()),
        ];
        fields(o, path, &["allow", "check", "coupled", "slots", "things"], &later)?;
        let sp = format!("{path}.slots");
        let so = obj(o.get("slots").unwrap_or(&Value::Null), &sp)?;
        fields(so, &sp, &["names", "type"], &[])?;
        let field = |f: &str| so.get(f).unwrap_or(&Value::Null);
        let layout = match s(field("type"), &format!("{sp}.type"))? {
            "exit" => true,
            "name" => false,
            x => return Err(format!("{sp}.type: {x:?} is not supported yet (exit or name)")),
        };
        let names = arr(field("names"), &format!("{sp}.names"))?;
        if names.is_empty() || names.len() > *left {
            return Err(format!(
                "{sp}: {} slots; a world's shuffles have 1 to at most {} slots together",
                names.len(),
                config::SHUFFLE_MAX_SLOTS
            ));
        }
        *left -= names.len();
        let mut slots: Vec<&str> = Vec::with_capacity(names.len());
        for k in names {
            slots.push(key(k, &sp)?);
        }
        let mut sorted = slots.clone();
        sorted.sort_unstable();
        if let Some(w) = sorted.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!("{sp}: {:?} is listed twice", w[0]));
        }
        let cp = format!("{path}.check");
        match (o.get("check").map(|c| s(c, &cp)).transpose()?, layout) {
            (None | Some("layout"), true) | (None | Some("none"), false) => {}
            (Some("none"), true) => {
                return Err(format!("{cp}: a shuffle that moves exits is `layout`"));
            }
            (Some("layout"), false) => {
                return Err(format!("{cp}: a shuffle of names it lists itself is `none`"));
            }
            (Some(x), _) => return Err(format!("{cp}: {x:?} is not supported yet (none, layout)")),
        }
        let coupled =
            o.get("coupled").map(|c| constant(c, &format!("{path}.coupled"))).transpose()?;
        if coupled == Some(true) && !layout {
            return Err(format!("{path}.coupled: only a shuffle that moves exits has doors"));
        }
        let tp = format!("{path}.things");
        let (mut things, mut slot_exits) = (Vec::new(), Vec::new());
        if layout {
            for k in &slots {
                let e = *by_key.get(k).ok_or_else(|| {
                    format!("{sp}: unknown exit {k:?} (an exit is named by its key)")
                })?;
                let Some(exit) = exits.get(e) else { continue };
                slot_exits.push(e);
                things.push(Thing { name: (*k).to_owned(), group: exit.group.clone(), rule: None });
            }
            // Every exit is a slot and a thing, so the things are the slots.
            let same = json!({"names": slots, "type": "exit"});
            if o.get("things").is_some_and(|t| *t != same) {
                return Err(format!("{tp}: the things of an exit shuffle are its slots for now"));
            }
        } else {
            things = Self::things(o.get("things").unwrap_or(&Value::Null), &tp)?;
        }
        // The table starts full and rows only take pairs away (doc 17 section 5.2).
        let (n, m) = (slots.len(), things.len());
        let mut allowed = vec![true; n * m];
        let of_things: Vec<(&str, Option<&str>)> =
            things.iter().map(|t| (t.name.as_str(), t.group.as_deref())).collect();
        // Only an exit has a group as a slot; a name slot is selected by name.
        let of_slots: Vec<(&str, Option<&str>)> =
            if layout { of_things.clone() } else { slots.iter().map(|k| (*k, None)).collect() };
        let (slot_side, thing_side) = (Side::of(&of_slots), Side::of(&of_things));
        let ap = format!("{path}.allow");
        let rows = o.get("allow").map(|a| arr(a, &ap)).transpose()?.map_or(&[][..], Vec::as_slice);
        if rows.len() > config::SHUFFLE_MAX_ROWS {
            return Err(format!("{ap}: {} rows, at most {}", rows.len(), config::SHUFFLE_MAX_ROWS));
        }
        for (k, row) in rows.iter().enumerate() {
            let rp = format!("{ap}[{k}]");
            let ro = obj(row, &rp)?;
            let forms = [
                ("slots", "only"),
                ("slots", "deny"),
                ("things", "only_in"),
                ("things", "deny_in"),
            ];
            let Some((subject, verb)) =
                forms.into_iter().find(|(a, b)| ro.contains_key(*a) && ro.contains_key(*b))
            else {
                return Err(format!(
                    "{rp}: a row is slots with only or deny, or things with only_in or deny_in"
                ));
            };
            fields(ro, &rp, &[subject, verb], &[("again", json!(false)), ("when", truth())])?;
            let chosen = |field: &str, side: &Side<'_>| {
                side.select(ro.get(field).unwrap_or(&Value::Null), &format!("{rp}.{field}"))
            };
            let (in_slots, in_things) = match subject {
                "slots" => (chosen(subject, &slot_side)?, chosen(verb, &thing_side)?),
                _ => (chosen(verb, &slot_side)?, chosen(subject, &thing_side)?),
            };
            for (cell, (i, j)) in
                allowed.iter_mut().zip((0..n).flat_map(|i| (0..m).map(move |j| (i, j))))
            {
                *cell &= match verb {
                    "only" => !in_slots[i] || in_things[j],
                    "only_in" => !in_things[j] || in_slots[i],
                    _ => !(in_slots[i] && in_things[j]),
                };
            }
        }
        if layout {
            // A one-way exit trades only with one-way exits, a door side only with door sides.
            let sided: Vec<bool> = slot_exits
                .iter()
                .map(|e| exits.get(*e).is_some_and(|x| x.partner.is_some()))
                .collect();
            for (cell, (i, j)) in
                allowed.iter_mut().zip((0..n).flat_map(|i| (0..m).map(move |j| (i, j))))
            {
                *cell &= sided.get(i) == sided.get(j);
            }
        }
        // The draw finds an assignment whenever one exists, so none here is none ever.
        let cell = |i: usize, j: usize| allowed.get(i * m + j).copied().unwrap_or(false);
        if assign(n, m, &cell, &mut SpecRng::from_u64(0)).is_none() {
            return Err(format!(
                "{path}: no assignment of things to slots satisfies the permission table"
            ));
        }
        let mut bits = vec![0u64; (n * m).div_ceil(64)];
        for (k, _) in allowed.iter().enumerate().filter(|x| *x.1) {
            bits[k / 64] |= 1 << (k % 64);
        }
        let keys = if layout {
            Vec::new()
        } else {
            slots.iter().map(|slot| format!("at.{name}.{slot}")).collect()
        };
        Ok(Shuffle {
            name: name.to_owned(),
            layout,
            coupled: coupled.unwrap_or(false),
            slots: slots.into_iter().map(str::to_owned).collect(),
            things,
            allowed: bits,
            exits: slot_exits,
            keys,
        })
    }

    /// The things of a `none` shuffle: a list of names, or a map from name to its fields.
    fn things(v: &Value, path: &str) -> R<Vec<Thing>> {
        let later = [
            ("count", json!(1)),
            ("fill", json!(false)),
            ("grants", Value::Null),
            ("item", Value::Null),
            ("min", json!(0)),
            ("to", Value::Null),
            ("value", Value::Null),
            ("weight", json!(1)),
            ("when", truth()),
        ];
        let most = config::SHUFFLE_MAX_SLOTS;
        let count = v.as_array().map(Vec::len).or_else(|| v.as_object().map(Map::len));
        if count.is_some_and(|n| n == 0 || n > most) {
            return Err(format!("{path}: a shuffle has 1 to at most {most} things"));
        }
        let mut out = Vec::new();
        match v {
            Value::Array(names) => {
                for k in names {
                    out.push(Thing { name: key(k, path)?.to_owned(), group: None, rule: None });
                }
            }
            Value::Object(map) => {
                for (k, t) in map {
                    let p = format!("{path}.{k}");
                    key(&json!(k), &p)?;
                    let o = obj(t, &p)?;
                    fields(o, &p, &["group", "rule"], &later)?;
                    let group = match o.get("group") {
                        None | Some(Value::Null) => None,
                        Some(g) => Some(key(g, &format!("{p}.group"))?.to_owned()),
                    };
                    out.push(Thing { name: k.clone(), group, rule: o.get("rule").cloned() });
                }
            }
            _ => return Err(format!("{path}: expected a list of names or a map of things")),
        }
        let mut names: Vec<&str> = out.iter().map(|t| t.name.as_str()).collect();
        names.sort_unstable();
        if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!("{path}: {:?} is listed twice", w[0]));
        }
        Ok(out)
    }

    /// Whether the permission table lets `thing` go to `slot`: the one reading of the table.
    pub(super) fn allows(&self, slot: usize, thing: usize) -> bool {
        let k = slot * self.things.len() + thing;
        thing < self.things.len()
            && self.allowed.get(k / 64).is_some_and(|w| w >> (k % 64) & 1 == 1)
    }
}

impl Decl {
    /// The declarations of manifest `m`, checked; `None` when it declares no shuffle.
    pub(crate) fn of(m: &Value) -> R<Option<Decl>> {
        Self::read(m).map_err(clip)
    }

    fn read(m: &Value) -> R<Option<Decl>> {
        if let Some(field) = LATER.into_iter().find(|f| m.get(f).is_some()) {
            return Err(format!(
                "`{field}` is not supported yet (design doc 17, sections 5.3 and 7)"
            ));
        }
        let raw = match m.get("shuffles") {
            Some(v) => Some(obj(v, "shuffles")?).filter(|o| !o.is_empty()),
            None => None,
        };
        let mut exits = exits_of(m, raw.is_some())?;
        let Some(raw) = raw else { return Ok(None) };
        let mut doors: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (k, e) in exits.iter().enumerate() {
            e.door.iter().for_each(|d| doors.entry(d).or_default().push(k));
        }
        let mut pairs = Vec::new();
        for (d, sides) in &doors {
            match sides.as_slice() {
                [_] => {}
                [a, b] => pairs.push((*a, *b)),
                more => {
                    return Err(format!(
                        "door {d:?} has {} exits: a door has one or two sides",
                        more.len()
                    ));
                }
            }
        }
        for (a, b) in pairs {
            exits.get_mut(a).into_iter().for_each(|e| e.partner = Some(b));
            exits.get_mut(b).into_iter().for_each(|e| e.partner = Some(a));
        }
        let mut by_key = HashMap::new();
        for (k, e) in exits.iter().enumerate() {
            if by_key.insert(e.key.as_str(), k).is_some() {
                return Err(format!("exit key {:?} is used twice", e.key));
            }
        }
        if raw.len() > config::SHUFFLES_MAX {
            return Err(format!(
                "shuffles: {} declared, at most {}",
                raw.len(),
                config::SHUFFLES_MAX
            ));
        }
        let mut shuffles: Vec<Shuffle> = Vec::new();
        // Exit to the name of the shuffle that lists it.
        let mut taken: HashMap<usize, &str> = HashMap::new();
        let mut left = config::SHUFFLE_MAX_SLOTS;
        for (name, sv) in raw {
            let p = format!("shuffles.{name}");
            let most = config::SHUFFLE_NAME_MAX_BYTES;
            if name.is_empty() || name.len() > most || name.contains('.') {
                return Err(format!("{p}: a shuffle name is 1 to {most} bytes without a dot"));
            }
            key(&json!(name), &p)?;
            if RESERVED.contains(&name.as_str()) {
                return Err(format!("{p}: the name is reserved"));
            }
            let sh = Shuffle::parse(name, sv, &p, &exits, &by_key, &mut left)?;
            for (e, k) in sh.exits.iter().zip(&sh.slots) {
                if let Some(other) = taken.insert(*e, name) {
                    return Err(format!(
                        "{p}.slots: exit {k:?} is in two shuffles ({other} and {name})"
                    ));
                }
            }
            shuffles.push(sh);
        }
        // A coupled shuffle writes the far side of each of its doors itself.
        for sh in shuffles.iter().filter(|sh| sh.coupled) {
            for (e, k) in sh.exits.iter().zip(&sh.slots) {
                let far = exits.get(*e).and_then(|x| x.partner).filter(|p| taken.contains_key(p));
                if let Some(far) = far.and_then(|p| exits.get(p)) {
                    return Err(format!(
                        "shuffles.{}: both sides of a door are listed ({k:?} and {:?}); a coupled \
                         shuffle lists one side and the other side follows",
                        sh.name, far.key
                    ));
                }
            }
        }
        let order = Self::order(&shuffles)?;
        Ok(Some(Decl { shuffles, order, exits }))
    }

    /// The run order of the `none` shuffles (doc 17 section 4.3): a shuffle runs after every
    /// shuffle its things' rules read, ties by key. The layout is one node after all of them:
    /// its check reads every rule, and nothing may read it yet. Reads are the only edges this
    /// slice can have; the writes a payload, a `when` or a pin makes need edges of their own
    /// when they are built (see #210).
    fn order(shuffles: &[Shuffle]) -> R<Vec<usize>> {
        // Per shuffle: the shuffles it reads, and where the read is written.
        let mut reads: Vec<Vec<(usize, String)>> = vec![Vec::new(); shuffles.len()];
        for (sh, out) in shuffles.iter().zip(&mut reads) {
            for t in &sh.things {
                let mut found = Vec::new();
                t.rule.iter().for_each(|rule| ats(rule, &mut found));
                for (name, slot) in found {
                    let at = format!("shuffles.{}.things.{}.rule", sh.name, t.name);
                    let j = shuffles
                        .iter()
                        .position(|x| x.name == name)
                        .ok_or_else(|| format!("{at}: unknown shuffle {name:?}"))?;
                    if shuffles.get(j).is_some_and(|x| x.layout) {
                        return Err(layout_read(&at, name));
                    }
                    out.push((j, format!("{at}: at.{name}.{slot}")));
                }
            }
        }
        let mut done = vec![false; shuffles.len()];
        let mut order = Vec::new();
        let ready = |i: usize, done: &[bool]| {
            !done[i] && reads[i].iter().all(|(j, _)| done.get(*j).copied().unwrap_or(false))
        };
        while let Some(i) = (0..shuffles.len()).find(|i| ready(*i, &done)) {
            done[i] = true;
            if !shuffles[i].layout {
                order.push(i);
            }
        }
        let Some(start) = done.iter().position(|d| !d) else { return Ok(order) };
        // What is left reads something that is left: walk the reads until a shuffle repeats.
        let next = |i: usize| reads[i].iter().find(|(j, _)| !done[*j]);
        let (mut walk, mut at) = (Vec::new(), start);
        while !walk.contains(&at) {
            walk.push(at);
            at = next(at).map_or(at, |x| x.0);
        }
        let from = walk.iter().position(|i| *i == at).unwrap_or(0);
        let mut msg = "shuffles depend on each other in a cycle:".to_owned();
        for i in &walk[from..] {
            if let Some((j, place)) = next(*i) {
                msg += &format!("\n  {} reads {} ({place})", shuffles[*i].name, shuffles[*j].name);
            }
        }
        Err(msg + "\nno order exists in which each shuffle sees the results it reads")
    }

    /// The form a compiled world stores: its shuffle section.
    pub(crate) fn stored(&self) -> R<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }

    /// A stored shuffle section, checked: every size bounded and every index in range, so
    /// nothing after this trusts the bytes. The manifest is not replayed.
    pub(super) fn load(section: &[u8]) -> R<Decl> {
        let d: Decl =
            serde_json::from_slice(section).map_err(|e| format!("shuffle section: {e}"))?;
        let bad = |what: &str| Err(format!("shuffle section: {what}"));
        let most = config::SHUFFLE_MAX_SLOTS;
        if d.shuffles.is_empty() || d.shuffles.len() > config::SHUFFLES_MAX {
            return bad("shuffle count");
        }
        let (mut slots, mut listed) = (0usize, vec![false; d.exits.len()]);
        for sh in &d.shuffles {
            let (n, m) = (sh.slots.len(), sh.things.len());
            slots += n;
            let name = sh.name.len() <= config::SHUFFLE_NAME_MAX_BYTES && !sh.name.is_empty();
            let table = n > 0 && m > 0 && m <= most && sh.allowed.len() == (n * m).div_ceil(64);
            let kind = if sh.layout {
                sh.exits.len() == n && m == n && sh.keys.is_empty()
            } else {
                sh.keys.len() == n && sh.exits.is_empty() && !sh.coupled
            };
            if !name || !table || !kind || slots > most {
                return bad("a shuffle's sizes");
            }
            for e in &sh.exits {
                match listed.get_mut(*e) {
                    Some(l) if !*l => *l = true,
                    _ => return bad("a shuffle's exits"),
                }
            }
        }
        let ascending = d.shuffles.windows(2).all(|w| w[0].name < w[1].name);
        let mut seen = vec![false; d.shuffles.len()];
        for i in &d.order {
            match (d.shuffles.get(*i), seen.get_mut(*i)) {
                (Some(sh), Some(s)) if !sh.layout && !*s => *s = true,
                _ => return bad("the run order"),
            }
        }
        let ordered = seen.iter().zip(&d.shuffles).all(|(s, sh)| *s != sh.layout);
        if !ordered || !ascending {
            return bad("the run order or a repeated name");
        }
        for (k, e) in d.exits.iter().enumerate() {
            let back = e.partner.map(|p| d.exits.get(p).and_then(|x| x.partner));
            if back.is_some_and(|b| b != Some(k) || e.partner == Some(k)) {
                return bad("a door's sides");
            }
        }
        let coupled = d.shuffles.iter().filter(|sh| sh.coupled).flat_map(|sh| &sh.exits);
        let far = |e: &usize| d.exits.get(*e).and_then(|x| x.partner);
        if coupled.filter_map(far).any(|p| listed.get(p).copied().unwrap_or(true)) {
            return bad("both sides of a coupled door");
        }
        Ok(d)
    }

    /// The `none` shuffle an `{"at": [shuffle, slot]}` read names, and the option that holds
    /// the slot's thing this seed.
    fn at(&self, operands: &[Value], path: &str) -> R<(&Shuffle, &str)> {
        let [Value::String(name), Value::String(slot)] = operands else {
            return Err(format!("{path}: `at` takes [shuffle, slot]"));
        };
        let sh = self
            .shuffles
            .iter()
            .find(|sh| sh.name == *name)
            .ok_or_else(|| format!("{path}: unknown shuffle {name:?}"))?;
        if sh.layout {
            return Err(layout_read(path, name));
        }
        let option = sh.slots.iter().position(|x| x == slot).and_then(|j| sh.keys.get(j));
        let option =
            option.ok_or_else(|| format!("{path}: shuffle {name:?} has no slot {slot:?}"))?;
        Ok((sh, option))
    }

    /// The rule of the thing a shuffle put in a slot, for every thing it could be (doc 17
    /// section 5.4): "this thing is there, and its rule", which the per-seed fold cuts down
    /// to the one that is there.
    fn union(sh: &Shuffle, option: &str, path: &str) -> R<Value> {
        let most = config::SHUFFLE_MAX_READ_THINGS;
        if sh.things.len() > most {
            return Err(format!(
                "{path}: shuffle {:?} is read as a rule and has {} things, at most {most}",
                sh.name,
                sh.things.len()
            ));
        }
        let mut parts = Vec::with_capacity(sh.things.len());
        for (j, t) in sh.things.iter().enumerate() {
            let rule = t.rule.as_ref().ok_or_else(|| {
                format!(
                    "{path}: shuffle {:?} is read as a rule, and its thing {:?} has no rule",
                    sh.name, t.name
                )
            })?;
            parts.push(json!({"all": [{"eq": [{"opt": option}, j]}, rule]}));
        }
        Ok(json!({"any": parts}))
    }

    /// The defs the compiler adds for manifest `m`: one per slot some rule reads, holding
    /// [`Decl::union`], so every read site refers to one shared copy. Named as the slot's
    /// option is. A read the compiler will refuse gets no def.
    pub(crate) fn defs(&self, m: &Value) -> Map<String, Value> {
        let mut found = Vec::new();
        ats(m, &mut found);
        let mut out = Map::new();
        for (shuffle, slot) in found {
            let operands = [json!(shuffle), json!(slot)];
            let Ok((sh, option)) = self.at(&operands, "") else { continue };
            if !out.contains_key(option)
                && let Ok(rule) = Self::union(sh, option, "")
            {
                out.insert(option.to_owned(), rule);
            }
        }
        out
    }

    /// The def a rule-position read `{"at": operands}` refers to ([`Decl::defs`]).
    pub(crate) fn rule_def(&self, operands: &Value, path: &str) -> R<&str> {
        let read = |path: &str| {
            let (sh, option) = self.at(arr(operands, path)?, path)?;
            Self::union(sh, option, path).map(|_| option)
        };
        read(path).map_err(clip)
    }

    /// What a comparison stands for when one side is an `at` read (the name test of doc 17
    /// section 5.4): a test of the slot's option against the named thing's index. `None`
    /// when neither side is one.
    pub(crate) fn name_test(&self, op: &str, x: &Value, y: &Value, path: &str) -> R<Option<Value>> {
        let (at, other) = match (at_of(x), at_of(y)) {
            (None, None) => return Ok(None),
            (Some(at), None) => (at, y),
            (None, Some(at)) => (at, x),
            _ => return Err(format!("{path}: `at` is compared to a thing name, not to an `at`")),
        };
        let test = || {
            let (sh, option) = self.at(at, path)?;
            let Some(thing) = other.as_str() else {
                return Err(format!("{path}: `at` is compared to a thing name"));
            };
            if op != "eq" && op != "ne" {
                return Err(format!("{path}: a thing name is tested with `eq` or `ne`"));
            }
            let j =
                sh.things.iter().position(|t| t.name == thing).ok_or_else(|| {
                    format!("{path}: shuffle {:?} has no thing {thing:?}", sh.name)
                })?;
            Ok(Some(json!({op: [{"opt": option}, j]})))
        };
        test().map_err(clip)
    }

    /// The options the compiler adds: one per slot of every `none` shuffle.
    pub(crate) fn option_keys(&self) -> impl Iterator<Item = &str> {
        self.shuffles.iter().flat_map(|sh| sh.keys.iter().map(String::as_str))
    }

    /// The run order by name: the `none` shuffles, then the layout when a shuffle moves exits.
    pub(super) fn order_names(&self) -> Vec<String> {
        let none =
            self.order.iter().filter_map(|i| self.shuffles.get(*i)).map(|sh| sh.name.clone());
        none.chain(self.shuffles.iter().any(|sh| sh.layout).then(|| LAYOUT.to_owned())).collect()
    }
}

fn layout_read(path: &str, name: &str) -> String {
    format!("{path}: reads the `layout` shuffle {name:?}; a rule cannot read a layout result yet")
}
