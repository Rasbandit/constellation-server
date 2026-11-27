//! The compiled rules section (`docs/world-rules-section.md`), machine-only: its byte layout,
//! the one bytecode node reader every consumer uses, and the compiler from the author-facing
//! JSON manifest. The compiler runs once per world version and is the only place that reads
//! JSON; its errors name the manifest path, never an offset or an index.

use std::collections::HashMap;

use bits::{Packer, width};
use seed_bundle::package::{SECTION_GAME, SECTION_RULES, SECTION_SHUFFLES, Section, file_bytes};
use serde_json::{Map, Value, json};

use crate::shuffle::Decl;

/// Layout version in the rules header.
pub const VERSION: u16 = 1;
/// Number of time slices a region mask can carry (`x.time_slices@0`).
pub const MAX_SLICES: u32 = 64;
/// Header: version, flags, item, location, region, counter, def, site counts, start region.
pub const HEADER: usize = 18;
/// Subsections in directory order.
pub const SUBSECTIONS: usize = 15;
/// Directory indexes.
pub mod sub {
    /// Option defaults, `i32` each.
    pub const OPTIONS: usize = 0;
    /// Roll slots: packed roll ids, then packed locations.
    pub const ROLLS: usize = 1;
    /// Time model.
    pub const TIME: usize = 2;
    /// Distinct `x.time_any` masks, `u64` each.
    pub const MASKS: usize = 3;
    /// Region flags, then the packed first site of every region group.
    pub const REGIONS: usize = 4;
    /// Site records: target and rule offset.
    pub const SITES: usize = 5;
    /// Packed def rule offsets.
    pub const DEFS: usize = 6;
    /// The bytecode.
    pub const CODE: usize = 7;
    /// Goal rule offset, `u32`.
    pub const GOAL: usize = 8;
    /// Item to sites (CSR).
    pub const ITEM_SITES: usize = 9;
    /// Counter to sites (CSR).
    pub const COUNTER_SITES: usize = 10;
    /// Sites whose rule reads time, set words.
    pub const TIME_SITES: usize = 11;
    /// Vanilla item per location, packed (`item count` = none).
    pub const LOCATIONS: usize = 12;
    /// Region, counter, def, option and roll keys (diagnostics and binding by key).
    pub const NAMES: usize = 13;
    /// Item groups that a sum of copies reads: packed starts, items, caps (0 = no cap).
    pub const GROUPS: usize = 14;
}
/// Site groups of a region, in site order.
pub const GROUPS: usize = 4;
/// Group index of exits, events, checks, stays.
pub const EXITS: usize = 0;
/// Event group.
pub const EVENTS: usize = 1;
/// Check group.
pub const CHECKS: usize = 2;
/// Stay group.
pub const STAYS: usize = 3;

/// Opcodes.
pub mod op {
    pub const TRUE: u8 = 0;
    pub const FALSE: u8 = 1;
    pub const ALL: u8 = 2;
    pub const ANY: u8 = 3;
    pub const HAS: u8 = 4;
    pub const HAS_N: u8 = 5;
    pub const HAS_ANY: u8 = 6;
    pub const CMP: u8 = 7;
    pub const TIME_ANY: u8 = 8;
    pub const REF: u8 = 9;
    pub const REACH: u8 = 10;
    pub const NOT: u8 = 11;
    pub const INT: u8 = 12;
    pub const COUNT: u8 = 13;
    pub const COUNTER: u8 = 14;
    pub const OPT: u8 = 15;
    pub const ROLL: u8 = 16;
    pub const ADD: u8 = 17;
    pub const MIN: u8 = 18;
    pub const MAX: u8 = 19;
    pub const MUL: u8 = 20;
    pub const DIV: u8 = 21;
    pub const GROUP: u8 = 22;
}

/// Comparison operators, in their `CMP` operand order (the manifest spells them the same).
pub const CMP_OPS: [&str; 6] = ["eq", "ne", "lt", "le", "gt", "ge"];

/// Compare two integers by `CMP` operand.
#[must_use]
pub fn compare(op: u8, a: i64, b: i64) -> bool {
    match op {
        0 => a == b,
        1 => a != b,
        2 => a < b,
        3 => a <= b,
        4 => a > b,
        _ => a >= b,
    }
}

/// Operand sizes in bytes (1 when the count is at most 256, else 2), fixed per section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Widths {
    /// Item operands.
    pub item: u8,
    /// Def operands.
    pub def: u8,
    /// Counter operands.
    pub counter: u8,
}

impl Widths {
    /// From the header counts.
    #[must_use]
    pub fn new(items: usize, defs: usize, counters: usize) -> Self {
        let w = |n: usize| if n <= 256 { 1 } else { 2 };
        Widths { item: w(items), def: w(defs), counter: w(counters) }
    }
}

/// One decoded bytecode node: opcode and operands. Children follow in prefix order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// Constant.
    Const(bool),
    /// All of `n` children taking `len` bytes.
    All(u8, u16),
    /// Any of `n` children taking `len` bytes.
    Any(u8, u16),
    /// At least `n` copies of an item.
    Has(u16, u8),
    /// Any of `n` items; the item operands follow.
    HasAny(u8),
    /// Comparison by `CMP` operand; two integer nodes follow.
    Cmp(u8),
    /// Time mask table index.
    TimeAny(u8),
    /// Def.
    Ref(u16),
    /// Region reached.
    Reach(u16),
    /// Negation of the following option-only rule.
    Not,
    /// Integer constant.
    Int(i16),
    /// Copies held.
    Count(u16),
    /// Counter value.
    Counter(u16),
    /// Option value.
    Opt(u8),
    /// Roll slot value.
    Roll(u16),
    /// Sum, minimum, maximum of `n` integer children (opcode kept).
    Fold(u8, u8),
    /// Product or floor division of two integer children (opcode kept).
    Pair(u8),
    /// A sum of (capped) copies over an item group, kept as a counter by the solve.
    Group(u16),
}

/// Little-endian operand of 1 or 2 bytes.
#[inline]
#[must_use]
pub fn operand(code: &[u8], at: usize, size: u8) -> Option<u16> {
    if size == 1 {
        code.get(at).map(|b| u16::from(*b))
    } else {
        code.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
}

/// The node at `pc` and the offset right after its own operands (its first child, if any).
#[inline]
#[must_use]
pub fn node(code: &[u8], pc: usize, w: Widths) -> Option<(Node, usize)> {
    let o = *code.get(pc)?;
    let a = pc + 1;
    let u8_at = |i: usize| code.get(i).copied();
    let u16_at = |i: usize| operand(code, i, 2);
    Some(match o {
        op::TRUE | op::FALSE => (Node::Const(o == op::TRUE), a),
        op::ALL | op::ANY => {
            let (n, len) = (u8_at(a)?, u16_at(a + 1)?);
            (if o == op::ALL { Node::All(n, len) } else { Node::Any(n, len) }, a + 3)
        }
        op::HAS => (Node::Has(operand(code, a, w.item)?, 1), a + usize::from(w.item)),
        op::HAS_N => {
            let i = operand(code, a, w.item)?;
            (Node::Has(i, u8_at(a + usize::from(w.item))?), a + usize::from(w.item) + 1)
        }
        op::HAS_ANY => (Node::HasAny(u8_at(a)?), a + 1),
        op::CMP => (Node::Cmp(u8_at(a)?), a + 1),
        op::TIME_ANY => (Node::TimeAny(u8_at(a)?), a + 1),
        op::REF => (Node::Ref(operand(code, a, w.def)?), a + usize::from(w.def)),
        op::REACH => (Node::Reach(u16_at(a)?), a + 2),
        op::NOT => (Node::Not, a),
        op::INT => (Node::Int(u16_at(a)?.cast_signed()), a + 2),
        op::COUNT => (Node::Count(operand(code, a, w.item)?), a + usize::from(w.item)),
        op::COUNTER => (Node::Counter(operand(code, a, w.counter)?), a + usize::from(w.counter)),
        op::OPT => (Node::Opt(u8_at(a)?), a + 1),
        op::ROLL => (Node::Roll(u16_at(a)?), a + 2),
        op::ADD | op::MIN | op::MAX => (Node::Fold(o, u8_at(a)?), a + 1),
        op::MUL | op::DIV => (Node::Pair(o), a),
        op::GROUP => (Node::Group(u16_at(a)?), a + 2),
        _ => return None,
    })
}

/// A packed list: `u32 count`, `u8 width` (at least 1, so a list's length is bounded by its
/// bytes), then the fields.
pub fn put_packed(out: &mut Vec<u8>, values: &[u32]) {
    let w = width(u64::from(values.iter().copied().max().unwrap_or(0))).max(1);
    out.extend_from_slice(&u32::try_from(values.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.push(w);
    let mut p = Packer::default();
    values.iter().for_each(|v| p.push(u64::from(*v), w));
    out.extend_from_slice(&p.finish());
}

/// A string list: `u16 count`, then per string `u16 length` and UTF-8 bytes.
fn put_strings<'a>(out: &mut Vec<u8>, keys: impl ExactSizeIterator<Item = &'a str>) {
    out.extend_from_slice(&u16::try_from(keys.len()).unwrap_or(u16::MAX).to_le_bytes());
    for k in keys {
        out.extend_from_slice(&u16::try_from(k.len()).unwrap_or(u16::MAX).to_le_bytes());
        out.extend_from_slice(&k.as_bytes()[..k.len().min(usize::from(u16::MAX))]);
    }
}

/// Game section stand-in: per table (items, then locations) `u16 count`, the ids as `i64`,
/// then the keys as a string list. Indexes are manifest order.
fn put_ids(out: &mut Vec<u8>, rows: &[(&str, i64)]) {
    out.extend_from_slice(&u16::try_from(rows.len()).unwrap_or(u16::MAX).to_le_bytes());
    rows.iter().for_each(|(_, id)| out.extend_from_slice(&id.to_le_bytes()));
    put_strings(out, rows.iter().map(|(k, _)| *k));
}

type R<T> = Result<T, String>;

/// Whether `CMP` operand `op` over sides that do (or do not) read state can only turn from
/// false to true as state grows: state may sit only on the side that must be larger.
#[must_use]
pub fn monotone_cmp(op: usize, left_state: bool, right_state: bool) -> bool {
    match op {
        // lt, le: the right side must be the larger one.
        2 | 3 => !left_state,
        // gt, ge.
        4 | 5 => !right_state,
        _ => !left_state && !right_state,
    }
}

/// A sum whose every operand is `{"count": k}` or `{"int": ["min", {"count": k}, cap]}`:
/// its members as (key, cap).
fn group(args: &[Value]) -> Option<Vec<(&str, Option<u16>)>> {
    args.iter()
        .map(|a| {
            if let Some(k) = a.get("count").and_then(Value::as_str) {
                return (a.as_object()?.len() == 1).then_some((k, None));
            }
            match a.get("int")?.as_array()?.as_slice() {
                [m, c, n] if m == "min" && c.as_object()?.len() == 1 => {
                    let cap = u16::try_from(n.as_i64()?).ok().filter(|x| *x < u16::MAX)?;
                    Some((c.get("count")?.as_str()?, Some(cap)))
                }
                _ => None,
            }
        })
        .collect()
}

/// An operand of `size` (1 or 2) bytes, little-endian.
fn put(out: &mut Vec<u8>, v: u16, size: u8) {
    out.extend_from_slice(&v.to_le_bytes()[..usize::from(size)]);
}

/// What a rule reads, for the dependency index and the `not` check.
#[derive(Debug, Clone, Default)]
struct Reads {
    items: Vec<u16>,
    counters: Vec<u16>,
    time: bool,
    region: bool,
}

impl Reads {
    /// Add what `o` reads; the lists stay sorted sets, so a def read twice does not double
    /// them (a chain of such defs would grow them exponentially).
    fn merge(&mut self, o: &Reads) {
        for (mine, theirs) in [(&mut self.items, &o.items), (&mut self.counters, &o.counters)] {
            mine.extend_from_slice(theirs);
            mine.sort_unstable();
            mine.dedup();
        }
        self.time |= o.time;
        self.region |= o.region;
    }

    fn state(&self) -> bool {
        !self.items.is_empty() || !self.counters.is_empty() || self.time || self.region
    }
}

pub(crate) fn arr<'a>(v: &'a Value, path: &str) -> R<&'a Vec<Value>> {
    v.as_array().ok_or_else(|| format!("{path}: expected an array"))
}

pub(crate) fn s<'a>(v: &'a Value, path: &str) -> R<&'a str> {
    v.as_str().ok_or_else(|| format!("{path}: expected a string"))
}

fn int(v: &Value, path: &str) -> R<i64> {
    v.as_i64().ok_or_else(|| format!("{path}: expected an integer"))
}

fn small<T: TryFrom<usize>>(n: usize, what: &str) -> R<T> {
    T::try_from(n).map_err(|_| format!("too many {what} ({n})"))
}

struct Compiler<'m> {
    items: HashMap<&'m str, u16>,
    locs: HashMap<&'m str, u16>,
    regions: HashMap<&'m str, u16>,
    counters: Vec<&'m str>,
    options: Vec<&'m str>,
    rolls: Vec<&'m str>,
    slots: Vec<(u8, u16)>,
    masks: Vec<u64>,
    raw_defs: &'m Map<String, Value>,
    def_names: Vec<&'m str>,
    /// Def offsets; `None` while the def is being compiled (cycle guard) or not yet reached.
    def_at: Vec<Option<u32>>,
    def_busy: Vec<bool>,
    def_reads: Vec<Reads>,
    /// Rule nodes open in the recursion, defs reached through `ref` included.
    depth: u32,
    /// The deepest an evaluation from the rule being compiled goes so far.
    peak: u32,
    /// Per compiled def: how deep its body nests, defs it reaches included, so a `ref` to a
    /// def compiled earlier still counts it (as the loader does).
    def_depth: Vec<u32>,
    code: Vec<u8>,
    dedupe: HashMap<Vec<u8>, u32>,
    /// Item groups: (item, cap) members, `None` for an uncapped count.
    groups: Vec<Vec<(u16, Option<u16>)>>,
    /// The world's shuffles: what an `at` read stands for ([`crate::shuffle`]).
    shuffles: Option<&'m Decl>,
    w: Widths,
}

/// Every `{"counter": key}` read and every event key, in first-seen order.
fn counter_keys<'m>(v: &'m Value, out: &mut Vec<&'m str>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if (k == "counter" || k == "event")
                    && let Some(c) = x.as_str()
                    && !out.contains(&c)
                {
                    out.push(c);
                }
                counter_keys(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| counter_keys(x, out)),
        _ => {}
    }
}

/// Compile a manifest into a package: the rules section and the game section stand-in.
///
/// # Errors
/// A message naming the manifest path of the first malformed or unknown construct.
pub fn compile(m: &Value) -> R<Vec<u8>> {
    let (rules, game, shuffles) = compiled(m)?;
    let more: Vec<(u16, &[u8])> = shuffles.iter().map(|b| (SECTION_SHUFFLES, &b[..])).collect();
    package(rules, game, &more)
}

/// The package file of a compiled world's two sections, then `more` sections stored as is
/// (id and bytes): the one place the file is laid out.
///
/// # Errors
/// A section over 4 GiB.
pub fn package(rules: Vec<u8>, game: Vec<u8>, more: &[(u16, &[u8])]) -> R<Vec<u8>> {
    let raw = |id, bytes| Section { id, bytes, deflate: false };
    let mut all = vec![raw(SECTION_GAME, game), raw(SECTION_RULES, rules)];
    all.extend(more.iter().map(|(id, bytes)| raw(*id, bytes.to_vec())));
    file_bytes(&all).map_err(|e| e.to_string())
}

/// The rules section and the game section stand-in.
///
/// # Errors
/// As [`compile`].
pub fn sections(m: &Value) -> R<(Vec<u8>, Vec<u8>)> {
    compiled(m).map(|(rules, game, _)| (rules, game))
}

/// The rules section, the game section stand-in, and the shuffle section when the world
/// declares shuffles.
pub(crate) type Compiled = (Vec<u8>, Vec<u8>, Option<Vec<u8>>);

/// [`sections`], and the shuffle section of a world that declares shuffles
/// ([`crate::shuffle::section`]): the manifest's shuffles are read once for all three.
///
/// # Errors
/// As [`compile`].
#[allow(clippy::too_many_lines)] // one pass in section order reads best as one function
pub(crate) fn compiled(m: &Value) -> R<Compiled> {
    let shuffles = Decl::of(m)?;
    let mut item_rows = Vec::new();
    let mut items = HashMap::new();
    for (k, it) in arr(&m["items"], "items")?.iter().enumerate() {
        let key = s(&it["key"], &format!("items[{k}].key"))?;
        let path = format!("items[{key:?}]");
        if items.insert(key, small::<u16>(k, "items")?).is_some() {
            return Err(format!("{path}: duplicate item key"));
        }
        item_rows.push((key, int(&it["id"], &format!("{path}.id"))?));
    }
    let mut loc_rows = Vec::new();
    let mut locs = HashMap::new();
    let mut vanilla = Vec::new();
    let none = u32::try_from(items.len()).unwrap_or(u32::MAX);
    for (k, l) in arr(&m["locations"], "locations")?.iter().enumerate() {
        let key = s(&l["key"], &format!("locations[{k}].key"))?;
        let path = format!("locations[{key:?}]");
        if locs.insert(key, small::<u16>(k, "locations")?).is_some() {
            return Err(format!("{path}: duplicate location key"));
        }
        loc_rows.push((key, int(&l["id"], &format!("{path}.id"))?));
        vanilla.push(match l.get("vanilla") {
            Some(v) => {
                let p = format!("{path}.vanilla");
                let k = s(v, &p)?;
                u32::from(*items.get(k).ok_or_else(|| format!("{p}: unknown item {k:?}"))?)
            }
            None => none,
        });
    }
    let raw_regions = arr(&m["regions"], "regions")?;
    let mut regions = HashMap::new();
    for (k, r) in raw_regions.iter().enumerate() {
        let key = s(&r["key"], &format!("regions[{k}].key"))?;
        if regions.insert(key, small::<u16>(k, "regions")?).is_some() {
            return Err(format!("regions[{key:?}]: duplicate region key"));
        }
    }
    let start = s(&m["start"], "start")?;
    let start = *regions.get(start).ok_or_else(|| format!("start: unknown region {start:?}"))?;
    let mut options = Vec::new();
    let mut defaults = Vec::new();
    for (k, o) in arr(&m["options"], "options")?.iter().enumerate() {
        let key = s(&o["key"], &format!("options[{k}].key"))?;
        let p = format!("options[{key:?}].default");
        let d = int(&o["default"], &p)?;
        defaults.push(i32::try_from(d).map_err(|_| format!("{p}: {d} does not fit 32 bits"))?);
        options.push(key);
    }
    // A `none` shuffle's result is one more option per slot: the index of the thing there
    // this seed, set by the shuffle stage and folded like any option.
    if let Some(k) = options.iter().find(|k| k.starts_with("at.")) {
        return Err(format!("options[{k:?}]: `at.` names are kept for shuffle results"));
    }
    if let Some(d) = &shuffles {
        for key in d.option_keys() {
            defaults.push(0);
            options.push(key);
        }
        // An option is one byte in a rule.
        let most = usize::from(u8::MAX) + 1;
        if options.len() > most {
            return Err(format!(
                "shuffles: {} options and slots that rules can read, at most {most} together",
                options.len()
            ));
        }
    }
    small::<u8>(options.len().saturating_sub(1), "options")?;
    let rolls: Vec<&str> = m
        .get("rolls")
        .and_then(Value::as_object)
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let mut counters = Vec::new();
    counter_keys(m, &mut counters);
    let raw_defs = m["defs"].as_object().ok_or("defs: expected an object")?;
    // A slot a rule reads as a rule is one more def: every thing it can hold with its rule,
    // compiled once and referred to by every read.
    let mut with_reads = None;
    if let Some(d) = &shuffles {
        if let Some(k) = raw_defs.keys().find(|k| k.starts_with("at.")) {
            return Err(format!("defs[{k:?}]: `at.` names are kept for shuffle results"));
        }
        let mut all = raw_defs.clone();
        all.extend(d.defs(m));
        with_reads = Some(all);
    }
    let raw_defs = with_reads.as_ref().unwrap_or(raw_defs);
    let w = Widths::new(items.len(), raw_defs.len(), counters.len());
    let mut c = Compiler {
        items,
        locs,
        regions,
        counters,
        options,
        rolls,
        slots: Vec::new(),
        masks: Vec::new(),
        raw_defs,
        def_names: raw_defs.keys().map(String::as_str).collect(),
        def_at: vec![None; raw_defs.len()],
        def_busy: vec![false; raw_defs.len()],
        def_reads: vec![Reads::default(); raw_defs.len()],
        depth: 0,
        peak: 0,
        def_depth: vec![0; raw_defs.len()],
        code: vec![op::TRUE, op::FALSE],
        dedupe: HashMap::new(),
        groups: Vec::new(),
        shuffles: shuffles.as_ref(),
        w,
    };
    for k in 0..raw_defs.len() {
        c.def(k)?;
    }
    // Every thing's rule is a rule like any other: checked here under its own path.
    for sh in shuffles.iter().flat_map(|d| &d.shuffles) {
        for (t, rule) in sh.things.iter().filter_map(|t| Some((t, t.rule.as_ref()?))) {
            let path = format!("shuffles.{}.things.{}.rule", sh.name, t.name);
            c.b(rule, &path, &mut Vec::new(), &mut Reads::default(), false)?;
        }
    }

    let t = &m["x.time"];
    let slices = int(&t["slices"], "x.time.slices")?;
    if !(1..=i64::from(MAX_SLICES)).contains(&slices) {
        return Err(format!("x.time.slices: must be 1..={MAX_SLICES}"));
    }
    let all = if slices == 64 { u64::MAX } else { (1u64 << slices) - 1 };

    // Sites in region order: exits, events, checks, stays. (target, rule, reads)
    let mut sites: Vec<(u32, u32, Reads)> = Vec::new();
    let mut flags = Vec::new();
    let mut starts = Vec::new();
    for r in raw_regions {
        let key = s(&r["key"], "regions")?;
        let base = format!("regions[{key:?}]");
        flags.push(u8::from(r["can_stay"].as_bool().unwrap_or(false)));
        let groups: [(&str, &str); GROUPS] =
            [("exits", "to"), ("events", "event"), ("checks", "loc"), ("stays", "slice")];
        for (g, (list, field)) in groups.into_iter().enumerate() {
            starts.push(small::<u32>(sites.len(), "sites")?);
            for e in arr(&r[list], &format!("{base}.{list}"))? {
                let name = match &e[field] {
                    Value::String(x) => format!("{x:?}"),
                    x => x.to_string(),
                };
                let path = format!("{base}.{list}[{name}]");
                let fp = format!("{path}.{field}");
                let target = match g {
                    EXITS => {
                        let to = s(&e[field], &fp)?;
                        u32::from(
                            *c.regions
                                .get(to)
                                .ok_or_else(|| format!("{fp}: unknown region {to:?}"))?,
                        )
                    }
                    EVENTS => c.counter(s(&e[field], &fp)?),
                    CHECKS => {
                        let l = s(&e[field], &fp)?;
                        u32::from(
                            *c.locs
                                .get(l)
                                .ok_or_else(|| format!("{fp}: unknown location {l:?}"))?,
                        )
                    }
                    _ => {
                        let sl = int(&e[field], &fp)?;
                        if !(0..slices).contains(&sl) {
                            return Err(format!("{fp}: slice {sl} is past x.time.slices"));
                        }
                        u32::try_from(sl).unwrap_or(0)
                    }
                };
                let mut reads = Reads::default();
                let rule = c.rule(&e["rule"], &format!("{path}.rule"), &mut reads, false)?;
                sites.push((target, rule, reads));
            }
        }
    }
    starts.push(small::<u32>(sites.len(), "sites")?);
    let site_count = small::<u16>(sites.len(), "sites")?;

    let mask = |v: &Value, p: &str| -> R<u64> {
        Ok(u64::try_from(int(v, p)?).map_err(|_| format!("{p}: expected a mask"))? & all)
    };
    let mut time = vec![u8::try_from(slices).unwrap_or(64)];
    time.extend_from_slice(&mask(&t["start_mask"], "x.time.start_mask")?.to_le_bytes());
    let owned = &t["owned"];
    // Half-day rules are dependency sites past the region sites: `site_count + h`.
    let mut half_reads = Vec::new();
    if owned.is_null() {
        time.push(0);
    } else {
        time.push(1);
        let mut reads = Reads::default();
        let when = c.rule(&owned["when"], "x.time.owned.when", &mut reads, false)?;
        if reads.state() {
            return Err("x.time.owned.when: may read options only".into());
        }
        time.extend_from_slice(&when.to_le_bytes());
        time.extend_from_slice(
            &mask(&owned["none_owned_mask"], "x.time.owned.none_owned_mask")?.to_le_bytes(),
        );
        let halves = arr(&owned["half_days"], "x.time.owned.half_days")?;
        time.push(small::<u8>(halves.len(), "half days")?);
        for (k, h) in halves.iter().enumerate() {
            let p = format!("x.time.owned.half_days[{k}]");
            let (a, z) =
                (int(&h["first"], &format!("{p}.first"))?, int(&h["last"], &format!("{p}.last"))?);
            let mut hm = 0u64;
            for sl in a..=z {
                hm |= 1u64.checked_shl(u32::try_from(sl).unwrap_or(64)).unwrap_or(0);
            }
            let mut reads = Reads::default();
            let rule = c.rule(&h["rule"], &format!("{p}.rule"), &mut reads, false)?;
            time.extend_from_slice(&(hm & all).to_le_bytes());
            time.extend_from_slice(&rule.to_le_bytes());
            half_reads.push(reads);
        }
    }
    let mut goal_reads = Reads::default();
    let goal = c.rule(&m["goal"], "goal", &mut goal_reads, true)?;

    // Dependency index over sites and half-day pseudo-sites.
    let mut item_sites = vec![Vec::new(); c.items.len()];
    let mut counter_sites = vec![Vec::new(); c.counters.len()];
    let mut time_words = vec![0u64; sites.len().div_ceil(64)];
    for (id, reads) in sites.iter().map(|x| &x.2).chain(&half_reads).enumerate() {
        let id = small::<u32>(id, "sites")?;
        let mut its = reads.items.clone();
        its.sort_unstable();
        its.dedup();
        its.iter().for_each(|i| item_sites[usize::from(*i)].push(id));
        let mut cs = reads.counters.clone();
        cs.sort_unstable();
        cs.dedup();
        cs.iter().for_each(|i| counter_sites[usize::from(*i)].push(id));
        if reads.time && (id as usize) < sites.len() {
            time_words[id as usize / 64] |= 1 << (id % 64);
        }
    }
    let csr = |lists: &[Vec<u32>]| {
        let mut out = Vec::new();
        let mut at = vec![0u32];
        lists.iter().for_each(|l| at.push(at[at.len() - 1] + u32::try_from(l.len()).unwrap_or(0)));
        put_packed(&mut out, &at);
        put_packed(&mut out, &lists.concat());
        out
    };

    let mut subs: Vec<Vec<u8>> = vec![Vec::new(); SUBSECTIONS];
    subs[sub::OPTIONS] = defaults.iter().flat_map(|d| d.to_le_bytes()).collect();
    put_packed(&mut subs[sub::ROLLS], &c.slots.iter().map(|x| u32::from(x.0)).collect::<Vec<_>>());
    put_packed(&mut subs[sub::ROLLS], &c.slots.iter().map(|x| u32::from(x.1)).collect::<Vec<_>>());
    subs[sub::TIME] = time;
    subs[sub::MASKS] = c.masks.iter().flat_map(|x| x.to_le_bytes()).collect();
    subs[sub::REGIONS] = flags;
    put_packed(&mut subs[sub::REGIONS], &starts);
    let tw = width(u64::from(sites.iter().map(|x| x.0).max().unwrap_or(0)));
    let rw = width(u64::from(sites.iter().map(|x| x.1).max().unwrap_or(0)));
    let mut p = Packer::default();
    for (target, rule, _) in &sites {
        p.push(u64::from(*target), tw);
        p.push(u64::from(*rule), rw);
    }
    subs[sub::SITES] = [tw, rw].into_iter().chain(p.finish()).collect();
    let defs: Vec<u32> = c.def_at.iter().map(|x| x.unwrap_or(1)).collect();
    put_packed(&mut subs[sub::DEFS], &defs);
    subs[sub::GOAL] = goal.to_le_bytes().to_vec();
    subs[sub::ITEM_SITES] = csr(&item_sites);
    subs[sub::COUNTER_SITES] = csr(&counter_sites);
    subs[sub::TIME_SITES] = time_words.iter().flat_map(|x| x.to_le_bytes()).collect();
    put_packed(&mut subs[sub::LOCATIONS], &vanilla);
    let names = &mut subs[sub::NAMES];
    let region_keys: Vec<&str> =
        raw_regions.iter().map(|r| r["key"].as_str().unwrap_or("")).collect();
    put_strings(names, region_keys.into_iter());
    put_strings(names, c.counters.iter().copied());
    put_strings(names, c.def_names.iter().copied());
    put_strings(names, c.options.iter().copied());
    put_strings(names, c.rolls.iter().copied());
    subs[sub::CODE] = std::mem::take(&mut c.code);
    let mut starts = vec![0u32];
    for g in &c.groups {
        starts.push(starts[starts.len() - 1] + small::<u32>(g.len(), "group members")?);
    }
    let members: Vec<&(u16, Option<u16>)> = c.groups.iter().flatten().collect();
    put_packed(&mut subs[sub::GROUPS], &starts);
    put_packed(&mut subs[sub::GROUPS], &members.iter().map(|m| u32::from(m.0)).collect::<Vec<_>>());
    put_packed(
        &mut subs[sub::GROUPS],
        &members.iter().map(|m| m.1.map_or(0, |c| u32::from(c) + 1)).collect::<Vec<_>>(),
    );

    let mut out = Vec::new();
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    for n in [item_rows.len(), loc_rows.len(), raw_regions.len(), c.counters.len(), raw_defs.len()]
    {
        out.extend_from_slice(&small::<u16>(n, "entries")?.to_le_bytes());
    }
    out.extend_from_slice(&site_count.to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    let mut at = HEADER + SUBSECTIONS * 8;
    for b in &subs {
        out.extend_from_slice(&small::<u32>(at, "bytes")?.to_le_bytes());
        out.extend_from_slice(&small::<u32>(b.len(), "bytes")?.to_le_bytes());
        at += b.len();
    }
    subs.iter().for_each(|b| out.extend_from_slice(b));
    let mut game = Vec::new();
    put_ids(&mut game, &item_rows);
    put_ids(&mut game, &loc_rows);
    Ok((out, game, shuffles.as_ref().map(Decl::stored).transpose()?))
}

impl<'m> Compiler<'m> {
    fn counter(&self, key: &str) -> u32 {
        self.counters.iter().position(|c| *c == key).map_or(0, |i| u32::try_from(i).unwrap_or(0))
    }

    fn item(&self, v: &Value, path: &str) -> R<u16> {
        let k = s(v, path)?;
        self.items.get(k).copied().ok_or_else(|| format!("{path}: unknown item {k:?}"))
    }

    /// Compile a top-level rule to its offset: constants are offsets 0 and 1, and identical
    /// rules share one copy.
    fn rule(&mut self, v: &Value, path: &str, reads: &mut Reads, goal: bool) -> R<u32> {
        let mut buf = Vec::new();
        self.b(v, path, &mut buf, reads, goal)?;
        Ok(match buf.as_slice() {
            [op::TRUE] => 0,
            [op::FALSE] => 1,
            _ => {
                if let Some(at) = self.dedupe.get(&buf) {
                    return Ok(*at);
                }
                let at = small::<u32>(self.code.len(), "bytecode bytes")?;
                self.code.extend_from_slice(&buf);
                self.dedupe.insert(buf, at);
                at
            }
        })
    }

    fn def(&mut self, k: usize) -> R<()> {
        if self.def_at[k].is_some() {
            return Ok(());
        }
        let name = self.def_names[k];
        if self.def_busy[k] {
            return Err(format!("defs[{name:?}]: def cycle through {name:?}"));
        }
        self.def_busy[k] = true;
        let raw = &self.raw_defs[name];
        let mut reads = Reads::default();
        let (base, outer) = (self.depth, self.peak);
        self.peak = base;
        let at = self.rule(raw, &format!("defs[{name:?}]"), &mut reads, false)?;
        self.def_depth[k] = self.peak - base;
        self.peak = self.peak.max(outer);
        self.def_at[k] = Some(at);
        self.def_reads[k] = reads;
        Ok(())
    }

    fn list(
        &mut self,
        opcode: u8,
        parts: &[Value],
        path: &str,
        out: &mut Vec<u8>,
        r: &mut Reads,
        goal: bool,
    ) -> R<()> {
        let n = u8::try_from(parts.len()).map_err(|_| format!("{path}: more than 255 parts"))?;
        let mut body = Vec::new();
        for (k, p) in parts.iter().enumerate() {
            self.b(p, &format!("{path}[{k}]"), &mut body, r, goal)?;
        }
        let len = u16::try_from(body.len()).map_err(|_| format!("{path}: rule too large"))?;
        out.extend_from_slice(&[opcode, n]);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&body);
        Ok(())
    }

    /// One level deeper in the rule recursion; the loader enforces the same cap.
    fn deeper(&mut self, path: &str) -> R<()> {
        self.depth += 1;
        self.reach(self.depth, path)
    }

    /// An evaluation from the rule being compiled goes `deep` nodes down.
    fn reach(&mut self, deep: u32, path: &str) -> R<()> {
        self.peak = self.peak.max(deep);
        if deep > config::RULES_MAX_DEPTH {
            return Err(format!("{path}: rules nested more than {} deep", config::RULES_MAX_DEPTH));
        }
        Ok(())
    }

    fn b(&mut self, v: &Value, path: &str, out: &mut Vec<u8>, r: &mut Reads, goal: bool) -> R<()> {
        self.deeper(path)?;
        let x = self.b_node(v, path, out, r, goal);
        self.depth -= 1;
        x
    }

    fn b_node(
        &mut self,
        v: &Value,
        path: &str,
        out: &mut Vec<u8>,
        r: &mut Reads,
        goal: bool,
    ) -> R<()> {
        let o =
            v.as_object().ok_or_else(|| format!("{path}: a rule must be an object, got {v}"))?;
        let k = if o.contains_key("has") {
            "has"
        } else {
            match o.len() {
                1 => o.keys().next().map_or("", String::as_str),
                _ => return Err(format!("{path}: a rule node has exactly one key")),
            }
        };
        let a = &o[k];
        let p = format!("{path}.{k}");
        if let Some(cmp) = CMP_OPS.iter().position(|x| *x == k) {
            let [x, y] = arr(a, &p)?.as_slice() else {
                return Err(format!("{p}: expected two operands"));
            };
            // A test of which thing a shuffle put in a slot is a test of that slot's option.
            if self.shuffles.is_some() && self.name_test(k, (x, y), (path, &p), out, r)? {
                return Ok(());
            }
            out.extend_from_slice(&[op::CMP, u8::try_from(cmp).unwrap_or(0)]);
            let (mut rx, mut ry) = (Reads::default(), Reads::default());
            self.i(x, &format!("{p}[0]"), out, &mut rx)?;
            self.i(y, &format!("{p}[1]"), out, &mut ry)?;
            if !monotone_cmp(cmp, rx.state(), ry.state()) {
                return Err(format!(
                    "{p}: this comparison can turn false as the player collects (it tests for \
                     fewer items or counters); compare with `ge` or `gt`, items on the left"
                ));
            }
            r.merge(&rx);
            r.merge(&ry);
            return Ok(());
        }
        match k {
            "true" => out.push(op::TRUE),
            "false" => out.push(op::FALSE),
            "all" | "any" => {
                let parts = arr(a, &p)?;
                match (parts.len(), k) {
                    (0, "all") => out.push(op::TRUE),
                    (0, _) => out.push(op::FALSE),
                    _ => self.list(
                        if k == "all" { op::ALL } else { op::ANY },
                        parts,
                        &p,
                        out,
                        r,
                        goal,
                    )?,
                }
            }
            "not" => {
                let mut inner = Reads::default();
                let mut body = Vec::new();
                self.b(a, &p, &mut body, &mut inner, goal)?;
                if inner.state() {
                    return Err(format!(
                        "{p}: non-monotone `not` (only option comparisons may be negated)"
                    ));
                }
                out.push(op::NOT);
                out.extend_from_slice(&body);
            }
            "has" => {
                let i = self.item(a, &p)?;
                r.items.push(i);
                match o.get("n") {
                    None => {
                        out.push(op::HAS);
                        put(out, i, self.w.item);
                    }
                    Some(n) => {
                        let np = format!("{path}.n");
                        let n = int(n, &np)?;
                        // At most zero copies always holds, as `count >= n` does.
                        let n = u8::try_from(n.max(0))
                            .map_err(|_| format!("{np}: {n} is above 255"))?;
                        out.push(op::HAS_N);
                        put(out, i, self.w.item);
                        out.push(n);
                    }
                }
            }
            "has_any" | "has_all" => {
                let list = arr(a, &p)?;
                let mut its = Vec::new();
                for (j, x) in list.iter().enumerate() {
                    its.push(self.item(x, &format!("{p}[{j}]"))?);
                }
                r.items.extend_from_slice(&its);
                let n = u8::try_from(its.len()).map_err(|_| format!("{p}: more than 255 items"))?;
                match (k, n) {
                    ("has_any", 0) => out.push(op::FALSE),
                    ("has_all", 0) => out.push(op::TRUE),
                    ("has_any", _) => {
                        out.extend_from_slice(&[op::HAS_ANY, n]);
                        its.iter().for_each(|i| put(out, *i, self.w.item));
                    }
                    _ => {
                        let mut body = Vec::new();
                        for i in &its {
                            body.push(op::HAS);
                            put(&mut body, *i, self.w.item);
                        }
                        out.extend_from_slice(&[op::ALL, n]);
                        out.extend_from_slice(
                            &u16::try_from(body.len()).unwrap_or(0).to_le_bytes(),
                        );
                        out.extend_from_slice(&body);
                    }
                }
            }
            "x.time_any" => {
                let m = u64::try_from(int(a, &p)?).map_err(|_| format!("{p}: expected a mask"))?;
                let ix = match self.masks.iter().position(|x| *x == m) {
                    Some(ix) => ix,
                    None => {
                        self.masks.push(m);
                        self.masks.len() - 1
                    }
                };
                r.time = true;
                out.extend_from_slice(&[
                    op::TIME_ANY,
                    u8::try_from(ix).map_err(|_| format!("{p}: more than 256 distinct masks"))?,
                ]);
            }
            "reach" => {
                if !goal {
                    return Err(format!("{p}: `reach` is allowed in the goal only"));
                }
                let reg = s(a, &p)?
                    .strip_prefix("region:")
                    .ok_or_else(|| format!("{p}: only region:<key> is supported"))?;
                let ix =
                    *self.regions.get(reg).ok_or_else(|| format!("{p}: unknown region {reg:?}"))?;
                r.region = true;
                out.push(op::REACH);
                out.extend_from_slice(&ix.to_le_bytes());
            }
            "ref" => {
                let name = s(a, &p)?;
                let k = self
                    .def_names
                    .iter()
                    .position(|d| *d == name)
                    .ok_or_else(|| format!("{p}: unknown def {name:?}"))?;
                self.def(k)?;
                self.reach(self.depth + self.def_depth[k], &p)?;
                let reads = self.def_reads[k].clone();
                r.merge(&reads);
                match self.def_at[k] {
                    // A constant def is inlined, so the bytes match a hand-written constant.
                    Some(0) => out.push(op::TRUE),
                    Some(1) => out.push(op::FALSE),
                    _ => {
                        out.push(op::REF);
                        put(out, u16::try_from(k).unwrap_or(0), self.w.def);
                    }
                }
            }
            // The rule of the thing a shuffle put in a slot: the slot's shared def.
            "at" => return self.at_rule(a, &p, out, r),
            _ => return Err(format!("{path}: unknown rule node {k:?}")),
        }
        Ok(())
    }

    /// A rule-position `at` read: a reference to the slot's shared def. Kept out of
    /// [`Compiler::b_node`] so the deep recursion there does not carry its locals.
    #[inline(never)]
    fn at_rule(&mut self, a: &Value, p: &str, out: &mut Vec<u8>, r: &mut Reads) -> R<()> {
        let d = self.shuffles.ok_or_else(|| format!("{p}: the world has no shuffles"))?;
        let def = json!({"ref": d.rule_def(a, p)?});
        self.b_node(&def, p, out, r, false)
    }

    /// A comparison with an `at` read on one side: compiled as the test of the slot's option
    /// it stands for; `false` when neither side is one. Out of line as [`Compiler::at_rule`].
    #[inline(never)]
    fn name_test(
        &mut self,
        op: &str,
        (x, y): (&Value, &Value),
        (path, p): (&str, &str),
        out: &mut Vec<u8>,
        r: &mut Reads,
    ) -> R<bool> {
        let Some(test) = self.shuffles.map(|d| d.name_test(op, x, y, p)).transpose()?.flatten()
        else {
            return Ok(false);
        };
        self.b_node(&test, path, out, r, false).map(|()| true)
    }

    fn i(&mut self, v: &Value, path: &str, out: &mut Vec<u8>, r: &mut Reads) -> R<()> {
        self.deeper(path)?;
        let x = self.i_node(v, path, out, r);
        self.depth -= 1;
        x
    }

    fn i_node(&mut self, v: &Value, path: &str, out: &mut Vec<u8>, r: &mut Reads) -> R<()> {
        if let Some(n) = v.as_i64() {
            let n = i16::try_from(n).map_err(|_| format!("{path}: {n} does not fit 16 bits"))?;
            out.push(op::INT);
            out.extend_from_slice(&n.to_le_bytes());
            return Ok(());
        }
        let o = v
            .as_object()
            .ok_or_else(|| format!("{path}: expected a number or an object, got {v}"))?;
        if let Some(k) = o.get("opt") {
            let p = format!("{path}.opt");
            let k = s(k, &p)?;
            let ix = self
                .options
                .iter()
                .position(|x| *x == k)
                .ok_or_else(|| format!("{p}: undeclared option {k:?}"))?;
            out.extend_from_slice(&[op::OPT, u8::try_from(ix).unwrap_or(0)]);
            return Ok(());
        }
        if let Some(name) = o.get("x.roll") {
            let p = format!("{path}.x.roll");
            let name = s(name, &p)?;
            let roll = self
                .rolls
                .iter()
                .position(|x| *x == name)
                .ok_or_else(|| format!("{p}: undeclared roll {name:?}"))?;
            let at_path = format!("{path}.at");
            let at = s(&o["at"], &at_path)?;
            let loc =
                *self.locs.get(at).ok_or_else(|| format!("{at_path}: unknown location {at:?}"))?;
            let key = (u8::try_from(roll).map_err(|_| format!("{p}: too many rolls"))?, loc);
            let slot = match self.slots.iter().position(|x| *x == key) {
                Some(s) => s,
                None => {
                    self.slots.push(key);
                    self.slots.len() - 1
                }
            };
            out.push(op::ROLL);
            out.extend_from_slice(&small::<u16>(slot, "roll reads")?.to_le_bytes());
            return Ok(());
        }
        if let Some(k) = o.get("count") {
            let i = self.item(k, &format!("{path}.count"))?;
            r.items.push(i);
            out.push(op::COUNT);
            put(out, i, self.w.item);
            return Ok(());
        }
        if let Some(k) = o.get("counter") {
            let c = u16::try_from(self.counter(s(k, &format!("{path}.counter"))?)).unwrap_or(0);
            r.counters.push(c);
            out.push(op::COUNTER);
            put(out, c, self.w.counter);
            return Ok(());
        }
        let p = format!("{path}.int");
        let a = arr(o.get("int").ok_or_else(|| format!("{path}: unknown integer node {v}"))?, &p)?;
        let (name, args) = a.split_first().ok_or_else(|| format!("{p}: empty"))?;
        let name = s(name, &format!("{p}[0]"))?;
        let (opcode, arity) = match name {
            "+" => (op::ADD, None),
            "min" => (op::MIN, None),
            "max" => (op::MAX, None),
            "*" => (op::MUL, Some(2)),
            "div" => (op::DIV, Some(2)),
            _ => return Err(format!("{p}: unknown integer operator {name:?}")),
        };
        if let Some(members) = (opcode == op::ADD && args.len() >= 2).then(|| group(args)).flatten()
        {
            let members = members
                .into_iter()
                .enumerate()
                .map(|(j, (k, cap))| Ok((self.item(&json!(k), &format!("{p}[{}]", j + 1))?, cap)))
                .collect::<R<Vec<_>>>()?;
            r.items.extend(members.iter().map(|m| m.0));
            let g = match self.groups.iter().position(|x| *x == members) {
                Some(g) => g,
                None => {
                    self.groups.push(members);
                    self.groups.len() - 1
                }
            };
            out.push(op::GROUP);
            out.extend_from_slice(&small::<u16>(g, "item groups")?.to_le_bytes());
            return Ok(());
        }
        match arity {
            Some(n) if args.len() != n => return Err(format!("{p}: {name} takes two operands")),
            None if args.is_empty() && opcode != op::ADD => {
                return Err(format!("{p}: {name} needs an operand"));
            }
            Some(_) => out.push(opcode),
            None => out.extend_from_slice(&[
                opcode,
                u8::try_from(args.len()).map_err(|_| format!("{p}: more than 255 operands"))?,
            ]),
        }
        if opcode == op::DIV && !args[1].as_i64().is_some_and(|d| d > 0) {
            return Err(format!("{p}: the divisor must be a positive constant"));
        }
        let mut reads = Vec::new();
        for (k, x) in args.iter().enumerate() {
            let mut rk = Reads::default();
            self.i(x, &format!("{p}[{}]", k + 1), out, &mut rk)?;
            reads.push(rk.state());
            r.merge(&rk);
        }
        // A product grows with what is collected only when the other factor is not negative.
        if opcode == op::MUL
            && ((reads[0] && !args[1].as_i64().is_some_and(|c| c >= 0))
                || (reads[1] && !args[0].as_i64().is_some_and(|c| c >= 0)))
        {
            return Err(format!(
                "{p}: a product that reads items needs a non-negative constant factor"
            ));
        }
        Ok(())
    }
}
