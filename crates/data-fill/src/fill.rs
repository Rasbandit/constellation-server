//! A player's instance resolved against their compiled world ([`Plan`]): what the fill
//! (`crate::room`) places, where it may go, and the state a solve starts from.

use std::collections::HashMap;

use serde::Deserialize;

use crate::bitrules::{CLASSES, NONE};
use crate::fold::{Folded, fold};
use crate::reach::{Reach, eval_static, eval_statics, saturation};
use crate::section::{CHECKS, EXITS};
use crate::world::World;

/// One seed's instance: what generation decided before the fill (see the proposal doc for
/// which of these could become data primitives).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct Instance {
    /// Option values.
    pub options: HashMap<String, i64>,
    /// Rolled values (shop prices).
    pub rolls: HashMap<String, HashMap<String, i64>>,
    /// Locations that exist this seed.
    pub locations: Vec<String>,
    /// Locations that may hold only non-progression items.
    pub excluded: Vec<String>,
    /// Location to the item locked there.
    pub locked: HashMap<String, String>,
    /// The item pool, repeats allowed.
    pub pool: Vec<String>,
    /// Item class (`progression`, `useful`, `filler`, `trap`) by item key.
    pub pool_class: HashMap<String, String>,
    /// Items the player starts with (on the wire).
    pub start_inventory: Vec<String>,
    /// Items logic assumes from the start (not on the wire).
    pub logic_start: HashMap<String, i64>,
    /// Item to placement group; such an item may go only to locations of its group.
    pub item_groups: HashMap<String, String>,
    /// Location to placement group.
    pub location_groups: HashMap<String, String>,
}

impl Instance {
    /// Per world item: its classification bits as a seed records them (1 progression, 2
    /// useful, 4 trap; filler and items outside the pool 0). The one place a pool class
    /// becomes bits.
    #[must_use]
    pub fn flags(&self, w: &World) -> Vec<u8> {
        // A world's items are indexed by `u16` (the compile refuses more).
        (0..=u16::MAX)
            .take(w.items())
            .map(|i| match self.pool_class.get(w.item_key(i)).map(String::as_str) {
                Some("progression") => 1,
                Some("useful") => 2,
                Some("trap") => 4,
                _ => 0,
            })
            .collect()
    }

    /// Per world item: whether its class is `progression` (what counts as held): bit 1 of
    /// [`Instance::flags`].
    #[must_use]
    pub fn progression(&self, w: &World) -> Vec<bool> {
        self.flags(w).into_iter().map(|f| f & 1 != 0).collect()
    }
}

/// An instance resolved to world indices, with the seed's option and roll values.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Option values, by option index (manifest defaults where the instance has none).
    pub(crate) opts: Vec<i32>,
    /// Rolled values, by roll slot.
    pub(crate) rolls: Vec<i16>,
    /// Clocks restrict time this seed (the time model's option-only condition holds).
    pub(crate) clocks: bool,
    /// The rules with this seed's options and rolls folded in.
    pub(crate) folded: Folded,
    /// The atom set a state starts from: the world's option atoms as this seed decides them.
    /// Empty when the plan leaves every rule as bytecode ([`Plan::interpreted`]).
    pub(crate) atoms: Vec<u64>,
    /// The plan's states answer rules by the world's bit rules.
    pub(crate) bits: bool,
    start: Vec<(u16, i64)>,
    /// Locations that exist (index into the world).
    pub included: Vec<u32>,
    pub(crate) excluded: Vec<bool>,
    pub(crate) locked: Vec<(u32, u16)>,
    /// Option-disabled locations with a vanilla item: reaching them grants it.
    self_grants: Vec<(u32, u16)>,
    /// Check sites, ascending, of the locations that do not exist this seed and grant nothing:
    /// nothing is ever placed there, so a fill's states never evaluate them ([`Reach::lean`]).
    pub(crate) absent: Vec<u32>,
    pub(crate) progression: Vec<u16>,
    pub(crate) rest: Vec<u16>,
    /// Per world item: its placement group, if it has one.
    item_group: Vec<Option<u32>>,
    loc_group: Vec<Option<u32>>,
    /// Per item: held count past which no rule reads more copies (see [`Plan::saturated_at`]).
    saturated: Vec<i64>,
    /// Per item: copies that exist this seed (pool, locked, self-granting checks, start).
    copies: Vec<i64>,
    /// Clock slices the start inventory alone owns (all slices when clocks do not restrict).
    /// A state starts from these, not from what an empty inventory owns: a start item that
    /// owns only a later slice must leave the first one out, as the solver does.
    pub(crate) owned_start: u64,
}

/// A map's entries in key order, so the first bad key an error names does not depend on the
/// map's hash order.
fn by_key<V>(m: &HashMap<String, V>) -> Vec<(&String, &V)> {
    let mut v: Vec<_> = m.iter().collect();
    v.sort_unstable_by_key(|x| x.0);
    v
}

/// Why a fill failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillError {
    /// The instance names something the manifest lacks.
    Instance(String),
    /// No reachable, allowed location was left for an item (every retry).
    Stuck {
        /// Item key.
        item: String,
        /// Attempts made.
        attempts: u32,
    },
    /// The world declares shuffles that have not run for this seed: the caller has to stage
    /// it ([`crate::shuffle::stage`]). Not a fault of the instance.
    Unstaged(&'static str),
    /// More items than locations, or the reverse.
    Count {
        /// Items to place.
        items: usize,
        /// Free locations.
        locations: usize,
    },
}

/// Folded rule offsets that are a constant (0 true, 1 false).
fn constant(sites: &[u32]) -> usize {
    sites.iter().filter(|at| **at <= 1).count()
}

impl Plan {
    /// Resolve an instance against a world.
    ///
    /// # Errors
    /// Unknown keys, and start counts below 0 or above the location count.
    pub fn new(w: &World, inst: &Instance) -> Result<Self, FillError> {
        let e = FillError::Instance;
        if w.has_shuffles() {
            // Its exits and its `at` options are not this seed's yet.
            return Err(FillError::Unstaged(crate::shuffle::UNSTAGED));
        }
        let mut opts = Vec::with_capacity(w.options());
        for k in 0..w.options() {
            let key = w.option_key(k);
            opts.push(match inst.options.get(key) {
                Some(v) => i32::try_from(*v)
                    .map_err(|_| e(format!("option {key:?}: {v} does not fit 32 bits")))?,
                None => w.option_default(k),
            });
        }
        let mut rolls = Vec::with_capacity(w.roll_slots());
        for k in 0..w.roll_slots() {
            let (name, loc) = w.roll_slot(k);
            // Mirrors the legacy world's `shop_prices.get(key, 0)`.
            let v = inst.rolls.get(name).and_then(|m| m.get(w.loc_key(loc))).copied().unwrap_or(0);
            rolls.push(
                i16::try_from(v)
                    .map_err(|_| e(format!("roll {name:?}: {v} does not fit 16 bits")))?,
            );
        }
        let clocks = w.time.owned.is_some_and(|o| eval_static(w, &opts, &rolls, o.when));
        let item = |k: &str| w.item(k).map_err(e);
        let loc = |k: &str| w.loc(k).map_err(e);
        let mut start = Vec::new();
        // Solves allocate and loop by held counts, so bound them by what a seed can place.
        let most = i64::try_from(w.locations()).unwrap_or(i64::MAX);
        for (k, n) in by_key(&inst.logic_start) {
            if !(0..=most).contains(n) {
                return Err(e(format!("logic_start {k:?}: {n} is not in 0 to {most} (locations)")));
            }
            start.push((item(k)?, *n));
        }
        for k in &inst.start_inventory {
            start.push((item(k)?, 1));
        }
        start.sort_unstable();
        let included = inst.locations.iter().map(|k| loc(k)).collect::<Result<Vec<_>, _>>()?;
        let mut is_included = vec![false; w.locations()];
        included.iter().for_each(|l| is_included[*l as usize] = true);
        let self_grants = (0..u32::try_from(w.locations()).unwrap_or(0))
            .filter(|l| !is_included[*l as usize])
            .filter_map(|l| w.vanilla(l).map(|v| (l, v)))
            .collect();
        let mut excluded = vec![false; w.locations()];
        for k in &inst.excluded {
            excluded[loc(k)? as usize] = true;
        }
        let mut locked = Vec::new();
        for (l, i) in by_key(&inst.locked) {
            locked.push((loc(l)?, item(i)?));
        }
        locked.sort_unstable();
        let (mut progression, mut rest) = (Vec::new(), Vec::new());
        for k in &inst.pool {
            let i = item(k)?;
            if inst.pool_class.get(k).map(String::as_str) == Some("progression") {
                progression.push(i);
            } else {
                rest.push(i);
            }
        }
        let mut groups: Vec<&String> =
            inst.item_groups.values().chain(inst.location_groups.values()).collect();
        groups.sort();
        groups.dedup();
        let gid = |g: &String| {
            u32::try_from(groups.iter().position(|x| *x == g).unwrap_or(0)).unwrap_or(0)
        };
        let mut item_group = vec![None; w.items()];
        for (k, g) in by_key(&inst.item_groups) {
            item_group[usize::from(item(k)?)] = Some(gid(g));
        }
        let mut loc_group = vec![None; w.locations()];
        for (k, g) in by_key(&inst.location_groups) {
            loc_group[loc(k)? as usize] = Some(gid(g));
        }
        // A confined progression item takes only the locations of its group that are not
        // excluded (`permits`). A group that has room for its copies only with the excluded
        // ones counted can never be filled: say so here, by name. (A group too small either
        // way is left to the fill, which reports it stuck.)
        let mut confined: Vec<u16> =
            progression.iter().copied().filter(|i| item_group[usize::from(*i)].is_some()).collect();
        confined.sort_unstable();
        for kind in confined.chunk_by(|a, b| a == b) {
            let group = item_group[usize::from(kind[0])];
            let open = |l: &&u32| {
                loc_group[**l as usize] == group
                    && locked.binary_search_by_key(*l, |x| x.0).is_err()
            };
            let all = included.iter().filter(open).count();
            let free = included.iter().filter(open).filter(|l| !excluded[**l as usize]).count();
            if free < kind.len() && all >= kind.len() {
                return Err(e(format!(
                    "item {:?}: {} progression copies are confined to a group with {free} \
                     locations that take progression ({} more are excluded)",
                    w.item_key(kind[0]),
                    kind.len(),
                    all - free
                )));
            }
        }
        let mut held = vec![0i64; w.items()];
        start.iter().for_each(|(i, n)| held[usize::from(*i)] += n);
        let folded = fold(w, &opts, &rolls, &held);
        let rules = w.bits();
        let mut atoms = vec![0u64; rules.words()];
        let decided = eval_statics(w, &opts, &rolls, rules.statics.iter().map(|s| s.1));
        for ((bit, _), _) in rules.statics.iter().zip(decided).filter(|x| x.1) {
            crate::bitrules::put(&mut atoms, *bit, true);
        }
        let mut holds = is_included;
        locked.iter().chain(&self_grants).for_each(|(l, _)| holds[*l as usize] = true);
        let mut absent: Vec<u32> = (0..w.locations())
            .filter(|l| !holds[*l])
            .flat_map(|l| w.sites_into(CHECKS, l).iter().copied())
            .collect();
        absent.sort_unstable();
        let mut plan = Plan {
            opts,
            rolls,
            clocks,
            folded,
            atoms,
            bits: true,
            start,
            included,
            excluded,
            locked,
            self_grants,
            absent,
            progression,
            rest,
            item_group,
            loc_group,
            saturated: vec![i64::MAX; w.items()],
            copies: Vec::new(),
            owned_start: w.time.all,
        };
        if plan.clocks {
            // A state reads this while it is built, and the first one is this very probe.
            plan.owned_start = plan.base(w, &[]).compute_owned();
        }
        // Bound the search by every copy that can exist this seed.
        let mut copies = vec![0i64; w.items()];
        let placed = plan.progression.iter().chain(&plan.rest);
        let fixed = plan.locked.iter().chain(&plan.self_grants).map(|x| &x.1);
        let mut add = |i: u16, n: i64| {
            let c = &mut copies[usize::from(i)];
            *c = c
                .checked_add(n)
                .ok_or_else(|| e(format!("item {:?}: too many copies", w.item_key(i))))?;
            Ok::<_, FillError>(())
        };
        for i in placed.chain(fixed) {
            add(*i, 1)?;
        }
        for (i, n) in &plan.start {
            add(*i, *n)?;
        }
        let mut kinds = plan.progression.clone();
        kinds.sort_unstable();
        kinds.dedup();
        for i in kinds {
            plan.saturated[i as usize] = saturation(w, &plan, i, copies[i as usize]);
        }
        plan.copies = copies;
        Ok(plan)
    }

    /// The same plan with every rule left as bytecode: what the bit rules are tested and
    /// measured against. Every solve of it gives what a solve of `self` gives.
    #[must_use]
    pub fn interpreted(mut self) -> Self {
        self.bits = false;
        self.atoms = Vec::new();
        self
    }

    /// The bit rules this plan's states read: the world's, or none.
    pub(crate) fn rules<'w>(&self, w: &'w World) -> &'w crate::bitrules::Bits {
        if self.bits { w.bits() } else { &NONE }
    }

    /// Rule sites per class of [`crate::RULE_CLASSES`] (counted), and the bytes the world's bit
    /// rules take (counted from their lists' lengths; held once per loaded world).
    #[must_use]
    pub fn bit_rules(&self, w: &World) -> ([u32; 5], usize) {
        (self.rules(w).classes, self.rules(w).bytes())
    }

    /// The same plan whose fills also evaluate the checks of absent locations: what skipping
    /// them is tested and measured against.
    #[must_use]
    pub fn with_absent_checks(mut self) -> Self {
        self.absent.clear();
        self
    }

    /// What this seed's fold leaves of the world's rules, all counted: rule sites per class and
    /// the bit rules' bytes ([`Plan::bit_rules`]), the atom sets a player holds, the sites whose
    /// rule is constant with the options, rolls and start items, and the absent locations'
    /// check sites a fill skips. Reads what the plan already holds: no rule is folded again.
    #[must_use]
    pub fn rules_summary(&self, w: &World) -> serde_json::Value {
        let (classes, bytes) = self.bit_rules(w);
        serde_json::json!({
            "rule_sites": self.folded.sites.len(),
            "classes": CLASSES.iter().zip(classes).collect::<HashMap<_, _>>(),
            "bit_rule_bytes_per_world": bytes,
            // Two sets of 8-byte words: the plan's (the option atoms) and one state's.
            "atom_set_bytes_per_player": self.atoms.len() * 2 * size_of::<u64>(),
            "constant_sites_with_start_items": constant(&self.folded.sites),
            "absent_check_sites": self.absent.len(),
        })
    }

    /// For measurements only (it folds every rule again and walks the regions):
    /// [`Plan::rules_summary`], and what the shrinking steps that were not built could remove,
    /// all counted. Sites constant with the
    /// options and rolls alone; regions no exit that can open leads to, and their sites; exits
    /// that are always open, and the regions whose only entrance that can open is one of those
    /// (what merging chains of open exits could remove).
    #[must_use]
    pub fn rules_report(&self, w: &World) -> serde_json::Value {
        let mut all = self.rules_summary(w);
        if let (Some(all), serde_json::Value::Object(more)) =
            (all.as_object_mut(), self.shrink_report(w))
        {
            all.extend(more);
        }
        all
    }

    /// The part of [`Plan::rules_report`] that folds and walks.
    fn shrink_report(&self, w: &World) -> serde_json::Value {
        let sites = &self.folded.sites;
        let options_only = fold(w, &self.opts, &self.rolls, &[]);
        let live = |r: usize| w.group(r, EXITS).filter(|e| sites[*e] != 1);
        let mut seen = vec![false; w.regions()];
        let mut queue = vec![w.start as usize];
        seen[w.start as usize] = true;
        // Per region: entrances that can open, from regions that can be reached, and how many
        // of them are always open.
        let mut ways = vec![(0u32, 0u32); w.regions()];
        while let Some(r) = queue.pop() {
            for e in live(r) {
                let to = w.site_target(e) as usize;
                ways[to].0 += 1;
                ways[to].1 += u32::from(sites[e] == 0);
                if !std::mem::replace(&mut seen[to], true) {
                    queue.push(to);
                }
            }
        }
        let dead = (0..w.regions()).filter(|r| !seen[*r]);
        serde_json::json!({
            "constant_sites_by_options": constant(&options_only.sites),
            "regions": w.regions(),
            "unreachable_regions": dead.clone().count(),
            "sites_in_unreachable_regions": dead
                .map(|r| w.group(r, EXITS).start..w.stays(r).end)
                .map(|x| x.len())
                .sum::<usize>(),
            "always_open_exits": (0..w.regions()).filter(|r| seen[*r]).flat_map(live)
                .filter(|e| sites[*e] == 0).count(),
            "regions_entered_only_by_one_open_exit": (0..w.regions())
                .filter(|r| *r != w.start as usize && ways[*r] == (1, 1)).count(),
        })
    }

    /// The same plan with the start items' `has` tests left in its rules (folded for the
    /// options and rolls alone): what folding them is tested against.
    #[cfg(test)]
    pub(crate) fn without_start_fold(mut self, w: &World) -> Self {
        self.folded = fold(w, &self.opts, &self.rolls, &[]);
        self
    }

    /// Start copies of `item` (the start inventory's and the logic start's).
    pub(crate) fn start_copies(&self, item: u16) -> i64 {
        self.start.iter().filter(|(i, _)| *i == item).map(|(_, n)| *n).sum()
    }

    /// Copies of `item` that exist this seed.
    pub(crate) fn copies(&self, item: usize) -> usize {
        self.copies.get(item).map_or(0, |n| usize::try_from(*n).unwrap_or(0))
    }

    /// The held count of a progression item from which no rule reads more copies: 0 when no
    /// rule reads it, `i64::MAX` when none (and for items that are not progression).
    #[must_use]
    pub fn saturated_at(&self, item: u16) -> i64 {
        self.saturated.get(item as usize).copied().unwrap_or(i64::MAX)
    }

    /// A state holding the start items plus `extra`, where reaching a location collects what
    /// `placed` says it holds (and option-disabled checks grant their vanilla item).
    #[must_use]
    pub fn state<'w>(&'w self, w: &'w World, extra: &[u16], placed: &[Option<u16>]) -> Reach<'w> {
        let mut r = Reach::new(w, self);
        self.solve_into(&mut r, extra, placed);
        r
    }

    /// [`Plan::state`] into an existing state, reusing its buffers.
    pub fn solve_into(&self, r: &mut Reach<'_>, extra: &[u16], placed: &[Option<u16>]) {
        r.reset();
        self.prime(r, extra);
        r.hold_all(placed);
        r.run();
    }

    pub(crate) fn prime(&self, r: &mut Reach<'_>, extra: &[u16]) {
        for (i, n) in &self.start {
            r.add(*i, *n);
        }
        for i in extra {
            r.add(*i, 1);
        }
        for (l, i) in &self.self_grants {
            r.hold(*l, *i);
        }
    }

    /// Start items plus `extra`, option-disabled checks self-granting, nothing placed, not yet
    /// run: the starting point for sphere-by-sphere collection.
    #[must_use]
    pub fn base<'w>(&'w self, w: &'w World, extra: &[u16]) -> Reach<'w> {
        let mut r = Reach::new(w, self);
        self.prime(&mut r, extra);
        r
    }

    /// [`Plan::base`] into an existing state, reusing its buffers.
    pub(crate) fn rebase(&self, r: &mut Reach<'_>, extra: &[u16]) {
        r.reset();
        self.prime(r, extra);
    }

    /// The solver's view only: start items plus `extra`, option-disabled checks self-granting,
    /// no placed items. This is what the oracle comparison runs.
    #[must_use]
    pub fn solve<'w>(&'w self, w: &'w World, extra: &[u16]) -> Reach<'w> {
        self.state(w, extra, &[])
    }

    /// Whether the placement rules let `item` sit at `loc` (its confinement group, and no
    /// progression on an excluded location). Logic is not consulted.
    #[must_use]
    pub fn permits(&self, item: u16, loc: u32, progression: bool) -> bool {
        self.allowed(item, loc) && !(progression && self.excluded[loc as usize])
    }

    /// Per world location: the locked item, `None` elsewhere.
    #[must_use]
    pub fn locked_placed(&self, w: &World) -> Vec<Option<u16>> {
        let mut v = vec![None; w.locations()];
        for (l, i) in &self.locked {
            v[*l as usize] = Some(*i);
        }
        v
    }

    pub(crate) fn allowed(&self, item: u16, loc: u32) -> bool {
        self.group(item).is_none_or(|g| self.loc_group[loc as usize] == Some(g))
    }

    /// Whether `item` has a placement group (it may go only to locations of that group).
    pub(crate) fn confined(&self, item: u16) -> bool {
        self.group(item).is_some()
    }

    fn group(&self, item: u16) -> Option<u32> {
        self.item_group.get(usize::from(item)).copied().flatten()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use spec_rng::SpecRng;

    /// Duplicated items read with thresholds (`has n`, a floor division, a capped sum), one
    /// item no rule reads, one item a rule can never be satisfied by alone, a region whose
    /// way in can be held up by an item found inside it (support that is not a derivation),
    /// events of two regions lighting one counter that a check and an exit read, and a stay
    /// rule that gates a later slice.
    fn world() -> (World, Instance) {
        world_with(false)
    }

    /// With `clocks`, a later slice of the day is owned only with `charm`, and a region opens in
    /// that slice.
    pub(crate) fn world_with(clocks: bool) -> (World, Instance) {
        let checks = |ls: &[(&str, serde_json::Value)]| -> Vec<serde_json::Value> {
            ls.iter().map(|(l, r)| json!({"loc": l, "rule": r})).collect()
        };
        let t = json!({"true": {}});
        let locs: Vec<String> = (1..=20).map(|i| format!("l{i}")).collect();
        let mut m = json!({
            "options": [],
            "items": [{"key": "key", "id": 1}, {"key": "coin", "id": 2}, {"key": "gem", "id": 3},
                      {"key": "boot", "id": 4}, {"key": "junk", "id": 5},
                      {"key": "rune", "id": 6}, {"key": "charm", "id": 7}],
            "locations": locs.iter().enumerate()
                .map(|(i, k)| json!({"key": k, "id": i + 1})).collect::<Vec<_>>(),
            "start": "a",
            "x.time": {"slices": 2, "start_mask": 1},
            "defs": {"rich": {"ge": [{"int": ["div", {"count": "coin"}, 2]}, 1]}},
            "regions": [
                {"key": "a", "can_stay": false,
                 "checks": checks(&[("l1", t.clone()), ("l2", t.clone()), ("l3", t.clone()),
                                    ("l4", json!({"ref": "rich"}))]),
                 "exits": [{"to": "b", "rule": {"has": "key"}},
                           {"to": "d", "rule": {"any": [{"has": "rune", "n": 2},
                                                        {"has": "charm"}]}}],
                 "events": [], "stays": []},
                {"key": "b", "can_stay": false,
                 "checks": checks(&[("l5", t.clone()), ("l6", t.clone()), ("l7", t.clone()),
                                    ("l8", t.clone()), ("l9", json!({"has": "key", "n": 2})),
                                    ("l10", json!({"ge": [{"int": ["+",
                                        {"int": ["min", {"count": "coin"}, 3]},
                                        {"count": "boot"}]}, 3]}))]),
                 "exits": [{"to": "c", "rule": {"has": "boot"}}],
                 "events": [{"event": "bell", "rule": {"has": "key", "n": 2}}], "stays": []},
                {"key": "c", "can_stay": true,
                 "checks": checks(&[("l11", t.clone()), ("l12", t.clone()), ("l13", t.clone()),
                                    ("l14", t.clone()), ("l15", json!({"x.time_any": 2})),
                                    ("l16", json!({"ge": [{"counter": "bell"}, 2]}))]),
                 "exits": [], "events": [], "stays": [{"slice": 1, "rule": {"has": "rune"}}]},
                {"key": "d", "can_stay": false,
                 "checks": checks(&[("l17", json!({"true": {}})), ("l18", json!({"true": {}})),
                                    ("l19", json!({"true": {}}))]),
                 "exits": [{"to": "e", "rule": {"ge": [{"counter": "bell"}, 2]}}],
                 "events": [{"event": "bell", "rule": {"has": "charm"}}], "stays": []},
                {"key": "e", "can_stay": false, "checks": checks(&[("l20", json!({"true": {}}))]),
                 "exits": [], "events": [], "stays": []}
            ],
            "goal": {"reach": "region:c"}
        });
        let mut pool = vec!["key"; 4];
        pool.extend(["coin"; 6]);
        pool.extend(["gem"; 3]);
        pool.extend(["boot", "junk", "junk", "junk", "rune", "rune", "charm"]);
        if clocks {
            m["x.time"] = json!({"slices": 2, "start_mask": 1, "owned": {
                "when": {"true": {}}, "none_owned_mask": 1,
                "half_days": [{"first": 0, "last": 0, "rule": {"true": {}}},
                              {"first": 1, "last": 1, "rule": {"has": "charm"}}]}});
            m["regions"][2]["can_stay"] = json!(true);
            m["regions"][2]["exits"] = json!([{"to": "e", "rule": {"x.time_any": 2}}]);
        }
        let class = |k: &str| if k == "junk" { "filler" } else { "progression" };
        let inst = Instance {
            locations: locs,
            pool: pool.iter().map(|s| (*s).to_owned()).collect(),
            pool_class: pool.iter().map(|k| ((*k).to_owned(), class(k).to_owned())).collect(),
            ..Instance::default()
        };
        (World::load(&m).unwrap(), inst)
    }

    #[test]
    fn saturation_is_the_count_past_which_no_rule_reads_an_item() {
        let (w, inst) = world();
        let plan = Plan::new(&w, &inst).unwrap();
        let at = |k: &str| plan.saturated_at(w.item(k).unwrap());
        assert_eq!(at("key"), 2, "an exit needs 1, a check needs 2");
        assert_eq!(at("coin"), 3, "a def needs 2, a capped sum needs 3");
        assert_eq!(at("gem"), 0, "no rule reads it");
        assert_eq!(at("boot"), i64::MAX, "a sum it can never satisfy alone keeps it read");
        assert_eq!(at("rune"), 2, "an alternative of an `any` saturates on its own");
    }

    #[test]
    fn removing_held_items_one_by_one_matches_a_fresh_solve() {
        for clocks in [false, true] {
            let (w, inst) = world_with(clocks);
            let plan = Plan::new(&w, &inst).unwrap();
            let (mut lit, mut waited) = (false, false);
            let late = w.loc("l15").unwrap();
            for seed in 1u64..=100 {
                let mut rng = SpecRng::from_u64(seed);
                let mut items = plan.progression.clone();
                rng.shuffle(&mut items);
                let mut locs = plan.included.clone();
                rng.shuffle(&mut locs);
                // Some items placed, the rest held from the start and taken back one by one.
                let cut = items.len() / 3;
                let mut placed = vec![None; w.locations()];
                for (l, i) in locs.iter().zip(&items[..cut]) {
                    placed[*l as usize] = Some(*i);
                }
                let mut held = items[cut..].to_vec();
                let mut r = plan.state(&w, &held, &placed);
                lit |= r.counters.iter().any(|c| *c >= 2);
                waited |= r.reached(late);
                while let Some(i) = held.pop() {
                    r.remove(i);
                    let fresh = plan.state(&w, &held, &placed);
                    let what = format!("seed {seed}, clocks {clocks}, {} left", held.len());
                    crate::reach::assert_same(&w, &r, &fresh, &what);
                }
            }
            assert!(lit && waited, "the counter and the stay rule were exercised");
        }
    }
}
