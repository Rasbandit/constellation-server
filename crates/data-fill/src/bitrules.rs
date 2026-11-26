//! A world's rules as bit rules, where they fit: an OR of terms, each a set of atoms and time
//! windows that must all hold. An atom is "at least n of this item", "this counter
//! (or group sum) is at least n", "this region is reached", or a comparison over options and
//! rolls alone, which a seed decides once. A solve keeps one bit per atom and answers such a
//! rule with a few bit tests; a rule of any other shape, or of more than
//! `config::RULE_BITS_MAX_TERMS` terms, stays bytecode.
//!
//! Built once per loaded world from its bytecode ([`Bits::of`]) and shared by every player and
//! seed of it: a seed only evaluates the option atoms ([`Bits::statics`]).

use std::collections::HashMap;

use crate::section::{Node, node, operand};
use crate::world::World;

/// Atom kinds: what the value an atom's threshold is held against counts. An item's copies.
pub(crate) const ITEM: usize = 0;
/// A counter's value.
pub(crate) const COUNTER: usize = 1;
/// An item group's sum.
pub(crate) const GROUP: usize = 2;
/// Whether a region is reached (1 or 0).
pub(crate) const REGION: usize = 3;
/// An option-only rule of the bytecode, decided per seed (`idx` is its place in
/// [`Bits::statics`]).
pub(crate) const STATIC: usize = 4;

/// Classes of rule sites, as [`Bits::classes`] counts them: no atom a solve can change (a
/// constant, or options only), an AND/OR of "has the item", "the event fired" and "the region
/// is reached", the same with a count above one or a group sum, either with a time window, and
/// whatever stays bytecode.
pub const CLASSES: [&str; 5] = ["constant", "plain", "counted", "timed", "bytecode"];

/// One atom: the value of kind `kind` at index `idx` is at least `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Atom {
    pub kind: u8,
    pub idx: u16,
    pub n: u16,
}

/// One way a rule holds: every test of `tests[at..at + n]` passes. A test is an atom bit that
/// must be set or, with [`TIME`], a window of `times` the region's mask must have a slice of.
#[derive(Debug, Clone, Copy)]
struct Term {
    at: u32,
    n: u16,
}

/// Marks a test as a time window: the rest of it is the window's place in `times`.
const TIME: u16 = 1 << 15;
/// A rule's range for "stays bytecode".
const BYTECODE: (u32, u32) = (u32::MAX, 0);

/// A world's bit rules.
#[derive(Debug, Clone)]
pub(crate) struct Bits {
    /// Per fact (site, then half-day, then the goal): its terms, or [`BYTECODE`].
    rules: Vec<(u32, u32)>,
    terms: Vec<Term>,
    /// Atom bits and time windows ([`TIME`]).
    tests: Vec<u16>,
    times: Vec<u64>,
    /// What each bit says.
    pub atoms: Vec<Atom>,
    /// Per kind of value a solve changes: where each index's atoms start in the list of
    /// (threshold, bit).
    by: [(Vec<u32>, Vec<(u16, u16)>); 4],
    /// The option atoms: bit, and the offset of the option-only rule that decides it.
    pub statics: Vec<(u16, u32)>,
    /// Rule sites per class ([`CLASSES`]).
    pub classes: [u32; 5],
}

/// No bit rules: every rule stays bytecode.
pub(crate) static NONE: Bits = Bits {
    rules: Vec::new(),
    terms: Vec::new(),
    tests: Vec::new(),
    times: Vec::new(),
    atoms: Vec::new(),
    by: [const { (Vec::new(), Vec::new()) }; 4],
    statics: Vec::new(),
    classes: [0; 5],
};

/// A conjunction while compiling: its tests (atom bits and time windows), ascending.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct And {
    atoms: Vec<u16>,
}

/// An integer node while compiling: a value an atom can count, a constant, or anything else
/// that reads options and rolls only.
enum Int {
    Read(usize, u16),
    K(i64),
    Fixed,
}

struct Builder<'a> {
    w: &'a World,
    ids: HashMap<Atom, u16>,
    atoms: Vec<Atom>,
    statics: Vec<(u16, u32)>,
    times: Vec<u64>,
    /// Per def: not compiled yet, or its terms (`None`: stays bytecode).
    defs: Vec<Option<Option<Vec<And>>>>,
}

impl Bits {
    /// The bit rules of a loaded world.
    pub fn of(w: &World) -> Self {
        let mut b = Builder {
            w,
            ids: HashMap::new(),
            atoms: Vec::new(),
            statics: Vec::new(),
            times: Vec::new(),
            defs: vec![None; w.defs()],
        };
        let mut out = Bits::clone(&NONE);
        let mut memo: HashMap<u32, ((u32, u32), usize)> = HashMap::new();
        let (sites, halves) = (w.site_count(), w.time.owned.map_or(0, |o| o.halves));
        let rules =
            (0..sites).map(|s| w.site(s).1).chain((0..halves).map(|h| w.half(h).1)).chain([w.goal]);
        for (fact, at) in rules.enumerate() {
            let (range, class) = *memo.entry(at).or_insert_with(|| match b.dnf(at as usize) {
                Some((terms, _)) => (out.push(&terms), b.class(&terms)),
                None => (BYTECODE, 4),
            });
            out.rules.push(range);
            out.classes[class] += u32::from(fact < sites);
        }
        let sizes = [w.items(), w.counters(), w.groups(), w.regions()];
        for (kind, size) in sizes.into_iter().enumerate() {
            let mut of: Vec<(u16, u16, u16)> = (0u16..)
                .zip(&b.atoms)
                .filter(|(_, a)| usize::from(a.kind) == kind)
                .map(|(bit, a)| (a.idx, a.n, bit))
                .collect();
            of.sort_unstable();
            let at = |i: usize| of.partition_point(|x| usize::from(x.0) < i);
            out.by[kind] = (
                (0..=size).map(|i| u32::try_from(at(i)).unwrap_or(u32::MAX)).collect(),
                of.iter().map(|x| (x.1, x.2)).collect(),
            );
        }
        out.atoms = b.atoms;
        out.statics = b.statics;
        out.times = b.times;
        out
    }

    /// Append a rule's terms; its range in `terms`.
    fn push(&mut self, terms: &[And]) -> (u32, u32) {
        let at = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let first = at(self.terms.len());
        for t in terms {
            self.terms.push(Term {
                at: at(self.tests.len()),
                n: u16::try_from(t.atoms.len()).unwrap_or(u16::MAX),
            });
            self.tests.extend_from_slice(&t.atoms);
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
            self.tests[term.at as usize..][..usize::from(term.n)].iter().all(|b| {
                if b & TIME == 0 {
                    set[usize::from(b >> 6)] >> (b & 63) & 1 == 1
                } else {
                    t & self.times[usize::from(b & !TIME)] != 0
                }
            })
        }))
    }

    /// Words an atom set takes.
    pub fn words(&self) -> usize {
        self.atoms.len().div_ceil(64)
    }

    /// The value of kind `kind` at `idx` is now `v`: set and clear its atoms in `set`.
    #[inline]
    pub fn set(&self, set: &mut [u64], kind: usize, idx: usize, v: i64) {
        let (starts, list) = &self.by[kind];
        let (Some(a), Some(z)) = (starts.get(idx), starts.get(idx + 1)) else { return };
        for &(n, bit) in &list[*a as usize..*z as usize] {
            let (w, m) = (usize::from(bit >> 6), 1u64 << (bit & 63));
            if v >= i64::from(n) {
                set[w] |= m;
            } else {
                set[w] &= !m;
            }
        }
    }

    /// Bytes the bit rules take (counted from the lists' lengths).
    pub fn bytes(&self) -> usize {
        let by: usize = self.by.iter().map(|(s, l)| s.len() * 4 + l.len() * 4).sum();
        let lists = self.rules.len() * 8 + self.terms.len() * 8 + self.tests.len() * 2;
        lists + self.times.len() * 8 + self.atoms.len() * 6 + self.statics.len() * 8 + by
    }
}

impl Builder<'_> {
    /// The one-atom rule; `None` when the atom bits no longer fit 16 bits.
    fn atom(&mut self, kind: usize, idx: u16, n: u16) -> Option<Vec<And>> {
        let a = Atom { kind: u8::try_from(kind).ok()?, idx, n };
        let bit = match self.ids.get(&a) {
            Some(bit) => *bit,
            None => {
                let bit = u16::try_from(self.atoms.len()).ok().filter(|b| *b < TIME)?;
                self.atoms.push(a);
                self.ids.insert(a, bit);
                bit
            }
        };
        Some(vec![And { atoms: vec![bit] }])
    }

    /// The atom of the option-only rule at `pc`.
    fn fixed(&mut self, pc: usize) -> Option<Vec<And>> {
        let pc = u32::try_from(pc).ok()?;
        let at = self.statics.iter().position(|s| s.1 == pc).unwrap_or(self.statics.len());
        let terms = self.atom(STATIC, u16::try_from(at).ok()?, 1)?;
        if at == self.statics.len() {
            self.statics.push((terms[0].atoms[0], pc));
        }
        Some(terms)
    }

    /// The class of a compiled rule ([`CLASSES`]).
    fn class(&self, terms: &[And]) -> usize {
        let tests = || terms.iter().flat_map(|t| &t.atoms);
        let mut atoms = tests().map(|b| self.atoms[usize::from(*b)]);
        let kind = |a: &Atom, k: usize| usize::from(a.kind) == k;
        if tests().any(|b| b & TIME != 0) {
            3
        } else if atoms.clone().all(|a| kind(&a, STATIC)) {
            0
        } else {
            1 + usize::from(atoms.any(|a| a.n > 1 || kind(&a, GROUP)))
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

    /// `a` and `b`; `None` past the term cap.
    fn and(a: &[And], b: &[And]) -> Option<Vec<And>> {
        if a.len() * b.len() > config::RULE_BITS_MAX_TERMS {
            return None;
        }
        let mut out = Vec::new();
        for x in a {
            for y in b {
                let mut atoms = [x.atoms.as_slice(), &y.atoms].concat();
                atoms.sort_unstable();
                atoms.dedup();
                let t = And { atoms };
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
        let w = self.w;
        let code = w.code();
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
            Node::Has(i, c) if c >= 1 => (self.atom(ITEM, i, u16::from(c))?, at),
            Node::HasAny(k) => {
                let iw = w.widths.item;
                let mut acc = Vec::new();
                for j in 0..usize::from(k) {
                    let i = operand(code, at + j * usize::from(iw), iw)?;
                    acc = Self::or(acc, self.atom(ITEM, i, 1)?)?;
                }
                (acc, at + usize::from(k) * usize::from(iw))
            }
            Node::Cmp(o) => {
                let (x, e) = self.int(at)?;
                let (y, e) = self.int(e)?;
                // `CMP_OPS` order: 2 lt, 3 le, 4 gt, 5 ge. A value against a constant floor.
                let (kind, idx, floor) = match (x, y, o) {
                    (Int::Read(kind, i), Int::K(k), 5) | (Int::K(k), Int::Read(kind, i), 3) => {
                        (kind, i, k)
                    }
                    (Int::Read(kind, i), Int::K(k), 4) | (Int::K(k), Int::Read(kind, i), 2) => {
                        (kind, i, k + 1)
                    }
                    (Int::Read(..), ..) | (_, Int::Read(..), _) => return None,
                    // Options and rolls on both sides: the seed decides it.
                    _ => return Some((self.fixed(pc)?, e)),
                };
                let n = u16::try_from(floor).ok().filter(|n| *n >= 1)?;
                (self.atom(kind, idx, n)?, e)
            }
            Node::TimeAny(m) => {
                let mask = w.mask(usize::from(m));
                let k = self.times.iter().position(|x| *x == mask).unwrap_or(self.times.len());
                if k == self.times.len() {
                    self.times.push(mask);
                }
                // A section holds at most 256 distinct masks.
                (vec![And { atoms: vec![TIME | u16::try_from(k).ok()?] }], at)
            }
            Node::Ref(d) => {
                let d = usize::from(d);
                if self.defs.get(d)?.is_none() {
                    let terms = self.dnf(w.def(d) as usize).map(|x| x.0);
                    self.defs[d] = Some(terms);
                }
                (self.defs[d].clone()??, at)
            }
            Node::Reach(r) => (self.atom(REGION, r, 1)?, at),
            // The loader allows `not` only over option-only rules: the seed decides it.
            Node::Not => {
                let (_, e) = self.dnf(at)?;
                (self.fixed(pc)?, e)
            }
            _ => return None,
        })
    }

    /// The integer node at `pc` and the offset after it; `None` for arithmetic over values a
    /// solve changes.
    fn int(&self, pc: usize) -> Option<(Int, usize)> {
        let (n, at) = node(self.w.code(), pc, self.w.widths)?;
        Some(match n {
            Node::Int(v) => (Int::K(i64::from(v)), at),
            Node::Count(i) => (Int::Read(ITEM, i), at),
            Node::Counter(c) => (Int::Read(COUNTER, c), at),
            Node::Group(g) => (Int::Read(GROUP, g), at),
            Node::Opt(_) | Node::Roll(_) => (Int::Fixed, at),
            Node::Fold(_, k) => (Int::Fixed, self.fixed_ints(at, usize::from(k))?),
            Node::Pair(_) => (Int::Fixed, self.fixed_ints(at, 2)?),
            _ => return None,
        })
    }

    /// The offset after `k` integer nodes from `pc` that read options, rolls and constants only.
    fn fixed_ints(&self, mut pc: usize, k: usize) -> Option<usize> {
        for _ in 0..k {
            let (x, e) = self.int(pc)?;
            if matches!(x, Int::Read(..)) {
                return None;
            }
            pc = e;
        }
        Some(pc)
    }
}

#[cfg(test)]
mod tests {
    use super::CLASSES;
    use crate::fill::Plan;
    use crate::fold::tests::{any_of, world};
    use serde_json::json;

    /// Every class of rule, and the term cap: an AND of four ORs of two is 16 terms and
    /// fits, one of five is 32 and stays bytecode. Whatever the class, a solve with the bit
    /// rules reaches what a solve of the bytecode reaches, for every inventory tried and for
    /// both values of the option.
    #[test]
    fn rules_are_classed_capped_and_answer_as_their_bytecode_does() {
        let pairs = [["a", "b"], ["c", "d"], ["e", "f"], ["g", "h"], ["i", "j"]];
        let wide =
            |n: usize| json!({"all": pairs[..n].iter().map(|p| any_of(*p)).collect::<Vec<_>>()});
        let sum = json!({"int": ["+", {"count": "a"}, {"int": ["min", {"count": "b"}, 1]}]});
        let easy = json!({"eq": [{"opt": "hard"}, 0]});
        let rules = [
            (json!({"true": {}}), "constant"),
            (easy.clone(), "constant"),
            (json!({"not": easy}), "constant"),
            (json!({"all": [{"has": "a"}, {"has": "e"}]}), "plain"),
            (json!({"any": [{"all": [easy, {"has": "a"}]}, {"has": "e"}]}), "plain"),
            (json!({"any": [{"ref": "d"}, {"ge": [{"counter": "lit"}, 1]}]}), "counted"),
            (json!({"gt": [{"counter": "lit"}, 1]}), "counted"),
            (json!({"ge": [sum, 2]}), "counted"),
            (json!({"all": [{"has": "c"}, {"x.time_any": 1}]}), "timed"),
            (json!({"all": [{"x.time_any": 1}, {"x.time_any": 3}, {"has": "b"}]}), "timed"),
            (json!({"le": [3, {"count": "a"}]}), "counted"),
            (json!({"ge": [{"count": "a"}, {"int": ["+", {"opt": "hard"}, 1]}]}), "bytecode"),
            (json!({"ge": [{"int": ["*", {"count": "a"}, 2]}, 2]}), "bytecode"),
            (wide(4), "plain"),
            (wide(5), "bytecode"),
        ];
        let (w, mut inst) = world(&rules.clone().map(|r| r.0));
        let mut want = [0u32; 5];
        for (_, class) in &rules {
            want[CLASSES.iter().position(|c| c == class).unwrap()] += 1;
        }
        // The two events are plain rule sites too.
        want[1] += 2;
        assert_eq!(config::RULE_BITS_MAX_TERMS, 16, "the cap `wide` is written for");
        // Inventories: every item alone, growing prefixes with up to three copies of each.
        let mut held: Vec<Vec<u16>> = (0..10).map(|i| vec![i]).collect();
        held.push(Vec::new());
        for copies in 1..=3 {
            for upto in 1..=10u16 {
                held.push((0..upto).flat_map(|i| vec![i; copies]).collect());
            }
        }
        for hard in [0, 1] {
            inst.options.insert("hard".to_owned(), hard);
            let plan = Plan::new(&w, &inst).unwrap();
            assert_eq!(plan.bit_rules(&w).0, want, "{CLASSES:?}");
            let plain = plan.clone().interpreted();
            assert_eq!(plain.bit_rules(&w), ([0; 5], 0));
            for inv in &held {
                let (a, b) = (plan.solve(&w, inv), plain.solve(&w, inv));
                crate::reach::assert_same(&w, &a, &b, &format!("hard {hard}, holding {inv:?}"));
                let reached = |l: u32| a.reached(l);
                assert_eq!((reached(1), reached(2)), (hard == 0, hard == 1), "the option rules");
            }
        }
    }
}
