//! Synthetic worlds for scaling measurements (`examples/synth_room.rs`): per player, a manifest
//! and an instance drawn from a seed, compiled through the real path ([`crate::section::compile`]
//! and [`crate::World::from_package`], via [`Compiled::new`]).
//!
//! The start region opens onto `layers` layers of regions; every region of a layer has
//! `branching` gated exits in from regions of the layer before. Gates mix `has` (with and
//! without counts), `any`, `all` with a def, and counters lit by events in the first layer.
//! Every gate can be met with every item held, so the goal (reach every region of the last
//! layer) holds with the whole pool.
//!
//! Two profiles: [`Shape::default`] (light: few sites, two time slices, short rules) and
//! [`Shape::heavy`], whose counts per check follow the experiment's test world as counted in
//! `docs/data-only-world-experiment.md` (1516 checks, 318 regions, 3685 rule sites, 107 defs,
//! 45 time slices, clocks as items).
//!
//! A third, [`Shape::wide`], is another kind of world: one region that holds every check, each
//! behind a deep OR-of-AND tree over counts of items and sums of counts, so every rule reads
//! many items and every item is read by many rules.
//!
//! And worlds built to be hard for a fill ([`Hostile`]): a chain of regions each opened by one
//! item, one item in front of nine tenths of the world, half the progression as such keys, and
//! a world with as much progression as locations that take it.

use serde_json::{Value, json};
use spec_rng::SpecRng;

use crate::fill::Instance;
use crate::room::Compiled;

/// The shape of every world of a synthetic room.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    /// Locations per world (all exist, none locked).
    pub checks: usize,
    /// Gate depth: layers of regions after the start region.
    pub layers: usize,
    /// Exits into each region from the layer before.
    pub branching: usize,
    /// Share of the pool that is progression.
    pub progression: f64,
    /// Clocks restrict time: only the first half-day is owned from the start, each later one
    /// with a progression item of its own.
    pub clocks: bool,
    /// Regions per check.
    pub regions: f64,
    /// Time slices of the day (2 to 64), split into `slices / 8` half-days (at least two).
    pub slices: usize,
    /// Events that light each counter.
    pub events: usize,
    /// Named defs per kind of progression item.
    pub defs: f64,
    /// Locations that do not exist this seed, per check: their rules are still evaluated.
    pub absent: f64,
    /// Longer rules: three checks in eight are gated like exits, half the gates carry a second
    /// condition, defs read other defs and time, and time leaves test a window of slices.
    pub rich: bool,
    /// One region only ([`Shape::wide`]); the fields about regions, time and defs are not read.
    pub wide: bool,
    /// A world built to be hard for a fill; `None`: the layered world or the wide one.
    pub hostile: Option<Hostile>,
}

/// Worlds where taking one item out of what is held changes much of what is reached, or where
/// a round has few locations to choose from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hostile {
    /// Every progression item is a key: region `k + 1` is entered from region `k` with item
    /// `k` (one copy), and the checks are spread evenly over the regions.
    Chain,
    /// One key: a tenth of the checks are in the start region and the rest behind one item.
    /// The other progression items (four copies a kind) gate single checks.
    Gated,
    /// Half the progression copies are keys of a chain, the other half gate single checks.
    Keys,
    /// All but one: the progression is `n` copies of one item and seven tenths of the checks
    /// are behind a gate that needs `n - 1` of them. Those checks take no progression, so the
    /// copies stay in front of the gate: a state that lacks one copy reaches the whole world,
    /// a state that lacks two reaches only the front.
    Brink,
    /// The layered world with half its pool progression and every other location excluded:
    /// as much progression as locations that take it, so a round has few to choose from and
    /// an attempt is often stuck.
    Tight,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            checks: 1000,
            layers: 6,
            branching: 2,
            progression: 0.19,
            clocks: false,
            regions: 0.125,
            slices: 2,
            events: 2,
            defs: 0.125,
            absent: 0.0,
            rich: false,
            wide: false,
            hostile: None,
        }
    }
}

impl Shape {
    /// The heavy profile: per check, the test world's counted regions (0.21), rule sites (2.4),
    /// defs and events; its 45 time slices; clocks on.
    #[must_use]
    pub fn heavy() -> Self {
        Shape {
            clocks: true,
            regions: 0.21,
            slices: 45,
            events: 6,
            defs: 1.5,
            absent: 0.85,
            rich: true,
            ..Shape::default()
        }
    }

    /// The wide profile: 131 checks in one region, three in five items progression, three
    /// checks in four behind a tree of `any` over `all` over `any` (now and then one level
    /// more) whose leaves are `has` with and without a count and sums of three counts.
    #[must_use]
    pub fn wide() -> Self {
        Shape { checks: 131, progression: 0.6, wide: true, ..Shape::default() }
    }
}

impl Shape {
    /// A light world of 1,000 checks made [`Hostile`] in the way `how` says.
    #[must_use]
    pub fn hostile(how: Hostile) -> Self {
        let progression = match how {
            Hostile::Tight => 0.5,
            // 100 keys: a longer chain leaves a round too few locations in a room of ten.
            Hostile::Chain => 0.1,
            _ => Shape::default().progression,
        };
        Shape { hostile: Some(how), progression, ..Shape::default() }
    }
}

/// The key worlds of [`Hostile`]: `links` keys, one copy each, key `k` on the only exit from
/// region `k` to region `k + 1`; the goal is the last region.
fn keyed(shape: &Shape, how: Hostile, pick: &mut dyn FnMut(usize) -> usize) -> (Value, Instance) {
    let checks = shape.checks.max(8);
    // At most 200 copies: a `has` counts to 255.
    let copies_total =
        ((checks as f64 * shape.progression).round() as usize).clamp(2, (checks / 4).min(200));
    let brink = how == Hostile::Brink;
    let links = match how {
        Hostile::Gated | Hostile::Brink => 1,
        Hostile::Keys => copies_total / 2,
        _ => copies_total,
    };
    // What is not a key: kinds of four copies, each read by single checks.
    let loose = if brink { 0 } else { (copies_total - links) / 4 };
    // The checks in front of the one gate: a tenth, or (brink) three tenths.
    let front = if brink { checks * 3 / 10 } else { checks / 10 };
    let (key, p) = (|k: usize| format!("k{k}"), |k: usize| format!("p{k}"));
    let regions = links + 1;
    // The region of a check: a tenth in front of the one key, or spread evenly.
    let region = |l: usize| match how {
        Hostile::Gated | Hostile::Brink => usize::from(l >= front),
        _ => l * regions / checks,
    };
    let mut region_checks: Vec<Vec<Value>> = vec![Vec::new(); regions];
    for l in 0..checks {
        let rule = if loose > 0 && pick(4) == 0 {
            json!({"has": p(pick(loose))})
        } else {
            json!({"true": {}})
        };
        region_checks[region(l)].push(json!({"loc": format!("l{l}"), "rule": rule}));
    }
    let regions_json: Vec<Value> = (0..regions)
        .map(|r| {
            let exits = if r + 1 < regions {
                let n = if brink { copies_total - 1 } else { 1 };
                json!([{"to": format!("r{}", r + 1), "rule": {"has": key(r), "n": n}}])
            } else {
                json!([])
            };
            json!({"key": format!("r{r}"), "can_stay": false, "checks": region_checks[r],
                   "exits": exits, "events": [], "stays": []})
        })
        .collect();
    let kinds = (0..links).map(key).chain((0..loose).map(p));
    let mut items: Vec<Value> =
        kinds.clone().enumerate().map(|(i, k)| json!({"key": k, "id": i + 1})).collect();
    items.push(json!({"key": "filler", "id": links + loose + 1}));
    let manifest = json!({
        "options": [],
        "items": items,
        "locations": (0..checks).map(|l| json!({"key": format!("l{l}"), "id": l + 1}))
            .collect::<Vec<_>>(),
        "start": "r0",
        "x.time": {"slices": 1, "start_mask": 1},
        "defs": {},
        "regions": regions_json,
        "goal": {"reach": format!("region:r{links}")},
    });
    let keys = if brink { copies_total } else { 1 };
    let mut pool: Vec<String> = (0..links)
        .flat_map(|k| vec![key(k); keys])
        .chain((0..loose).flat_map(|k| vec![p(k); 4]))
        .collect();
    pool.resize(checks, "filler".to_owned());
    let mut pool_class: std::collections::HashMap<String, String> =
        kinds.map(|k| (k, "progression".to_owned())).collect();
    pool_class.insert("filler".to_owned(), "filler".to_owned());
    let mut inst = Instance {
        locations: (0..checks).map(|l| format!("l{l}")).collect(),
        pool,
        pool_class,
        ..Instance::default()
    };
    if brink {
        inst.excluded = (front..checks).map(|l| format!("l{l}")).collect();
    }
    (manifest, inst)
}

/// The one-region world of [`Shape::wide`].
fn wide(shape: &Shape, pick: &mut dyn FnMut(usize) -> usize) -> (Value, Instance) {
    let checks = shape.checks.max(8);
    let copies_total = ((checks as f64 * shape.progression).round() as usize).clamp(3, checks);
    let (kinds, copies) = (copies_total / 3, 3);
    let p = |k: usize| format!("p{k}");
    fn tree(
        depth: usize,
        any: bool,
        leaf: &dyn Fn(&mut dyn FnMut(usize) -> usize) -> Value,
        pick: &mut dyn FnMut(usize) -> usize,
    ) -> Value {
        if depth == 0 {
            return leaf(pick);
        }
        let parts: Vec<Value> =
            (0..2 + pick(2)).map(|_| tree(depth - 1, !any, leaf, pick)).collect();
        json!({if any { "any" } else { "all" }: parts})
    }
    let leaf = |pick: &mut dyn FnMut(usize) -> usize| match pick(10) {
        0..=4 => json!({"has": p(pick(kinds)), "n": 1 + pick(copies)}),
        5..=7 => json!({"has": p(pick(kinds))}),
        _ => {
            let sum: Vec<Value> = (0..3).map(|_| json!({"count": p(pick(kinds))})).collect();
            json!({"ge": [{"int": ["+", sum[0], sum[1], sum[2]]}, 1 + pick(copies)]})
        }
    };
    let rules: Vec<Value> = (0..checks)
        .map(|l| {
            let rule = if pick(4) == 0 {
                json!({"true": {}})
            } else {
                tree(3 + pick(4) / 3, true, &leaf, pick)
            };
            json!({"loc": format!("l{l}"), "rule": rule})
        })
        .collect();
    let events: Vec<Value> =
        (0..6).map(|_| json!({"event": "lit", "rule": tree(2, true, &leaf, pick)})).collect();
    let mut items: Vec<Value> = (0..kinds).map(|k| json!({"key": p(k), "id": k + 1})).collect();
    items.push(json!({"key": "filler", "id": kinds + 1}));
    let manifest = json!({
        "options": [],
        "items": items,
        "locations": (0..checks).map(|l| json!({"key": format!("l{l}"), "id": l + 1}))
            .collect::<Vec<_>>(),
        "start": "r0",
        "x.time": {"slices": 1, "start_mask": 1},
        "defs": {},
        "regions": [{"key": "r0", "can_stay": false, "checks": rules, "exits": [],
                     "events": events, "stays": []}],
        "goal": {"ge": [{"counter": "lit"}, 6]},
    });
    let mut pool: Vec<String> = (0..kinds).flat_map(|k| vec![p(k); copies]).collect();
    pool.resize(checks, "filler".to_owned());
    let mut pool_class: std::collections::HashMap<String, String> =
        (0..kinds).map(|k| (p(k), "progression".to_owned())).collect();
    pool_class.insert("filler".to_owned(), "filler".to_owned());
    let inst = Instance {
        locations: (0..checks).map(|l| format!("l{l}")).collect(),
        pool,
        pool_class,
        ..Instance::default()
    };
    (manifest, inst)
}

/// One player's world and instance. The same `seed` and `player` give the same bytes.
#[must_use]
pub fn world(shape: &Shape, seed: u64, player: u64) -> (Value, Instance) {
    let mut rng = SpecRng::from_u64_tagged(seed, b"synthrm\0", player);
    let mut pick = |n: usize| usize::try_from(rng.below(n as u64).unwrap_or(0)).unwrap_or(0);
    if shape.wide {
        return wide(shape, &mut pick);
    }
    if let Some(how) = shape.hostile.filter(|h| *h != Hostile::Tight) {
        return keyed(shape, how, &mut pick);
    }
    let checks = shape.checks.max(1);
    let regions = ((checks as f64 * shape.regions) as usize).max(2);
    let slices = shape.slices.clamp(2, 64);
    let halves = (slices / 8).max(2);
    // Half-day `h` is slices `half(h).0..=half(h).1`; the later half of the day as a mask.
    let half = |h: usize| (h * slices / halves, (h + 1) * slices / halves - 1);
    let late: u64 = (slices / 2..slices).fold(0, |m, b| m | 1 << b);
    let layers = shape.layers.clamp(1, regions - 1);
    // Region 0 is the start; the others are spread evenly over the layers.
    let layer = |r: usize| if r == 0 { 0 } else { (r - 1) * layers / (regions - 1) + 1 };
    let in_layer: Vec<Vec<usize>> =
        (0..=layers).map(|d| (0..regions).filter(|r| layer(*r) == d).collect()).collect();
    // Progression copies, spread over kinds of about four copies each.
    let copies_total = ((checks as f64 * shape.progression).round() as usize).clamp(1, checks);
    let kinds = (copies_total / 4).max(1);
    let copies = |k: usize| copies_total / kinds + usize::from(k < copies_total % kinds);
    let p = |k: usize| format!("p{k}");
    let has = |pick: &mut dyn FnMut(usize) -> usize| {
        let k = pick(kinds);
        match pick(3) {
            0 => json!({"has": p(k), "n": 1 + pick(copies(k))}),
            _ => json!({"has": p(k)}),
        }
    };
    // A time leaf: the later half of the day, or (rich) a window of up to eight slices.
    let time_leaf = |pick: &mut dyn FnMut(usize) -> usize| {
        if !shape.rich || slices <= 2 {
            return json!({"x.time_any": late});
        }
        let from = 1 + pick(slices - 1);
        let window = (from..slices.min(from + 1 + pick(8))).fold(0u64, |m, b| m | 1 << b);
        json!({"x.time_any": window})
    };
    let defs = ((kinds as f64 * shape.defs) as usize).max(1);
    let def_rules: serde_json::Map<String, Value> = (0..defs)
        .map(|d| {
            // Rich: chains of up to four defs, and one def in seven reads time.
            let (first, second) = (has(&mut pick), has(&mut pick));
            let last = match d {
                _ if !shape.rich => has(&mut pick),
                _ if d % 7 == 3 => time_leaf(&mut pick),
                _ if d % 4 != 0 => json!({"ref": format!("d{}", d - 1)}),
                _ => has(&mut pick),
            };
            let rule = json!({"any": [first, {"all": [second, last]}]});
            (format!("d{d}"), rule)
        })
        .collect();
    // Counters, each lit by two events in the first layer, read from the third layer on.
    let counters = (regions / 16).max(1);
    let mut events: Vec<Vec<Value>> = vec![Vec::new(); regions];
    for c in 0..counters {
        for _ in 0..shape.events {
            let r = in_layer[1][pick(in_layer[1].len())];
            events[r].push(json!({"event": format!("c{c}"), "rule": has(&mut pick)}));
        }
    }
    let gate = |d: usize, pick: &mut dyn FnMut(usize) -> usize| {
        let first = match pick(20) {
            0..=7 => has(pick),
            8..=11 => json!({"any": [has(pick), has(pick)]}),
            12..=15 => json!({"all": [has(pick), {"ref": format!("d{}", pick(defs))}]}),
            16..=17 if d >= 3 => json!({"ge": [{"counter": format!("c{}", pick(counters))}, 2]}),
            _ => has(pick),
        };
        if shape.rich && pick(2) == 0 {
            let second = json!({"any": [has(pick), {"ref": format!("d{}", pick(defs))}]});
            return json!({"all": [first, second]});
        }
        first
    };
    let mut exits: Vec<Vec<Value>> = vec![Vec::new(); regions];
    for r in 1..regions {
        let d = layer(r);
        for _ in 0..shape.branching.max(1) {
            let from = in_layer[d - 1][pick(in_layer[d - 1].len())];
            exits[from].push(json!({"to": format!("r{r}"), "rule": gate(d, &mut pick)}));
        }
    }
    // Every fourth region past the start can wait into the later slice.
    let can_stay = |r: usize| r % 4 == 3;
    let mut region_checks: Vec<Vec<Value>> = vec![Vec::new(); regions];
    // The checks that exist, then the absent ones (in the manifest, not in the instance).
    let locations = checks + (checks as f64 * shape.absent) as usize;
    for l in 0..locations {
        let r = l % regions;
        let rule = match pick(8) {
            0 if can_stay(r) => time_leaf(&mut pick),
            1 | 2 => has(&mut pick),
            3..=5 if shape.rich => gate(layer(r), &mut pick),
            _ => json!({"true": {}}),
        };
        region_checks[r].push(json!({"loc": format!("l{l}"), "rule": rule}));
    }
    let regions_json: Vec<Value> = (0..regions)
        .map(|r| {
            let stays = if can_stay(r) && r % 8 == 7 {
                json!([{"slice": slices / 2, "rule": has(&mut pick)}])
            } else {
                json!([])
            };
            json!({"key": format!("r{r}"), "can_stay": can_stay(r),
                   "checks": region_checks[r], "exits": exits[r], "events": events[r],
                   "stays": stays})
        })
        .collect();
    let mut time = json!({"slices": slices, "start_mask": 1});
    if shape.clocks {
        // The first half-day is owned from the start, each later one with an item of its own.
        let half_days: Vec<Value> = (0..halves)
            .map(|h| {
                let rule =
                    if h == 0 { json!({"true": {}}) } else { json!({"has": p((h - 1) % kinds)}) };
                json!({"first": half(h).0, "last": half(h).1, "rule": rule})
            })
            .collect();
        time["owned"] = json!({"when": {"true": {}}, "none_owned_mask": 1, "half_days": half_days});
    }
    let goal: Vec<Value> =
        in_layer[layers].iter().map(|r| json!({"reach": format!("region:r{r}")})).collect();
    let mut items: Vec<Value> = (0..kinds).map(|k| json!({"key": p(k), "id": k + 1})).collect();
    items.push(json!({"key": "useful", "id": kinds + 1}));
    items.push(json!({"key": "filler", "id": kinds + 2}));
    let manifest = json!({
        "options": [],
        "items": items,
        "locations": (0..locations).map(|l| json!({"key": format!("l{l}"), "id": l + 1}))
            .collect::<Vec<_>>(),
        "start": "r0",
        "x.time": time,
        "defs": def_rules,
        "regions": regions_json,
        "goal": {"all": goal},
    });
    let mut pool: Vec<String> = (0..kinds).flat_map(|k| vec![p(k); copies(k)]).collect();
    let rest = checks - pool.len();
    pool.extend((0..rest).map(|i| if i % 4 == 0 { "useful" } else { "filler" }.to_owned()));
    let mut pool_class: std::collections::HashMap<String, String> =
        (0..kinds).map(|k| (p(k), "progression".to_owned())).collect();
    pool_class.insert("useful".to_owned(), "useful".to_owned());
    pool_class.insert("filler".to_owned(), "filler".to_owned());
    let mut inst = Instance {
        locations: (0..checks).map(|l| format!("l{l}")).collect(),
        pool,
        pool_class,
        ..Instance::default()
    };
    if shape.hostile == Some(Hostile::Tight) {
        inst.excluded = (0..checks).step_by(2).map(|l| format!("l{l}")).collect();
    }
    (manifest, inst)
}

/// A room of `players` synthetic worlds (player `p` drawn as [`world`]`(shape, seed, p)`),
/// compiled and resolved on up to `workers` threads.
///
/// # Errors
/// As [`Compiled::new`].
pub fn room(shape: &Shape, seed: u64, players: usize, workers: usize) -> Result<Compiled, String> {
    let built: Vec<(Value, Instance)> =
        (0..players as u64).map(|p| world(shape, seed, p)).collect();
    let manifests: Vec<&Value> = built.iter().map(|b| &b.0).collect();
    let insts: Vec<&Instance> = built.iter().map(|b| &b.1).collect();
    Compiled::new(&manifests, &insts, workers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::{Given, Path, fill_room_on, fill_room_spheres};
    use crate::spheres::RoomSpheres;
    use crate::world::World;

    fn small(clocks: bool) -> Shape {
        Shape { checks: 60, layers: 3, clocks, ..Shape::default() }
    }

    fn fill(c: &Compiled, seed: u64) -> (Vec<Vec<Option<Given>>>, crate::Stats, usize) {
        fill_on(c, seed, None)
    }

    /// A beatable fill on `path` (`None`: the path a room of that size takes).
    fn fill_on(
        c: &Compiled,
        seed: u64,
        path: Option<Path>,
    ) -> (Vec<Vec<Option<Given>>>, crate::Stats, usize) {
        let (seats, mut rng) = (c.seats(), SpecRng::from_u64(seed));
        let (f, sp) = match path {
            None => fill_room_spheres(&seats, &mut rng, config::FILL_MAX_ATTEMPTS),
            Some(path) => fill_room_on(&seats, &mut rng, config::FILL_MAX_ATTEMPTS, path),
        }
        .unwrap();
        assert!(f.all_beatable(), "seed {seed}: {:?}", f.beatable);
        (f.placed, f.stats, sp.spheres.len())
    }

    /// A room of `players` worlds of `shape` whose instances `change` adjusted (by player).
    fn room_with(
        shape: &Shape,
        seed: u64,
        players: u64,
        change: impl Fn(u64, &mut Instance),
    ) -> Result<Compiled, String> {
        let built: Vec<(Value, Instance)> = (0..players)
            .map(|p| {
                let (m, mut inst) = world(shape, seed, p);
                change(p, &mut inst);
                (m, inst)
            })
            .collect();
        let manifests: Vec<&Value> = built.iter().map(|b| &b.0).collect();
        let insts: Vec<&Instance> = built.iter().map(|b| &b.1).collect();
        Compiled::new(&manifests, &insts, 1)
    }

    /// Confine `p0` to the group `home`: the locations `l` with `l % every < most`.
    fn confine(inst: &mut Instance, every: usize, most: usize) {
        inst.item_groups.insert("p0".to_owned(), "home".to_owned());
        for l in (0..inst.locations.len()).filter(|l| l % every < most) {
            inst.location_groups.insert(format!("l{l}"), "home".to_owned());
        }
    }

    /// The pinned fills as text: rooms of three on `path`, and player 1 alone.
    fn pinned(path: Option<Path>) -> String {
        let mut out = String::new();
        for clocks in [false, true] {
            // 60 checks make 7 regions: `home` is the locations of the first two.
            let c = room_with(&small(clocks), 11, 3, |_, inst| confine(inst, 7, 2)).unwrap();
            for seed in 1..=4 {
                out += &format!("{:?}\n", fill_on(&c, seed, path).0);
                let one = crate::fill(
                    &c.worlds[0],
                    &c.plans[0],
                    &mut SpecRng::from_u64(seed),
                    config::FILL_MAX_ATTEMPTS,
                )
                .unwrap();
                assert!(one.beatable, "seed {seed}");
                out += &format!("{:?}\n", one.placed);
            }
        }
        out
    }

    /// The exact placements of fixed seeds, room and single player, with and without clocks,
    /// with `p0` confined to a placement group: pinned, so a change to how the fill scans its
    /// locations or looks up groups must keep every byte.
    #[test]
    fn placements_of_fixed_seeds_are_pinned() {
        let out = pinned(None);
        let p0 = |row: &str| row.matches("item: 0 }").count();
        assert!(out.lines().any(|r| p0(r) > 0), "the confined item was placed");
        assert_eq!(format!("{:016x}", crate::check::fnv1a(out.as_bytes())), "036064f5ec95175d");
    }

    /// The same pin with the rooms forced onto the forward path, on one thread and on four.
    #[test]
    fn the_pinned_placements_are_the_same_on_the_forward_path() {
        for workers in [1, 4] {
            let out = pinned(Some(Path::Forward { workers }));
            let hash = format!("{:016x}", crate::check::fnv1a(out.as_bytes()));
            assert_eq!(hash, "036064f5ec95175d", "{workers} workers");
        }
    }

    /// The same pin with the rooms forced onto the split path.
    #[test]
    fn the_pinned_placements_are_the_same_on_the_split_path() {
        let out = pinned(Some(Path::Split));
        assert_eq!(format!("{:016x}", crate::check::fnv1a(out.as_bytes())), "036064f5ec95175d");
    }

    /// The split path reaches every round's state by adding to one solve and taking the
    /// additions back; that state is the least fixed point a fresh solve of the round gives,
    /// so it must decide everything exactly as the forward path and truth maintenance do:
    /// placements, spheres, attempts, rounds, what is reached, or the error. Rooms of 1 to 12
    /// players; both profiles, clocks on and off, a high progression share; the hostile
    /// worlds and the wide one; excluded and locked locations, start items, confined items;
    /// fills that are stuck on every attempt and fills that need a retry (an attempt that
    /// ends in the middle of the rounds); every rule as bytecode; every player on one world;
    /// a world of enough rounds for seven levels of halves.
    #[test]
    fn the_split_path_places_what_a_solve_per_round_places() {
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let dense = Shape { progression: 0.7, ..small(true) };
        let hostile = |how| Shape { checks: 60, ..Shape::hostile(how) };
        let cases = [
            (small(false), ""),
            (small(true), "confined"),
            (small(false), "excluded+locked+start"),
            (heavy, ""),
            (heavy, "confined+excluded+locked+start"),
            (dense, "excluded"),
            (dense, "confined+locked"),
            (small(true), "cramped"),
            // Worlds where one item out changes much of what is reached, and deep rules.
            (hostile(Hostile::Chain), ""),
            (hostile(Hostile::Gated), ""),
            (hostile(Hostile::Keys), ""),
            (hostile(Hostile::Brink), ""),
            (hostile(Hostile::Tight), ""),
            (Shape::wide(), ""),
        ];
        let (mut rooms, mut stuck, mut retried, mut undone) = (0, 0, 0, 0);
        for (shape, what) in cases {
            for players in [1u64, 2, 3, 8, 12] {
                let c = room_with(&shape, 23, players, |p, inst| change(what, p, inst)).unwrap();
                for seed in 1..=4 {
                    let at = format!("{what:?}, {players} players, seed {seed}");
                    let (split, _, _) = outcome(&c, seed, Path::Split);
                    assert_eq!(split, outcome(&c, seed, Path::Forward { workers: 1 }).0, "{at}");
                    assert_eq!(split, outcome(&c, seed, Path::Carry).0, "{at}");
                    rooms += 1;
                    stuck += u32::from(split.is_err());
                    retried += u32::from(split.is_ok_and(|x| x.2 > 1));
                    let mut rng = SpecRng::from_u64(seed);
                    let filled =
                        fill_room_on(&c.seats(), &mut rng, config::FILL_MAX_ATTEMPTS, Path::Split);
                    undone += filled.map_or(0, |f| f.0.profile.undone);
                }
            }
        }
        // As much progression as locations that take it: stuck at first, filled on a later
        // attempt, with the same draws on every path.
        let tight = Shape { progression: 0.5, ..small(true) };
        for (players, seed) in [(3, 4), (8, 6), (9, 7), (12, 6)] {
            let c = room_with(&tight, 23, players, |p, inst| change("half", p, inst)).unwrap();
            let (split, _, _) = outcome(&c, seed, Path::Split);
            assert_eq!(split, outcome(&c, seed, Path::Carry).0, "{players} players, seed {seed}");
            retried += u32::from(split.is_ok_and(|x| x.2 > 1));
        }
        assert_eq!(rooms, 280);
        assert!(stuck > 0 && retried >= 4, "{stuck} stuck, {retried} retried");
        // Every rule as bytecode, and every player on one world (`Compiled::shared`).
        for (shape, players) in [(heavy, 3), (hostile(Hostile::Brink), 2), (Shape::wide(), 2)] {
            let mut c = room_with(&shape, 23, players, |_, _| {}).unwrap();
            c.plans = c.plans.into_iter().map(crate::Plan::interpreted).collect();
            let (m, inst) = world(&shape, 23, 0);
            let one = vec![&inst; 9];
            let shared = Compiled::shared(World::load(&m).unwrap(), &one, 1).unwrap();
            for seed in 1..=4 {
                for (room, what) in [(&c, "bytecode"), (&shared, "one world")] {
                    let (split, stats, _) = outcome(room, seed, Path::Split);
                    let at = format!("{shape:?}, {what}, seed {seed}");
                    assert_eq!(split, outcome(room, seed, Path::Forward { workers: 1 }).0, "{at}");
                    assert!(what != "bytecode" || stats.bits == 0, "{at}");
                }
            }
        }
        // Rounds enough for seven levels of halves (the rooms above have four to six).
        let deep = Shape { checks: 400, ..Shape::heavy() };
        let c = room_with(&deep, 23, 2, |_, _| {}).unwrap();
        let (split, _, _) = outcome(&c, 1, Path::Split);
        assert!(split.as_ref().is_ok_and(|f| f.3 > 64), "rounds: {:?}", split.map(|f| f.3));
        assert_eq!(split, outcome(&c, 1, Path::Forward { workers: 1 }).0, "400 checks");
        assert!(undone > 100_000, "{undone} journal steps undone");
    }

    /// Rule nodes of a rule in the manifest's JSON: one per object, plus its list operands.
    fn nodes(rule: &Value) -> usize {
        let inner = |v: &Value| v.as_array().map_or(0, |a| a.iter().map(nodes).sum());
        1 + rule.as_object().map_or(0, |o| o.values().map(inner).sum())
    }

    /// The heavy profile against the test world's counts per check
    /// (`docs/data-only-world-experiment.md`: 1516 checks, 318 regions, 3685 rule sites, 107
    /// defs, 45 slices), within a fifth; about 2.6 nodes per rule site.
    #[test]
    fn the_heavy_profile_has_the_test_worlds_counts_per_check() {
        let shape = Shape::heavy();
        let (m, inst) = world(&shape, 1, 0);
        let checks = inst.locations.len() as f64;
        let regions = m["regions"].as_array().unwrap();
        let (mut sites, mut size) = (0usize, 0usize);
        for r in regions {
            for group in ["checks", "exits", "events", "stays"] {
                for s in r[group].as_array().unwrap() {
                    sites += 1;
                    size += nodes(&s["rule"]);
                }
            }
        }
        let near = |got: f64, want: f64, what: &str| {
            assert!((got / want - 1.0).abs() <= 0.2, "{what}: {got}, the test world has {want}");
        };
        near(regions.len() as f64 / checks, 318.0 / 1516.0, "regions per check");
        near(sites as f64 / checks, 3685.0 / 1516.0, "rule sites per check");
        near(m["defs"].as_object().unwrap().len() as f64 / checks, 107.0 / 1516.0, "defs");
        near(size as f64 / sites as f64, 2.6, "nodes per rule site");
        assert_eq!(m["x.time"]["slices"], 45);
        assert!(m["x.time"]["owned"]["half_days"].as_array().unwrap().len() > 2, "clocks on");
        assert!(regions.iter().any(|r| !r["events"].as_array().unwrap().is_empty()), "events");
        eprintln!(
            "heavy: {} regions, {sites} sites, {} defs, {:.2} nodes per site",
            regions.len(),
            m["defs"].as_object().unwrap().len(),
            size as f64 / sites as f64
        );
        // A small room of it fills beatable, the same bytes for a seed, items crossing worlds.
        let shape = Shape { checks: 60, layers: 3, ..shape };
        let (a, b) = (room(&shape, 7, 3, 1).unwrap(), room(&shape, 7, 3, 3).unwrap());
        for seed in 1..=3 {
            let x = fill(&a, seed);
            assert_eq!(x, fill(&b, seed), "seed {seed}");
            let crossed =
                x.0.iter()
                    .enumerate()
                    .any(|(p, r)| r.iter().flatten().any(|g| usize::from(g.to) != p));
            assert!(crossed && x.1.removals > 0, "seed {seed}: items cross, removals ran");
        }
    }

    /// The wide profile is one region of deep rules: 131 checks and 6 events, over 15 rule
    /// nodes a site, some rules past the bit rules' term cap. It fills beatable, alone and in a
    /// room, the same with bit rules and with bytecode only.
    #[test]
    fn the_wide_profile_is_one_region_of_deep_rules_and_fills() {
        let shape = Shape::wide();
        let (m, inst) = world(&shape, 1, 0);
        let region = &m["regions"].as_array().unwrap()[0];
        let sites = || ["checks", "events"].into_iter().flat_map(|g| region[g].as_array().unwrap());
        let size: usize = sites().map(|s| nodes(&s["rule"])).sum();
        assert_eq!((m["regions"].as_array().unwrap().len(), inst.locations.len()), (1, 131));
        assert_eq!(sites().count(), 137);
        assert!(size / 137 >= 15, "{} nodes per rule site", size / 137);
        for players in [1, 3] {
            let c = room(&shape, 1, players, 1).unwrap();
            let classes = c.plans[0].bit_rules(&c.worlds[0]).0;
            assert!(classes[2] > 15 && classes[4] > 30, "counted and bytecode rules: {classes:?}");
            let plain = Compiled {
                plans: c.plans.iter().cloned().map(crate::Plan::interpreted).collect(),
                worlds: c.worlds.clone(),
                progression: c.progression.clone(),
            };
            for seed in 1..=3 {
                let (a, sa, _) = outcome(&c, seed, Path::Carry);
                let (b, sb, _) = outcome(&plain, seed, Path::Carry);
                assert!(a.as_ref().is_ok_and(|f| f.4.iter().all(|b| *b)), "seed {seed}: {a:?}");
                assert_eq!((a, crate::Stats { bits: 0, ..sa }), (b, sb), "seed {seed}");
                assert!(sa.bits > 0 && sa.bits < sa.evals, "{sa:?}");
            }
        }
    }

    /// Everything a fill decides, or the error it ends with as text.
    type Outcome =
        Result<(Vec<Vec<Option<Given>>>, RoomSpheres, u32, u32, Vec<bool>, Vec<usize>), String>;

    fn outcome(c: &Compiled, seed: u64, path: Path) -> (Outcome, crate::Stats, u32) {
        let mut rng = SpecRng::from_u64(seed);
        match fill_room_on(&c.seats(), &mut rng, config::FILL_MAX_ATTEMPTS, path) {
            Ok((f, sp)) => {
                (Ok((f.placed, sp, f.attempts, f.rounds, f.beatable, f.reached)), f.stats, f.solves)
            }
            Err(e) => (Err(format!("{e:?}")), crate::Stats::default(), 0),
        }
    }

    /// Instance changes the identity test mixes, by name.
    fn change(what: &str, p: u64, inst: &mut Instance) {
        let has = |k: &str| what.split('+').any(|w| w == k);
        if has("confined") {
            confine(inst, 5, 2);
        }
        if has("excluded") {
            // A sixth of the locations take no progression; player 2's are others.
            let off = usize::from(p == 1);
            inst.excluded = (0..inst.locations.len())
                .filter(|l| l % 6 == off)
                .map(|l| format!("l{l}"))
                .collect();
        }
        if has("locked") {
            // A progression item and a filler leave the pool for fixed locations.
            for (key, loc) in [("p1", "l3"), ("filler", "l10")] {
                let at = inst.pool.iter().position(|k| k == key).unwrap();
                inst.locked.insert(loc.to_owned(), inst.pool.remove(at));
            }
        }
        if has("start") {
            inst.start_inventory.push("p1".to_owned());
            inst.logic_start.insert("p0".to_owned(), 1);
        }
        if has("half") {
            inst.excluded = (0..inst.locations.len()).step_by(2).map(|l| format!("l{l}")).collect();
        }
        if has("cramped") {
            // More copies of `p0` than its group has locations: every attempt is stuck.
            confine(inst, inst.locations.len(), 1);
        }
    }

    /// Folding the `has` tests the start items meet changes nothing a fill decides: the same
    /// plans with those tests left in the rules (`Plan::without_start_fold`) give the same
    /// outcome, by bit rules and by bytecode, carried and forward.
    #[test]
    fn folding_the_start_items_changes_nothing_a_fill_decides() {
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let cases = [(small(false), "excluded+locked+start"), (heavy, "confined+locked+start")];
        let mut removals = 0;
        for (shape, what) in cases {
            for players in [1u64, 3, 9] {
                for bits in [true, false] {
                    let plain = |p: crate::Plan| if bits { p } else { p.interpreted() };
                    let room =
                        || room_with(&shape, 23, players, |p, inst| change(what, p, inst)).unwrap();
                    let (mut folded, mut unfolded) = (room(), room());
                    folded.plans = folded.plans.into_iter().map(plain).collect();
                    unfolded.plans = (unfolded.plans.into_iter().zip(&unfolded.worlds))
                        .map(|(p, w)| plain(p.without_start_fold(w)))
                        .collect();
                    for path in [Path::Carry, Path::Forward { workers: 1 }] {
                        for seed in 1..=3 {
                            let (a, stats, _) = outcome(&folded, seed, path);
                            let (b, _, _) = outcome(&unfolded, seed, path);
                            let at = format!("{what}, {players} players, bits {bits}, {path:?}");
                            assert_eq!(a, b, "{at}, seed {seed}");
                            removals += stats.removals;
                        }
                    }
                }
            }
        }
        assert!(removals > 0, "no item was taken back");
    }

    /// A round's state is the unique least fixed point for what is held and placed, and
    /// placement reads only what that state reaches: so a fresh forward solve per round
    /// (`Path::Forward`, on one thread and on four) must decide everything exactly as truth
    /// maintenance (`Path::Carry`) does: placements, spheres, attempts, rounds, or the error.
    /// Rooms below, at and above `config::ROOM_FORWARD_MIN_PLAYERS`; both profiles, clocks on
    /// and off, a high progression share; excluded and locked locations, start items, confined
    /// items (placed one at a time, carried, on both paths); fills that are stuck on every
    /// attempt and fills that need a retry.
    #[test]
    fn a_forward_solve_per_round_places_what_truth_maintenance_does() {
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let dense = Shape { progression: 0.7, ..small(true) };
        let cases = [
            (small(false), ""),
            (small(true), "confined"),
            (small(false), "excluded+locked+start"),
            (heavy, ""),
            (heavy, "confined+excluded+locked+start"),
            (dense, "excluded"),
            (dense, "confined+locked"),
            (small(true), "cramped"),
        ];
        let (mut rooms, mut stuck, mut fresh, mut retracted) = (0, 0, 0, 0);
        for (shape, what) in cases {
            for players in [1u64, 3, 8, 9, 12] {
                let c = room_with(&shape, 23, players, |p, inst| change(what, p, inst)).unwrap();
                for seed in 1..=2 {
                    let (a, sa, solves) = outcome(&c, seed, Path::Carry);
                    let at = format!("{shape:?}, {what:?}, {players} players, seed {seed}");
                    for workers in [1, 4] {
                        let (b, sb, forward) = outcome(&c, seed, Path::Forward { workers });
                        assert_eq!(a, b, "{at}, {workers} workers");
                        // The forward path solved afresh per round and took nothing back there.
                        assert!(a.is_err() || forward > solves, "{at}");
                        assert!(what.contains("confined") || sb.removals == 0, "{at}");
                        fresh += forward;
                    }
                    rooms += 1;
                    retracted += sa.retractions;
                    match &a {
                        Ok(f) => assert!(f.4.iter().all(|b| *b), "beatable, {at}"),
                        Err(e) => {
                            assert!(e.contains("Stuck") && what == "cramped", "{at}: {e}");
                            stuck += 1;
                        }
                    }
                }
            }
        }
        assert!(rooms == 80 && fresh > 0 && retracted > 0, "both paths ran");
        assert_eq!(stuck, 10, "the cramped rooms, every size and seed");
        // As much progression as locations that take it: these seeds are stuck at first and
        // fill on a later attempt, with the same draws on every path.
        let tight = Shape { progression: 0.5, ..small(true) };
        for (players, seed) in [(3, 4), (8, 6), (9, 7), (12, 6)] {
            let c = room_with(&tight, 23, players, |p, inst| change("half", p, inst)).unwrap();
            let (a, ..) = outcome(&c, seed, Path::Carry);
            let attempts = a.as_ref().map(|f| f.2).unwrap();
            assert!(attempts > 1, "{players} players, seed {seed}: {attempts} attempts");
            for workers in [1, 4] {
                let (b, ..) = outcome(&c, seed, Path::Forward { workers });
                assert_eq!(a, b, "{players} players, seed {seed}, {workers} workers");
            }
        }
    }

    /// A plan's bit rules answer what its bytecode does: fills with them, and with every rule
    /// left as bytecode (`Plan::interpreted`), agree in everything they decide and in every work
    /// counter but the one that says who answered. Both profiles, clocks on and off, start
    /// items, locked, excluded and confined locations; one player and rooms; both paths and the
    /// level mode.
    #[test]
    fn bit_rules_decide_what_the_bytecode_does() {
        use crate::room::{Algorithm, Dials, fill_room_by};
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let (mut bits, mut evals) = (0, 0);
        for (shape, what) in [
            (small(false), ""),
            (small(true), "confined+excluded+locked+start"),
            (heavy, ""),
            (heavy, "confined+locked+start"),
        ] {
            for players in [1u64, 3, 9] {
                let c = room_with(&shape, 23, players, |p, inst| change(what, p, inst)).unwrap();
                let plain = Compiled {
                    plans: c.plans.iter().cloned().map(crate::Plan::interpreted).collect(),
                    worlds: c.worlds.clone(),
                    progression: c.progression.clone(),
                };
                for seed in 1..=2 {
                    let at = format!("{shape:?}, {what:?}, {players} players, seed {seed}");
                    for path in [Path::Carry, Path::Forward { workers: 1 }] {
                        let (a, sa, solves) = outcome(&c, seed, path);
                        let (b, sb, plainly) = outcome(&plain, seed, path);
                        assert_eq!(sb.bits, 0, "{at}: bytecode only");
                        assert_eq!(
                            (a, crate::Stats { bits: 0, ..sa }, solves),
                            (b, sb, plainly),
                            "{at}, {path:?}"
                        );
                        bits += sa.bits;
                        evals += sa.evals;
                    }
                    let level = |c: &Compiled| {
                        let (path, mode) =
                            (Path::Forward { workers: 1 }, Algorithm::Level(Dials::default()));
                        let mut rng = SpecRng::from_u64(seed);
                        fill_room_by(&c.seats(), &mut rng, config::FILL_MAX_ATTEMPTS, path, mode)
                            .map(|x| (x.0.placed, x.0.fast, x.0.repairs, x.0.stats.evals, x.1))
                            .map_err(|e| format!("{e:?}"))
                    };
                    assert_eq!(level(&c), level(&plain), "{at}, level fill");
                }
            }
        }
        assert!(bits * 2 > evals, "bit rules answered {bits} of {evals} evaluations");
    }

    /// A fill's states never evaluate the checks of locations that do not exist this seed and
    /// grant nothing. Against the same plans with those checks evaluated
    /// (`Plan::with_absent_checks`): everything a fill decides is the same, on both paths and
    /// in the level mode, with fewer rule evaluations on the heavy profile (about every second
    /// location of it is absent) and the same count on the light one (none is).
    #[test]
    fn skipping_absent_checks_decides_the_same_with_fewer_evaluations() {
        use crate::room::{Algorithm, Dials, fill_room_by};
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        for (shape, what) in
            [(small(true), "confined+locked+start"), (heavy, ""), (heavy, "locked")]
        {
            for players in [1u64, 3, 9] {
                let c = room_with(&shape, 23, players, |p, inst| change(what, p, inst)).unwrap();
                let whole = Compiled {
                    plans: c.plans.iter().cloned().map(crate::Plan::with_absent_checks).collect(),
                    worlds: c.worlds.clone(),
                    progression: c.progression.clone(),
                };
                let absent = c.plans[0].rules_report(&c.worlds[0])["absent_check_sites"].clone();
                assert_eq!(absent != 0, shape.absent > 0.0, "{absent} absent check sites");
                for seed in 1..=2 {
                    let at = format!("{shape:?}, {what:?}, {players} players, seed {seed}");
                    for path in [Path::Carry, Path::Forward { workers: 1 }] {
                        let (a, sa, solves) = outcome(&c, seed, path);
                        let (b, sb, wholly) = outcome(&whole, seed, path);
                        assert_eq!((&a, solves), (&b, wholly), "{at}, {path:?}");
                        assert!(a.is_ok(), "{at}");
                        if shape.absent > 0.0 {
                            assert!(sa.evals < sb.evals, "{at}: {} of {}", sa.evals, sb.evals);
                        } else {
                            assert_eq!(sa, sb, "{at}");
                        }
                    }
                    let level = |c: &Compiled| {
                        let (path, mode) =
                            (Path::Forward { workers: 1 }, Algorithm::Level(Dials::default()));
                        let mut rng = SpecRng::from_u64(seed);
                        fill_room_by(&c.seats(), &mut rng, config::FILL_MAX_ATTEMPTS, path, mode)
                            .map(|x| (x.0.placed, x.0.fast, x.0.repairs, x.1))
                            .map_err(|e| format!("{e:?}"))
                    };
                    assert_eq!(level(&c), level(&whole), "{at}, level fill");
                }
            }
        }
        // One state, lean and whole: the same regions, counters and existing locations.
        let c = room(&heavy, 23, 1, 1).unwrap();
        let (w, plan) = (&c.worlds[0], &c.plans[0]);
        let whole = plan.state(w, &plan.progression, &[]);
        let mut lean = crate::Reach::new(w, plan).lean();
        plan.solve_into(&mut lean, &plan.progression, &[]);
        assert_eq!((&lean.masks, &lean.counters), (&whole.masks, &whole.counters));
        assert!(plan.included.iter().all(|l| lean.reached(*l) == whole.reached(*l)));
        let reached =
            |r: &crate::Reach<'_>| (0..w.locations()).filter(|l| r.reached(*l as u32)).count();
        assert!(reached(&lean) < reached(&whole), "the absent locations are not asked");
    }

    /// The forward path's states run on several threads: the placements, the spheres and every
    /// work counter are the same with one worker and with many (the steps are the same ones;
    /// only who runs a state differs). The last room is large enough for threads to start.
    #[test]
    fn the_forward_path_is_the_same_with_one_worker_and_with_many() {
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let large = Shape { checks: 150, ..heavy };
        let mut threaded = false;
        for (shape, sizes) in [
            (small(false), &[2usize, 3, 9][..]),
            (small(true), &[5]),
            (heavy, &[9]),
            (large, &[12]),
        ] {
            for players in sizes {
                let c = room(&shape, 31, *players, 1).unwrap();
                let sites: usize = c.worlds.iter().map(World::site_count).sum();
                threaded |= sites >= config::ROOM_FORWARD_MIN_PENDING;
                for seed in 1..=2 {
                    let one = outcome(&c, seed, Path::Forward { workers: 1 });
                    assert!(one.0.is_ok() && one.1.evals > 0);
                    for workers in [2, 3, 64] {
                        let many = outcome(&c, seed, Path::Forward { workers });
                        assert_eq!(
                            one, many,
                            "{shape:?}, {players} players, seed {seed}, {workers}"
                        );
                    }
                }
            }
        }
        assert!(threaded, "one room has enough rule sites for a step to start threads");
    }

    /// Level fill and shuffle fill over both profiles, one player and rooms, with placement groups, excluded
    /// and locked locations, start items and clocks: every seed is held against the exact
    /// fill's (`assert_fast`), on the carried path and forward on one thread and on several,
    /// which must all place the same.
    #[test]
    fn the_opt_in_modes_are_beatable_reach_what_the_exact_fill_does_and_ignore_the_worker_count() {
        use crate::room::{Algorithm, Dials, assert_fast};
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let (mut fast, mut fell, mut all, mut swaps) = (0, 0, 0, 0);
        for (shape, players, what) in [
            (small(false), 1, ""),
            (small(true), 1, "confined+excluded"),
            (small(false), 3, "confined+excluded+locked"),
            (small(true), 3, "locked+start"),
            (heavy, 1, "locked"),
            (heavy, 3, "confined"),
            (heavy, 9, "excluded"),
            (small(false), 2, "half"),
        ] {
            let c = room_with(&shape, 17, players, |p, inst| change(what, p, inst)).unwrap();
            let seats = c.seats();
            for seed in 1..=24 {
                let dials = Dials {
                    depth: [0, 100, -100][seed as usize % 3],
                    goal_only: seed % 12 > 9,
                    skip_empty: seed % 4 < 2,
                };
                let mode =
                    if seed > 12 { Algorithm::Shuffle(dials) } else { Algorithm::Level(dials) };
                let Some(one) = assert_fast(&seats, seed, Path::Forward { workers: 1 }, mode)
                else {
                    continue;
                };
                for path in [Path::Carry, Path::Forward { workers: 4 }] {
                    let other = assert_fast(&seats, seed, path, mode).unwrap();
                    assert_eq!(one.placed, other.placed, "{what}, seed {seed}, {path:?}");
                    assert_eq!(
                        (one.fast, one.repairs, one.fallbacks),
                        (other.fast, other.repairs, other.fallbacks),
                        "{what}, seed {seed}, {path:?}"
                    );
                    // The same steps on any number of threads; the carried path's first solve
                    // runs in rank order, which evaluates in another order.
                    assert!(path == Path::Carry || one.stats == other.stats, "{what}, {seed}");
                }
                all += 1;
                fast += u32::from(one.fast);
                fell += one.fallbacks;
                swaps += one.swaps;
            }
        }
        println!(
            "the modes placed {fast} of {all} rooms themselves, {fell} fallbacks, {swaps} swaps"
        );
        assert!(all > 160 && fast * 2 > all, "the modes placed {fast} of {all} rooms themselves");
        assert!(swaps > 0, "no shuffle needed a swap");
    }

    /// Rooms where a round moves several items: a move that lands in its owner's own world is
    /// counted at once, and the moves after it in the round must see the same reached
    /// locations whether the checks of empty locations sleep or not.
    #[test]
    fn skipping_empty_checks_places_the_same_in_rooms_of_several_moves_a_round() {
        use crate::room::{Algorithm, Dials, fill_room_by};
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let (mut rooms, mut moved) = (0, 0);
        for (shape, players) in [(small(false), 3), (small(true), 4), (heavy, 3), (heavy, 6)] {
            let c = room(&shape, 23, players, 1).unwrap();
            let seats = c.seats();
            for seed in 1..=40 {
                let run = |skip_empty| {
                    let mode = Algorithm::Shuffle(Dials { skip_empty, ..Dials::default() });
                    let mut rng = SpecRng::from_u64(seed);
                    fill_room_by(&seats, &mut rng, config::FILL_MAX_ATTEMPTS, Path::Carry, mode)
                        .map(|x| (x.0.placed, x.0.swaps, x.0.fast, x.1))
                        .map_err(|e| format!("{e:?}"))
                };
                let (with, without) = (run(true), run(false));
                assert_eq!(
                    with, without,
                    "{players} players, clocks {}, seed {seed}",
                    shape.clocks
                );
                rooms += 1;
                moved += with.map_or(0, |x| x.1);
            }
        }
        assert!(moved > rooms / 2, "{moved} moves in {rooms} rooms: too few to test rounds");
    }

    /// The point of the mode: far fewer rule evaluations than a solve per round, and the same
    /// seed for the same draws.
    #[test]
    fn level_fill_evaluates_far_fewer_rules_and_is_deterministic() {
        use crate::room::{Algorithm, Dials, fill_room_by};
        let c = room(&Shape { checks: 200, ..Shape::default() }, 5, 9, 1).unwrap();
        let seats = c.seats();
        let path = Path::Forward { workers: 1 };
        let run = |seed, mode| {
            let mut rng = SpecRng::from_u64(seed);
            fill_room_by(&seats, &mut rng, config::FILL_MAX_ATTEMPTS, path, mode).unwrap().0
        };
        let (mut exact, mut level) = (0, 0);
        for seed in 1..=3 {
            let mode = Algorithm::Level(Dials::default());
            let (a, b) = (run(seed, mode), run(seed, mode));
            assert_eq!(a.placed, b.placed, "seed {seed}");
            assert_ne!(a.placed, run(seed + 100, mode).placed, "seed {seed}: seeds differ");
            if a.fast {
                exact += run(seed, Algorithm::Exact).stats.evals;
                level += a.stats.evals;
            }
        }
        assert!(level > 0 && level * 5 < exact, "level {level} against exact {exact} evaluations");
    }

    /// A room below `config::ROOM_FORWARD_MIN_PLAYERS` is carried, one at it goes forward, on
    /// the workers asked for, capped by `config::ROOM_FORWARD_WORKERS` and never below one.
    #[test]
    fn the_path_changes_at_the_configured_room_size() {
        let (at, cap) = (config::ROOM_FORWARD_MIN_PLAYERS, config::ROOM_FORWARD_WORKERS);
        assert_eq!(Path::of(1, 4), Path::Carry);
        assert_eq!(Path::of(at - 1, 4), Path::Carry);
        assert_eq!(Path::of(at, 1), Path::Forward { workers: 1 });
        assert_eq!(Path::of(at + 1, 0), Path::Forward { workers: 1 });
        let Path::Forward { workers } = Path::of(at, usize::MAX) else { panic!("forward") };
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        assert_eq!(workers, cap.min(cores), "the cap, or the machine's cores when fewer");
        // The room fill takes that path: a room at the threshold solves afresh per round.
        let c = room(&small(false), 5, at, 1).unwrap();
        let (f, _) =
            fill_room_spheres(&c.seats(), &mut SpecRng::from_u64(1), config::FILL_MAX_ATTEMPTS)
                .unwrap();
        assert!(f.solves > 1 && f.stats.removals == 0, "solves {}", f.solves);
    }

    #[test]
    fn a_small_synthetic_room_is_beatable_and_the_same_for_a_seed() {
        for clocks in [false, true] {
            let shape = small(clocks);
            let (x, y) = (world(&shape, 7, 1), world(&shape, 7, 1));
            assert_eq!((x.0, x.1.pool), (y.0, y.1.pool), "same seed, same manifest and pool");
            assert_ne!(world(&shape, 7, 1).0, world(&shape, 7, 2).0, "players differ");
            let a = room(&shape, 7, 3, 1).unwrap();
            let b = room(&shape, 7, 3, 3).unwrap();
            for seed in 1..=3 {
                let x = fill(&a, seed);
                assert_eq!(x, fill(&b, seed), "clocks {clocks}, seed {seed}");
                assert!(x.1.removals > 0 && x.1.retractions > 0, "the take-back path ran");
                let crossed =
                    x.0.iter()
                        .enumerate()
                        .any(|(p, row)| row.iter().flatten().any(|g| usize::from(g.to) != p));
                assert!(crossed, "items cross worlds");
            }
        }
    }
}
