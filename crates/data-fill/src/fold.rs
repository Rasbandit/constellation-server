//! The per-seed fold: the section's bytecode with this seed's option and roll values folded
//! in, so a solve never re-evaluates what the seed already decided. Same bytecode layout; a
//! site whose rule folds to a constant points at offset 0 (true) or 1 (false). Built once per
//! seed, about the size of the section's bytecode.
//!
//! The folded rules are then also kept as bit rules ([`Bits`]) where they fit: an OR of terms,
//! each a set of atoms ("at least n of this item", "this counter is at least n", "this region
//! is reached") that must all hold, and at most one time window. A solve keeps one bit per atom
//! and answers such a rule with a few word tests; a rule of any other shape, or of more than
//! `config::RULE_BITS_MAX_TERMS` terms, stays bytecode.

use std::collections::HashMap;

use crate::section::{Node, compare, node, op, operand};
use crate::world::World;

/// Folded rules for one seed.
#[derive(Debug, Clone, Default)]
pub(crate) struct Folded {
    pub code: Vec<u8>,
    /// Rule offset per site.
    pub sites: Vec<u32>,
    /// Rule offset per def.
    pub defs: Vec<u32>,
    /// Rule offset per half-day.
    pub halves: Vec<u32>,
    pub goal: u32,
    /// The same rules as bit rules, where they fit.
    pub bits: Bits,
}

enum B {
    K(bool),
    Code(Vec<u8>),
}

enum I {
    K(i64),
    Code(Vec<u8>),
}

struct Folder<'a> {
    w: &'a World,
    code: &'a [u8],
    opts: &'a [i32],
    rolls: &'a [i16],
    defs: Vec<Option<Option<bool>>>,
}

/// Fold every rule of `w` for these values.
pub(crate) fn fold(w: &World, opts: &[i32], rolls: &[i16]) -> Folded {
    let mut f = Folder { w, code: w.code(), opts, rolls, defs: vec![None; w.defs()] };
    let mut out = Folded { code: vec![op::TRUE, op::FALSE], ..Folded::default() };
    let mut memo: HashMap<u32, u32> = HashMap::new();
    let mut top = |f: &mut Folder<'_>, out: &mut Folded, at: u32| -> u32 {
        if let Some(x) = memo.get(&at) {
            return *x;
        }
        let x = match f.b(at as usize).0 {
            B::K(v) => u32::from(!v),
            B::Code(c) => {
                let x = u32::try_from(out.code.len()).unwrap_or(1);
                out.code.extend_from_slice(&c);
                x
            }
        };
        memo.insert(at, x);
        x
    };
    out.defs = (0..w.defs()).map(|d| top(&mut f, &mut out, w.def(d))).collect();
    out.sites = (0..w.site_count()).map(|s| top(&mut f, &mut out, w.site(s).1)).collect();
    let halves = w.time.owned.map_or(0, |o| o.halves);
    out.halves = (0..halves).map(|h| top(&mut f, &mut out, w.half(h).1)).collect();
    out.goal = top(&mut f, &mut out, w.goal);
    out.bits = Bits::of(w, &out);
    out
}

impl Folder<'_> {
    fn def(&mut self, d: usize) -> Option<bool> {
        if let Some(k) = self.defs[d] {
            return k;
        }
        let k = match self.b(self.w.def(d) as usize).0 {
            B::K(v) => Some(v),
            B::Code(_) => None,
        };
        self.defs[d] = Some(k);
        k
    }

    fn b(&mut self, pc: usize) -> (B, usize) {
        let Some((n, at)) = node(self.code, pc, self.w.widths) else {
            return (B::K(false), pc + 1);
        };
        let orig = |end: usize, code: &[u8]| B::Code(code[pc..end].to_vec());
        match n {
            Node::Const(v) => (B::K(v), at),
            Node::All(k, len) | Node::Any(k, len) => {
                let all = matches!(n, Node::All(..));
                let stop = at + usize::from(len);
                let mut parts = 0usize;
                let mut body = Vec::new();
                let mut p = at;
                let mut decided = None;
                for _ in 0..k {
                    let (v, e) = self.b(p);
                    p = e;
                    match v {
                        B::K(v) if v == all => {}
                        B::K(v) => {
                            decided = Some(v);
                            break;
                        }
                        B::Code(c) => {
                            parts += 1;
                            body.extend_from_slice(&c);
                        }
                    }
                }
                if let Some(v) = decided {
                    return (B::K(v), stop);
                }
                match parts {
                    0 => (B::K(all), stop),
                    1 => (B::Code(body), stop),
                    m => {
                        // A folded constant can be a byte longer than the read it replaces, so
                        // a body near the limit may outgrow its length field: keep the
                        // original bytes, which read the same values at solve time.
                        let Ok(len) = u16::try_from(body.len()) else {
                            return (orig(stop, self.code), stop);
                        };
                        let mut c =
                            vec![if all { op::ALL } else { op::ANY }, u8::try_from(m).unwrap_or(0)];
                        c.extend_from_slice(&len.to_le_bytes());
                        c.extend_from_slice(&body);
                        (B::Code(c), stop)
                    }
                }
            }
            Node::HasAny(k) => {
                let end = at + usize::from(k) * usize::from(self.w.widths.item);
                (orig(end, self.code), end)
            }
            Node::Has(..) | Node::TimeAny(_) | Node::Reach(_) => (orig(at, self.code), at),
            Node::Ref(d) => match self.def(usize::from(d)) {
                Some(v) => (B::K(v), at),
                None => (orig(at, self.code), at),
            },
            Node::Not => {
                let (v, e) = self.b(at);
                // The loader allows `not` only over option-only rules, which always fold.
                (
                    match v {
                        B::K(v) => B::K(!v),
                        B::Code(_) => orig(e, self.code),
                    },
                    e,
                )
            }
            Node::Cmp(o) => {
                let (x, e1) = self.i(at);
                let (y, e2) = self.i(e1);
                if let (I::K(a), I::K(b)) = (&x, &y) {
                    return (B::K(compare(o, *a, *b)), e2);
                }
                let mut c = vec![op::CMP, o];
                self.emit(&mut c, x, at, e1);
                self.emit(&mut c, y, e1, e2);
                (B::Code(c), e2)
            }
            _ => (B::K(false), at),
        }
    }

    /// Append a folded integer: a constant that fits 16 bits as `INT`, otherwise the original
    /// bytes `pc..end` (they read the same option and roll values at solve time).
    fn emit(&self, out: &mut Vec<u8>, v: I, pc: usize, end: usize) {
        match v {
            I::K(k) => match i16::try_from(k) {
                Ok(k) => {
                    out.push(op::INT);
                    out.extend_from_slice(&k.to_le_bytes());
                }
                Err(_) => out.extend_from_slice(&self.code[pc..end]),
            },
            I::Code(c) => out.extend_from_slice(&c),
        }
    }

    fn i(&mut self, pc: usize) -> (I, usize) {
        let Some((n, at)) = node(self.code, pc, self.w.widths) else { return (I::K(0), pc + 1) };
        match n {
            Node::Int(v) => (I::K(i64::from(v)), at),
            Node::Opt(o) => (I::K(self.opts.get(usize::from(o)).map_or(0, |v| i64::from(*v))), at),
            Node::Roll(s) => {
                (I::K(self.rolls.get(usize::from(s)).map_or(0, |v| i64::from(*v))), at)
            }
            Node::Count(_) | Node::Counter(_) | Node::Group(_) => {
                (I::Code(self.code[pc..at].to_vec()), at)
            }
            Node::Fold(o, k) => {
                let mut args = Vec::new();
                let mut p = at;
                for _ in 0..k {
                    let (v, e) = self.i(p);
                    args.push((v, p, e));
                    p = e;
                }
                if args.iter().all(|(v, ..)| matches!(v, I::K(_))) {
                    let ks = args
                        .iter()
                        .filter_map(|(v, ..)| if let I::K(k) = v { Some(*k) } else { None });
                    let v = match o {
                        op::ADD => ks.fold(0i64, i64::saturating_add),
                        op::MIN => ks.min().unwrap_or(0),
                        _ => ks.max().unwrap_or(0),
                    };
                    return (I::K(v), p);
                }
                let mut c = vec![o, k];
                for (v, a, e) in args {
                    self.emit(&mut c, v, a, e);
                }
                (I::Code(c), p)
            }
            Node::Pair(o) => {
                let (x, e1) = self.i(at);
                let (y, e2) = self.i(e1);
                if let (I::K(a), I::K(b)) = (&x, &y) {
                    let v = if o == op::MUL {
                        a.saturating_mul(*b)
                    } else {
                        a.checked_div_euclid(*b).unwrap_or(0)
                    };
                    return (I::K(v), e2);
                }
                let mut c = vec![o];
                self.emit(&mut c, x, at, e1);
                self.emit(&mut c, y, e1, e2);
                (I::Code(c), e2)
            }
            _ => (I::K(0), at),
        }
    }
}

/// Atom kinds: what the value an atom's threshold is held against counts.
pub(crate) const ITEM: usize = 0;
/// A counter's value.
pub(crate) const COUNTER: usize = 1;
/// An item group's sum.
pub(crate) const GROUP: usize = 2;
/// Whether a region is reached (1 or 0).
pub(crate) const REGION: usize = 3;

/// Classes of rule sites, as [`Bits::classes`] counts them: a constant, an AND/OR of "has the
/// item", "the event fired" and "the region is reached", the same with a count above one or a
/// group sum, either with a time window, and whatever stays bytecode.
pub const CLASSES: [&str; 5] = ["constant", "plain", "counted", "timed", "bytecode"];

/// One atom: the value of kind `kind` at index `idx` is at least `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Atom {
    pub kind: usize,
    pub idx: u16,
    pub n: u16,
}

/// One way a rule holds: every test of `tests[at..at + n]` (a word of the atom set has all the
/// bits of a mask) and, when `timed`, a slice of `time` in the region's mask.
#[derive(Debug, Clone, Copy)]
struct Term {
    time: u64,
    at: u32,
    n: u16,
    timed: bool,
}

/// A rule's range for "stays bytecode".
const BYTECODE: (u32, u32) = (u32::MAX, 0);

/// The folded rules as bit rules.
#[derive(Debug, Clone, Default)]
pub(crate) struct Bits {
    /// Per fact (site, then half-day, then the goal): its terms, or [`BYTECODE`].
    rules: Vec<(u32, u32)>,
    terms: Vec<Term>,
    /// Word of the atom set and the bits it must have.
    tests: Vec<(u32, u64)>,
    /// What each bit says.
    pub atoms: Vec<Atom>,
    /// Per kind and index: the atoms over that value, as (threshold, bit).
    by: [Vec<Vec<(u16, u32)>>; 4],
    /// Rule sites per class ([`CLASSES`]).
    pub classes: [u32; 5],
}

/// A conjunction while compiling: atom bits, ascending, and at most one time window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct And {
    atoms: Vec<u32>,
    time: Option<u64>,
}

struct Builder<'a> {
    w: &'a World,
    f: &'a Folded,
    ids: HashMap<Atom, u32>,
    atoms: Vec<Atom>,
    /// Per def: not compiled yet, or its terms (`None`: stays bytecode).
    defs: Vec<Option<Option<Vec<And>>>>,
}

impl Bits {
    fn of(w: &World, f: &Folded) -> Self {
        let mut b =
            Builder { w, f, ids: HashMap::new(), atoms: Vec::new(), defs: vec![None; w.defs()] };
        let mut out = Bits::default();
        let mut memo: HashMap<u32, ((u32, u32), usize)> = HashMap::new();
        let rules = f.sites.iter().chain(&f.halves).chain([&f.goal]);
        for (fact, at) in rules.enumerate() {
            let (range, class) = *memo.entry(*at).or_insert_with(|| match b.dnf(*at as usize) {
                Some((terms, _)) => (out.push(&terms), b.class(&terms)),
                None => (BYTECODE, 4),
            });
            out.rules.push(range);
            out.classes[class] += u32::from(fact < f.sites.len());
        }
        for (bit, a) in b.atoms.iter().enumerate() {
            let list = &mut out.by[a.kind];
            if list.len() <= usize::from(a.idx) {
                list.resize(usize::from(a.idx) + 1, Vec::new());
            }
            list[usize::from(a.idx)].push((a.n, u32::try_from(bit).unwrap_or(u32::MAX)));
        }
        out.atoms = b.atoms;
        out
    }

    /// Append a rule's terms; its range in `terms`.
    fn push(&mut self, terms: &[And]) -> (u32, u32) {
        let at = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let first = at(self.terms.len());
        for t in terms {
            let start = self.tests.len();
            for bit in &t.atoms {
                let (word, mask) = (bit / 64, 1u64 << (bit % 64));
                match self.tests[start..].last_mut() {
                    Some(last) if last.0 == word => last.1 |= mask,
                    _ => self.tests.push((word, mask)),
                }
            }
            self.terms.push(Term {
                time: t.time.unwrap_or(0),
                at: at(start),
                n: u16::try_from(self.tests.len() - start).unwrap_or(u16::MAX),
                timed: t.time.is_some(),
            });
        }
        (first, at(self.terms.len()))
    }

    /// Whether fact `f` holds for the atom set `set` and region mask `t`; `None` when its rule
    /// stays bytecode.
    #[inline]
    pub fn holds(&self, set: &[u64], f: usize, t: u64) -> Option<bool> {
        let (a, z) = *self.rules.get(f)?;
        let terms = self.terms.get(a as usize..z as usize)?;
        Some(terms.iter().any(|term| {
            (!term.timed || t & term.time != 0)
                && self.tests[term.at as usize..][..usize::from(term.n)]
                    .iter()
                    .all(|(w, m)| set[*w as usize] & m == *m)
        }))
    }

    /// Words an atom set takes.
    pub fn words(&self) -> usize {
        self.atoms.len().div_ceil(64)
    }

    /// The value of kind `kind` at `idx` is now `v`: set and clear its atoms in `set`.
    #[inline]
    pub fn set(&self, set: &mut [u64], kind: usize, idx: usize, v: i64) {
        let Some(list) = self.by[kind].get(idx) else { return };
        for &(n, bit) in list {
            let (w, m) = (bit as usize / 64, 1u64 << (bit % 64));
            if v >= i64::from(n) {
                set[w] |= m;
            } else {
                set[w] &= !m;
            }
        }
    }

    /// Bytes the bit rules take (counted from the lists' lengths).
    pub fn bytes(&self) -> usize {
        let by: usize = self.by.iter().flatten().map(|l| 24 + l.len() * 8).sum();
        self.rules.len() * 8
            + self.terms.len() * 16
            + self.tests.len() * 16
            + self.atoms.len() * 16
            + by
    }
}

impl Builder<'_> {
    fn atom(&mut self, kind: usize, idx: u16, n: u16) -> Vec<And> {
        let a = Atom { kind, idx, n };
        let next = u32::try_from(self.atoms.len()).unwrap_or(u32::MAX);
        let bit = *self.ids.entry(a).or_insert_with(|| {
            self.atoms.push(a);
            next
        });
        vec![And { atoms: vec![bit], time: None }]
    }

    /// The class of a compiled rule ([`CLASSES`]).
    fn class(&self, terms: &[And]) -> usize {
        let atoms = || terms.iter().flat_map(|t| &t.atoms).map(|b| self.atoms[*b as usize]);
        if terms.iter().any(|t| t.time.is_some()) {
            3
        } else if atoms().next().is_none() {
            0
        } else {
            1 + usize::from(atoms().any(|a| a.n > 1 || a.kind == GROUP))
        }
    }

    /// `a` or `b`; `None` past the term cap.
    fn or(mut a: Vec<And>, b: Vec<And>) -> Option<Vec<And>> {
        for t in b {
            if !a.contains(&t) {
                a.push(t);
            }
        }
        (a.len() <= config::RULE_BITS_MAX_TERMS).then_some(a)
    }

    /// `a` and `b`; `None` past the term cap, or for a term that would test two time windows.
    fn and(a: &[And], b: &[And]) -> Option<Vec<And>> {
        if a.len() * b.len() > config::RULE_BITS_MAX_TERMS {
            return None;
        }
        let mut out = Vec::new();
        for x in a {
            for y in b {
                let time = match (x.time, y.time) {
                    (Some(p), Some(q)) if p != q => return None,
                    (p, q) => p.or(q),
                };
                let mut atoms = [x.atoms.as_slice(), &y.atoms].concat();
                atoms.sort_unstable();
                atoms.dedup();
                let t = And { atoms, time };
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        Some(out)
    }

    /// The boolean node at `pc` as terms, and the offset after it; `None` for a shape that
    /// stays bytecode.
    fn dnf(&mut self, pc: usize) -> Option<(Vec<And>, usize)> {
        let (w, f) = (self.w, self.f);
        let code = &f.code;
        let (n, at) = node(code, pc, w.widths)?;
        Some(match n {
            Node::Const(v) => (if v { vec![And::default()] } else { Vec::new() }, at),
            Node::All(k, len) | Node::Any(k, len) => {
                let all = matches!(n, Node::All(..));
                let mut acc = if all { vec![And::default()] } else { Vec::new() };
                let mut p = at;
                for _ in 0..k {
                    let (d, e) = self.dnf(p)?;
                    p = e;
                    acc = if all { Self::and(&acc, &d)? } else { Self::or(acc, d)? };
                }
                (acc, at + usize::from(len))
            }
            Node::Has(i, c) if c >= 1 => (self.atom(ITEM, i, u16::from(c)), at),
            Node::HasAny(k) => {
                let iw = w.widths.item;
                let mut acc = Vec::new();
                for j in 0..usize::from(k) {
                    let i = operand(code, at + j * usize::from(iw), iw)?;
                    acc = Self::or(acc, self.atom(ITEM, i, 1))?;
                }
                (acc, at + usize::from(k) * usize::from(iw))
            }
            Node::Cmp(o) => {
                let (x, e) = self.int(at)?;
                let (y, e) = self.int(e)?;
                // `CMP_OPS` order: 2 lt, 3 le, 4 gt, 5 ge. A value against a constant floor.
                let ((kind, idx), floor) = match (x, y, o) {
                    (Ok(read), Err(k), 5) | (Err(k), Ok(read), 3) => (read, k),
                    (Ok(read), Err(k), 4) | (Err(k), Ok(read), 2) => (read, k + 1),
                    _ => return None,
                };
                let n = u16::try_from(floor).ok().filter(|n| *n >= 1)?;
                (self.atom(kind, idx, n), e)
            }
            Node::TimeAny(m) => {
                (vec![And { atoms: Vec::new(), time: Some(w.mask(usize::from(m))) }], at)
            }
            Node::Ref(d) => {
                let d = usize::from(d);
                if self.defs.get(d)?.is_none() {
                    let terms = self.dnf(*self.f.defs.get(d)? as usize).map(|x| x.0);
                    self.defs[d] = Some(terms);
                }
                (self.defs[d].clone()??, at)
            }
            Node::Reach(r) => (self.atom(REGION, r, 1), at),
            _ => return None,
        })
    }

    /// The integer node at `pc`: a value an atom can count (kind and index) or a constant.
    fn int(&self, pc: usize) -> Option<(Result<(usize, u16), i64>, usize)> {
        let (n, at) = node(&self.f.code, pc, self.w.widths)?;
        Some((
            match n {
                Node::Int(v) => Err(i64::from(v)),
                Node::Count(i) => Ok((ITEM, i)),
                Node::Counter(c) => Ok((COUNTER, c)),
                Node::Group(g) => Ok((GROUP, g)),
                _ => return None,
            },
            at,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::CLASSES;
    use crate::fill::{Instance, Plan};
    use crate::world::World;
    use serde_json::{Value, json};

    /// One region whose check `l{k}` has rule `rules[k]`, over items `a` to `j`, a counter lit
    /// by two events, and a group sum.
    fn world(rules: &[Value]) -> (World, Instance) {
        let keys: Vec<String> = ('a'..='j').map(String::from).collect();
        let checks: Vec<Value> = rules
            .iter()
            .enumerate()
            .map(|(k, r)| json!({"loc": format!("l{k}"), "rule": r}))
            .collect();
        let m = json!({
            "options": [{"key": "hard", "default": 1}],
            "items": keys.iter().enumerate().map(|(k, i)| json!({"key": i, "id": k + 1}))
                .collect::<Vec<_>>(),
            "locations": (0..rules.len()).map(|k| json!({"key": format!("l{k}"), "id": k + 1}))
                .collect::<Vec<_>>(),
            "start": "s",
            "x.time": {"slices": 2, "start_mask": 1},
            "defs": {"d": {"any": [{"has": "a"}, {"has": "b", "n": 2}]}},
            "regions": [{"key": "s", "can_stay": false, "stays": [], "exits": [],
                         "events": [{"event": "lit", "rule": {"has": "c"}},
                                    {"event": "lit", "rule": {"has": "d"}}],
                         "checks": checks}],
            "goal": {"reach": "region:s"}
        });
        let inst = Instance {
            locations: (0..rules.len()).map(|k| format!("l{k}")).collect(),
            ..Instance::default()
        };
        (World::load(&m).unwrap(), inst)
    }

    fn any_of(items: [&str; 2]) -> Value {
        json!({"any": [{"has": items[0]}, {"has": items[1]}]})
    }

    /// Every class of rule, and the term cap: an AND of four ORs of two is 16 terms and
    /// fits, one of five is 32 and stays bytecode. Whatever the class, a solve with the bit
    /// rules reaches what a solve of the bytecode reaches, for every inventory tried.
    #[test]
    fn rules_are_classed_capped_and_answer_as_their_bytecode_does() {
        let pairs = [["a", "b"], ["c", "d"], ["e", "f"], ["g", "h"], ["i", "j"]];
        let wide =
            |n: usize| json!({"all": pairs[..n].iter().map(|p| any_of(*p)).collect::<Vec<_>>()});
        let sum = json!({"int": ["+", {"count": "a"}, {"int": ["min", {"count": "b"}, 1]}]});
        let rules = [
            (json!({"true": {}}), "constant"),
            (json!({"eq": [{"opt": "hard"}, 0]}), "constant"),
            (json!({"all": [{"has": "a"}, {"has": "e"}]}), "plain"),
            (json!({"any": [{"ref": "d"}, {"ge": [{"counter": "lit"}, 1]}]}), "counted"),
            (json!({"gt": [{"counter": "lit"}, 1]}), "counted"),
            (json!({"ge": [sum, 2]}), "counted"),
            (json!({"all": [{"has": "c"}, {"x.time_any": 1}]}), "timed"),
            (json!({"le": [3, {"count": "a"}]}), "counted"),
            (json!({"ge": [{"int": ["*", {"count": "a"}, 2]}, 2]}), "bytecode"),
            (wide(4), "plain"),
            (wide(5), "bytecode"),
        ];
        let (w, inst) = world(&rules.clone().map(|r| r.0));
        let plan = Plan::new(&w, &inst).unwrap();
        let mut want = [0u32; 5];
        for (_, class) in &rules {
            want[CLASSES.iter().position(|c| c == class).unwrap()] += 1;
        }
        // The two events are plain rule sites too.
        want[1] += 2;
        assert_eq!(plan.bit_rules().0, want, "{CLASSES:?}");
        assert_eq!(config::RULE_BITS_MAX_TERMS, 16, "the cap `wide` is written for");
        let plain = plan.clone().interpreted();
        assert_eq!(plain.bit_rules(), ([0; 5], 0));
        // Inventories: every item alone, growing prefixes with up to three copies of each.
        let mut held: Vec<Vec<u16>> = (0..10).map(|i| vec![i]).collect();
        held.push(Vec::new());
        for copies in 1..=3 {
            for upto in 1..=10u16 {
                held.push((0..upto).flat_map(|i| vec![i; copies]).collect());
            }
        }
        for inv in &held {
            let (a, b) = (plan.solve(&w, inv), plain.solve(&w, inv));
            crate::reach::assert_same(&w, &a, &b, &format!("holding {inv:?}"));
        }
    }
}
