//! Incremental reachability over the compiled rules section: the least fixed point of regions
//! (with time masks), events and locations for an inventory. Rules are read in place from
//! the bytecode; collecting an item rechecks only the sites the section's item index lists.
//!
//! Semantics follow the world's own solver: a region holds a time mask; a rule is evaluated
//! against the mask of the region it sits in; an open exit passes the whole mask on; a region
//! where time can pass expands its mask forward until a stay rule fails. All of it is monotone
//! in items, counters and masks, so the fixed point does not depend on evaluation order.
//!
//! What is added to a fixed point can be taken back exactly: a state keeps a journal of every
//! step since a mark ([`Reach::undo_to`]), which is how a fill reaches the state of each group
//! it places (`crate::room`).

use std::cell::Cell;

use bits::BitSet;

use crate::bitrules::{Bits, COUNTER, GROUP, ITEM, REGION, STATIC};
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
    /// Bytecode nodes read, for [`Reach::replay`] (counted only where `crate::COUNTED`).
    nodes: Cell<u64>,
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
        if crate::COUNTED {
            self.memo.nodes.set(self.memo.nodes.get() + 1);
        }
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
        if crate::COUNTED {
            self.memo.nodes.set(self.memo.nodes.get() + 1);
        }
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
    eval_statics(w, opts, rolls, [rule])[0]
}

/// [`eval_static`] for several rules.
pub(crate) fn eval_statics(
    w: &World,
    opts: &[i32],
    rolls: &[i16],
    rules: impl IntoIterator<Item = u32>,
) -> Vec<bool> {
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
    rules.into_iter().map(|rule| env.eval(rule as usize, 0)).collect()
}

/// Work counters of one or more solves (the complexity contract in the rules section doc).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Rule evaluations (sites whose rule was run; stay and clock rules not counted).
    pub evals: u64,
    /// Of `evals`: answered by a bit rule, not by the bytecode.
    pub bits: u64,
    /// Worklist pushes: regions queued plus sites queued.
    pub pushes: u64,
    /// Items collected during a run (each walks its dependency list once).
    pub collected: u64,
    /// Dependency entries walked for collected items, summed.
    pub rechecks: u64,
    /// The most dependency entries one collected item walked.
    pub max_recheck: u64,
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
        })
    }

    /// Add another run's counters.
    pub fn absorb(&mut self, o: &Stats) {
        self.evals += o.evals;
        self.bits += o.bits;
        self.pushes += o.pushes;
        self.collected += o.collected;
        self.rechecks += o.rechecks;
        self.max_recheck = self.max_recheck.max(o.max_recheck);
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
}
/// Growths per region the state reserves room for.
const GROWS: usize = 4;

/// One reachability state. Items are added with [`Reach::add`] and [`Reach::hold`]; what was
/// added since a mark is taken back with [`Reach::undo_to`]. Reset it to reuse its buffers for
/// another solve of the same world and plan.
///
/// Every fact has a rank, unique and increasing in derivation order: a site that held (an
/// exit, event or check), a half-day rule that held, a growth of a region's mask (the slices
/// added at once: the start, an exit passing its source's slices, or waiting forward). A
/// collected copy has the rank of the check that reached its location.
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
    /// The bit rules this state answers by ([`Plan::rules`]).
    rules: &'w Bits,
    /// One bit per atom of the bit rules: whether it holds now. Kept up to date with `counts`,
    /// `counters`, `groups` and `masks`; the option atoms are the plan's.
    atoms: Vec<u64>,
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
    /// This state serves a fill ([`Reach::lean`]).
    lean: bool,
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
    grows: Vec<Vec<u64>>,
    /// Per location: rank it was reached at (the first check that held).
    loc_rank: Vec<u32>,
    /// Per item: ranks of the copies collected, ascending.
    copies: Vec<Vec<u32>>,
    /// Per counter: ranks of the events that fired, ascending.
    lit: Vec<Vec<u32>>,
    gate: Gate,
    /// Every step since [`Reach::keep_journal`], in order, for [`Reach::undo_to`].
    journal: Vec<Step>,
    journaling: bool,
}

/// A fixed point of a journaled state to come back to ([`Reach::mark`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mark {
    /// Journal steps then.
    steps: usize,
    /// The next rank then.
    seq: u32,
}

impl Mark {
    /// Journal steps the state held at the mark.
    pub(crate) fn steps(self) -> usize {
        self.steps
    }
}

/// One change a forward run or its caller made to a state, as the journal keeps it: enough to
/// take it back when everything after it was taken back first.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// The site held and took a rank.
    Fired(u32),
    /// The region's mask gained its last growth.
    Grow(u32),
    /// The region was visited for the first time.
    Seen(u32),
    /// The half-day rule held and took a rank.
    Half(u32),
    /// A copy of the item was collected at the rank.
    Copy(u16, u32),
    /// The location, which held nothing, was given something to hold.
    Hold(u32),
    /// The location was given another player's item to hold.
    Away(u32),
}

/// What later runs may derive while [`Reach::only_what_holds`] is on (sized once, reused).
#[derive(Debug, Clone)]
struct Gate {
    /// Sites and half-days that may hold: as a set, and in order.
    out: BitSet,
    out_list: Vec<u32>,
    /// Runs derive only what `out` holds.
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
            rules: plan.rules(w),
            atoms: plan.atoms.clone(),
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
            lean: false,
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
            loc_rank: vec![NONE; w.locations()],
            // At most one copy per existing copy of the seed, so these never grow.
            copies: (0..w.items()).map(|i| Vec::with_capacity(plan.copies(i))).collect(),
            gate: Gate { out: BitSet::new(sites + halves), out_list: Vec::new(), active: false },
            journal: Vec::new(),
            journaling: false,
        };
        r.start();
        r
    }

    /// This state as a fill's: the checks of the locations that do not exist this seed and
    /// grant nothing (`Plan::absent`) are never evaluated, here and after every reset. Nothing
    /// is ever placed there and a fill asks only about locations that exist, so everything else
    /// the state derives is the same; [`Reach::reached`] says `false` for those locations.
    #[must_use]
    pub fn lean(mut self) -> Self {
        self.lean = true;
        self.skip_absent();
        self
    }

    /// Mark the absent locations' checks done: done without a rank.
    fn skip_absent(&mut self) {
        for &s in &self.plan.absent {
            self.done.insert(s as usize);
        }
    }

    /// Back to the empty state, keeping every buffer.
    pub fn reset(&mut self) {
        self.reset_from(1);
    }

    /// [`Reach::reset`], handing out ranks from `seq` on.
    pub(crate) fn reset_from(&mut self, seq: u32) {
        debug_assert!(self.atoms_agree(), "an atom bit went stale");
        self.counts.fill(0);
        self.counters.fill(0);
        self.groups.fill(0);
        self.atoms.copy_from_slice(&self.plan.atoms);
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
        self.journal.clear();
        self.journaling = false;
        self.start();
        if self.lean {
            self.skip_absent();
        }
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
        self.grow(self.w.start, start_mask);
    }

    fn next(&mut self) -> u32 {
        let k = self.seq;
        // The rank `NONE` stands for "none": a rank must never reach it, whatever the build.
        assert!(k < NONE, "the ranks of a solve ran out");
        self.seq += 1;
        k
    }

    /// Whether location `l` is reached.
    #[must_use]
    pub fn reached(&self, l: u32) -> bool {
        self.reached.contains(l as usize)
    }

    /// The reached locations as set words: for telling whether two solves reached the same.
    pub(crate) fn reached_words(&self) -> &[u64] {
        self.reached.words()
    }

    /// Whether a rule that is one step from open reads `item`: an exit, event or check of a
    /// reached region that does not hold yet (the check of a location that holds nothing does
    /// not count: reaching it changes nothing), or a half-day not yet owned.
    /// Stay rules are left out: they have no state of their own to ask.
    pub(crate) fn wants(&self, item: u16) -> bool {
        let w = self.w;
        let sc = w.site_count();
        w.item_sites(usize::from(item)).iter().any(|s| {
            let s = *s as usize;
            if s >= sc {
                return self.half_rank.contains(&NONE);
            }
            let (r, g) = w.site_place(s);
            let l = w.site_target(s) as usize;
            let opens = g != CHECKS || self.holds[l].is_some() || self.away[l].is_some();
            opens && g != STAYS && self.masks[r] != 0 && !self.done.contains(s)
        })
    }

    /// Whether one more copy of `item` would make a rule on the frontier hold right now: one of
    /// the rules [`Reach::wants`] names, evaluated with the copy counted, and also the check of
    /// a location that holds nothing (reaching it is a place more for an item). Call on a fixed
    /// point; nothing is changed but the work counter.
    pub(crate) fn opens(&mut self, item: u16) -> bool {
        let w = self.w;
        let sc = w.site_count();
        // A def found true is cached as true for good (rules are monotone): what the extra
        // copy makes true must not stay behind.
        let reads = w.def_reads_item(usize::from(item));
        let defs: Vec<u32> = self.def_cache.iter().filter(|_| reads).map(Cell::get).collect();
        self.count_change(item, 1);
        let mut evals = 0;
        let hit = w.item_sites(usize::from(item)).iter().any(|s| {
            let s = *s as usize;
            if s >= sc {
                return (0..self.half_rank.len())
                    .any(|h| self.half_rank[h] == NONE && self.now(sc + h, 0));
            }
            let (r, g) = w.site_place(s);
            if g == STAYS || self.masks[r] == 0 || self.done.contains(s) {
                return false;
            }
            evals += 1;
            self.now(s, self.masks[r])
        });
        self.count_change(item, -1);
        self.def_cache.iter().zip(defs).for_each(|(c, was)| c.set(was));
        self.stats.evals += evals;
        hit
    }

    /// Set what a location holds; it is collected when the location is reached. A location
    /// holds one item: holding the same again changes nothing.
    pub fn hold(&mut self, loc: u32, item: u16) {
        let l = loc as usize;
        if self.holds[l] == Some(item) {
            return;
        }
        let before = self.holds[l].replace(item);
        // The journal keeps no old item: undoing would leave the location empty.
        assert!(before.is_none() || !self.journaling, "a journaled location held an item");
        self.log(Step::Hold(loc));
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
        self.log(Step::Away(loc));
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

    fn log(&mut self, step: Step) {
        if self.journaling {
            self.journal.push(step);
        }
    }

    /// From now until the next reset, keep a journal of every step, so that what is added to
    /// this fixed point (items, what locations hold, and all a run derives from them) can be
    /// taken back with [`Reach::undo_to`]. Call after a [`Reach::run`].
    pub(crate) fn keep_journal(&mut self) {
        debug_assert!(self.ran && self.idle(), "a journal starts at a fixed point");
        self.journaling = true;
    }

    /// Steps the journal has room for without growing (what it holds on the heap).
    #[cfg(test)]
    pub(crate) fn journal_room(&self) -> usize {
        self.journal.capacity()
    }

    /// Stop keeping the journal and forget it: nothing more is taken back.
    pub(crate) fn end_journal(&mut self) {
        self.journaling = false;
        self.journal.clear();
    }

    /// Where the journal and the ranks stand: a fixed point to come back to.
    pub(crate) fn mark(&self) -> Mark {
        Mark { steps: self.journal.len(), seq: self.seq }
    }

    /// Take back every step after `mark` ([`Reach::mark`], taken at a fixed point), last
    /// first: the state is again the fixed point it was then, value for value. Only additions
    /// followed the mark (rules are monotone, a forward run retracts nothing), so each step is
    /// undone by removing what it added, and the ranks handed out since are handed out again.
    /// Returns the steps taken back.
    pub(crate) fn undo_to(&mut self, mark: Mark) -> usize {
        debug_assert!(self.idle(), "undo from a fixed point");
        debug_assert!(mark.steps <= self.journal.len(), "a mark of another state, or a later one");
        let w = self.w;
        let undone = self.journal.len().saturating_sub(mark.steps);
        // No fact with a rank handed out since the mark is left: the ranks go on from there,
        // so a state's ranks stay within one state's facts however often it is added to.
        self.seq = mark.seq;
        let mark = mark.steps;
        while self.journal.len() > mark {
            let Some(step) = self.journal.pop() else { break };
            match step {
                Step::Copy(item, k) => self.uncollect(item, k),
                Step::Hold(l) => self.holds[l as usize] = None,
                Step::Away(l) => self.away[l as usize] = None,
                Step::Seen(r) => self.seen.remove(r as usize),
                Step::Half(h) => {
                    // The owned clocks are what the half-days that hold give (`clocks`).
                    self.half_rank[h as usize] = NONE;
                    self.owned = self.owned_at(NONE);
                }
                Step::Grow(r) => {
                    let r = r as usize;
                    if let Some(bits) = self.grows[r].pop() {
                        self.masks[r] &= !bits;
                        self.rules.set(&mut self.atoms, REGION, r, i64::from(self.masks[r] != 0));
                    }
                }
                Step::Fired(s) => {
                    let s = s as usize;
                    let k = std::mem::replace(&mut self.fired[s], NONE);
                    self.done.remove(s);
                    let t = w.site_target(s) as usize;
                    match w.site_place(s).1 {
                        EVENTS => {
                            self.counters[t] -= 1;
                            self.rules.set(&mut self.atoms, COUNTER, t, self.counters[t]);
                            if let Ok(at) = self.lit[t].binary_search(&k) {
                                self.lit[t].remove(at);
                            }
                        }
                        CHECKS if self.loc_rank[t] == k => {
                            self.loc_rank[t] = NONE;
                            self.reached.remove(t);
                        }
                        _ => {}
                    }
                }
            }
        }
        if undone > 0 {
            // A def found true with what was taken back is no longer known.
            self.forget_defs();
        }
        undone
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
        let bits = self.rules;
        bits.set(&mut self.atoms, ITEM, i, after);
        for &(g, cap) in self.w.item_groups(i) {
            let (g, cap) = (usize::from(g), i64::from(cap));
            self.groups[g] += after.min(cap) - before.min(cap);
            bits.set(&mut self.atoms, GROUP, g, self.groups[g]);
        }
    }

    /// Whether every atom bit says what the values it is kept from say.
    fn atoms_agree(&self) -> bool {
        self.rules.atoms.iter().enumerate().all(|(bit, a)| {
            let i = usize::from(a.idx);
            let v = match usize::from(a.kind) {
                ITEM => self.counts[i],
                COUNTER => self.counters[i],
                GROUP => self.groups[i],
                REGION => i64::from(self.masks[i] != 0),
                // An option atom: the seed decided it, no value of the state moves it.
                _ => return usize::from(a.kind) == STATIC,
            };
            let bit = u16::try_from(bit).unwrap_or(u16::MAX);
            crate::bitrules::bit(&self.atoms, bit) == (v >= i64::from(a.n))
        })
    }

    /// A copy of `item` collected at rank `k`; after the first run the rules reading it are
    /// rechecked.
    fn collect(&mut self, item: u16, k: u32) {
        self.log(Step::Copy(item, k));
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
            // Items added before the first run skipped their rechecks; the clock rules are the
            // one place that reads them before any region is visited.
            if self.plan.clocks {
                self.site(self.w.site_count());
            }
        }
    }

    fn forget_defs(&mut self) {
        self.def_cache.iter().for_each(|c| c.set(0));
        self.epoch += 1;
    }

    /// Owned clocks as of rank `k`.
    fn owned_at(&self, k: u32) -> u64 {
        if !self.plan.clocks {
            return self.w.time.all;
        }
        let held = (0..self.half_rank.len()).filter(|h| self.half_rank[*h] < k);
        held.fold(self.owned0, |a, h| a | self.w.half(h).0)
    }

    /// Let later runs derive only the sites and half-days that hold now, until
    /// [`Reach::release`]: enough for any smaller inventory, whose fixed point is inside this
    /// one. Kept across [`Reach::reset`].
    pub(crate) fn only_what_holds(&mut self) {
        let sc = self.w.site_count();
        let holding = (0..sc).filter(|s| self.done.contains(*s));
        let halves = (0..self.half_rank.len()).filter(|h| self.half_rank[*h] != NONE);
        for f in holding.chain(halves.map(|h| sc + h)) {
            self.gate.out.insert(f);
            self.gate.out_list.push(u32::try_from(f).unwrap_or(0));
        }
        self.gate.active = true;
    }

    /// End [`Reach::only_what_holds`].
    pub(crate) fn release(&mut self) {
        self.gate.active = false;
        for s in self.gate.out_list.drain(..) {
            self.gate.out.remove(s as usize);
        }
    }

    /// Whether the goal rule holds now.
    #[must_use]
    pub fn goal(&self) -> bool {
        self.now(self.w.site_count() + self.half_rank.len(), 0)
    }

    /// For measurements: evaluate the rule of every exit, event and check of every reached
    /// region against the state as it is (by its bit rule where it has one), changing nothing.
    /// Returns the rules evaluated, how many hold, and the bytecode nodes and def bodies read
    /// for them (all counted).
    #[must_use]
    pub fn replay(&self) -> [u64; 4] {
        let (w, memo) = (self.w, &self.def_memo);
        let (nodes, bodies) = (memo.nodes.get(), memo.bodies.get());
        let (mut evals, mut held) = (0, 0);
        for (r, m) in self.masks.iter().enumerate().filter(|(_, m)| **m != 0) {
            for s in [EXITS, EVENTS, CHECKS].into_iter().flat_map(|g| w.group(r, g)) {
                evals += 1;
                held += u64::from(self.now(s, *m));
            }
        }
        [evals, held, memo.nodes.get() - nodes, memo.bodies.get() - bodies]
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

    /// The folded bytecode of fact `f`: a site, a half-day past the sites, then the goal.
    /// (The same layout is spelled in `Bits::of`, `Reach::goal` and `Reach::clocks`: see #208.)
    fn rule(&self, f: usize) -> u32 {
        let (folded, sc) = (&self.plan.folded, self.w.site_count());
        if f < sc {
            folded.sites[f]
        } else {
            folded.halves.get(f - sc).copied().unwrap_or(folded.goal)
        }
    }

    /// Whether fact `f` holds now, in a region of mask `t`, and whether its bit rule said so
    /// (not its bytecode).
    fn answer(&self, f: usize, t: u64) -> (bool, bool) {
        match self.rules.holds(&self.atoms, f, t) {
            Some(v) => (v, true),
            None => (self.env().eval(self.rule(f) as usize, t), false),
        }
    }

    /// Whether fact `f` holds now, in a region of mask `t`.
    fn now(&self, f: usize, t: u64) -> bool {
        self.answer(f, t).0
    }

    /// [`Reach::now`] for site `s`, counted as a rule evaluation.
    fn eval(&mut self, s: usize, t: u64) -> bool {
        let (v, by_bits) = self.answer(s, t);
        self.stats.evals += 1;
        self.stats.bits += u64::from(by_bits);
        v
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

    /// New slices `new` of region `r`, one growth (it takes a rank, as every fact does).
    fn set_bits(&mut self, r: usize, new: u64) {
        if new == 0 {
            return;
        }
        self.next();
        self.log(Step::Grow(u32::try_from(r).unwrap_or(0)));
        self.grows[r].push(new);
        self.masks[r] |= new;
        self.rules.set(&mut self.atoms, REGION, r, 1);
    }

    fn grow(&mut self, r: u32, m: u64) {
        let i = r as usize;
        let new = m & !self.masks[i];
        if new != 0 {
            self.set_bits(i, new);
            self.requeue(i);
        }
    }

    fn visit(&mut self, r: usize) {
        let w = self.w;
        let first = !self.seen.contains(r);
        if first {
            self.log(Step::Seen(u32::try_from(r).unwrap_or(0)));
        }
        self.seen.insert(r);
        if self.can_stay[r] {
            let m = self.expand(r, self.masks[r]);
            self.set_bits(r, m & !self.masks[r]);
        }
        let m = self.masks[r];
        for s in w.group(r, EXITS) {
            if self.done.contains(s) {
                self.grow(w.site_target(s), m);
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
        if m == 0 || self.done.contains(s) || (self.gate.active && !self.gate.out.contains(s)) {
            return;
        }
        let target = w.site_target(s);
        if !self.eval(s, m) {
            return;
        }
        self.done.insert(s);
        let k = self.next();
        self.fired[s] = k;
        self.log(Step::Fired(u32::try_from(s).unwrap_or(0)));
        match g {
            EXITS => self.grow(target, m),
            EVENTS => {
                let c = target as usize;
                self.counters[c] += 1;
                self.rules.set(&mut self.atoms, COUNTER, c, self.counters[c]);
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
            let blocked = self.gate.active && !self.gate.out.contains(sc + h);
            if self.half_rank[h] == NONE && !blocked && self.now(sc + h, 0) {
                self.half_rank[h] = self.next();
                self.log(Step::Half(u32::try_from(h).unwrap_or(0)));
            }
        }
        let owned = self.owned_at(NONE);
        if owned == self.owned {
            return;
        }
        self.owned = owned;
        self.grow(w.start, owned);
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
                    Some(s) if !self.now(s, m) => {
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
        let m = (0..o.halves)
            .filter(|h| self.now(w.site_count() + h, 0))
            .fold(0, |a, h| a | w.half(h).0);
        if m == 0 { o.none_owned_mask } else { m }
    }
}

#[cfg(test)]
impl Reach<'_> {
    /// The first difference between two states of one world and plan, value for value and
    /// growth for growth in order (not only the fixed point), or `None`.
    pub(crate) fn differs(&self, o: &Self) -> Option<&'static str> {
        [
            (self.counts != o.counts, "counts"),
            (self.counters != o.counters, "counters"),
            (self.groups != o.groups, "groups"),
            (self.atoms != o.atoms, "atoms"),
            (self.masks != o.masks, "masks"),
            (self.reached.words() != o.reached.words(), "reached"),
            (self.holds != o.holds, "holds"),
            (self.away != o.away, "away"),
            (self.seen.words() != o.seen.words(), "seen"),
            (self.done.words() != o.done.words(), "done"),
            (self.fired != o.fired, "fired"),
            (self.half_rank != o.half_rank, "half_rank"),
            (self.loc_rank != o.loc_rank, "loc_rank"),
            (self.copies != o.copies, "copies"),
            (self.lit != o.lit, "lit"),
            (self.owned != o.owned, "owned"),
            (self.seq != o.seq, "seq"),
            (self.grows != o.grows, "grows"),
            (!self.idle() || !o.idle(), "queues"),
        ]
        .into_iter()
        .find_map(|(differs, what)| differs.then_some(what))
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

    /// A journal takes room only for what is journaled: starting one reserves nothing, however
    /// many copies an instance starts with (start items are never journaled), and it grows
    /// with the steps taken.
    #[test]
    fn a_journal_holds_room_only_for_its_steps() {
        let (w, mut inst) = clock_world(&[]);
        inst.logic_start.insert("dawn".to_owned(), 1);
        inst.start_inventory = vec!["dusk".to_owned(); 1];
        let plan = Plan::new(&w, &inst).unwrap();
        let mut r = plan.solve(&w, &[]);
        r.keep_journal();
        assert_eq!(r.journal_room(), 0, "room reserved before a step was taken");
        let mark = r.mark();
        r.add(w.item("dawn").unwrap(), 1);
        r.run();
        assert!((1..=16).contains(&r.journal_room()), "{} steps of room", r.journal_room());
        assert!(r.undo_to(mark) >= 1);
    }

    /// A rank never reaches the value that stands for "none": a state that would hand it out
    /// stops, in every build, instead of wrapping.
    #[test]
    #[should_panic(expected = "the ranks of a solve ran out")]
    fn running_out_of_ranks_stops_the_solve() {
        let (w, inst) = clock_world(&[]);
        let plan = Plan::new(&w, &inst).unwrap();
        let mut r = Reach::new(&w, &plan);
        r.set_seq(NONE);
        r.run();
    }

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
        let r = plan.state(&w, &[key], &[]);
        assert!(r.reached(w.loc("la").unwrap()));
        let solve = r.def_memo.bodies.get();
        assert!(solve <= 41 * r.stats.evals.max(1), "solve: {solve} def bodies");
    }

    /// A replay of the fixed point evaluates every exit, event and check of the reached
    /// regions, finding the ones that are done.
    #[test]
    fn a_replay_is_counted() {
        let (w, inst) = cyclic();
        let plan = Plan::new(&w, &inst).unwrap();
        let item = |k: &str| w.item(k).unwrap();
        let r = plan.state(&w, &[item("key"), item("boot")], &[]);
        // Every region is reached: 5 exits, 1 event and 4 checks, all holding; every rule of
        // this world is a bit rule, so only the plan left as bytecode reads nodes.
        assert_eq!(r.replay(), [10, 10, 0, 0]);
        let plain = plan.clone().interpreted();
        let [evals, held, nodes, bodies] =
            plain.state(&w, &[item("key"), item("boot")], &[]).replay();
        assert_eq!((evals, held, bodies), (10, 10, 0));
        assert!(nodes >= evals, "{nodes} nodes");
        // Without the boot the late check does not hold.
        assert_eq!(plan.state(&w, &[item("key")], &[]).replay()[..2], [10, 9]);
    }
}
