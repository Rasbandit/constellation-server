//! Incremental reachability over the compiled rules section: the least fixed point of regions
//! (with time masks), events and locations for an inventory. Rules are read in place from
//! the bytecode; collecting an item rechecks only the sites the section's item index lists.
//!
//! Semantics follow the world's own solver: a region holds a time mask; a rule is evaluated
//! against the mask of the region it sits in; an open exit passes the whole mask on; a region
//! where time can pass expands its mask forward until a stay rule fails. All of it is monotone
//! in items, counters and masks, so the fixed point does not depend on evaluation order.
//!
//! Taking an item back is truth maintenance: every fact (a site that held, a half-day, a slice
//! of a region) carries the rank it was derived at, and its support is checked against the
//! state as of that rank (only facts of lower rank), so support that runs through the fact
//! itself never counts.

use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bits::BitSet;

use crate::fill::Plan;
use crate::section::{CHECKS, EVENTS, EXITS, Node, STAYS, compare, node, operand};
use crate::world::World;

/// What a rule reads besides its region's mask.
trait View {
    fn count(&self, i: u16) -> i64;
    fn counter(&self, c: u16) -> i64;
    fn group(&self, g: u16) -> i64;
    /// Whether region `r` is reached.
    fn region(&self, r: u16) -> bool;
}

/// Values in plain slices: the current state of a solve, or a scratch one.
struct Now<'a> {
    counts: &'a [i64],
    counters: &'a [i64],
    groups: &'a [i64],
    masks: &'a [u64],
}

impl View for Now<'_> {
    fn count(&self, i: u16) -> i64 {
        self.counts.get(usize::from(i)).copied().unwrap_or(0)
    }
    fn counter(&self, c: u16) -> i64 {
        self.counters.get(usize::from(c)).copied().unwrap_or(0)
    }
    fn group(&self, g: u16) -> i64 {
        self.groups.get(usize::from(g)).copied().unwrap_or(0)
    }
    fn region(&self, r: u16) -> bool {
        self.masks.get(usize::from(r)).is_some_and(|m| *m != 0)
    }
}

/// A solve state as of a rank: only the facts derived before it.
struct At<'a, 'w> {
    r: &'a Reach<'w>,
    rank: u32,
}

impl View for At<'_, '_> {
    fn count(&self, i: u16) -> i64 {
        let i = usize::from(i);
        let got = self.r.copies[i].partition_point(|k| *k < self.rank);
        self.r.adjust[i] + i64::try_from(got).unwrap_or(i64::MAX)
    }
    fn counter(&self, c: u16) -> i64 {
        let got = self.r.lit[usize::from(c)].partition_point(|k| *k < self.rank);
        i64::try_from(got).unwrap_or(i64::MAX)
    }
    fn group(&self, g: u16) -> i64 {
        let members = self.r.w.group_members(usize::from(g));
        members.map(|(i, cap)| self.count(i).min(cap.map_or(i64::MAX, i64::from))).sum()
    }
    fn region(&self, r: u16) -> bool {
        self.r.mask_at(usize::from(r), self.rank) != 0
    }
}

/// What a rule is evaluated against.
struct Env<'a, V> {
    w: &'a World,
    code: &'a [u8],
    defs: &'a [u32],
    opts: &'a [i32],
    rolls: &'a [i16],
    v: V,
    /// Per def: 0 unknown, `TRUE` known true, else the epoch it was known false at.
    cache: &'a [Cell<u32>],
    epoch: u32,
    memo: &'a DefMemo,
}

/// Def cache value for "true". Rules are monotone, so a true def stays true for the solve;
/// a false one is known only until the next change to an item or counter some def reads (the
/// epoch).
const TRUE: u32 = u32::MAX;

/// Def values within one evaluation (one root rule, one state or rank, one mask), so a def a
/// rule reaches twice is evaluated once (a chain of defs that each read the next twice costs
/// its length, not 2^length). Entries carry the evaluation's stamp, so starting one is O(1)
/// and a solve step allocates nothing.
#[derive(Debug, Clone, Default)]
struct DefMemo {
    /// Per def: `stamp << 1 | value`; a stale stamp means unknown.
    vals: Vec<Cell<u64>>,
    stamp: Cell<u64>,
    /// Def bodies evaluated (memo misses), for the complexity tests.
    bodies: Cell<u64>,
}

impl DefMemo {
    fn new(defs: usize) -> Self {
        Self { vals: (0..defs).map(|_| Cell::new(0)).collect(), ..Self::default() }
    }

    fn begin(&self) {
        self.stamp.set(self.stamp.get() + 1);
    }

    fn get(&self, d: usize) -> Option<bool> {
        let v = self.vals.get(d)?.get();
        (v >> 1 == self.stamp.get()).then_some(v & 1 == 1)
    }

    fn set(&self, d: usize, v: bool) {
        self.bodies.set(self.bodies.get() + 1);
        if let Some(c) = self.vals.get(d) {
            c.set(self.stamp.get() << 1 | u64::from(v));
        }
    }
}

impl<V: View> Env<'_, V> {
    /// Evaluate the boolean rule at `pc` against region mask `t`: one evaluation.
    fn eval(&self, pc: usize, t: u64) -> bool {
        self.memo.begin();
        self.b(pc, t).0
    }

    /// The boolean node at `pc` against region mask `t`, and the offset after it.
    fn b(&self, pc: usize, t: u64) -> (bool, usize) {
        let Some((n, at)) = node(self.code, pc, self.w.widths) else { return (false, pc + 1) };
        match n {
            Node::Const(v) => (v, at),
            Node::All(k, len) | Node::Any(k, len) => {
                let want = matches!(n, Node::Any(..));
                let stop = at + usize::from(len);
                let mut pc = at;
                for _ in 0..k {
                    let (v, e) = self.b(pc, t);
                    if v == want {
                        return (want, stop);
                    }
                    pc = e;
                }
                (!want, stop)
            }
            Node::Has(i, c) => (self.count(i) >= i64::from(c), at),
            Node::HasAny(k) => {
                let iw = self.w.widths.item;
                let hit = (0..usize::from(k)).any(|j| {
                    operand(self.code, at + j * usize::from(iw), iw)
                        .is_some_and(|i| self.count(i) > 0)
                });
                (hit, at + usize::from(k) * usize::from(iw))
            }
            Node::Cmp(op) => {
                let (a, e) = self.i(at);
                let (b, e) = self.i(e);
                (compare(op, a, b), e)
            }
            Node::TimeAny(m) => (t & self.w.mask(usize::from(m)) != 0, at),
            Node::Ref(d) => (self.def(usize::from(d), t), at),
            Node::Reach(r) => (self.v.region(r), at),
            Node::Not => {
                let (v, e) = self.b(at, t);
                (!v, e)
            }
            _ => (false, at),
        }
    }

    fn def(&self, d: usize, t: u64) -> bool {
        let Some(at) = self.defs.get(d) else { return false };
        let cached = self.cache.get(d).filter(|_| self.w.def_timeless(d));
        match cached.map(Cell::get) {
            Some(TRUE) => return true,
            Some(e) if e == self.epoch => return false,
            _ => {}
        }
        if let Some(v) = self.memo.get(d) {
            return v;
        }
        let v = self.b(*at as usize, t).0;
        self.memo.set(d, v);
        if let Some(c) = cached {
            c.set(if v { TRUE } else { self.epoch });
        }
        v
    }

    fn count(&self, i: u16) -> i64 {
        self.v.count(i)
    }

    // shortcut: saturating arithmetic instead of D2.5's overflow error; values here stay tiny.
    // Upgrade when the validator rejects manifests whose expressions can overflow.
    fn i(&self, pc: usize) -> (i64, usize) {
        let Some((n, at)) = node(self.code, pc, self.w.widths) else { return (0, pc + 1) };
        match n {
            Node::Int(v) => (i64::from(v), at),
            Node::Count(i) => (self.count(i), at),
            Node::Counter(c) => (self.v.counter(c), at),
            Node::Group(g) => (self.v.group(g), at),
            Node::Opt(o) => (self.opts.get(usize::from(o)).map_or(0, |v| i64::from(*v)), at),
            Node::Roll(s) => (self.rolls.get(usize::from(s)).map_or(0, |v| i64::from(*v)), at),
            Node::Fold(op, k) => {
                let mut pc = at;
                let mut acc: Option<i64> = None;
                for _ in 0..k {
                    let (v, e) = self.i(pc);
                    pc = e;
                    acc = Some(match (acc, op) {
                        (None, _) => v,
                        (Some(a), crate::section::op::ADD) => a.saturating_add(v),
                        (Some(a), crate::section::op::MIN) => a.min(v),
                        (Some(a), _) => a.max(v),
                    });
                }
                (acc.unwrap_or(0), pc)
            }
            Node::Pair(op) => {
                let (a, e) = self.i(at);
                let (b, e) = self.i(e);
                let v = if op == crate::section::op::MUL {
                    a.saturating_mul(b)
                } else {
                    a.checked_div_euclid(b).unwrap_or(0)
                };
                (v, e)
            }
            _ => (0, at),
        }
    }
}

/// Evaluate an option-only rule (no state) for a seed's option and roll values.
pub(crate) fn eval_static(w: &World, opts: &[i32], rolls: &[i16], rule: u32) -> bool {
    let defs: Vec<u32> = (0..w.defs()).map(|d| w.def(d)).collect();
    let env = Env {
        w,
        code: w.code(),
        defs: &defs,
        opts,
        rolls,
        v: Now { counts: &[], counters: &[], groups: &[], masks: &[] },
        cache: &[],
        epoch: 1,
        memo: &DefMemo::new(w.defs()),
    };
    env.eval(rule as usize, 0)
}

/// The held count of `item` from which no rule this seed reads more copies: with at least that
/// many held, every test that reads it (a `has`, or a comparison over its count) is already true
/// whatever else is held, so more copies change no rule. 0 when no rule reads it, `i64::MAX`
/// when no count up to `max` gets there. Rules are monotone, so "true with nothing else held"
/// means true in every state.
pub(crate) fn saturation(w: &World, plan: &Plan, item: u16, max: i64) -> i64 {
    let f = &plan.folded;
    let mut s = Sat {
        w,
        plan,
        x: item,
        max,
        defs: vec![None; w.defs()],
        counts: vec![0; w.items()],
        memo: DefMemo::new(w.defs()),
    };
    let sites = w.site_count();
    w.item_sites(item as usize)
        .iter()
        .map(|&id| {
            let id = id as usize;
            let rule = if id < sites { f.sites[id] } else { f.halves[id - sites] };
            s.b(rule as usize).0
        })
        .max()
        .unwrap_or(0)
}

/// Walks folded rules for [`saturation`]; unknown shapes count as never saturated.
struct Sat<'a> {
    w: &'a World,
    plan: &'a Plan,
    x: u16,
    max: i64,
    defs: Vec<Option<i64>>,
    counts: Vec<i64>,
    memo: DefMemo,
}

impl Sat<'_> {
    /// Count of `x` past which the boolean node at `pc` no longer reads it, and its end.
    fn b(&mut self, pc: usize) -> (i64, usize) {
        let code = &self.plan.folded.code;
        let Some((n, at)) = node(code, pc, self.w.widths) else { return (i64::MAX, pc + 1) };
        match n {
            Node::Const(_) | Node::TimeAny(_) | Node::Reach(_) => (0, at),
            Node::All(k, len) | Node::Any(k, len) => {
                let (mut need, mut pc) = (0, at);
                for _ in 0..k {
                    let (v, e) = self.b(pc);
                    need = need.max(v);
                    pc = e;
                }
                (need, at + usize::from(len))
            }
            Node::Has(i, c) => (if i == self.x { i64::from(c) } else { 0 }, at),
            Node::HasAny(k) => {
                let iw = self.w.widths.item;
                let hit = (0..usize::from(k))
                    .any(|j| operand(code, at + j * usize::from(iw), iw) == Some(self.x));
                (i64::from(hit), at + usize::from(k) * usize::from(iw))
            }
            Node::Cmp(_) => {
                let (ra, e) = self.i(at);
                let (rb, e) = self.i(e);
                (if ra || rb { self.first_true(pc) } else { 0 }, e)
            }
            Node::Ref(d) => {
                let d = usize::from(d);
                let need = match self.defs.get(d).copied().flatten() {
                    Some(v) => v,
                    None => {
                        let v = match self.plan.folded.defs.get(d) {
                            Some(at) => self.b(*at as usize).0,
                            None => i64::MAX,
                        };
                        if let Some(slot) = self.defs.get_mut(d) {
                            *slot = Some(v);
                        }
                        v
                    }
                };
                (need, at)
            }
            Node::Not => {
                let (v, e) = self.b(at);
                (if v > 0 { i64::MAX } else { 0 }, e)
            }
            _ => (i64::MAX, at),
        }
    }

    /// Whether the integer node at `pc` reads `x` (unknown shapes say yes), and its end.
    fn i(&self, pc: usize) -> (bool, usize) {
        let Some((n, at)) = node(&self.plan.folded.code, pc, self.w.widths) else {
            return (true, pc + 1);
        };
        match n {
            Node::Int(_) | Node::Counter(_) | Node::Opt(_) | Node::Roll(_) => (false, at),
            Node::Count(i) => (i == self.x, at),
            Node::Group(g) => (self.w.item_groups(self.x as usize).iter().any(|m| m.0 == g), at),
            Node::Fold(_, k) => self.children(at, usize::from(k)),
            Node::Pair(_) => self.children(at, 2),
            _ => (true, at),
        }
    }

    fn children(&self, mut pc: usize, k: usize) -> (bool, usize) {
        let mut reads = false;
        for _ in 0..k {
            let (r, e) = self.i(pc);
            reads |= r;
            pc = e;
        }
        (reads, pc)
    }

    /// Fewest copies of `x` that make the comparison at `pc` true with nothing else held.
    fn first_true(&mut self, pc: usize) -> i64 {
        let x = self.x as usize;
        let mut groups = vec![0; self.w.groups()];
        for k in 0..=self.max {
            self.counts[x] = k;
            for &(g, cap) in self.w.item_groups(x) {
                groups[usize::from(g)] = k.min(i64::from(cap));
            }
            let env = Env {
                w: self.w,
                code: &self.plan.folded.code,
                defs: &self.plan.folded.defs,
                opts: &self.plan.opts,
                rolls: &self.plan.rolls,
                v: Now { counts: &self.counts, counters: &[], groups: &groups, masks: &[] },
                cache: &[],
                epoch: 1,
                memo: &self.memo,
            };
            if env.eval(pc, 0) {
                self.counts[x] = 0;
                return k;
            }
        }
        self.counts[x] = 0;
        i64::MAX
    }
}

/// Work counters of one or more solves (the complexity contract in the rules section doc).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Rule evaluations (sites whose rule was run; stay and clock rules not counted).
    pub evals: u64,
    /// Worklist pushes: regions queued plus sites queued.
    pub pushes: u64,
    /// Items collected during a run (each walks its dependency list once).
    pub collected: u64,
    /// Dependency entries walked for collected items, summed.
    pub rechecks: u64,
    /// The most dependency entries one collected item walked.
    pub max_recheck: u64,
    /// Items taken back that some rule still read (each a truth maintenance pass).
    pub removals: u64,
    /// Facts retracted by those removals, summed.
    pub retractions: u64,
    /// The most facts one removal retracted.
    pub max_retractions: u64,
    /// Retracted facts that hold again once the same removal re-derived (churn); the rest of
    /// `retractions` are facts the removal lost for good.
    pub churn: u64,
}

impl Stats {
    /// The counters as a report: totals, per solve and per collected item.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // counts far below 2^52
    pub fn summary(&self, fills: u64, solves: u64) -> serde_json::Value {
        let per = |x: u64, n: u64| (x as f64 / n.max(1) as f64 * 10.0).round() / 10.0;
        serde_json::json!({
            "fills": fills, "solves": solves, "solves_per_fill": per(solves, fills),
            "rule_evals": self.evals, "rule_evals_per_fill": per(self.evals, fills),
            "rule_evals_per_solve": per(self.evals, solves),
            "worklist_pushes": self.pushes, "worklist_pushes_per_solve": per(self.pushes, solves),
            "items_collected": self.collected,
            "sites_rechecked_per_collected_item": {"mean": per(self.rechecks, self.collected),
                                                    "max": self.max_recheck},
            "removals": self.removals,
            "retractions_per_removal": {"mean": per(self.retractions, self.removals),
                                        "max": self.max_retractions},
            "churn": {"rederived": self.churn, "net_lost": self.retractions - self.churn,
                      "share": (self.churn as f64 / self.retractions.max(1) as f64 * 1e3).round()
                          / 1e3},
        })
    }

    /// Add another run's counters.
    pub fn absorb(&mut self, o: &Stats) {
        self.evals += o.evals;
        self.pushes += o.pushes;
        self.collected += o.collected;
        self.rechecks += o.rechecks;
        self.max_recheck = self.max_recheck.max(o.max_recheck);
        self.removals += o.removals;
        self.retractions += o.retractions;
        self.max_retractions = self.max_retractions.max(o.max_retractions);
        self.churn += o.churn;
    }
}

/// Rank of a fact that does not hold.
const NONE: u32 = u32::MAX;

/// What one player's state tells another's (see [`crate::room`]): a location of this state holds
/// an item that belongs to the other player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Msg {
    /// The location was reached at `rank`: its item is a new copy for `to`, found at that rank.
    Give { to: u16, item: u16, rank: u32 },
    /// The copy found at rank `from` was found at `rank` instead (`NONE`: not found any more).
    Move { to: u16, item: u16, from: u32, rank: u32 },
}
/// Support of a slice of the start region: the start mask (with clocks, the owned clocks).
const BY_START: u32 = u32::MAX;
/// Support of a slice reached by waiting from the slice before it.
const BY_WAIT: u32 = u32::MAX - 1;
/// Growths per region the state reserves room for.
const GROWS: usize = 4;

/// One reachability state, with the derivation of everything it holds. Items are added with
/// [`Reach::add`] and [`Reach::hold`]; a start copy is taken back exactly with
/// [`Reach::remove`]. Reset it to reuse its buffers for another solve of the same world and plan.
///
/// Every fact has a rank, unique and increasing in derivation order: a site that held (an
/// exit, event or check), a half-day rule that held, a growth of a region's mask (the slices
/// one support added at once: the start, an exit passing its source's slices, or waiting
/// forward). A collected copy has the rank of the check that reached its location. The state
/// as of rank `k` is every fact of rank below `k`; each fact holds in the state as of its own
/// rank (a waited slice may also start from the slice before it in the same growth), so the
/// ranks are a derivation and every fact is in the least fixed point.
#[derive(Debug, Clone)]
pub struct Reach<'w> {
    w: &'w World,
    plan: &'w Plan,
    /// Copies held per item.
    pub counts: Vec<i64>,
    /// Counter values.
    pub counters: Vec<i64>,
    /// Item group sums (capped copies), kept up to date as items are added.
    groups: Vec<i64>,
    /// Time mask per region; 0 means not reached.
    pub masks: Vec<u64>,
    reached: BitSet,
    /// Item to collect when a location is reached (placed, locked or a self-granting check).
    holds: Vec<Option<u16>>,
    /// Per location: the item (and the player it belongs to) that goes to another player's
    /// state when the location is reached.
    away: Vec<Option<(u16, u16)>>,
    /// Messages for other players' states, in order; the room drains it after every step.
    pub(crate) outbox: Vec<Msg>,
    can_stay: Vec<bool>,
    seen: BitSet,
    done: BitSet,
    queued: Vec<bool>,
    pending: Vec<bool>,
    regions_q: Vec<u32>,
    sites_q: Vec<u32>,
    owned: u64,
    /// Owned clocks with nothing held (the solver's first clock evaluation).
    owned0: u64,
    /// A run has started. Before it, adding an item needs no recheck: the first visit of a
    /// region evaluates every one of its sites.
    ran: bool,
    /// Work counters since the last reset.
    pub stats: Stats,
    def_cache: Vec<Cell<u32>>,
    def_memo: DefMemo,
    /// Bumped on every item or counter change; starts at 1 (0 means unknown in the cache).
    epoch: u32,
    /// Next rank.
    seq: u32,
    /// Per site: its rank (`NONE`: not done).
    fired: Vec<u32>,
    /// Per half-day rule: its rank.
    half_rank: Vec<u32>,
    /// Per region: its mask growths, ascending by rank; their slices are disjoint and make up
    /// the mask.
    grows: Vec<Vec<Grow>>,
    /// Per region: the lowest slice of each growth waiting for a support check.
    queued_grows: Vec<u64>,
    /// Per location: rank it was reached at (the first check that held).
    loc_rank: Vec<u32>,
    /// Per item: ranks of the copies collected, ascending.
    copies: Vec<Vec<u32>>,
    /// Per counter: ranks of the events that fired, ascending (the counter as of a rank is a
    /// binary search, not a scan of its events).
    lit: Vec<Vec<u32>>,
    /// Per item: copies held from the start (before any run, less what was removed).
    adjust: Vec<i64>,
    tm: Tms,
}

/// Slices a region's mask gained at one rank, from one support.
#[derive(Debug, Clone, Copy)]
struct Grow {
    rank: u32,
    bits: u64,
    /// `BY_START`, `BY_WAIT`, or the exit that passed them.
    why: u32,
}

/// Scratch state of a removal (sized once, reused).
#[derive(Debug, Clone)]
struct Tms {
    /// Facts whose support may be gone, lowest rank first.
    heap: BinaryHeap<Reverse<(u32, u32)>>,
    queued: BitSet,
    /// Sites and half-days retracted: the only ones that can hold again.
    out: BitSet,
    out_list: Vec<u32>,
    /// Regions that lost a slice.
    lost: Vec<u32>,
    /// Growths retracted by this removal: region and the slices it lost.
    lost_grows: Vec<(u32, u64)>,
    lost_flag: BitSet,
    /// Re-deriving after a removal: only retracted sites are evaluated.
    active: bool,
}

impl<'w> Reach<'w> {
    /// Empty state: nothing held, start region queued. Call [`Reach::run`].
    #[must_use]
    pub fn new(w: &'w World, plan: &'w Plan) -> Self {
        let n = w.regions();
        let sites = w.site_count();
        let halves = w.time.owned.map_or(0, |o| o.halves);
        let mut r = Reach {
            w,
            plan,
            counts: vec![0; w.items()],
            counters: vec![0; w.counters()],
            // At most one rank per event, so these never grow.
            lit: (0..w.counters())
                .map(|c| Vec::with_capacity(w.sites_into(EVENTS, c).len()))
                .collect(),
            groups: vec![0; w.groups()],
            masks: vec![0; n],
            reached: BitSet::new(w.locations()),
            holds: vec![None; w.locations()],
            away: vec![None; w.locations()],
            outbox: Vec::new(),
            // The start region never waits (the solver's initial time state).
            can_stay: (0..n).map(|r| r != w.start as usize && w.can_stay(r)).collect(),
            seen: BitSet::new(n),
            done: BitSet::new(sites),
            queued: vec![false; n],
            pending: vec![false; sites + halves],
            // `queued` and `pending` keep each entry at most once, so these never grow.
            regions_q: Vec::with_capacity(n),
            sites_q: Vec::with_capacity(sites + halves),
            owned: w.time.all,
            owned0: w.time.all,
            ran: false,
            stats: Stats::default(),
            def_cache: (0..w.defs()).map(|_| Cell::new(0)).collect(),
            def_memo: DefMemo::new(w.defs()),
            epoch: 1,
            seq: 1,
            fired: vec![NONE; sites],
            half_rank: vec![NONE; halves],
            // A few growths per region; a region that needs more grows its list once and the
            // reused state keeps the capacity.
            grows: (0..n).map(|_| Vec::with_capacity(GROWS)).collect(),
            queued_grows: vec![0; n],
            loc_rank: vec![NONE; w.locations()],
            // At most one copy per existing copy of the seed, so these never grow.
            copies: (0..w.items()).map(|i| Vec::with_capacity(plan.copies(i))).collect(),
            adjust: vec![0; w.items()],
            tm: Tms {
                heap: BinaryHeap::new(),
                queued: BitSet::new(sites + halves),
                out: BitSet::new(sites + halves),
                out_list: Vec::new(),
                lost: Vec::new(),
                lost_grows: Vec::new(),
                lost_flag: BitSet::new(n),
                active: false,
            },
        };
        r.start();
        r
    }

    /// Back to the empty state, keeping every buffer.
    pub fn reset(&mut self) {
        self.reset_from(1);
    }

    /// [`Reach::reset`], handing out ranks from `seq` on.
    pub(crate) fn reset_from(&mut self, seq: u32) {
        self.counts.fill(0);
        self.counters.fill(0);
        self.groups.fill(0);
        self.masks.fill(0);
        self.reached.clear();
        self.holds.fill(None);
        self.away.fill(None);
        self.outbox.clear();
        self.seen.clear();
        self.done.clear();
        self.queued.fill(false);
        self.pending.fill(false);
        self.regions_q.clear();
        self.sites_q.clear();
        self.owned = self.w.time.all;
        self.owned0 = self.w.time.all;
        self.ran = false;
        self.stats = Stats::default();
        self.def_cache.iter().for_each(|c| c.set(0));
        self.epoch = 1;
        self.seq = seq;
        self.fired.fill(NONE);
        self.half_rank.fill(NONE);
        self.grows.iter_mut().for_each(Vec::clear);
        self.loc_rank.fill(NONE);
        self.copies.iter_mut().for_each(Vec::clear);
        self.lit.iter_mut().for_each(Vec::clear);
        self.adjust.fill(0);
        self.start();
    }

    fn start(&mut self) {
        let start_mask = if self.plan.clocks {
            // The clocks the start inventory owns (not the assumed items): they hold from rank 0.
            self.owned = self.plan.owned_start;
            self.owned0 = self.owned;
            self.owned
        } else {
            self.w.time.start_mask
        };
        self.grow(self.w.start, start_mask, BY_START);
    }

    fn next(&mut self) -> u32 {
        let k = self.seq;
        self.seq += 1;
        k
    }

    /// Whether location `l` is reached.
    #[must_use]
    pub fn reached(&self, l: u32) -> bool {
        self.reached.contains(l as usize)
    }

    /// Set what a location holds; it is collected when the location is reached. A location
    /// holds one item: holding the same again changes nothing.
    pub fn hold(&mut self, loc: u32, item: u16) {
        let l = loc as usize;
        if self.holds[l] == Some(item) {
            return;
        }
        self.holds[l] = Some(item);
        if self.reached.contains(l) {
            self.collect(item, self.loc_rank[l]);
        }
    }

    /// Every location's held item at once (`None` keeps what is held), before a run.
    pub fn hold_all(&mut self, placed: &[Option<u16>]) {
        for (l, i) in placed.iter().enumerate() {
            if let Some(i) = i {
                self.hold(u32::try_from(l).unwrap_or(0), *i);
            }
        }
    }

    /// Like [`Reach::hold`] for an item that belongs to player `to` (not this state's): reaching
    /// the location sends it to the room as a [`Msg::Give`] instead of collecting it here.
    pub(crate) fn hold_away(&mut self, loc: u32, to: u16, item: u16) {
        let l = loc as usize;
        if self.away[l] == Some((to, item)) {
            return;
        }
        debug_assert!(self.away[l].is_none() && self.holds[l].is_none(), "one item per location");
        self.away[l] = Some((to, item));
        if self.reached.contains(l) {
            self.outbox.push(Msg::Give { to, item, rank: self.loc_rank[l] });
        }
    }

    /// The next rank this state hands out. Several states in one room take ranks from one
    /// sequence, so ranks order every derivation of the room: the room passes the sequence
    /// from state to state ([`Reach::set_seq`]).
    pub(crate) fn seq(&self) -> u32 {
        self.seq
    }

    pub(crate) fn set_seq(&mut self, seq: u32) {
        self.seq = seq;
    }

    /// Whether there is nothing left to run.
    pub(crate) fn idle(&self) -> bool {
        self.pending() == 0
    }

    /// Regions and sites queued for the next run: a rough size of it.
    pub(crate) fn pending(&self) -> usize {
        self.regions_q.len() + self.sites_q.len()
    }

    /// A copy of `item` for this player, found elsewhere at rank `rank`.
    pub(crate) fn give(&mut self, item: u16, rank: u32) {
        self.collect(item, rank);
    }

    /// The copy of `item` found at rank `from` was found at `rank` (`NONE`: not found any more):
    /// the facts that read it between the two ranks are checked in the next cascade.
    pub(crate) fn shift(&mut self, item: u16, from: u32, rank: u32) {
        self.uncollect(item, from);
        if rank != NONE {
            self.collect_quiet(item, rank);
        }
        let w = self.w;
        self.suspect_readers(w.item_sites(usize::from(item)), from, rank);
    }

    /// Add `n` copies of an item (rules that read it are rechecked on the next [`Reach::run`]).
    /// Before the first run they are start copies, after it each is a fact of its own.
    pub fn add(&mut self, item: u16, n: i64) {
        if !self.ran {
            self.count_change(item, n);
            return;
        }
        for _ in 0..n {
            let k = self.next();
            self.collect(item, k);
        }
    }

    /// `n` more copies of `item` in the counts and group sums.
    fn count_change(&mut self, item: u16, n: i64) {
        let i = usize::from(item);
        if self.w.def_reads_item(i) {
            self.epoch += 1;
        }
        let before = self.counts[i];
        let after = before + n;
        self.counts[i] = after;
        for &(g, cap) in self.w.item_groups(i) {
            let cap = i64::from(cap);
            self.groups[usize::from(g)] += after.min(cap) - before.min(cap);
        }
    }

    /// A copy of `item` collected at rank `k`; after the first run the rules reading it are
    /// rechecked.
    fn collect(&mut self, item: u16, k: u32) {
        self.count_change(item, 1);
        let v = &mut self.copies[usize::from(item)];
        let at = v.partition_point(|x| *x < k);
        v.insert(at, k);
        if !self.ran {
            return;
        }
        let w = self.w;
        let sites = w.item_sites(usize::from(item));
        for &s in sites {
            self.push_site(s);
        }
        let walked = sites.len() as u64;
        self.stats.collected += 1;
        self.stats.rechecks += walked;
        self.stats.max_recheck = self.stats.max_recheck.max(walked);
    }

    /// Drop the copy of `item` collected at rank `k`.
    fn uncollect(&mut self, item: u16, k: u32) {
        let v = &mut self.copies[usize::from(item)];
        if let Ok(at) = v.binary_search(&k) {
            v.remove(at);
            self.count_change(item, -1);
        }
    }

    /// Run to the fixed point.
    pub fn run(&mut self) {
        self.begin();
        loop {
            if let Some(r) = self.regions_q.pop() {
                self.queued[r as usize] = false;
                self.visit(r as usize);
            } else if let Some(s) = self.sites_q.pop() {
                self.pending[s as usize] = false;
                self.site(s as usize);
            } else {
                return;
            }
        }
    }

    /// Start the run: what is held now is the start inventory. Copies that arrive later, from
    /// other players or from reached locations, are facts of their own.
    pub(crate) fn begin(&mut self) {
        if !self.ran {
            self.ran = true;
            self.adjust.copy_from_slice(&self.counts);
            // Items added before the first run skipped their rechecks; the clock rules are the
            // one place that reads them before any region is visited.
            if self.plan.clocks {
                self.site(self.w.site_count());
            }
        }
    }

    /// Take one start copy of `item` back and bring the fixed point down to that of the smaller
    /// inventory, after a [`Reach::run`]. With as many copies left as the plan's saturation
    /// count, no rule changes. Otherwise the facts that read the item are checked, lowest rank
    /// first, against the state as of their own rank; one that still holds there keeps its
    /// place, one that does not is retracted and the facts that read it are checked in turn.
    /// Retracted facts are then derived again where the smaller state still supports them (they
    /// get new ranks); nothing that did not hold before is evaluated, since the new fixed point
    /// is inside the old one.
    pub fn remove(&mut self, item: u16) {
        let before = self.stats.retractions;
        if !self.start_removal(item) {
            return;
        }
        self.cascade();
        self.end_cascade(self.stats.retractions - before);
        self.rederive();
    }

    /// The first half of [`Reach::remove`]: the copy goes and its readers are queued for a
    /// support check. `false` when nothing can change (saturated).
    pub(crate) fn start_removal(&mut self, item: u16) -> bool {
        let i = usize::from(item);
        self.adjust[i] -= 1;
        self.count_change(item, -1);
        if self.adjust[i] >= self.plan.saturated_at(item) {
            return false;
        }
        self.forget_defs();
        self.stats.removals += 1;
        let w = self.w;
        self.suspect_readers(w.item_sites(i), 0, NONE);
        true
    }

    /// A removal's cascade is over: it retracted `n` facts, over every state it reached.
    pub(crate) fn end_cascade(&mut self, n: u64) {
        self.stats.max_retractions = self.stats.max_retractions.max(n);
    }

    /// The lowest rank among the facts waiting for a support check.
    pub(crate) fn next_suspect(&self) -> Option<u32> {
        self.tm.heap.peek().map(|Reverse((k, _))| *k)
    }

    fn forget_defs(&mut self) {
        self.def_cache.iter().for_each(|c| c.set(0));
        self.epoch += 1;
    }

    fn rank_of(&self, f: usize) -> u32 {
        let sc = self.w.site_count();
        if f < sc { self.fired[f] } else { self.half_rank[f - sc] }
    }

    /// Queue site or half-day `f` (of rank `k`) for a support check if `lo < k <= hi`.
    fn suspect(&mut self, f: usize, k: u32, lo: u32, hi: u32) {
        if k != NONE && k > lo && k <= hi && !self.tm.queued.contains(f) {
            self.tm.queued.insert(f);
            self.tm.heap.push(Reverse((k, u32::try_from(f).unwrap_or(0))));
        }
    }

    /// Queue the growths of region `r` with rank in `lo < k <= hi` whose support `pick`
    /// selects, when they have a slice in the mask it gives.
    fn suspect_grows(&mut self, r: usize, lo: u32, hi: u32, pick: impl Fn(u32) -> Option<u64>) {
        let id = u32::try_from(self.w.site_count() + self.half_rank.len() + r).unwrap_or(0);
        let from = self.grows[r].partition_point(|g| g.rank <= lo);
        for g in &self.grows[r][from..] {
            let head = g.bits & g.bits.wrapping_neg();
            if g.rank > hi || self.queued_grows[r] & head != 0 {
                continue;
            }
            if pick(g.why).is_some_and(|need| g.bits & need != 0) {
                self.queued_grows[r] |= head;
                self.tm.heap.push(Reverse((g.rank, id)));
            }
        }
    }

    /// Queue the facts that read through `list` (a dependency list: sites, half-days past the
    /// sites, and stays, whose reader is the waited slice) with rank in `lo < k <= hi`.
    fn suspect_readers(&mut self, list: &[u32], lo: u32, hi: u32) {
        let w = self.w;
        let sc = w.site_count();
        for &s in list {
            let s = s as usize;
            if s >= sc {
                self.suspect(s, self.half_rank[s - sc], lo, hi);
                continue;
            }
            let (r, g) = w.site_place(s);
            if g == STAYS {
                let bit = 1u64 << w.site_target(s);
                self.suspect_grows(r, lo, hi, |y| (y == BY_WAIT).then_some(bit));
            } else {
                self.suspect(s, self.fired[s], lo, hi);
            }
        }
    }

    fn cascade(&mut self) {
        while self.cascade_one() {}
    }

    /// Check the lowest-ranked suspect; `false` when there is none.
    pub(crate) fn cascade_one(&mut self) -> bool {
        let hb = self.w.site_count() + self.half_rank.len();
        let Some(Reverse((k, f))) = self.tm.heap.pop() else { return false };
        let f = f as usize;
        if f >= hb {
            self.check_grow(f - hb, k);
            return true;
        }
        self.tm.queued.remove(f);
        if self.rank_of(f) == k && !self.supported(f, k) {
            self.retract(f, k);
        }
        true
    }

    /// Check the growth of region `r` at rank `k`: keep the slices still supported, retract
    /// the rest.
    fn check_grow(&mut self, r: usize, k: u32) {
        let Ok(i) = self.grows[r].binary_search_by_key(&k, |g| g.rank) else { return };
        let g = self.grows[r][i];
        let head = g.bits & g.bits.wrapping_neg();
        if self.queued_grows[r] & head == 0 {
            return;
        }
        self.queued_grows[r] &= !head;
        let kept = self.grow_support(r, &g);
        let lost = g.bits & !kept;
        if lost == 0 {
            return;
        }
        self.stats.retractions += 1;
        self.tm.lost_grows.push((u32::try_from(r).unwrap_or(0), lost));
        if kept == 0 {
            self.grows[r].remove(i);
        } else {
            self.grows[r][i].bits = kept;
        }
        self.retract_bits(r, lost, k);
    }

    /// Whether fact `f` holds in the state as of its rank `k`.
    fn supported(&mut self, f: usize, k: u32) -> bool {
        let sc = self.w.site_count();
        let hb = sc + self.half_rank.len();
        if f < sc {
            let (r, _) = self.w.site_place(f);
            let t = self.mask_at(r, k);
            self.stats.evals += 1;
            return t != 0 && self.eval_at(self.plan.folded.sites[f], t, k);
        }
        debug_assert!(f < hb, "a growth is checked by `check_grow`");
        self.eval_at(self.plan.folded.halves[f - sc], 0, k)
    }

    fn eval_at(&self, rule: u32, t: u64, k: u32) -> bool {
        let env = Env {
            w: self.w,
            code: &self.plan.folded.code,
            defs: &self.plan.folded.defs,
            opts: &self.plan.opts,
            rolls: &self.plan.rolls,
            v: At { r: self, rank: k },
            cache: &[],
            epoch: 1,
            memo: &self.def_memo,
        };
        env.eval(rule as usize, t)
    }

    /// Region `r`'s mask as of rank `k`.
    fn mask_at(&self, r: usize, k: u32) -> u64 {
        self.grows[r].iter().take_while(|g| g.rank < k).fold(0, |m, g| m | g.bits)
    }

    /// Owned clocks as of rank `k`.
    fn owned_at(&self, k: u32) -> u64 {
        if !self.plan.clocks {
            return self.w.time.all;
        }
        let held = (0..self.half_rank.len()).filter(|h| self.half_rank[*h] < k);
        held.fold(self.owned0, |a, h| a | self.w.half(h).0)
    }

    /// The slices of growth `g` of region `r` its support still gives in the state as of its
    /// rank.
    fn grow_support(&self, r: usize, g: &Grow) -> u64 {
        let (w, k) = (self.w, g.rank);
        match g.why {
            BY_START if r == w.start as usize => {
                g.bits & if self.plan.clocks { self.owned_at(k) } else { w.time.start_mask }
            }
            BY_START => 0,
            BY_WAIT => {
                if !self.can_stay[r] {
                    return 0;
                }
                let before = self.mask_at(r, k);
                let owned = if self.plan.clocks { self.owned_at(k) } else { w.time.all };
                let (mut x, mut kept) = (g.bits, 0u64);
                // Waiting starts from an owned slice and passes only owned ones (`expand`),
                // slice by slice: the one before may be in this growth.
                while x != 0 {
                    let b = x.trailing_zeros() as usize;
                    x &= x - 1;
                    let ok = b > 0
                        && ((before | kept) & owned) >> (b - 1) & 1 == 1
                        && owned >> b & 1 == 1
                        && w.stays(r)
                            .find(|s| w.site_target(*s) as usize == b)
                            .is_none_or(|s| self.eval_at(self.plan.folded.sites[s], before, k));
                    if ok {
                        kept |= 1 << b;
                    }
                }
                kept
            }
            e => {
                let e = e as usize;
                if self.fired.get(e).is_some_and(|f| *f < k) {
                    g.bits & self.mask_at(w.site_place(e).0, k)
                } else {
                    0
                }
            }
        }
    }

    fn retract(&mut self, f: usize, k: u32) {
        self.stats.retractions += 1;
        let w = self.w;
        let sc = w.site_count();
        self.tm.out.insert(f);
        self.tm.out_list.push(u32::try_from(f).unwrap_or(0));
        if f >= sc {
            self.retract_half(f - sc);
            return;
        }
        let was = std::mem::replace(&mut self.fired[f], NONE);
        self.done.remove(f);
        let (_, g) = w.site_place(f);
        let t = w.site_target(f) as usize;
        match g {
            EXITS => {
                let e = u32::try_from(f).unwrap_or(NONE);
                self.suspect_grows(t, k, NONE, |y| (y == e).then_some(u64::MAX));
            }
            EVENTS => {
                self.counters[t] -= 1;
                if let Ok(at) = self.lit[t].binary_search(&was) {
                    self.lit[t].remove(at);
                }
                if w.def_reads_counter(t) {
                    self.epoch += 1;
                }
                self.suspect_readers(w.counter_sites(t), k, NONE);
            }
            _ => {
                if self.loc_rank[t] != k {
                    return;
                }
                let checks = w.sites_into(CHECKS, t).iter();
                let next = checks.map(|c| self.fired[*c as usize]).min().unwrap_or(NONE);
                self.loc_rank[t] = next;
                if next == NONE {
                    self.reached.remove(t);
                }
                if let Some((to, item)) = self.away[t] {
                    self.outbox.push(Msg::Move { to, item, from: k, rank: next });
                }
                if let Some(z) = self.holds[t] {
                    self.uncollect(z, k);
                    if next != NONE {
                        self.collect_quiet(z, next);
                    }
                    self.suspect_readers(w.item_sites(usize::from(z)), k, next);
                }
            }
        }
    }

    /// [`Reach::collect`] without rechecks (a copy moved to a later rank).
    fn collect_quiet(&mut self, item: u16, k: u32) {
        self.count_change(item, 1);
        let v = &mut self.copies[usize::from(item)];
        let at = v.partition_point(|x| *x < k);
        v.insert(at, k);
    }

    fn retract_half(&mut self, h: usize) {
        let w = self.w;
        let k = self.half_rank[h];
        self.half_rank[h] = NONE;
        self.owned = self.owned_at(NONE);
        let lost = w.half(h).0 & !self.owned0;
        if lost == 0 {
            return;
        }
        let start = w.start as usize;
        for r in 0..self.masks.len() {
            self.suspect_grows(r, k, NONE, |y| match y {
                BY_WAIT => Some(lost | lost << 1),
                BY_START if r == start => Some(lost),
                _ => None,
            });
        }
    }

    /// Slices `lost` of region `r` went with its growth at rank `k`.
    fn retract_bits(&mut self, r: usize, lost: u64, k: u32) {
        let w = self.w;
        self.masks[r] &= !lost;
        if !self.tm.lost_flag.contains(r) {
            self.tm.lost_flag.insert(r);
            self.tm.lost.push(u32::try_from(r).unwrap_or(0));
        }
        let first = self.grows[r].first().map_or(NONE, |g| g.rank);
        if first == NONE {
            self.seen.remove(r);
        }
        // Sites of the region: those reading time, and all of them once the region was not yet
        // reached as of their rank.
        for g in [EXITS, EVENTS, CHECKS] {
            for s in w.group(r, g) {
                let ks = self.fired[s];
                if w.time_dep(s) || first >= ks {
                    self.suspect(s, ks, k, NONE);
                }
            }
        }
        for e in w.group(r, EXITS) {
            if self.fired[e] != NONE {
                let (t, e) = (w.site_target(e) as usize, u32::try_from(e).unwrap_or(NONE));
                self.suspect_grows(t, k, NONE, |y| (y == e).then_some(lost));
            }
        }
        self.suspect_grows(r, k, NONE, |y| (y == BY_WAIT).then_some(u64::MAX));
    }

    /// Let later runs derive only the sites and half-days that hold now, until
    /// [`Reach::release`]: enough for any smaller inventory, whose fixed point is inside this
    /// one. Kept across [`Reach::reset`].
    pub(crate) fn only_what_holds(&mut self) {
        let sc = self.w.site_count();
        let holding = (0..sc).filter(|s| self.done.contains(*s));
        let halves = (0..self.half_rank.len()).filter(|h| self.half_rank[*h] != NONE);
        for f in holding.chain(halves.map(|h| sc + h)) {
            self.tm.out.insert(f);
            self.tm.out_list.push(u32::try_from(f).unwrap_or(0));
        }
        self.tm.active = true;
    }

    /// End [`Reach::only_what_holds`].
    pub(crate) fn release(&mut self) {
        self.tm.active = false;
        for s in self.tm.out_list.drain(..) {
            self.tm.out.remove(s as usize);
        }
    }

    /// After the retractions of a removal: derive again what the smaller state supports.
    fn rederive(&mut self) {
        self.begin_rederive();
        self.run();
        self.end_rederive();
    }

    /// Gate [`Reach::run`] to the retracted sites and queue them.
    pub(crate) fn begin_rederive(&mut self) {
        self.forget_defs();
        self.tm.active = true;
        for k in 0..self.tm.out_list.len() {
            let s = self.tm.out_list[k];
            self.push_site(s);
        }
        for k in 0..self.tm.lost.len() {
            let r = self.tm.lost[k] as usize;
            self.pull(r);
        }
    }

    /// Gate to the sites that were retracted by earlier removals only, without queueing
    /// anything (a state that lost nothing itself but is handed copies back).
    pub(crate) fn gate(&mut self) {
        self.tm.active = true;
    }

    pub(crate) fn end_rederive(&mut self) {
        self.tm.active = false;
        // Churn: retracted facts that hold again now (each retraction pushed one entry).
        let sc = self.w.site_count();
        for s in self.tm.out_list.drain(..) {
            let s = s as usize;
            self.tm.out.remove(s);
            let back = if s < sc { self.fired[s] } else { self.half_rank[s - sc] };
            self.stats.churn += u64::from(back != NONE);
        }
        for (r, lost) in self.tm.lost_grows.drain(..) {
            self.stats.churn += u64::from(self.masks[r as usize] & lost == lost);
        }
        for r in self.tm.lost.drain(..) {
            self.tm.lost_flag.remove(r as usize);
        }
    }

    /// Region `r` lost slices: pass it again what the start and its done exits give.
    fn pull(&mut self, r: usize) {
        let w = self.w;
        let ri = u32::try_from(r).unwrap_or(0);
        if r == w.start as usize {
            let m = if self.plan.clocks { self.owned } else { w.time.start_mask };
            self.grow(ri, m, BY_START);
        }
        for &e in w.sites_into(EXITS, r) {
            if self.done.contains(e as usize) {
                let m = self.masks[w.site_place(e as usize).0];
                self.grow(ri, m, e);
            }
        }
        if self.masks[r] != 0 {
            self.requeue(r);
        }
    }

    /// Whether the goal rule holds now.
    #[must_use]
    pub fn goal(&self) -> bool {
        self.env().eval(self.plan.folded.goal as usize, 0)
    }

    /// Reached regions with their masks.
    pub fn reached_regions(&self) -> impl Iterator<Item = (u32, u64)> + '_ {
        self.masks
            .iter()
            .enumerate()
            .filter(|(_, m)| **m != 0)
            .map(|(i, m)| (u32::try_from(i).unwrap_or(0), *m))
    }

    fn env(&self) -> Env<'_, Now<'_>> {
        Env {
            w: self.w,
            code: &self.plan.folded.code,
            defs: &self.plan.folded.defs,
            opts: &self.plan.opts,
            rolls: &self.plan.rolls,
            v: Now {
                counts: &self.counts,
                counters: &self.counters,
                groups: &self.groups,
                masks: &self.masks,
            },
            cache: &self.def_cache,
            epoch: self.epoch,
            memo: &self.def_memo,
        }
    }

    fn eval(&mut self, rule: u32, t: u64) -> bool {
        self.stats.evals += 1;
        self.env().eval(rule as usize, t)
    }

    fn push_site(&mut self, s: u32) {
        let i = s as usize;
        if !self.done.contains(i) && !self.pending[i] {
            self.pending[i] = true;
            self.stats.pushes += 1;
            self.sites_q.push(s);
        }
    }

    fn requeue(&mut self, r: usize) {
        if !self.queued[r] {
            self.queued[r] = true;
            self.stats.pushes += 1;
            self.regions_q.push(u32::try_from(r).unwrap_or(0));
        }
    }

    /// New slices `new` of region `r`, one growth supported by `why`.
    fn set_bits(&mut self, r: usize, new: u64, why: u32) {
        if new == 0 {
            return;
        }
        let rank = self.next();
        self.grows[r].push(Grow { rank, bits: new, why });
        self.masks[r] |= new;
    }

    fn grow(&mut self, r: u32, m: u64, why: u32) {
        let i = r as usize;
        let new = m & !self.masks[i];
        if new != 0 {
            self.set_bits(i, new, why);
            self.requeue(i);
        }
    }

    fn visit(&mut self, r: usize) {
        let w = self.w;
        let first = !self.seen.contains(r);
        self.seen.insert(r);
        if self.can_stay[r] {
            let m = self.expand(r, self.masks[r]);
            self.set_bits(r, m & !self.masks[r], BY_WAIT);
        }
        let m = self.masks[r];
        for s in w.group(r, EXITS) {
            if self.done.contains(s) {
                self.grow(w.site_target(s), m, u32::try_from(s).unwrap_or(0));
            } else if first || w.time_dep(s) {
                self.site(s);
            }
        }
        for g in [EVENTS, CHECKS] {
            for s in w.group(r, g) {
                if !self.done.contains(s) && (first || w.time_dep(s)) {
                    self.site(s);
                }
            }
        }
    }

    fn site(&mut self, s: usize) {
        let w = self.w;
        if s >= w.site_count() {
            self.clocks();
            return;
        }
        let (r, g) = w.site_place(s);
        if g == STAYS {
            if self.masks[r] != 0 && self.can_stay[r] {
                self.requeue(r);
            }
            return;
        }
        let m = self.masks[r];
        if m == 0 || self.done.contains(s) || (self.tm.active && !self.tm.out.contains(s)) {
            return;
        }
        let target = w.site_target(s);
        if !self.eval(self.plan.folded.sites[s], m) {
            return;
        }
        self.done.insert(s);
        let k = self.next();
        self.fired[s] = k;
        match g {
            EXITS => self.grow(target, m, u32::try_from(s).unwrap_or(0)),
            EVENTS => {
                let c = target as usize;
                self.counters[c] += 1;
                let at = self.lit[c].partition_point(|x| *x < k);
                self.lit[c].insert(at, k);
                if w.def_reads_counter(c) {
                    self.epoch += 1;
                }
                for &d in w.counter_sites(c) {
                    self.push_site(d);
                }
            }
            _ => {
                let l = target as usize;
                if !self.reached.contains(l) {
                    self.reached.insert(l);
                    self.loc_rank[l] = k;
                    if let Some(item) = self.holds[l] {
                        self.collect(item, k);
                    } else if let Some((to, item)) = self.away[l] {
                        self.outbox.push(Msg::Give { to, item, rank: k });
                    }
                }
            }
        }
    }

    /// A half-day rule's input changed: evaluate the ones not holding, recompute the owned
    /// clocks, and pass a change on to the start region and every region that can wait.
    fn clocks(&mut self) {
        let w = self.w;
        let sc = w.site_count();
        for h in 0..self.half_rank.len() {
            let blocked = self.tm.active && !self.tm.out.contains(sc + h);
            if self.half_rank[h] == NONE
                && !blocked
                && self.env().eval(self.plan.folded.halves[h] as usize, 0)
            {
                self.half_rank[h] = self.next();
            }
        }
        let owned = self.owned_at(NONE);
        if owned == self.owned {
            return;
        }
        self.owned = owned;
        self.grow(w.start, owned, BY_START);
        for i in 0..self.masks.len() {
            if self.masks[i] != 0 && self.can_stay[i] {
                self.requeue(i);
            }
        }
    }

    /// `ExpandTimeForward`: from each reachable slice, wait forward until a stay rule fails.
    fn expand(&mut self, r: usize, m: u64) -> u64 {
        let w = self.w;
        let stays = w.stays(r);
        let clocks = self.plan.clocks;
        let all = w.time.all;
        if stays.is_empty() && !clocks {
            let mut x = m;
            for sh in [1, 2, 4, 8, 16, 32] {
                x |= x << sh;
            }
            return x & all;
        }
        let filtered = if clocks { m & self.owned } else { m };
        let mut out = filtered;
        let mut can_wait = false;
        for i in 0..64u32 {
            let bit = 1u64 << i;
            if bit & all == 0 {
                break;
            }
            if filtered & bit != 0 {
                can_wait = true;
            } else if can_wait {
                if clocks && self.owned & bit == 0 {
                    can_wait = false;
                    continue;
                }
                match stays.clone().find(|s| w.site_target(*s) == i) {
                    Some(s) if !self.env().eval(self.plan.folded.sites[s] as usize, m) => {
                        can_wait = false;
                    }
                    _ => out |= bit,
                }
            }
        }
        out
    }

    /// The clocks owned with the start inventory held (see [`Plan::owned_start`]).
    pub(crate) fn compute_owned(&self) -> u64 {
        let w = self.w;
        let Some(o) = w.time.owned.filter(|_| self.plan.clocks) else { return w.time.all };
        let env = self.env();
        let m = (0..o.halves)
            .filter(|h| env.eval(self.plan.folded.halves[*h] as usize, 0))
            .fold(0, |a, h| a | w.half(h).0);
        if m == 0 { o.none_owned_mask } else { m }
    }
}

#[cfg(test)]
/// Same fixed point: regions with masks, counters, counts and reached locations.
pub(crate) fn assert_same(w: &World, got: &Reach<'_>, want: &Reach<'_>, what: &str) {
    assert_eq!(got.masks, want.masks, "masks, {what}");
    assert_eq!(got.counters, want.counters, "counters, {what}");
    assert_eq!(got.counts, want.counts, "counts, {what}");
    let reached = |r: &Reach<'_>| (0..w.locations()).filter(|l| r.reached(*l as u32)).count();
    assert_eq!(reached(got), reached(want), "reached, {what}");
    for l in 0..u32::try_from(w.locations()).unwrap() {
        assert_eq!(got.reached(l), want.reached(l), "location {l}, {what}");
    }
    assert_eq!(got.goal(), want.goal(), "goal, {what}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::Instance;
    use serde_json::json;

    /// A clock world of two slices where `dawn` owns the first and `dusk` the second (none
    /// owned: the first, as the solver starts).
    fn clock_world(start: &[&str]) -> (World, Instance) {
        let m = json!({
            "options": [],
            "items": [{"key": "dawn", "id": 1}, {"key": "dusk", "id": 2}],
            "locations": [{"key": "l", "id": 1}],
            "start": "a",
            "x.time": {"slices": 2, "start_mask": 1, "owned": {
                "when": {"true": {}}, "none_owned_mask": 1,
                "half_days": [{"first": 0, "last": 0, "rule": {"has": "dawn"}},
                              {"first": 1, "last": 1, "rule": {"has": "dusk"}}]}},
            "defs": {},
            "regions": [{"key": "a", "can_stay": false,
                         "checks": [{"loc": "l", "rule": {"true": {}}}],
                         "exits": [], "events": [], "stays": []}],
            "goal": {"reach": "region:a"}
        });
        let inst = Instance {
            locations: vec!["l".to_owned()],
            start_inventory: start.iter().map(|k| (*k).to_owned()).collect(),
            ..Instance::default()
        };
        (World::load(&m).unwrap(), inst)
    }

    /// The solver's start state owns the union of the owned half-days, or the first slice only
    /// when none is owned: a start item that owns only the later slice leaves the first one out.
    #[test]
    fn a_start_clock_that_leaves_out_the_first_slice_does_not_own_it() {
        for (start, extra, want) in [
            (&[][..], &[][..], 0b01u64),
            (&["dawn"][..], &[][..], 0b01),
            (&["dusk"][..], &[][..], 0b10),
            (&["dusk"][..], &["dawn"][..], 0b11),
            (&[][..], &["dawn", "dusk"][..], 0b11),
        ] {
            let (w, inst) = clock_world(start);
            let plan = Plan::new(&w, &inst).unwrap();
            let extra: Vec<u16> = extra.iter().map(|k| w.item(k).unwrap()).collect();
            let r = plan.solve(&w, &extra);
            assert_eq!(r.masks[w.start as usize], want, "start {start:?}, held {extra:?}");
        }
    }

    /// Support that exists only through the fact itself: `a` opens with `key` or with the `gem`
    /// found in `b`, which is reached only through `a`; an event in `a` lights a counter that
    /// opens `c`, whose exit leads back to `a`; and the later slice of `a` is reached by waiting
    /// (with `boot`) and passed back and forth between `a` and `b`.
    fn cyclic() -> (World, Instance) {
        let t = json!({"true": {}});
        let m = json!({
            "options": [],
            "items": [{"key": "key", "id": 1}, {"key": "gem", "id": 2}, {"key": "boot", "id": 3}],
            "locations": [{"key": "la", "id": 1}, {"key": "lb", "id": 2}, {"key": "lc", "id": 3},
                          {"key": "late", "id": 4}],
            "start": "s",
            "x.time": {"slices": 2, "start_mask": 1},
            "defs": {},
            "regions": [
                {"key": "s", "can_stay": false, "checks": [], "events": [], "stays": [],
                 "exits": [{"to": "a", "rule": {"any": [{"has": "key"}, {"has": "gem"}]}}]},
                {"key": "a", "can_stay": true,
                 "checks": [{"loc": "la", "rule": t},
                            {"loc": "late", "rule": {"x.time_any": 2}}],
                 "events": [{"event": "lit", "rule": t}],
                 "stays": [{"slice": 1, "rule": {"has": "boot"}}],
                 "exits": [{"to": "b", "rule": t}, {"to": "c", "rule": {"ge": [{"counter": "lit"}, 1]}}]},
                {"key": "b", "can_stay": false, "checks": [{"loc": "lb", "rule": t}],
                 "events": [], "stays": [], "exits": [{"to": "a", "rule": t}]},
                {"key": "c", "can_stay": false, "checks": [{"loc": "lc", "rule": t}],
                 "events": [], "stays": [], "exits": [{"to": "a", "rule": t}]}
            ],
            "goal": {"reach": "region:c"}
        });
        let pool = ["key", "gem", "boot", "boot"];
        let inst = Instance {
            locations: ["la", "lb", "lc", "late"].map(str::to_owned).to_vec(),
            pool: pool.map(str::to_owned).to_vec(),
            pool_class: pool.iter().map(|k| ((*k).to_owned(), "progression".to_owned())).collect(),
            ..Instance::default()
        };
        (World::load(&m).unwrap(), inst)
    }

    #[test]
    fn removal_does_not_keep_support_that_runs_through_itself() {
        let (w, inst) = cyclic();
        let plan = Plan::new(&w, &inst).unwrap();
        let item = |k: &str| w.item(k).unwrap();
        let mut placed = vec![None; w.locations()];
        placed[w.loc("lb").unwrap() as usize] = Some(item("gem"));
        for (extra, take) in [
            (vec!["key", "boot"], "key"),
            (vec!["key", "boot"], "boot"),
            (vec!["key", "boot", "boot"], "boot"),
        ] {
            let extra: Vec<u16> = extra.into_iter().map(item).collect();
            let mut r = plan.state(&w, &extra, &placed);
            assert!(r.reached(w.loc("lc").unwrap()), "the full inventory reaches everything");
            r.remove(item(take));
            let mut less = extra.clone();
            less.remove(less.iter().position(|i| *i == item(take)).unwrap());
            assert_same(&w, &r, &plan.state(&w, &less, &placed), &format!("without {take}"));
        }
        let r = {
            let mut r = plan.state(&w, &[item("key"), item("boot")], &placed);
            r.remove(item("key"));
            r
        };
        assert!(!r.reached(w.loc("la").unwrap()), "a gem behind `a` does not open `a`");
        assert_eq!(r.masks[w.region("a").unwrap() as usize], 0);
    }

    /// Defs `d0` to `d{n}`, each reading the next twice (`all`, so both are evaluated while
    /// they hold), and the last reading an item and time, so no def is cached per solve.
    fn doubling(n: usize) -> (World, Instance) {
        let mut defs = serde_json::Map::new();
        for k in 0..n {
            let next = json!({"ref": format!("d{}", k + 1)});
            defs.insert(format!("d{k}"), json!({"all": [next, next]}));
        }
        defs.insert(format!("d{n}"), json!({"all": [{"has": "key"}, {"x.time_any": 1}]}));
        let m = json!({
            "options": [],
            "items": [{"key": "key", "id": 1}],
            "locations": [{"key": "la", "id": 1}],
            "start": "s",
            "x.time": {"slices": 2, "start_mask": 1},
            "defs": defs,
            "regions": [{"key": "s", "can_stay": false, "events": [], "stays": [], "exits": [],
                         "checks": [{"loc": "la", "rule": {"ref": "d0"}}]}],
            "goal": {"true": {}}
        });
        let inst = Instance { locations: vec!["la".to_owned()], ..Instance::default() };
        (World::load(&m).unwrap(), inst)
    }

    #[test]
    fn a_def_read_twice_is_evaluated_once_per_evaluation() {
        let (w, inst) = doubling(40);
        assert!(!w.def_timeless(0), "time-dependent, so not cached per solve");
        let plan = Plan::new(&w, &inst).unwrap();
        let key = w.item("key").unwrap();
        let mut r = plan.state(&w, &[key], &[]);
        assert!(r.reached(w.loc("la").unwrap()));
        let solve = r.def_memo.bodies.get();
        assert!(solve <= 41 * r.stats.evals.max(1), "solve: {solve} def bodies");
        // Taking the key back checks the site as of its rank (the take-back path).
        r.remove(key);
        assert!(!r.reached(w.loc("la").unwrap()));
        let back = r.def_memo.bodies.get() - solve;
        assert!((1..=41 * 4).contains(&back), "take-back: {back} def bodies");
    }

    #[test]
    fn removal_retracts_only_what_depended_on_the_item() {
        let (w, inst) = cyclic();
        let plan = Plan::new(&w, &inst).unwrap();
        let item = |k: &str| w.item(k).unwrap();
        let mut r = plan.state(&w, &[item("key"), item("boot")], &[]);
        r.stats = Stats::default();
        r.remove(item("boot"));
        // The waited slice of `a`, the copies of it `b` and `c` got, and the check reading it.
        assert_eq!(r.stats.retractions, 4, "{:?}", r.stats);
        assert_eq!(r.stats.churn, 0, "nothing retracted holds again: {:?}", r.stats);
        assert!(r.reached(w.loc("lc").unwrap()));
        assert!(!r.reached(w.loc("late").unwrap()));
    }

    /// `a` opens with `key`, or with the `gem` lying in `b` (open from the start, but its gem is
    /// found after `a` opened); `la` in `a` is free and `lk` needs `key`.
    fn two_ways() -> (World, Instance) {
        let t = json!({"true": {}});
        let m = json!({
            "options": [],
            "items": [{"key": "key", "id": 1}, {"key": "gem", "id": 2}],
            "locations": [{"key": "la", "id": 1}, {"key": "lk", "id": 2}, {"key": "lb", "id": 3}],
            "start": "s",
            "x.time": {"slices": 1, "start_mask": 1},
            "defs": {},
            "regions": [
                {"key": "s", "can_stay": false, "checks": [], "events": [], "stays": [],
                 "exits": [{"to": "a", "rule": {"any": [{"has": "key"}, {"has": "gem"}]}},
                           {"to": "b", "rule": t}]},
                {"key": "a", "can_stay": false, "events": [], "stays": [], "exits": [],
                 "checks": [{"loc": "la", "rule": t}, {"loc": "lk", "rule": {"has": "key"}}]},
                {"key": "b", "can_stay": false, "checks": [{"loc": "lb", "rule": t}],
                 "events": [], "stays": [], "exits": []}
            ],
            "goal": {"true": {}}
        });
        let inst = Instance {
            locations: ["la", "lk", "lb"].map(str::to_owned).to_vec(),
            pool: ["key", "gem"].map(str::to_owned).to_vec(),
            pool_class: ["key", "gem"]
                .iter()
                .map(|k| ((*k).to_owned(), "progression".to_owned()))
                .collect(),
            ..Instance::default()
        };
        (World::load(&m).unwrap(), inst)
    }

    #[test]
    fn churn_counts_retracted_facts_that_the_same_removal_derives_again() {
        let (w, inst) = two_ways();
        let plan = Plan::new(&w, &inst).unwrap();
        let item = |k: &str| w.item(k).unwrap();
        let mut placed = vec![None; w.locations()];
        placed[w.loc("lb").unwrap() as usize] = Some(item("gem"));
        let mut r = plan.state(&w, &[item("key")], &placed);
        r.stats = Stats::default();
        r.remove(item("key"));
        // The exit into `a` (derived with `key`, before the gem was found), `a`'s growth, `la`
        // and `lk` go; the gem derives the exit, the growth and `la` again, `lk` stays lost.
        assert_eq!(r.stats.retractions, 4, "{:?}", r.stats);
        assert_eq!(r.stats.churn, 3, "{:?}", r.stats);
        assert!(r.reached(w.loc("la").unwrap()));
        assert!(!r.reached(w.loc("lk").unwrap()));
        let s = r.stats.summary(1, 1);
        assert_eq!(s["churn"], serde_json::json!({"rederived": 3, "net_lost": 1, "share": 0.75}));
    }
}
