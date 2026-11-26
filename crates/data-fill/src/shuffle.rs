//! Generic shuffles, first slice (design doc 17 of the docs repo, decision D-137): a shuffle
//! assigns things to slots under a permission table that rows only narrow, and its result
//! folds into the seed's world before the item fill, which stays the exact fill and does not
//! know a shuffle ran.
//!
//! Two kinds are built. A **`none`** shuffle has slots it names itself and things that may
//! carry a rule; a rule reads the result with `{"at": [shuffle, slot]}`, which the compiler
//! ([`crate::section`]) turns into tests of a per-seed option, so the per-seed fold
//! (`crate::fold`) leaves exactly the assigned thing's rule. A **`layout`** shuffle moves
//! exits: every exit it lists is a slot ("leaving through this exit") and a thing ("arriving
//! where this exit leads in the unshuffled game"). Two exits that name one `door` are its two
//! sides; a coupled shuffle lists one side and the other follows, so a door leads back where
//! it was entered. An exit with no second side is one-way and trades only with one-way exits.
//!
//! Per world and seed, [`stage`] runs the `none` shuffles in the order their reads give (a
//! cycle is a load error), then one joint layout solve over every `layout` shuffle, then the
//! whole-world check: holding every item of the pool, every region and every check that exists
//! is reached and the goal holds. A failed attempt starts all of it again on the next
//! attempt's streams, at most [`config::SHUFFLE_MAX_ATTEMPTS`] times. Streams are keyed by
//! seed, player, shuffle name and attempt, never by position, and are apart from the fill's.
//!
//! The entrance model (typed groups with a table of who may connect to whom, one-way and
//! two-way exits, coupling, judging a layout with every item held, and placing branches before
//! dead ends) is learned from Archipelago's generic entrance randomizer, as the design doc
//! credits it with file and line. This is our own code; nothing is copied from it.
//!
//! Not built here (each is refused at load by name): `when`, pins, counts and repeats,
//! assignment pools, presets, start and item slots, `reach` shuffles and a world's own fill,
//! invariants, and reading a `layout` result.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value, json};
use spec_rng::SpecRng;

use crate::fill::{Instance, Plan};
use crate::room::{Compiled, too_many};
use crate::section::{EXITS, arr, s};
use crate::world::World;

type R<T> = Result<T, String>;

/// Per shuffle, slot to thing, by name: what a seed records of one world's shuffles. For a
/// coupled door both sides are listed, the side that followed too.
pub type Table = BTreeMap<String, BTreeMap<String, String>>;

/// Names no shuffle may take: they key the random streams of the layout solve and of a
/// world's own fill (doc 17 section 4.3).
const RESERVED: [&str; 2] = ["layout", "own_fill"];

/// The stream key of the joint layout solve.
const LAYOUT: &str = "layout";

fn obj<'a>(v: &'a Value, path: &str) -> R<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| format!("{path}: expected an object"))
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
#[derive(Debug, Clone)]
struct Exit {
    key: String,
    region: String,
    /// Position among its region's exits.
    index: usize,
    door: Option<String>,
    group: Option<String>,
    /// The other side of its door.
    partner: Option<usize>,
}

/// One thing of a shuffle.
#[derive(Debug, Clone)]
pub(crate) struct Thing {
    pub name: String,
    group: Option<String>,
    /// The rule a slot that got this thing reads (`none` shuffles).
    pub rule: Option<Value>,
}

/// One declared shuffle.
#[derive(Debug, Clone)]
pub(crate) struct Shuffle {
    pub name: String,
    /// Moves exits (check level `layout`); otherwise check level `none`.
    pub layout: bool,
    coupled: bool,
    pub slots: Vec<String>,
    pub things: Vec<Thing>,
    /// The permission table: a row per slot, a cell per thing.
    allowed: Vec<bool>,
    /// Per slot of a `layout` shuffle: its exit.
    exits: Vec<usize>,
    /// Per slot of a `none` shuffle: the option that holds the index of its thing this seed.
    pub keys: Vec<String>,
}

/// A world's shuffle declarations, checked as far as the text alone allows.
#[derive(Debug, Clone)]
pub(crate) struct Decl {
    /// In key order.
    pub shuffles: Vec<Shuffle>,
    /// The `none` shuffles in run order: each after every shuffle its rules read, ties by key.
    order: Vec<usize>,
    exits: Vec<Exit>,
}

/// The shuffle section of manifest `m` as JSON: its `shuffles` as written and every exit that
/// has a key (`region`, position, `door`, `group`). `None` for a world that declares none.
fn section_value(m: &Value) -> R<Option<Value>> {
    let Some(shuffles) = m.get("shuffles") else { return Ok(None) };
    if obj(shuffles, "shuffles")?.is_empty() {
        return Ok(None);
    }
    let mut exits = Vec::new();
    for r in m["regions"].as_array().into_iter().flatten() {
        for (index, e) in r["exits"].as_array().into_iter().flatten().enumerate() {
            match e.get("key") {
                Some(k) => exits.push(json!({"door": e.get("door"), "group": e.get("group"),
                                             "index": index, "key": k, "region": r["key"]})),
                None if e.get("door").is_some() || e.get("group").is_some() => {
                    return Err(format!(
                        "regions[{}].exits[{index}]: an exit with a door or a group needs a key",
                        r["key"]
                    ));
                }
                None => {}
            }
        }
    }
    Ok(Some(json!({"exits": exits, "shuffles": shuffles})))
}

/// The shuffle section of manifest `m`, checked (`seed_bundle::package::SECTION_SHUFFLES`);
/// `None` for a world that declares no shuffle.
///
/// # Errors
/// A message naming the manifest path of what is malformed or not supported yet.
pub fn section(m: &Value) -> R<Option<Vec<u8>>> {
    let Some(v) = section_value(m)? else { return Ok(None) };
    Decl::parse(&v)?;
    serde_json::to_vec(&v).map(Some).map_err(|e| e.to_string())
}

/// Every `{"at": [shuffle, slot]}` read in rule `v`.
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

/// A row selector, `{"names": [...]}`, `{"groups": [...]}` or both: which of the members
/// (their index by name, and their group) it names.
fn select(
    v: &Value,
    path: &str,
    index: &HashMap<&str, usize>,
    groups: &[Option<&str>],
) -> R<Vec<bool>> {
    let o = obj(v, path)?;
    fields(o, path, &["groups", "names"], &[])?;
    let mut out = vec![false; groups.len()];
    for k in o.get("names").map(|x| arr(x, path)).transpose()?.into_iter().flatten() {
        let k = s(k, path)?;
        out[*index.get(k).ok_or_else(|| format!("{path}: unknown name {k:?}"))?] = true;
    }
    for g in o.get("groups").map(|x| arr(x, path)).transpose()?.into_iter().flatten() {
        let g = s(g, path)?;
        if !groups.contains(&Some(g)) {
            return Err(format!("{path}: unknown group {g:?}"));
        }
        groups.iter().enumerate().filter(|x| *x.1 == Some(g)).for_each(|(i, _)| out[i] = true);
    }
    Ok(out)
}

/// A constant rule or a JSON boolean.
fn constant(v: &Value, path: &str) -> R<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        x if *x == json!({"true": {}}) => Ok(true),
        x if *x == json!({"false": {}}) => Ok(false),
        _ => Err(format!("{path}: only true or false is supported yet")),
    }
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
        let truth = json!({"true": {}});
        let later = [
            ("after", json!([])),
            ("kind", Value::Null),
            ("pin", json!([])),
            ("pool", Value::Null),
            ("scope", json!("seed")),
            ("vanilla", Value::Null),
            ("when", truth.clone()),
        ];
        fields(o, path, &["allow", "check", "coupled", "slots", "things"], &later)?;
        let sp = format!("{path}.slots");
        let so = obj(&v["slots"], &sp)?;
        fields(so, &sp, &["names", "type"], &[])?;
        let layout = match s(&so["type"], &format!("{sp}.type"))? {
            "exit" => true,
            "name" => false,
            x => return Err(format!("{sp}.type: {x:?} is not supported yet (exit or name)")),
        };
        let mut slots = Vec::new();
        let mut slot_index = HashMap::new();
        let names = arr(&so["names"], &format!("{sp}.names"))?;
        if names.is_empty() || names.len() > *left {
            return Err(format!(
                "{sp}: {} slots; a world's shuffles have 1 to at most {} slots together",
                names.len(),
                config::SHUFFLE_MAX_SLOTS
            ));
        }
        *left -= names.len();
        for k in names {
            let k = key(k, &sp)?;
            if slot_index.insert(k, slots.len()).is_some() {
                return Err(format!("{sp}: {k:?} is listed twice"));
            }
            slots.push(k.to_owned());
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
                let e = *by_key.get(k.as_str()).ok_or_else(|| {
                    format!("{sp}: unknown exit {k:?} (an exit is named by its key)")
                })?;
                slot_exits.push(e);
                things.push(Thing { name: k.clone(), group: exits[e].group.clone(), rule: None });
            }
            // Every exit is a slot and a thing, so the things are the slots.
            let same = json!({"names": slots, "type": "exit"});
            if o.get("things").is_some_and(|t| *t != same) {
                return Err(format!("{tp}: the things of an exit shuffle are its slots for now"));
            }
        } else {
            things = Self::things(&o.get("things").cloned().unwrap_or(Value::Null), &tp)?;
        }
        if things.len() > config::SHUFFLE_MAX_SLOTS {
            return Err(format!("{tp}: at most {} things", config::SHUFFLE_MAX_SLOTS));
        }
        // The table starts full and rows only take pairs away (doc 17 section 5.2).
        let (n, m) = (slots.len(), things.len());
        let mut allowed = vec![true; n * m];
        let thing_index: HashMap<&str, usize> =
            things.iter().enumerate().map(|(j, t)| (t.name.as_str(), j)).collect();
        let thing_groups: Vec<Option<&str>> = things.iter().map(|t| t.group.as_deref()).collect();
        // Only an exit has a group as a slot; a name slot is selected by name.
        let slot_groups = if layout { thing_groups.clone() } else { vec![None; n] };
        let rows = o.get("allow").map(|a| arr(a, &format!("{path}.allow"))).transpose()?;
        for (k, row) in rows.into_iter().flatten().enumerate() {
            let rp = format!("{path}.allow[{k}]");
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
            fields(ro, &rp, &[subject, verb], &[("again", json!(false)), ("when", truth.clone())])?;
            let chosen = |field: &str, slot_side: bool| {
                let (index, groups) = if slot_side {
                    (&slot_index, &slot_groups)
                } else {
                    (&thing_index, &thing_groups)
                };
                select(&row[field], &format!("{rp}.{field}"), index, groups)
            };
            let (in_slots, in_things) = match subject {
                "slots" => (chosen(subject, true)?, chosen(verb, false)?),
                _ => (chosen(verb, true)?, chosen(subject, false)?),
            };
            for (i, j) in (0..n).flat_map(|i| (0..m).map(move |j| (i, j))) {
                allowed[i * m + j] &= match verb {
                    "only" => !in_slots[i] || in_things[j],
                    "only_in" => !in_things[j] || in_slots[i],
                    _ => !(in_slots[i] && in_things[j]),
                };
            }
        }
        if layout {
            // A one-way exit trades only with one-way exits, a door side only with door sides.
            let sided: Vec<bool> = slot_exits.iter().map(|e| exits[*e].partner.is_some()).collect();
            for (i, j) in (0..n).flat_map(|i| (0..m).map(move |j| (i, j))) {
                allowed[i * m + j] &= sided[i] == sided[j];
            }
        }
        // The draw finds an assignment whenever one exists, so none here is none ever.
        if assign(n, m, &allowed, &mut SpecRng::from_u64(0)).is_none() {
            return Err(format!(
                "{path}: no assignment of things to slots satisfies the permission table"
            ));
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
            slots,
            things,
            allowed,
            exits: slot_exits,
            keys,
        })
    }

    /// The things of a `none` shuffle: a list of names, or a map from name to its fields.
    fn things(v: &Value, path: &str) -> R<Vec<Thing>> {
        let truth = json!({"true": {}});
        let later = [
            ("count", json!(1)),
            ("fill", json!(false)),
            ("grants", Value::Null),
            ("item", Value::Null),
            ("min", json!(0)),
            ("to", Value::Null),
            ("value", Value::Null),
            ("weight", json!(1)),
            ("when", truth),
        ];
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

    fn allows(&self, slot: usize, thing: usize) -> bool {
        self.allowed.get(slot * self.things.len() + thing).copied().unwrap_or(false)
    }
}

impl Decl {
    /// The declarations of manifest `m`, checked; `None` when it declares no shuffle.
    pub(crate) fn of(m: &Value) -> R<Option<Decl>> {
        section_value(m)?.map(|v| Decl::parse(&v)).transpose()
    }

    /// Check a shuffle section's JSON: the one validation of what a manifest declares, for
    /// the compiler and for the loader of a stored section alike.
    fn parse(v: &Value) -> R<Decl> {
        fields(obj(v, "shuffle section")?, "shuffle section", &["exits", "shuffles"], &[])?;
        let raw = arr(&v["exits"], "shuffle section exits")?;
        if raw.len() > 2 * config::SHUFFLE_MAX_SLOTS {
            return Err(format!(
                "shuffles: {} exits have a key, at most {}",
                raw.len(),
                2 * config::SHUFFLE_MAX_SLOTS
            ));
        }
        let mut exits: Vec<Exit> = Vec::with_capacity(raw.len());
        for e in raw {
            let p = format!("exit {}", e["key"]);
            fields(obj(e, &p)?, &p, &["door", "group", "index", "key", "region"], &[])?;
            let optional = |f: &str| match &e[f] {
                Value::Null => Ok(None),
                x => key(x, &format!("{p}.{f}")).map(|k| Some(k.to_owned())),
            };
            let index = e["index"].as_u64().and_then(|i| usize::try_from(i).ok());
            exits.push(Exit {
                key: key(&e["key"], &p)?.to_owned(),
                region: s(&e["region"], &format!("{p}.region"))?.to_owned(),
                index: index.ok_or_else(|| format!("{p}.index: expected a position"))?,
                door: optional("door")?,
                group: optional("group")?,
                partner: None,
            });
        }
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
            exits[a].partner = Some(b);
            exits[b].partner = Some(a);
        }
        let mut by_key = HashMap::new();
        for (k, e) in exits.iter().enumerate() {
            if by_key.insert(e.key.as_str(), k).is_some() {
                return Err(format!("exit key {:?} is used twice", e.key));
            }
        }
        let raw = obj(&v["shuffles"], "shuffles")?;
        if raw.len() > config::SHUFFLES_MAX {
            return Err(format!(
                "shuffles: {} declared, at most {}",
                raw.len(),
                config::SHUFFLES_MAX
            ));
        }
        let mut shuffles: Vec<Shuffle> = Vec::new();
        // Exit to the shuffle that lists it.
        let mut taken: HashMap<usize, usize> = HashMap::new();
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
            for e in &sh.exits {
                if let Some(other) = taken.insert(*e, shuffles.len()) {
                    return Err(format!(
                        "{p}.slots: exit {:?} is in two shuffles ({} and {name})",
                        exits[*e].key, shuffles[other].name
                    ));
                }
            }
            shuffles.push(sh);
        }
        // A coupled shuffle writes the far side of each of its doors itself.
        for sh in shuffles.iter().filter(|sh| sh.coupled) {
            for e in &sh.exits {
                if let Some(p) = exits[*e].partner.filter(|p| taken.contains_key(p)) {
                    return Err(format!(
                        "shuffles.{}: both sides of a door are listed ({:?} and {:?}); a coupled \
                         shuffle lists one side and the other side follows",
                        sh.name, exits[*e].key, exits[p].key
                    ));
                }
            }
        }
        let order = Self::order(&shuffles)?;
        Ok(Decl { shuffles, order, exits })
    }

    /// The run order of the `none` shuffles (doc 17 section 4.3): a shuffle runs after every
    /// shuffle its things' rules read, ties by key. The layout is one node after all of them,
    /// since its check reads every rule and nothing may read it yet.
    fn order(shuffles: &[Shuffle]) -> R<Vec<usize>> {
        // Per shuffle: the shuffles it reads, and where the read is written.
        let mut reads: Vec<Vec<(usize, String)>> = vec![Vec::new(); shuffles.len()];
        for (i, sh) in shuffles.iter().enumerate() {
            for t in &sh.things {
                let mut found = Vec::new();
                t.rule.iter().for_each(|rule| ats(rule, &mut found));
                for (name, slot) in found {
                    let at = format!("shuffles.{}.things.{}.rule", sh.name, t.name);
                    let j = shuffles
                        .iter()
                        .position(|x| x.name == name)
                        .ok_or_else(|| format!("{at}: unknown shuffle {name:?}"))?;
                    if shuffles[j].layout {
                        return Err(layout_read(&at, name));
                    }
                    reads[i].push((j, format!("{at}: at.{name}.{slot}")));
                }
            }
        }
        let mut done = vec![false; shuffles.len()];
        let mut order = Vec::new();
        while let Some(i) =
            (0..shuffles.len()).find(|i| !done[*i] && reads[*i].iter().all(|(j, _)| done[*j]))
        {
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
        let j = sh
            .slots
            .iter()
            .position(|x| x == slot)
            .ok_or_else(|| format!("{path}: shuffle {name:?} has no slot {slot:?}"))?;
        Ok((sh, &sh.keys[j]))
    }

    /// What `at` stands for where a rule is expected (doc 17 section 5.4): the rule of the
    /// thing assigned to the slot, written as "this thing is there, and its rule" for every
    /// thing, which the per-seed fold cuts down to the one that is there.
    pub(crate) fn rule_form(&self, at: &Value, path: &str) -> R<Value> {
        let (sh, option) = self.at(arr(at, path)?, path)?;
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
        let (sh, option) = self.at(at, path)?;
        let Some(thing) = other.as_str() else {
            return Err(format!("{path}: `at` is compared to a thing name"));
        };
        if op != "eq" && op != "ne" {
            return Err(format!("{path}: a thing name is tested with `eq` or `ne`"));
        }
        let j = sh
            .things
            .iter()
            .position(|t| t.name == thing)
            .ok_or_else(|| format!("{path}: shuffle {:?} has no thing {thing:?}", sh.name))?;
        Ok(Some(json!({op: [{"opt": option}, j]})))
    }

    /// The options the compiler adds: one per slot of every `none` shuffle.
    pub(crate) fn option_keys(&self) -> impl Iterator<Item = &str> {
        self.shuffles.iter().flat_map(|sh| sh.keys.iter().map(String::as_str))
    }
}

fn layout_read(path: &str, name: &str) -> String {
    format!("{path}: reads the `layout` shuffle {name:?}; a rule cannot read a layout result yet")
}

/// An index below `n` from the stream; `None` when `n` is 0.
fn pick(rng: &mut SpecRng, n: usize) -> Option<usize> {
    rng.below(u64::try_from(n).ok()?).and_then(|k| usize::try_from(k).ok())
}

/// The `none` reference draw (doc 17 section 4.5, without pins, counts and weights, which this
/// slice does not have): visit the slots in a seeded order, most restricted first, and give
/// each one allowed thing that is still free, drawn evenly. A slot with none left frees one by
/// moving earlier slots to other allowed things (the augmenting step of bipartite matching),
/// so the draw fails only when no assignment exists. `allowed` has a row per slot.
fn assign(slots: usize, things: usize, allowed: &[bool], rng: &mut SpecRng) -> Option<Vec<usize>> {
    let ok = |slot: usize, t: usize| allowed.get(slot * things + t).copied().unwrap_or(false);
    let mut order: Vec<usize> = (0..slots).collect();
    rng.shuffle(&mut order);
    let choices: Vec<usize> =
        (0..slots).map(|slot| (0..things).filter(|t| ok(slot, *t)).count()).collect();
    order.sort_by_key(|slot| choices[*slot]);
    let mut owner: Vec<Option<usize>> = vec![None; things];
    for slot in order {
        let free: Vec<usize> =
            (0..things).filter(|t| ok(slot, *t) && owner[*t].is_none()).collect();
        match pick(rng, free.len()) {
            Some(k) => owner[free[k]] = Some(slot),
            None => {
                if !augment(slot, things, allowed, &mut owner, &mut vec![false; things]) {
                    return None;
                }
            }
        }
    }
    let mut got = vec![usize::MAX; slots];
    owner.iter().enumerate().for_each(|(t, o)| o.iter().for_each(|slot| got[*slot] = t));
    Some(got)
}

/// Give `slot` a thing, moving the slots that hold its allowed things on to others. Nests at
/// most one level per slot ([`config::SHUFFLE_MAX_SLOTS`]).
fn augment(
    slot: usize,
    things: usize,
    allowed: &[bool],
    owner: &mut [Option<usize>],
    seen: &mut [bool],
) -> bool {
    for t in 0..things {
        if seen[t] || !allowed.get(slot * things + t).copied().unwrap_or(false) {
            continue;
        }
        seen[t] = true;
        if owner[t].is_none_or(|o| augment(o, things, allowed, owner, seen)) {
            owner[t] = Some(slot);
            return true;
        }
    }
    false
}

/// One exit a `layout` shuffle lists: a slot, and the thing "arrive where it led".
#[derive(Debug)]
struct Door {
    /// Index in [`Decl::exits`].
    exit: usize,
    site: usize,
    region: usize,
    /// Where it leads in the unshuffled game.
    target: usize,
    shuffle: usize,
    /// Its position among its shuffle's slots, and the first door of that shuffle.
    local: usize,
    first: usize,
    /// For a door side of a coupled shuffle: the far side's site and exit.
    partner: Option<(usize, usize)>,
    /// Index in [`Layout::behind`].
    behind: usize,
}

/// What the joint layout solve needs of a world, made once at load.
#[derive(Debug)]
struct Layout {
    doors: Vec<Door>,
    /// Per region: where its exits that no shuffle moves lead.
    fixed: Vec<Vec<u16>>,
    /// Per region: the doors in it.
    doors_in: Vec<Vec<usize>>,
    /// Per distinct door region: the regions that reach it by fixed exits, itself included,
    /// ascending.
    behind: Vec<Vec<u16>>,
    /// Per region: how many doors it reaches by fixed exits.
    opens: Vec<u32>,
}

/// A world's shuffles bound to its regions, exits and options: checked once at load, shared
/// by every seed.
#[derive(Debug)]
pub(crate) struct Bound {
    decl: Decl,
    layout: Option<Layout>,
}

impl Bound {
    /// Check a shuffle section against the world it came with.
    pub(crate) fn bind(w: &World, section: &[u8]) -> R<Bound> {
        let v: Value =
            serde_json::from_slice(section).map_err(|e| format!("shuffle section: {e}"))?;
        let decl = Decl::parse(&v)?;
        for option in decl.option_keys() {
            if !(0..w.options()).any(|k| w.option_key(k) == option) {
                return Err(format!("shuffle section: the rules have no option {option:?}"));
            }
        }
        // Per exit: its site, its region and where it leads.
        let mut sites = Vec::with_capacity(decl.exits.len());
        for e in &decl.exits {
            let r = w.region(&e.region).map_err(|m| format!("exit {:?}: {m}", e.key))? as usize;
            let span = w.group(r, EXITS);
            let site =
                span.start.checked_add(e.index).filter(|x| *x < span.end).ok_or_else(|| {
                    format!("exit {:?}: region {:?} has no exit {}", e.key, e.region, e.index)
                })?;
            sites.push((site, r, w.site_target(site) as usize));
        }
        let distinct: HashSet<usize> = sites.iter().map(|x| x.0).collect();
        if distinct.len() != sites.len() {
            return Err("shuffle section: two keys name one exit".to_owned());
        }
        for (k, e) in decl.exits.iter().enumerate() {
            let back = e.partner.map(|p| (sites[p].2, sites[p].1));
            if back.is_some_and(|b| b != (sites[k].1, sites[k].2)) {
                return Err(format!(
                    "door {:?}: its two sides are not each other's way back",
                    e.door.as_deref().unwrap_or_default()
                ));
            }
        }
        let layout = Layout::new(w, &decl, &sites)?;
        Ok(Bound { decl, layout })
    }

    /// The shuffles a failed whole-world check is reported under.
    fn names(&self) -> String {
        let of = |layout: bool| {
            let names: Vec<String> = (self.decl.shuffles.iter())
                .filter(|sh| sh.layout == layout)
                .map(|sh| format!("{:?}", sh.name))
                .collect();
            names.join(", ")
        };
        if self.layout.is_some() { format!("{} (layout)", of(true)) } else { of(false) }
    }

    /// One attempt (doc 17 section 4.4, steps 1 to 3): the `none` shuffles in order, the joint
    /// layout solve, the fold, the whole-world check.
    fn attempt(
        &self,
        w: &World,
        inst: &mut Cow<'_, Instance>,
        stream: &dyn Fn(&str) -> R<SpecRng>,
    ) -> Result<Staged, Fail> {
        let mut table = Table::new();
        for sh in self.decl.order.iter().map(|i| &self.decl.shuffles[*i]) {
            let mut rng = stream(&sh.name).map_err(Fail::Hard)?;
            let got = assign(sh.slots.len(), sh.things.len(), &sh.allowed, &mut rng)
                .ok_or_else(|| Fail::Hard(format!("shuffle {:?}: no assignment", sh.name)))?;
            let row = table.entry(sh.name.clone()).or_default();
            for ((slot, option), t) in sh.slots.iter().zip(&sh.keys).zip(got) {
                let thing = sh.things.get(t).map_or("", |x| x.name.as_str());
                row.insert(slot.clone(), thing.to_owned());
                inst.to_mut().options.insert(option.clone(), i64::try_from(t).unwrap_or(0));
            }
        }
        let mut exits = Vec::new();
        if let Some(l) = &self.layout {
            let mut rng = stream(LAYOUT).map_err(Fail::Hard)?;
            let fail = |why| Fail::Attempt(self.names(), why);
            exits = l.grow(&self.decl, &mut rng, w.start as usize, &mut table).map_err(fail)?;
        }
        let world = w.staged(&exits);
        let plan = Plan::new(&world, inst).map_err(|e| Fail::Hard(format!("{e:?}")))?;
        whole_world(&world, &plan).map_err(|why| Fail::Attempt(self.names(), why))?;
        Ok(Staged { world, plan, table, attempts: 1 })
    }
}

impl Layout {
    fn new(w: &World, decl: &Decl, sites: &[(usize, usize, usize)]) -> R<Option<Layout>> {
        let mut doors = Vec::new();
        // Sites a shuffle writes: its slots, and the far sides that follow.
        let mut moved = HashSet::new();
        for (shuffle, sh) in decl.shuffles.iter().enumerate().filter(|x| x.1.layout) {
            let first = doors.len();
            for (local, exit) in sh.exits.iter().copied().enumerate() {
                let (site, region, target) = sites[exit];
                let partner = decl.exits[exit].partner.filter(|_| sh.coupled);
                let partner = partner.map(|p| (sites[p].0, p));
                moved.insert(site);
                moved.extend(partner.map(|p| p.0));
                doors.push(Door {
                    exit,
                    site,
                    region,
                    target,
                    shuffle,
                    local,
                    first,
                    partner,
                    behind: 0,
                });
            }
        }
        if doors.is_empty() {
            return Ok(None);
        }
        let regions = w.regions();
        let small = |r: usize| u16::try_from(r).unwrap_or(u16::MAX);
        let (mut fixed, mut back) = (vec![Vec::new(); regions], vec![Vec::new(); regions]);
        for r in 0..regions {
            for site in w.group(r, EXITS).filter(|site| !moved.contains(site)) {
                let to = w.site_target(site) as usize;
                fixed[r].push(small(to));
                back[to].push(r);
            }
        }
        let mut doors_in = vec![Vec::new(); regions];
        let mut behind: Vec<Vec<u16>> = Vec::new();
        let mut of_region: HashMap<usize, usize> = HashMap::new();
        let mut seen = vec![usize::MAX; regions];
        let mut opens = vec![0u32; regions];
        let mut pairs = 0usize;
        for (k, d) in doors.iter_mut().enumerate() {
            doors_in[d.region].push(k);
            d.behind = match of_region.get(&d.region) {
                Some(b) => *b,
                None => {
                    // Walk the fixed exits backwards from the door's region.
                    let b = behind.len();
                    let mut found = vec![d.region];
                    seen[d.region] = b;
                    let mut at = 0;
                    while let Some(r) = found.get(at).copied() {
                        for from in back[r].iter().copied() {
                            if seen[from] != b {
                                seen[from] = b;
                                found.push(from);
                            }
                        }
                        at += 1;
                    }
                    let mut found: Vec<u16> = found.into_iter().map(small).collect();
                    found.sort_unstable();
                    behind.push(found);
                    of_region.insert(d.region, b);
                    b
                }
            };
            pairs += behind[d.behind].len();
            if pairs > config::SHUFFLE_MAX_BEHIND {
                return Err(format!(
                    "shuffles: more than {} pairs of a region and a shuffled exit it reaches by \
                     exits that are not shuffled",
                    config::SHUFFLE_MAX_BEHIND
                ));
            }
            behind[d.behind].iter().for_each(|r| opens[usize::from(*r)] += 1);
        }
        Ok(Some(Layout { doors, fixed, doors_in, behind, opens }))
    }

    /// The joint layout solve (doc 17 section 4.5): grow from the start, each step connecting
    /// one open door in a region already placed to an allowed thing, preferring a branch that
    /// leads to a new region, then a dead end that leads to a new region, then anything
    /// allowed. Returns every exit site it wrote with its new target, and records the result
    /// in `table`.
    ///
    /// A door counts as usable as soon as its region is placed: whether its own rule can be
    /// met there is left to the whole-world check, which runs the real solver on the result.
    fn grow(
        &self,
        decl: &Decl,
        rng: &mut SpecRng,
        start: usize,
        table: &mut Table,
    ) -> Result<Vec<(usize, u32)>, String> {
        let n = self.doors.len();
        let key = |d: &Door| decl.exits[d.exit].key.as_str();
        let mut placed = vec![false; self.fixed.len()];
        let mut opens = self.opens.clone();
        let mut used = vec![false; n];
        // Open doors in placed regions.
        let mut open: Vec<usize> = Vec::new();
        let mut out = Vec::with_capacity(2 * n);
        let reach = |from: usize, placed: &mut Vec<bool>, open: &mut Vec<usize>| {
            let mut stack = vec![from];
            while let Some(r) = stack.pop() {
                if std::mem::replace(&mut placed[r], true) {
                    continue;
                }
                open.extend_from_slice(&self.doors_in[r]);
                stack.extend(self.fixed[r].iter().map(|t| usize::from(*t)));
            }
        };
        reach(start, &mut placed, &mut open);
        let mut candidates = Vec::new();
        for _ in 0..n {
            let Some(k) = pick(rng, open.len()) else {
                return Err("exits are left in regions the layout cannot reach".to_owned());
            };
            let x = &self.doors[open.swap_remove(k)];
            let sh = &decl.shuffles[x.shuffle];
            let near = &self.behind[x.behind];
            let mut best = 3;
            for (t, thing) in self.doors.iter().enumerate().skip(x.first).take(sh.slots.len()) {
                if used[t] || !sh.allows(x.local, thing.local) {
                    continue;
                }
                let to = thing.target;
                // The door being connected is no longer one the new region opens.
                let own =
                    u32::from(near.binary_search(&u16::try_from(to).unwrap_or(u16::MAX)).is_ok());
                let tier = match (placed[to], opens[to] > own) {
                    (false, true) => 0,
                    (false, false) => 1,
                    (true, _) => 2,
                };
                if tier < best {
                    best = tier;
                    candidates.clear();
                }
                if tier == best {
                    candidates.push(t);
                }
            }
            let Some(t) = pick(rng, candidates.len()).map(|k| candidates[k]) else {
                return Err(format!("no allowed thing is left for exit {:?}", key(x)));
            };
            let thing = &self.doors[t];
            used[t] = true;
            let row = table.entry(sh.name.clone()).or_default();
            row.insert(key(x).to_owned(), key(thing).to_owned());
            out.push((x.site, u32::try_from(thing.target).unwrap_or(0)));
            // Coupled: the far side of where this door now leads comes back here.
            if let (Some((site, far)), Some(own)) = (thing.partner, decl.exits[x.exit].partner) {
                out.push((site, u32::try_from(x.region).unwrap_or(0)));
                row.insert(decl.exits[far].key.clone(), decl.exits[own].key.clone());
            }
            near.iter().for_each(|r| opens[usize::from(*r)] -= 1);
            reach(thing.target, &mut placed, &mut open);
        }
        Ok(out)
    }
}

/// The whole-world check (doc 17 section 4.5): holding every item still in the pool, with a
/// fixed item collected only where its check is reached, every region and every check that
/// exists this seed is reached and the goal holds.
fn whole_world(w: &World, plan: &Plan) -> Result<(), String> {
    let held: Vec<u16> = plan.progression.iter().chain(&plan.rest).copied().collect();
    let r = plan.state(w, &held, &plan.locked_placed(w));
    if let Some(region) = r.masks.iter().position(|m| *m == 0) {
        let region = w.region_key(u32::try_from(region).unwrap_or(0));
        return Err(format!("region {region:?} is not reached with every item held"));
    }
    if let Some(l) = plan.included.iter().find(|l| !r.reached(**l)) {
        return Err(format!("check {:?} is not reached with every item held", w.loc_key(*l)));
    }
    if !r.goal() {
        return Err("the goal does not hold with every item held".to_owned());
    }
    Ok(())
}

/// Why an attempt ended.
enum Fail {
    /// The shuffles and the reason: the next attempt may pass.
    Attempt(String, String),
    /// Nothing another attempt can change.
    Hard(String),
}

/// One world of one seed with its shuffles folded in: an ordinary world to the fill.
#[derive(Debug)]
pub struct Staged {
    /// The world with this seed's layout: the exits lead where the shuffles put them.
    pub world: World,
    /// The player's instance resolved against it, with every `none` result folded into the
    /// rules.
    pub plan: Plan,
    /// What each shuffle assigned.
    pub table: Table,
    /// Attempts taken (1: no restart).
    pub attempts: u32,
}

/// Run the shuffles of `w` for `player` (from 0) of the room seeded `seed`. `None` for a world
/// that declares no shuffle: nothing is drawn and the caller fills it as before. The same
/// seed and player give the same result.
///
/// # Errors
/// An instance the world lacks keys for, or no valid result within
/// [`config::SHUFFLE_MAX_ATTEMPTS`] attempts: the message names the shuffles, the attempts and
/// what the last attempt failed on.
pub fn stage(w: &World, inst: &Instance, seed: u64, player: u32) -> R<Option<Staged>> {
    let Some(bound) = w.shuffles() else { return Ok(None) };
    let mut root = [0u8; 32];
    root[..8].copy_from_slice(&seed.to_le_bytes());
    let of_world = spec_rng::derive(&root, b"shuffles", player).ok_or("no random stream")?;
    let mut inst = Cow::Borrowed(inst);
    let mut last = (String::new(), String::new());
    let cap = config::SHUFFLE_MAX_ATTEMPTS;
    for attempt in 1..=cap {
        // Keyed by name and attempt, never by position (doc 17 section 4.3).
        let stream = |name: &str| {
            spec_rng::derive(&of_world, name.as_bytes(), attempt)
                .map(SpecRng::from_seed)
                .ok_or_else(|| format!("shuffle {name:?}: no random stream"))
        };
        match bound.attempt(w, &mut inst, &stream) {
            Ok(staged) => return Ok(Some(Staged { attempts: attempt, ..staged })),
            Err(Fail::Attempt(what, why)) => last = (what, why),
            Err(Fail::Hard(e)) => return Err(e),
        }
    }
    Err(format!("shuffle {}: no valid result in {cap} attempts (the last: {})", last.0, last.1))
}

/// A room staged: per player their world (player `p` plays `worlds[p % worlds.len()]`, one
/// each or one for all, as [`Compiled`] holds them) with its shuffles run and its plan, and
/// each player's table. A world without shuffles is resolved as it is. One thread: a world's
/// stage is small beside its fill.
///
/// # Errors
/// As [`stage`] and [`Plan::new`], naming the player.
pub fn room(worlds: &[World], insts: &[&Instance], seed: u64) -> R<(Compiled, Vec<Table>)> {
    too_many(insts.len()).map_err(|e| format!("{e:?}"))?;
    let mut c = Compiled { worlds: Vec::new(), plans: Vec::new(), progression: Vec::new() };
    let mut tables = Vec::with_capacity(insts.len());
    for (p, inst) in insts.iter().enumerate() {
        let who = |e: String| format!("player {}: {e}", p + 1);
        let w = worlds.get(p % worlds.len().max(1)).ok_or("a room needs a world")?;
        let player = u32::try_from(p).map_err(|e| e.to_string())?;
        let staged = match stage(w, inst, seed, player).map_err(who)? {
            Some(staged) => staged,
            None => Staged {
                plan: Plan::new(w, inst).map_err(|e| who(format!("{e:?}")))?,
                world: w.clone(),
                table: Table::new(),
                attempts: 0,
            },
        };
        c.progression.push(inst.progression(&staged.world));
        c.worlds.push(staged.world);
        c.plans.push(staged.plan);
        tables.push(staged.table);
    }
    Ok((c, tables))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid assignment for every seed, also where a slot must push an earlier one on; and
    /// none when the table has no assignment.
    #[test]
    fn the_none_draw_finds_an_assignment_whenever_one_exists() {
        // Slots 0 and 1 take things 0 and 1, slot 2 takes 0 or 2, slot 3 takes 2 or 3.
        let rows = [
            [true, true, false, false],
            [true, true, false, false],
            [true, false, true, false],
            [false, false, true, true],
        ];
        let allowed: Vec<bool> = rows.concat();
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..200 {
            let got = assign(4, 4, &allowed, &mut SpecRng::from_u64(seed)).unwrap();
            assert!(got.iter().enumerate().all(|(slot, t)| rows[slot][*t]), "seed {seed}: {got:?}");
            let mut things = got.clone();
            things.sort_unstable();
            assert_eq!(things, [0, 1, 2, 3], "seed {seed}");
            assert_eq!(got, assign(4, 4, &allowed, &mut SpecRng::from_u64(seed)).unwrap());
            seen.insert(got);
        }
        assert_eq!(seen.len(), 2, "both assignments the table has: {seen:?}");
        // Three slots that all need thing 0 or 1.
        let tight = [true, true, false, true, true, false, true, true, false];
        assert!(assign(3, 3, &tight, &mut SpecRng::from_u64(1)).is_none());
        // More things than slots leaves things over; more slots than things has no assignment.
        assert_eq!(assign(1, 3, &[true; 3], &mut SpecRng::from_u64(1)).map(|g| g.len()), Some(1));
        assert!(assign(2, 1, &[true; 2], &mut SpecRng::from_u64(1)).is_none());
    }
}
