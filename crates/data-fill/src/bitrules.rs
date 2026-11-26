//! A world's rules as bit rules, where they fit: an OR of terms, each a set of atoms and time
//! windows that must all hold. An atom is "at least n of this item", "this counter
//! (or group sum) is at least n", "this region is reached", or a comparison over options and
//! rolls alone, which a seed decides once. A solve keeps one bit per atom and answers such a
//! rule with a few bit tests; a rule of any other shape, of more than
//! `config::RULE_BITS_MAX_TERMS` terms or of a term of more than
//! `config::RULE_BITS_MAX_TERM_TESTS` tests, stays bytecode, and so does every rule once the
//! world holds `config::RULE_BITS_MAX_WORLD_TESTS` tests.
//!
//! Built once per loaded world from its bytecode ([`Bits::of`]) and read by every player and
//! seed of it: a seed only evaluates the option atoms ([`Bits::statics`]). A room that clones
//! its worlds clones their bit rules with them.

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

/// Whether bit `b` of the atom set `set` is set.
#[inline]
pub(crate) fn bit(set: &[u64], b: u16) -> bool {
    set[usize::from(b >> 6)] >> (b & 63) & 1 == 1
}

/// Set (`on`) or clear bit `b` of the atom set `set`.
#[inline]
pub(crate) fn put(set: &mut [u64], b: u16, on: bool) {
    let (w, m) = (usize::from(b >> 6), 1u64 << (b & 63));
    if on {
        set[w] |= m;
    } else {
        set[w] &= !m;
    }
}

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
    /// Per option-only rule offset: its place in `statics`.
    fixed: HashMap<u32, usize>,
    times: Vec<u64>,
    /// Per def: not compiled yet, or its terms (`None`: stays bytecode).
    defs: Vec<Option<Option<Vec<And>>>>,
}

impl Bits {
    /// The bit rules of a loaded world.
    pub fn of(w: &World) -> Self {
        Self::within(w, config::RULE_BITS_MAX_WORLD_TESTS)
    }

    /// [`Bits::of`] holding at most `budget` tests in all: a rule that would pass it stays
    /// bytecode.
    fn within(w: &World, budget: usize) -> Self {
        let mut b = Builder {
            w,
            ids: HashMap::new(),
            atoms: Vec::new(),
            statics: Vec::new(),
            fixed: HashMap::new(),
            times: Vec::new(),
            defs: vec![None; w.defs()],
        };
        let mut out = Bits::clone(&NONE);
        let mut memo: HashMap<u32, ((u32, u32), usize)> = HashMap::new();
        let (sites, halves) = (w.site_count(), w.time.owned.map_or(0, |o| o.halves));
        let rules =
            (0..sites).map(|s| w.site(s).1).chain((0..halves).map(|h| w.half(h).1)).chain([w.goal]);
        for (fact, at) in rules.enumerate() {
            let (range, class) = *memo.entry(at).or_insert_with(|| {
                let fits = |t: &[And]| {
                    out.tests.len() + t.iter().map(|t| t.atoms.len()).sum::<usize>() <= budget
                };
                match b.dnf(at as usize).filter(|(terms, _)| fits(terms)) {
                    Some((terms, _)) => (out.push(&terms), b.class(&terms)),
                    None => (BYTECODE, 4),
                }
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
        // The lists were grown by `push`: give back what they do not use.
        out.rules.shrink_to_fit();
        out.terms.shrink_to_fit();
        out.tests.shrink_to_fit();
        out.times.shrink_to_fit();
        out.atoms.shrink_to_fit();
        out.statics.shrink_to_fit();
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
                    bit(set, *b)
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
            put(set, bit, v >= i64::from(n));
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
        let at = self.fixed.get(&pc).copied().unwrap_or(self.statics.len());
        let terms = self.atom(STATIC, u16::try_from(at).ok()?, 1)?;
        if at == self.statics.len() {
            self.statics.push((terms[0].atoms[0], pc));
            self.fixed.insert(pc, at);
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

    /// `a` and `b`; `None` past the term cap, or for a term past the cap on its tests.
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
                if atoms.len() > config::RULE_BITS_MAX_TERM_TESTS {
                    return None;
                }
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
    use super::{Bits, CLASSES, TIME};
    use crate::fill::Plan;
    use crate::fold::tests::{any_of, world, world_of};
    use serde_json::{Value, json};
    use spec_rng::SpecRng;

    /// Rule sites that stay bytecode, of a world's checks and its two events.
    fn bytecode(w: &crate::World) -> u32 {
        w.bits().classes[4]
    }

    /// A def of 40 `has` tests referred to from 50 sites that each add one test: copying the
    /// def's term into every site would hold 50 times 41 tests for a bytecode of about 90
    /// nodes, so a term past `config::RULE_BITS_MAX_TERM_TESTS` stays bytecode, the def and
    /// every site that refers to it. A def of 31 tests still fits with the site's own. Both
    /// decide what their bytecode decides. And a world stops at its budget of tests: the rules
    /// after it stay bytecode.
    #[test]
    fn a_long_def_copied_into_many_sites_and_a_world_past_its_budget_stay_bytecode() {
        assert_eq!(config::RULE_BITS_MAX_TERM_TESTS, 32, "the cap the defs are written for");
        let keys: Vec<String> =
            (0..60).map(|k| format!("x{k}")).chain(["c", "d"].map(String::from)).collect();
        let all = |n: usize| json!({"all": keys[..n].iter().map(|k| json!({"has": k})).collect::<Vec<_>>()});
        let defs = json!({"long": all(40), "fits": all(31)});
        let site = |d: &str, k: usize| json!({"all": [{"ref": d}, {"has": keys[40 + k % 20]}]});
        let rules: Vec<Value> =
            (0..50).map(|k| site("long", k)).chain((0..20).map(|k| site("fits", k))).collect();
        let (w, inst) = world_of(&keys, &defs, &rules);
        // 20 distinct rules over `long` (the 50 sites repeat them), none of them a bit rule.
        assert_eq!(bytecode(&w), 50);
        let bits = w.bits();
        // The 20 distinct rules over `fits`, the two events and the goal.
        assert_eq!(bits.tests.len(), 20 * 32 + 3);
        let plan = Plan::new(&w, &inst).unwrap();
        let plain = plan.clone().interpreted();
        for held in [31usize, 40, 45, 60] {
            let inv: Vec<u16> = (0..u16::try_from(held).unwrap()).collect();
            let (a, b) = (plan.solve(&w, &inv), plain.solve(&w, &inv));
            crate::reach::assert_same(&w, &a, &b, &format!("holding the first {held}"));
            let reached = (0..70).filter(|l| a.reached(*l)).count();
            assert_eq!(
                reached,
                [0, 0, 5 * 3 + 5, 70][[31, 40, 45, 60].iter().position(|h| *h == held).unwrap()]
            );
        }
        // The world's budget: 20 rules of 32 tests each, room for three of them.
        let small = Bits::within(&w, 100);
        assert_eq!(small.tests.len(), 3 * 32 + 3);
        assert_eq!(small.classes[4], 50 + 20 - 3);
        let set = vec![u64::MAX; small.words()];
        let by_bits = checks(&w).filter(|s| small.holds(&set, *s, 1).is_some()).count();
        assert_eq!(by_bits, 3);
    }

    /// The check sites of a world, in order.
    fn checks(w: &crate::World) -> impl Iterator<Item = usize> + '_ {
        (0..w.site_count()).filter(|s| w.site_place(*s).1 == crate::section::CHECKS)
    }

    /// The term cap at its edge: an OR of 16 distinct atoms is a bit rule, of 17 it is not.
    #[test]
    fn the_seventeenth_term_stays_bytecode() {
        assert_eq!(config::RULE_BITS_MAX_TERMS, 16);
        let any = |n: u32| json!({"any": (1..=n).map(|k| json!({"has": "a", "n": k})).collect::<Vec<_>>()});
        let (w, inst) = world(&[any(16), any(17)]);
        assert_eq!(bytecode(&w), 1);
        let none = vec![0u64; w.bits().words()];
        let by_bits: Vec<bool> =
            checks(&w).map(|s| w.bits().holds(&none, s, 1).is_some()).collect();
        assert_eq!(by_bits, [true, false]);
        let plan = Plan::new(&w, &inst).unwrap();
        let plain = plan.clone().interpreted();
        let (a, b) = (plan.solve(&w, &[0]), plain.solve(&w, &[0]));
        crate::reach::assert_same(&w, &a, &b, "one copy of a");
        assert!(a.reached(0) && a.reached(1));
    }

    /// More distinct atoms than 15 bits name: the atoms past the last bit are not made, their
    /// rules stay bytecode, and the solve decides what the bytecode decides.
    #[test]
    fn rules_past_the_last_atom_bit_stay_bytecode() {
        // 32,700 thresholds of the counter, then 30 of each of the ten items: 33,000 atoms asked
        // for (the events' two are among them), 232 more than there are bits.
        let counter = (1..=32_700).map(|k| json!({"ge": [{"counter": "lit"}, k]}));
        let has = ('a'..='j')
            .flat_map(|i| (1..=30).map(move |k| json!({"has": String::from(i), "n": k})));
        let rules: Vec<Value> = counter.chain(has).collect();
        let n = rules.len();
        let (w, inst) = world(&rules);
        assert_eq!(w.bits().atoms.len(), usize::from(TIME));
        assert_eq!(bytecode(&w), 232);
        let plan = Plan::new(&w, &inst).unwrap();
        let plain = plan.clone().interpreted();
        for inv in [vec![], vec![2u16], vec![2, 3]] {
            let (a, b) = (plan.solve(&w, &inv), plain.solve(&w, &inv));
            crate::reach::assert_same(&w, &a, &b, &format!("holding {inv:?}"));
            let reached = (0..u32::try_from(n).unwrap()).filter(|l| a.reached(*l)).count();
            // The counter's thresholds 1 and 2, and one copy of `c` and of `d`.
            assert_eq!(reached, 2 * inv.len());
        }
    }

    /// A random rule over the test world's items, counter, def, group sum and time windows.
    fn random_rule(rng: &mut SpecRng, depth: u32) -> Value {
        let mut below = |n: u64| usize::try_from(rng.below(n).unwrap_or(0)).unwrap_or(0);
        let item = |k: usize| String::from(char::from(b'a' + u8::try_from(k % 10).unwrap_or(0)));
        if depth > 0 && below(3) > 0 {
            let kids: Vec<Value> = (0..2 + below(2)).map(|_| random_rule(rng, depth - 1)).collect();
            let op = if rng.below(2) == Some(0) { "all" } else { "any" };
            return json!({op: kids});
        }
        let (x, y, n) = (item(below(10)), item(below(10)), 1 + below(3));
        match below(7) {
            0 | 1 => json!({"has": x, "n": n}),
            2 => json!({"ge": [{"counter": "lit"}, 1 + n % 2]}),
            3 => json!({"ref": "d"}),
            4 => {
                json!({"ge": [{"int": ["+", {"count": x}, {"int": ["min", {"count": y}, 1]}]}, n]})
            }
            5 => json!({"x.time_any": if n == 1 { 1 } else { 3 }}),
            _ => json!({"gt": [{"count": x}, n - 1]}),
        }
    }

    /// Random rule trees, and random sequences of taking a start copy back and adding a copy:
    /// after every step the state with bit rules holds what the state of the bytecode holds
    /// (thresholds above one, sums losing a copy and the counter going down included).
    #[test]
    fn random_rules_under_random_adds_and_removals_decide_as_their_bytecode_does() {
        let (mut by_bits, mut lost) = (0, 0);
        for seed in 1..=60u64 {
            let mut rng = SpecRng::from_u64(seed);
            let rules: Vec<Value> = (0..12).map(|_| random_rule(&mut rng, 3)).collect();
            let (w, inst) = world(&rules);
            by_bits += 14 - bytecode(&w);
            let plan = Plan::new(&w, &inst).unwrap();
            let plain = plan.clone().interpreted();
            let mut pick =
                |n: usize| usize::try_from(rng.below(n as u64).unwrap_or(0)).unwrap_or(0);
            let mut start: Vec<u16> =
                (0..10 + pick(20)).map(|_| u16::try_from(pick(10)).unwrap()).collect();
            let (mut a, mut b) = (plan.solve(&w, &start), plain.solve(&w, &start));
            crate::reach::assert_same(&w, &a, &b, &format!("seed {seed}, start {start:?}"));
            for step in 0..40 {
                let what = if start.is_empty() || pick(4) == 0 {
                    let i = u16::try_from(pick(10)).unwrap();
                    for r in [&mut a, &mut b] {
                        r.add(i, 1);
                        r.run();
                    }
                    format!("added {i}")
                } else {
                    let i = start.swap_remove(pick(start.len()));
                    let before = (0..12).filter(|l| a.reached(*l)).count();
                    a.remove(i);
                    b.remove(i);
                    lost += usize::from((0..12).filter(|l| a.reached(*l)).count() < before);
                    format!("removed {i}")
                };
                crate::reach::assert_same(&w, &a, &b, &format!("seed {seed}, step {step}: {what}"));
            }
        }
        assert!(
            by_bits > 300 && lost > 50,
            "{by_bits} bit rules, {lost} removals that lost a check"
        );
    }

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
