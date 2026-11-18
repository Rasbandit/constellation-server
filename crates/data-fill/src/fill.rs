//! Assumed fill over a compiled world, in the shape of the legacy generator's restrictive fill:
//! pop an item, assume every item not yet placed is held, sweep what placed items become
//! reachable, and put the item in the first shuffled location that is reachable and allows it.

use std::collections::HashMap;

use serde::Deserialize;
use spec_rng::SpecRng;

use crate::fold::{Folded, fold};
use crate::reach::{Reach, Stats, eval_static, saturation};
use crate::spheres::{Spheres, from_state};
use crate::world::World;

/// One seed's instance: what generation decided before the fill (see the proposal doc for
/// which of these could become data primitives).
#[derive(Debug, Clone, Default, Deserialize)]
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
    /// Per world item: whether its class is `progression` (what counts as held).
    #[must_use]
    pub fn progression(&self, w: &World) -> Vec<bool> {
        (0..u16::try_from(w.items()).unwrap_or(0))
            .map(|i| self.pool_class.get(w.item_key(i)).is_some_and(|c| c == "progression"))
            .collect()
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
    start: Vec<(u16, i64)>,
    /// Locations that exist (index into the world).
    pub included: Vec<u32>,
    excluded: Vec<bool>,
    locked: Vec<(u32, u16)>,
    /// Option-disabled locations with a vanilla item: reaching them grants it.
    self_grants: Vec<(u32, u16)>,
    progression: Vec<u16>,
    rest: Vec<u16>,
    item_group: HashMap<u16, u32>,
    loc_group: Vec<Option<u32>>,
    /// Per item: held count past which no rule reads more copies (see [`Plan::saturated_at`]).
    saturated: Vec<i64>,
    /// Per item: copies that exist this seed (pool, locked, self-granting checks, start).
    copies: Vec<i64>,
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
    /// More items than locations, or the reverse.
    Count {
        /// Items to place.
        items: usize,
        /// Free locations.
        locations: usize,
    },
}

impl Plan {
    /// Resolve an instance against a world.
    ///
    /// # Errors
    /// Unknown keys, and start counts below 0 or above the location count.
    pub fn new(w: &World, inst: &Instance) -> Result<Self, FillError> {
        let e = FillError::Instance;
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
        let mut item_group = HashMap::new();
        for (k, g) in by_key(&inst.item_groups) {
            item_group.insert(item(k)?, gid(g));
        }
        let mut loc_group = vec![None; w.locations()];
        for (k, g) in by_key(&inst.location_groups) {
            loc_group[loc(k)? as usize] = Some(gid(g));
        }
        let folded = fold(w, &opts, &rolls);
        let mut plan = Plan {
            opts,
            rolls,
            clocks,
            folded,
            start,
            included,
            excluded,
            locked,
            self_grants,
            progression,
            rest,
            item_group,
            loc_group,
            saturated: vec![i64::MAX; w.items()],
            copies: Vec::new(),
        };
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

    fn prime(&self, r: &mut Reach<'_>, extra: &[u16]) {
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

    fn allowed(&self, item: u16, loc: u32) -> bool {
        self.item_group.get(&item).is_none_or(|g| self.loc_group[loc as usize] == Some(*g))
    }
}

/// Result of a fill.
#[derive(Debug, Clone)]
pub struct Filled {
    /// Item per world location index (`None` for locations that do not exist this seed).
    pub placed: Vec<Option<u16>>,
    /// Full fills started (1 means no retry).
    pub attempts: u32,
    /// Reachability solves run from scratch.
    pub solves: u32,
    /// Steps whose removal retracted something (and derived it again where still supported).
    pub rederived: u32,
    /// Work counters across all solves.
    pub stats: Stats,
    /// The goal holds with every placed item collected (what [`validate`] answers, read off
    /// the fill's own state).
    pub beatable: bool,
    /// Existing locations reached with every placed item collected.
    pub reached: usize,
}

/// Fill one seed. Retries the whole fill with the next draws from `rng` when stuck.
///
/// # Errors
/// [`FillError`] when every attempt is stuck or the counts do not match.
pub fn fill(
    w: &World,
    plan: &Plan,
    rng: &mut SpecRng,
    max_attempts: u32,
) -> Result<Filled, FillError> {
    fill_with(w, plan, rng, max_attempts, true, None).map(|x| x.0)
}

/// [`fill`], and the seed's spheres (see [`crate::spheres::spheres`]) from the fill's own final
/// state. `progression[item]` says whether an item counts as held.
///
/// # Errors
/// As [`fill`].
pub fn fill_spheres(
    w: &World,
    plan: &Plan,
    rng: &mut SpecRng,
    max_attempts: u32,
    progression: &[bool],
) -> Result<(Filled, Spheres), FillError> {
    let (f, s) = fill_with(w, plan, rng, max_attempts, true, Some(progression))?;
    Ok((f, s.unwrap_or_else(|| unreachable!("asked for spheres"))))
}

/// [`fill`], with `extend` off re-solving from scratch at every step (the reference the
/// extension is tested against), and the spheres when `progression` is given.
fn fill_with(
    w: &World,
    plan: &Plan,
    rng: &mut SpecRng,
    max_attempts: u32,
    extend: bool,
    progression: Option<&[bool]>,
) -> Result<(Filled, Option<Spheres>), FillError> {
    let mut last = None;
    // One solve state and one held list for the whole fill: each step resets them.
    let mut s = Steps {
        w,
        plan,
        r: Reach::new(w, plan),
        held: Vec::new(),
        solves: 0,
        rederived: 0,
        stats: Stats::default(),
        extend,
    };
    for attempt in 1..=max_attempts {
        match s.try_fill(rng) {
            Ok(placed) => {
                let reached = plan.included.iter().filter(|l| s.r.reached(**l)).count();
                let beatable = s.r.goal();
                let sp = progression.map(|p| from_state(w, plan, &mut s.r, &placed, p));
                let filled = Filled {
                    attempts: attempt,
                    solves: s.solves,
                    rederived: s.rederived,
                    stats: s.stats,
                    beatable,
                    reached,
                    placed,
                };
                return Ok((filled, sp));
            }
            Err(FillError::Stuck { item, .. }) => last = Some(item),
            Err(e) => return Err(e),
        }
    }
    Err(FillError::Stuck { item: last.unwrap_or_default(), attempts: max_attempts })
}

struct Steps<'w> {
    w: &'w World,
    plan: &'w Plan,
    r: Reach<'w>,
    held: Vec<u16>,
    solves: u32,
    rederived: u32,
    stats: Stats,
    extend: bool,
}

impl Steps<'_> {
    fn try_fill(&mut self, rng: &mut SpecRng) -> Result<Vec<Option<u16>>, FillError> {
        let plan = self.plan;
        let mut placed: Vec<Option<u16>> = vec![None; self.w.locations()];
        for (l, i) in &plan.locked {
            placed[*l as usize] = Some(*i);
        }
        let free: Vec<u32> =
            plan.included.iter().copied().filter(|l| placed[*l as usize].is_none()).collect();
        let items = plan.progression.len() + plan.rest.len();
        if items != free.len() {
            return Err(FillError::Count { items, locations: free.len() });
        }
        // Confined items first, as the world's pre_fill does, each into its own group.
        let (mut confined, mut prog): (Vec<u16>, Vec<u16>) =
            plan.progression.iter().partition(|i| plan.item_group.contains_key(i));
        rng.shuffle(&mut confined);
        rng.shuffle(&mut prog);
        let mut locs = free.clone();
        rng.shuffle(&mut locs);
        // The state carries over from one phase to the next: one full solve per fill.
        let mut prev = None;
        self.restrictive(&mut confined, &prog, &mut locs, &mut placed, &mut prev)?;
        locs.retain(|l| !plan.excluded[*l as usize]);
        self.restrictive(&mut prog, &[], &mut locs, &mut placed, &mut prev)?;
        // Everything else goes anywhere left, excluded locations included.
        let mut rest = plan.rest.clone();
        rng.shuffle(&mut rest);
        let mut left: Vec<u32> =
            free.into_iter().filter(|l| placed[*l as usize].is_none()).collect();
        rng.shuffle(&mut left);
        for (l, i) in left.into_iter().zip(rest) {
            placed[l as usize] = Some(i);
        }
        // The finished seed, for the beatability check: collect the last placed item and the
        // rest, which only adds to the fixed point.
        match prev {
            Some(_) if self.extend => {
                self.r.stats = Stats::default();
                self.r.hold_all(&placed);
                self.r.run();
                self.stats.absorb(&self.r.stats);
            }
            _ => {
                self.plan.solve_into(&mut self.r, &[], &placed);
                self.solves += 1;
            }
        }
        Ok(placed)
    }

    fn restrictive(
        &mut self,
        items: &mut Vec<u16>,
        also_held: &[u16],
        locs: &mut Vec<u32>,
        placed: &mut [Option<u16>],
        prev: &mut Option<(u32, u16)>,
    ) -> Result<(), FillError> {
        // `prev`: the location the previous item went to, while `r` is a fixed point that only
        // lacks it.
        while let Some(item) = items.pop() {
            // Placing the previous item at a reached location only adds, so carry on from the
            // last fixed point; then take this item back with truth maintenance.
            let carried = match *prev {
                Some((l, i)) if self.extend => {
                    self.r.stats = Stats::default();
                    self.r.hold(l, i);
                    self.r.run();
                    self.r.remove(item);
                    self.rederived += u32::from(self.r.stats.retractions > 0);
                    self.stats.absorb(&self.r.stats);
                    true
                }
                _ => false,
            };
            if !carried {
                self.held.clear();
                self.held.extend_from_slice(items);
                self.held.extend_from_slice(also_held);
                self.plan.solve_into(&mut self.r, &self.held, placed);
                self.solves += 1;
                self.stats.absorb(&self.r.stats);
            }
            let r = &self.r;
            let pos = locs.iter().position(|l| r.reached(*l) && self.plan.allowed(item, *l));
            let Some(pos) = pos else {
                return Err(FillError::Stuck {
                    item: self.w.item_key(item).to_owned(),
                    attempts: 1,
                });
            };
            let l = locs.remove(pos);
            placed[l as usize] = Some(item);
            *prev = Some((l, item));
        }
        Ok(())
    }
}

/// Play the seed from the start items: is the goal reached, and how many existing locations?
#[must_use]
pub fn validate(w: &World, plan: &Plan, placed: &[Option<u16>]) -> (bool, usize) {
    let s = plan.state(w, &[], placed);
    let reached = plan.included.iter().filter(|l| s.reached(**l)).count();
    (s.goal(), reached)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn world_with(clocks: bool) -> (World, Instance) {
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
                let mut sb = [0u8; 32];
                sb[..8].copy_from_slice(&seed.to_le_bytes());
                let mut rng = SpecRng::from_seed(sb);
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

    #[test]
    fn carrying_the_fixed_point_keeps_every_placement() {
        for clocks in [false, true] {
            carry(clocks);
        }
    }

    fn carry(clocks: bool) {
        let (w, inst) = world_with(clocks);
        let plan = Plan::new(&w, &inst).unwrap();
        let (mut with, mut without, mut rederived) = (0, 0, 0);
        for seed in 1u64..=200 {
            let mut sb = [0u8; 32];
            sb[..8].copy_from_slice(&seed.to_le_bytes());
            let progression = inst.progression(&w);
            let (a, sa) = fill_with(
                &w,
                &plan,
                &mut SpecRng::from_seed(sb),
                config::FILL_MAX_ATTEMPTS,
                true,
                Some(&progression),
            )
            .unwrap();
            let (b, _) = fill_with(
                &w,
                &plan,
                &mut SpecRng::from_seed(sb),
                config::FILL_MAX_ATTEMPTS,
                false,
                None,
            )
            .unwrap();
            assert_eq!(a.placed, b.placed, "seed {seed}, clocks {clocks}");
            // The report read off the fill's state is the fresh sweep's.
            assert_eq!((a.beatable, a.reached), validate(&w, &plan, &a.placed), "seed {seed}");
            let fresh = crate::spheres::spheres(&w, &plan, &a.placed, &progression);
            assert_eq!(sa, Some(fresh), "seed {seed}, clocks {clocks}");
            assert_eq!(a.attempts, b.attempts, "seed {seed}");
            with += a.solves;
            rederived += a.rederived;
            without += b.solves;
        }
        assert!(with < without, "reuse saved no solve: {with} vs {without}");
        assert!(rederived > 0, "no step kept a prefix of the derivation");
    }
}
