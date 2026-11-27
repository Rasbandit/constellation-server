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
    /// Entrance shuffle ([`crate::shuffle`]): the first exit into every region is the outside
    /// of a two-way door with an open way back, and one coupled `layout` shuffle moves all of
    /// them inside two type groups. With `branching` 1 a door is the only way into a region.
    /// The start region's checks are then all open, so a first item can always be found
    /// (without it the heavy profile can draw a world nothing can fill, issue 209).
    pub doors: bool,
    /// No time context: one slice, no clocks, no stays, and what would test time tests an
    /// item. An exit shuffle needs it (`crate::shuffle` refuses a world with a time context).
    pub timeless: bool,
    /// A `none` shuffle ([`crate::shuffle`]): eight guards over eight halls, each guard
    /// beaten with one kind of progression item, and one check in eight behind a hall's guard.
    pub guards: bool,
    /// Confined items: every copy of `p0` may only go to the group `home`, two locations in
    /// five ([`confine`]`(_, 5, 2)`).
    pub confined: bool,
    /// Half the gates of the layered world are a sum of the copies of three kinds ([`Sums`]).
    pub sums: Sums,
}

/// The sum gates of [`Shape::sums`]: drawn alike, so the two worlds differ only in weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sums {
    /// No sum gates: the worlds as before the flag.
    #[default]
    Off,
    /// `count(a) + count(b) + count(c) >= t`.
    Counts,
    /// The same three kinds weighted 1 to 4, at least `t` times the first kind's weight.
    Weighted,
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
            doors: false,
            timeless: false,
            guards: false,
            confined: false,
            sums: Sums::Off,
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
        let rule = open_first(l, rule);
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
            let rule = open_first(l, rule);
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

/// The rule of check `l`, drawn as `rule`: the first check, in the start region, is open, so a
/// fill always has a location for a first item. The draws are made either way, so the rest of
/// the world is as before.
fn open_first(l: usize, rule: Value) -> Value {
    if l == 0 { json!({"true": {}}) } else { rule }
}

/// One player's world and instance. The same `seed` and `player` give the same bytes.
#[must_use]
pub fn world(shape: &Shape, seed: u64, player: u64) -> (Value, Instance) {
    let (manifest, mut inst) = drawn(shape, seed, player);
    if shape.confined {
        confine(&mut inst, 5, 2);
    }
    (manifest, inst)
}

/// Confine `p0` to the group `home`: the locations `l` with `l % every < most`.
pub fn confine(inst: &mut Instance, every: usize, most: usize) {
    inst.item_groups.insert("p0".to_owned(), "home".to_owned());
    for l in (0..inst.locations.len()).filter(|l| l % every < most) {
        inst.location_groups.insert(format!("l{l}"), "home".to_owned());
    }
}

fn drawn(shape: &Shape, seed: u64, player: u64) -> (Value, Instance) {
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
    let slices = if shape.timeless { 1 } else { shape.slices.clamp(2, 64) };
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
        if shape.timeless {
            return has(pick);
        }
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
    // `t` copies of the first kind alone meet it, so every gate still holds with the whole pool.
    let sum = |pick: &mut dyn FnMut(usize) -> usize| {
        let kinds3 = [pick(kinds), pick(kinds), pick(kinds)];
        let weights = kinds3.map(|_| 1 + pick(4));
        let t = 1 + pick(copies(kinds3[0]));
        let weighted = shape.sums == Sums::Weighted;
        let mut terms = vec![json!("+")];
        terms.extend(kinds3.iter().zip(weights).map(|(k, w)| {
            let c = json!({"count": p(*k)});
            if weighted { json!({"int": ["*", c, w]}) } else { c }
        }));
        json!({"ge": [{"int": terms}, if weighted { t * weights[0] } else { t }]})
    };
    let gate = |d: usize, pick: &mut dyn FnMut(usize) -> usize| {
        if shape.sums != Sums::Off && pick(2) == 0 {
            return sum(pick);
        }
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
        for b in 0..shape.branching.max(1) {
            let from = in_layer[d - 1][pick(in_layer[d - 1].len())];
            let mut exit = json!({"to": format!("r{r}"), "rule": gate(d, &mut pick)});
            if shape.doors && b == 0 {
                let door = format!("d{r}");
                exit["key"] = json!(format!("e{r}"));
                exit["group"] = json!(format!("g{}", r % 2));
                exits[r].push(json!({"to": format!("r{from}"), "rule": {"true": {}},
                                     "key": format!("b{r}"), "door": door}));
                exit["door"] = json!(door);
            }
            exits[from].push(exit);
        }
    }
    // Every fourth region past the start can wait into the later slice.
    let can_stay = |r: usize| r % 4 == 3 && !shape.timeless;
    let mut region_checks: Vec<Vec<Value>> = vec![Vec::new(); regions];
    // The checks that exist, then the absent ones (in the manifest, not in the instance).
    let locations = checks + (checks as f64 * shape.absent) as usize;
    for l in 0..locations {
        let r = l % regions;
        let rule = match pick(8) {
            _ if shape.doors && r == 0 => json!({"true": {}}),
            _ if shape.guards && l % 8 == 3 => json!({"at": ["guards", format!("h{}", l / 8 % 8)]}),
            0 if can_stay(r) => time_leaf(&mut pick),
            1 | 2 => has(&mut pick),
            3..=5 if shape.rich => gate(layer(r), &mut pick),
            _ => json!({"true": {}}),
        };
        let rule = open_first(l, rule);
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
    if shape.clocks && !shape.timeless {
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
    let mut manifest = json!({
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
    if shape.doors {
        let group = |g: &str| json!({"slots": {"groups": [g]}, "only": {"groups": [g]}});
        manifest["shuffles"] = json!({"entrances": {
            "coupled": true,
            "slots": {"type": "exit", "names": (1..regions).map(|r| format!("e{r}")).collect::<Vec<_>>()},
            "allow": [group("g0"), group("g1")],
        }});
    }
    if shape.guards {
        let things: serde_json::Map<String, Value> =
            (0..8).map(|k| (format!("g{k}"), json!({"rule": {"has": p(k % kinds)}}))).collect();
        let halls: Vec<String> = (0..8).map(|k| format!("h{k}")).collect();
        manifest["shuffles"]["guards"] =
            json!({"slots": {"type": "name", "names": halls}, "things": things});
    }
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
        // The odd ones, so the open first check (`open_first`) can take progression.
        inst.excluded = (1..checks).step_by(2).map(|l| format!("l{l}")).collect();
    }
    (manifest, inst)
}

/// [`world`] as an upload carries it: the manifest named, game `Synth` and version `1` (what an
/// upload's check asks for).
#[must_use]
pub fn uploaded(shape: &Shape, seed: u64, player: u64) -> (Value, Instance) {
    let (mut manifest, instance) = world(shape, seed, player);
    (manifest["game"], manifest["version"]) = (json!("Synth"), json!("1"));
    (manifest, instance)
}

/// `n` players of one `instance`, named `P0`, `P1` and so on.
#[must_use]
pub fn players(instance: &Instance, n: usize) -> Vec<crate::seed::Player> {
    (0..n)
        .map(|p| crate::seed::Player { name: format!("P{p}"), instance: instance.clone() })
        .collect()
}

/// A room of `players` synthetic worlds (player `p` drawn as [`world`]`(shape, seed, p)`),
/// compiled and resolved.
///
/// # Errors
/// As [`Compiled::new`].
pub fn room(shape: &Shape, seed: u64, players: usize) -> Result<Compiled, String> {
    mixed(std::slice::from_ref(shape), seed, players)
}

/// A room of `players` synthetic worlds of `shapes` in turn: player `p` drawn as
/// [`world`]`(shapes[p % shapes.len()], seed, p)`. A room whose piles differ in length and
/// shape.
///
/// # Errors
/// As [`Compiled::new`].
pub fn mixed(shapes: &[Shape], seed: u64, players: usize) -> Result<Compiled, String> {
    mixed_with(shapes, seed, players, |_, _| {})
}

/// [`mixed`] with each player's instance changed by `change` (by player) before it is
/// resolved.
///
/// # Errors
/// As [`Compiled::new`].
pub fn mixed_with(
    shapes: &[Shape],
    seed: u64,
    players: usize,
    change: impl Fn(u64, &mut Instance),
) -> Result<Compiled, String> {
    let built: Vec<(Value, Instance)> = (0..players as u64)
        .map(|p| {
            let (m, mut inst) = world(&shapes[p as usize % shapes.len()], seed, p);
            change(p, &mut inst);
            (m, inst)
        })
        .collect();
    let manifests: Vec<&Value> = built.iter().map(|b| &b.0).collect();
    let insts: Vec<&Instance> = built.iter().map(|b| &b.1).collect();
    Compiled::new(&manifests, &insts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::{Given, Rule, fill_room, fill_room_spheres, fill_room_test};
    use crate::spheres::RoomSpheres;
    use crate::world::World;

    /// Every drawn world has a location that can take progression and is reached with nothing
    /// held, so a fill has somewhere to put a first item: without one no placement is beatable
    /// (the heavy world of 120 checks drew none on seed 8). Every kind of world, at several
    /// sizes, over many seeds.
    #[test]
    fn every_world_reaches_a_location_with_nothing_held() {
        let hostile = |how| Shape::hostile(how);
        let kinds = [
            Shape::default(),
            Shape::heavy(),
            Shape { sums: Sums::Counts, ..Shape::heavy() },
            Shape { sums: Sums::Weighted, ..Shape::default() },
            Shape { sums: Sums::Weighted, ..Shape::heavy() },
            Shape::wide(),
            hostile(Hostile::Chain),
            hostile(Hostile::Gated),
            hostile(Hostile::Keys),
            hostile(Hostile::Brink),
            hostile(Hostile::Tight),
        ];
        let mut worlds = 0;
        for shape in kinds {
            for checks in [40, 120, 240, 1000] {
                let shape = Shape { checks, layers: 3, ..shape };
                for seed in 1..=if checks == 1000 { 4 } else { 40 } {
                    let (m, inst) = world(&shape, seed, seed % 3);
                    let w = World::load(&m).unwrap();
                    let plan = crate::Plan::new(&w, &inst).unwrap();
                    let r = plan.solve(&w, &[]);
                    let open =
                        plan.included.iter().any(|l| r.reached(*l) && !plan.excluded[*l as usize]);
                    assert!(open, "{shape:?}, seed {seed}: nothing reached with nothing held");
                    worlds += 1;
                }
            }
        }
        assert_eq!(worlds, 11 * (3 * 40 + 4));
    }

    fn small(clocks: bool) -> Shape {
        Shape { checks: 60, layers: 3, clocks, ..Shape::default() }
    }

    fn fill(c: &Compiled, seed: u64) -> (Vec<Vec<Option<Given>>>, crate::Stats, usize) {
        fill_on(c, seed, false)
    }

    /// The product's fill of `c`, or with `fresh` the cross-check that solves every group's
    /// state afresh (`room::fill_room_test`), on up to `attempts` attempts.
    fn filled(
        c: &Compiled,
        seed: u64,
        attempts: u32,
        fresh: bool,
    ) -> Result<(crate::RoomFilled, RoomSpheres), crate::FillError> {
        let mut rng = SpecRng::from_u64(seed);
        fill_room_test(&c.seats(), &mut rng, attempts, (config::FILL_REPAIR_EVALS, Rule::of, fresh))
    }

    /// A beatable fill, of the product or with `fresh` of the cross-check.
    fn fill_on(
        c: &Compiled,
        seed: u64,
        fresh: bool,
    ) -> (Vec<Vec<Option<Given>>>, crate::Stats, usize) {
        let (f, sp) = if fresh {
            filled(c, seed, config::FILL_MAX_ATTEMPTS, true)
        } else {
            fill_room_spheres(&c.seats(), &mut SpecRng::from_u64(seed), config::FILL_MAX_ATTEMPTS)
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
        mixed_with(std::slice::from_ref(shape), seed, usize::try_from(players).unwrap(), change)
    }

    /// The pinned fills as text: rooms of three (with `fresh`, by the cross-check), and player 1
    /// alone.
    fn pinned(fresh: bool) -> String {
        let mut out = String::new();
        for clocks in [false, true] {
            // 60 checks make 7 regions: `home` is the locations of the first two.
            let c = room_with(&small(clocks), 11, 3, |_, inst| confine(inst, 7, 2)).unwrap();
            for seed in 1..=4 {
                out += &format!("{:?}\n", fill_on(&c, seed, fresh).0);
                // Player 1 alone: a room of one, its locations' items as one world's.
                let one = &c.seats()[..1];
                let f = fill_room(one, &mut SpecRng::from_u64(seed), config::FILL_MAX_ATTEMPTS)
                    .unwrap();
                assert!(f.all_beatable(), "seed {seed}");
                let placed: Vec<Option<u16>> =
                    f.placed[0].iter().map(|g| g.map(|g| g.item)).collect();
                out += &format!("{placed:?}\n");
            }
        }
        out
    }

    /// A snapshot: the exact placements of fixed seeds, room and single player, with and
    /// without clocks, with `p0` confined to a placement group. It catches a change nobody
    /// meant and a difference between machines. When placements are meant to change, update
    /// the hash in a commit that says so. Its rooms never divide a round or move an item, so
    /// the round procedure of issue 212 places them as before (and deepest first would too:
    /// a player's round items here are five copies of one kind, `p0` being confined, so any
    /// order of them is the same placements).
    #[test]
    fn placements_of_fixed_seeds_are_pinned() {
        let out = pinned(false);
        let p0 = |row: &str| row.matches("item: 0 }").count();
        assert!(out.lines().any(|r| p0(r) > 0), "the confined item was placed");
        assert_eq!(crate::check::fnv1a(out.as_bytes()), "036064f5ec95175d");
    }

    /// The same pin with the rooms filled by a fresh solve of every group's state: two live
    /// computations of the same placements (clocks, and a confined item).
    #[test]
    fn the_pinned_placements_are_the_same_by_a_solve_per_round() {
        let out = pinned(true);
        assert_eq!(crate::check::fnv1a(out.as_bytes()), "036064f5ec95175d");
    }

    /// A snapshot of the repaired rounds: a room of 22 chain worlds, where rounds are divided
    /// and items moved in place (issue 212), by the fill and by its cross-check. Update it in a
    /// commit that says so.
    #[test]
    fn the_repaired_rounds_of_a_room_of_chains_are_pinned() {
        let c = room(&Shape::hostile(Hostile::Chain), 1, 22).unwrap();
        for fresh in [false, true] {
            let f = once(&c, 1, fresh).unwrap();
            let counts = (f.attempts, f.rounds, f.divided, f.swaps);
            let hash = crate::check::placements_hash(&f.placed);
            assert_eq!(
                (counts, hash.as_str()),
                ((1, 139, 46, 8), "92838999e01ee2cb"),
                "fresh {fresh}"
            );
        }
    }

    /// A confined progression item never lands on an excluded location of its group (#203):
    /// `p0` is confined to a group of 24 locations, every other one of them excluded. Over
    /// many seeds, alone and in a room, by the fill and by its cross-check. And a group that could hold its item's
    /// copies only by using excluded locations is refused when the instance is resolved, by
    /// name, instead of being filled onto them or tried twenty times.
    #[test]
    fn a_confined_progression_item_never_takes_an_excluded_location() {
        let shape = small(false);
        let grouped = |inst: &mut Instance, most: usize| {
            confine(inst, 5, 2);
            inst.excluded = (0..inst.locations.len())
                .filter(|l| l % 5 < 2)
                .enumerate()
                .filter(|(k, _)| k % 2 == 0 || *k >= most)
                .map(|(_, l)| format!("l{l}"))
                .collect();
        };
        let (mut fills, mut copies) = (0, 0);
        let mut on_excluded = |c: &Compiled, placed: &[Vec<Option<Given>>], what: &str| {
            for (p, row) in placed.iter().enumerate() {
                let (w, plan) = (&c.worlds[p], &c.plans[p]);
                let p0 = w.item("p0").unwrap();
                for (l, g) in row.iter().enumerate() {
                    if g.is_some_and(|g| g.item == p0 && usize::from(g.to) == p) {
                        copies += 1;
                        assert!(!plan.excluded[l], "{what}: p0 on excluded l{l} of player {p}");
                    }
                }
            }
            fills += 1;
        };
        // A third more seeds than when three paths each filled the room: as many fills.
        for seed in 1..=40 {
            let one = room_with(&shape, 23, 1, |_, inst| grouped(inst, 24)).unwrap();
            let mut rng = SpecRng::from_u64(seed);
            if let Ok(f) = fill_room(&one.seats(), &mut rng, 20) {
                on_excluded(&one, &f.placed, &format!("alone, seed {seed}"));
            }
            let room = room_with(&shape, 23, 3, |_, inst| grouped(inst, 24)).unwrap();
            for fresh in [false, true] {
                if let Ok((f, _)) = filled(&room, seed, 20, fresh) {
                    on_excluded(&room, &f.placed, &format!("fresh {fresh}, seed {seed}"));
                }
            }
        }
        assert!(fills >= 100 && copies > 500, "{fills} fills, {copies} copies of p0 seen");
        // Only two of the group's 24 locations take progression: fewer than p0's copies.
        let e = room_with(&shape, 23, 1, |_, inst| grouped(inst, 4)).unwrap_err();
        assert!(e.contains("p0") && e.contains("excluded"), "{e}");
    }

    /// The split path reaches every round's state by adding to one solve and taking the
    /// additions back; that state is the least fixed point a fresh solve of the round gives,
    /// so it must decide everything exactly as a fresh solve of every round does:
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
                    let (split, _, _) = outcome(&c, seed, false);
                    assert_eq!(split, outcome(&c, seed, true).0, "{at}");
                    rooms += 1;
                    stuck += u32::from(split.is_err());
                    retried += u32::from(split.is_ok_and(|x| x.2 > 1));
                    let f = filled(&c, seed, config::FILL_MAX_ATTEMPTS, false);
                    undone += f.map_or(0, |f| f.0.profile.undone);
                }
            }
        }
        // As much progression as locations that take it: seeds that were stuck at first in
        // drawn order. They fill in one attempt now, the same on every path.
        let tight = Shape { progression: 0.5, ..small(true) };
        for (players, seed) in [(3, 4), (8, 6), (9, 7), (12, 6)] {
            let c = room_with(&tight, 23, players, |p, inst| change("half", p, inst)).unwrap();
            let (split, _, _) = outcome(&c, seed, false);
            assert_eq!(split, outcome(&c, seed, true).0, "{players} players, seed {seed}");
            retried += u32::from(split.is_ok_and(|x| x.2 > 1));
        }
        assert_eq!(rooms, 280);
        assert!(stuck > 0 && retried == 0, "{stuck} stuck, {retried} retried");
        // Every rule as bytecode, and every player on one world (`Compiled::shared`).
        for (shape, players) in [(heavy, 3), (hostile(Hostile::Brink), 2), (Shape::wide(), 2)] {
            let mut c = room_with(&shape, 23, players, |_, _| {}).unwrap();
            c.plans = c.plans.into_iter().map(crate::Plan::interpreted).collect();
            let (m, inst) = world(&shape, 23, 0);
            let one = vec![&inst; 9];
            let shared = Compiled::shared(World::load(&m).unwrap(), &one).unwrap();
            for seed in 1..=4 {
                for (room, what) in [(&c, "bytecode"), (&shared, "one world")] {
                    let (split, stats, _) = outcome(room, seed, false);
                    let at = format!("{shape:?}, {what}, seed {seed}");
                    assert_eq!(split, outcome(room, seed, true).0, "{at}");
                    assert!(what != "bytecode" || stats.bits == 0, "{at}");
                }
            }
        }
        // Rounds enough for seven levels of halves (the rooms above have four to six).
        let deep = Shape { checks: 400, ..Shape::heavy() };
        let c = room_with(&deep, 23, 2, |_, _| {}).unwrap();
        let (split, _, _) = outcome(&c, 1, false);
        assert!(split.as_ref().is_ok_and(|f| f.3 > 64), "rounds: {:?}", split.map(|f| f.3));
        assert_eq!(split, outcome(&c, 1, true).0, "400 checks");
        assert!(undone > 100_000, "{undone} journal steps undone");
    }

    /// One attempt (with `fresh`, of the cross-check): everything it decides, or its error.
    fn one_pass(c: &Compiled, seed: u64, fresh: bool) -> Outcome {
        filled(c, seed, 1, fresh)
            .map(|(f, sp)| (f.placed, sp, f.attempts, f.rounds, f.beatable, f.reached))
            .map_err(String::from)
    }

    /// One attempt (with `fresh`, of the cross-check), as filled, or its error.
    fn once(c: &Compiled, seed: u64, fresh: bool) -> Result<crate::RoomFilled, String> {
        filled(c, seed, 1, fresh).map(|(f, _)| f).map_err(String::from)
    }

    /// One attempt whose rounds are formed by `rule` (a baseline), or its error.
    fn baseline(
        c: &Compiled,
        seed: u64,
        rule: fn(u32) -> Rule,
    ) -> Result<crate::RoomFilled, String> {
        let mut rng = SpecRng::from_u64(seed);
        fill_room_test(&c.seats(), &mut rng, 1, (config::FILL_REPAIR_EVALS, rule, false))
            .map(|(f, _)| f)
            .map_err(String::from)
    }

    /// The rounds before issue 212: drawn order, never divided, no item moved.
    fn drawn(_: u32) -> Rule {
        Rule { deepest: false, repair: false }
    }

    /// A fill that holds up by itself: every goal reachable, the report a fresh solve's, every
    /// location that exists holds one item, and every progression item lies where it is reached.
    fn sound(c: &Compiled, f: &crate::RoomFilled, at: &str) {
        let seats = c.seats();
        assert!(f.all_beatable(), "{at}");
        let fresh = crate::room::validate_room(&seats, &f.placed).unwrap();
        assert_eq!((f.beatable.clone(), f.reached.clone()), fresh, "{at}");
        let sp = crate::spheres::room_spheres(&seats, &f.placed).unwrap();
        for (p, (s, row)) in seats.iter().zip(&f.placed).enumerate() {
            assert_eq!(row.iter().flatten().count(), s.plan.included.len(), "{at}");
            for (l, g) in row.iter().enumerate() {
                if g.is_some_and(|g| crate::room::is_progression(&seats, g)) {
                    let spot = (u16::try_from(p).unwrap(), u32::try_from(l).unwrap());
                    assert!(
                        !sp.unreachable.contains(&spot),
                        "{at}: progression at {spot:?} out of reach"
                    );
                }
            }
        }
    }

    /// Issue 212: rooms of worlds where one key hides everything behind it, at sizes the
    /// rounds in drawn order were stuck on at every attempt (chain from 22 players, keys from
    /// 16). In the order drawn, rounds divided where they do not fit and items moved in place,
    /// they fill in one pass, sound, and the item chains stay at least as long as deepest first
    /// gives (counted in spheres). The rounds as they were are stuck on the same room and seed.
    #[test]
    fn rooms_of_chains_fill_in_one_pass_by_repair_where_drawn_order_was_stuck() {
        for (how, players) in [(Hostile::Chain, 22), (Hostile::Keys, 16)] {
            let c = room(&Shape::hostile(how), 1, players).unwrap();
            let seats = c.seats();
            for seed in [1, 2] {
                let at = format!("{how:?}, {players} players, seed {seed}");
                let f = once(&c, seed, false).expect(&at);
                assert_eq!(f.attempts, 1, "{at}");
                assert!(
                    f.divided > 0 && f.swaps > 0,
                    "{at}: {} divided, {} moved",
                    f.divided,
                    f.swaps
                );
                sound(&c, &f, &at);
                let spheres = |f: &crate::RoomFilled| {
                    crate::spheres::room_spheres(&seats, &f.placed).unwrap().spheres.len()
                };
                let deepest =
                    baseline(&c, seed, |_| Rule { deepest: true, repair: true }).expect(&at);
                assert!(
                    spheres(&f) > spheres(&deepest),
                    "{at}: {} against {}",
                    spheres(&f),
                    spheres(&deepest)
                );
                let drawn = baseline(&c, seed, drawn);
                assert!(drawn.is_err_and(|e| e.contains("Stuck")), "{at}: drawn order filled");
            }
        }
    }

    /// One player: a round is one item, so nothing is divided, and where the assumed fill has
    /// nowhere to put an item (no free location reached without it) the item takes a placed
    /// one's location instead of the fill starting over. Rooms of one tight world (as much
    /// progression as locations that take it): some seeds fill in one pass only by a move,
    /// sound, where the rounds as they were needed another attempt.
    #[test]
    fn a_player_alone_is_repaired_in_place_instead_of_starting_over() {
        let c = room(&Shape { checks: 200, ..Shape::hostile(Hostile::Tight) }, 23, 1).unwrap();
        let mut moved = 0;
        for seed in 1..=24 {
            let at = format!("seed {seed}");
            let f = once(&c, seed, false).expect(&at);
            assert_eq!((f.attempts, f.divided), (1, 0), "{at}");
            sound(&c, &f, &at);
            if f.swaps > 0 {
                moved += 1;
                assert!(baseline(&c, seed, drawn).is_err(), "{at}");
            }
        }
        assert!(moved > 2, "{moved} seeds moved an item");
    }

    /// The work an attempt may spend on moves is bounded (`config::FILL_REPAIR_EVALS`): with no
    /// budget at all, or one that stops after the first solve, the room of chains that needs
    /// divisions and moves (both charged) is stuck in one attempt, with the clean error and the item's name; given a second
    /// attempt, it goes deepest first and fills without a move. With the configured budget it
    /// fills in one attempt within it, and the work counted is the same by the cross-check.
    #[test]
    fn repair_has_a_budget_of_work_and_a_stuck_attempt_goes_deepest_first() {
        let c = room(&Shape::hostile(Hostile::Chain), 1, 22).unwrap();
        let seats = c.seats();
        let rng = || SpecRng::from_u64(1);
        let fill_room_budget =
            |seats: &[crate::Seat<'_>], rng: &mut SpecRng, (n, budget), fresh| {
                fill_room_test(seats, rng, n, (budget, Rule::of, fresh)).map(|x| x.0)
            };
        for budget in [0, 1] {
            match fill_room_budget(&seats, &mut rng(), (1, budget), false) {
                Err(crate::FillError::Stuck { item, attempts: 1 }) => {
                    assert!(item.starts_with('k'));
                }
                other => panic!("budget {budget}: {:?}", other.map(|f| f.attempts)),
            }
        }
        // The first attempt's first division spent the (empty) budget: one unit, the
        // evaluations of this room's start solve (counted); deepest first needs no division
        // and no move.
        let f = fill_room_budget(&seats, &mut rng(), (2, 0), false).unwrap();
        assert_eq!((f.attempts, f.swaps, f.divided, f.repair_evals), (2, 0, 0, 242));
        sound(&c, &f, "deepest first");
        let budget = config::FILL_REPAIR_EVALS;
        let f = fill_room_budget(&seats, &mut rng(), (1, budget), false).unwrap();
        assert!(f.swaps > 0 && f.repair_evals > 0, "{} moved", f.swaps);
        assert!(f.repair_evals < budget, "{} evaluations", f.repair_evals);
        // Half of what the room took: the budget runs out part way, the same by the cross-check.
        let half = (2, f.repair_evals / 2);
        let split = fill_room_budget(&seats, &mut rng(), half, false).unwrap();
        assert_eq!(split.attempts, 2);
        let other = fill_room_budget(&seats, &mut rng(), (1, budget), true).unwrap();
        assert_eq!((other.repair_evals, other.placed), (f.repair_evals, f.placed.clone()));
        let other = fill_room_budget(&seats, &mut rng(), half, true).unwrap();
        let got = (other.attempts, other.repair_evals, other.placed);
        assert_eq!(got, (2, split.repair_evals, split.placed.clone()), "half");
    }

    /// Many rooms and seeds, one attempt each: a room that fills is beatable by a fresh solve
    /// of the whole room, every location that exists holds one item, and the same seed gives
    /// the same room again. Counted: most fill, and some rounds are divided.
    #[test]
    fn one_pass_fills_are_beatable_and_the_same_for_a_seed() {
        let hostile = |how| Shape { checks: 80, ..Shape::hostile(how) };
        let shapes = [
            small(false),
            small(true),
            Shape { checks: 80, layers: 3, ..Shape::heavy() },
            hostile(Hostile::Chain),
            hostile(Hostile::Keys),
            hostile(Hostile::Gated),
            hostile(Hostile::Brink),
            Shape { checks: 200, ..Shape::hostile(Hostile::Tight) },
            Shape::wide(),
        ];
        // Mixed rooms: piles of different lengths and shapes side by side.
        let mixes = [
            vec![hostile(Hostile::Chain), small(false)],
            vec![
                small(true),
                hostile(Hostile::Keys),
                Shape { checks: 80, layers: 3, ..Shape::heavy() },
            ],
        ];
        let rooms = shapes.iter().map(std::slice::from_ref).chain(mixes.iter().map(Vec::as_slice));
        let (mut filled, mut stuck, mut divided, mut moved) = (0, 0, 0, 0);
        for (n, shapes) in rooms.enumerate() {
            for players in [1usize, 2, 5, 13] {
                let c = mixed(shapes, 40 + n as u64, players).unwrap();
                let seats = c.seats();
                let piles = seats.iter().map(|s| s.plan.progression.len()).max().unwrap();
                for seed in 1..=6 {
                    let at = format!("shape {n}, {players} players, seed {seed}");
                    let got = one_pass(&c, seed, false);
                    assert_eq!(got, one_pass(&c, seed, false), "{at}");
                    let Ok((placed, _, _, rounds, beatable, reached)) = got else {
                        stuck += 1;
                        continue;
                    };
                    filled += 1;
                    // More groups placed than the longest pile has items: one was divided, or
                    // the rounds went on after an item was moved.
                    divided += u32::from(rounds as usize > piles);
                    moved += u32::from(once(&c, seed, false).unwrap().swaps > 0);
                    assert!(beatable.iter().all(|b| *b), "{at}");
                    let fresh = crate::room::validate_room(&seats, &placed).unwrap();
                    assert_eq!((beatable, reached), fresh, "{at}");
                    for (s, row) in seats.iter().zip(&placed) {
                        let held = row.iter().flatten().count();
                        assert_eq!(held, s.plan.included.len(), "{at}");
                    }
                }
            }
        }
        assert!(
            filled > 200 && divided > 0 && moved > 0,
            "{filled} filled, {stuck} stuck, {divided} divided, {moved} moved"
        );
    }

    /// A round that does not fit its state is divided, and an item with nowhere to go takes
    /// the place of a seated one, the same way by the fill and by its cross-check: rooms with as
    /// much progression as locations that take it, one attempt.
    /// Counted: some of them fill only because of that.
    #[test]
    fn a_repaired_round_places_the_same_by_a_solve_per_round() {
        let tight = Shape { checks: 200, ..Shape::hostile(Hostile::Tight) };
        let chain = Shape { checks: 80, ..Shape::hostile(Hostile::Chain) };
        let (mut repaired, mut moved) = (0, 0);
        for (shapes, players) in [
            (vec![tight], 2usize),
            (vec![tight], 3),
            (vec![tight], 8),
            (vec![tight], 12),
            (vec![tight, chain, small(true)], 9),
        ] {
            let c = mixed(&shapes, 23, players).unwrap();
            let piles = c.seats().iter().map(|s| s.plan.progression.len()).max().unwrap();
            for seed in 1..=8 {
                let at = format!("{shapes:?}, {players} players, seed {seed}");
                let split = one_pass(&c, seed, false);
                assert_eq!(split, one_pass(&c, seed, true), "{at}");
                repaired += u32::from(split.is_ok_and(|x| x.3 as usize > piles));
                moved += u32::from(once(&c, seed, false).is_ok_and(|f| f.swaps > 0));
            }
        }
        assert!(repaired > 8 && moved > 0, "{repaired} repaired, {moved} moved");
    }

    /// A room nothing can fill: every location in front of the last region is excluded, so
    /// the key to the last region has nowhere to go. The fill ends with the item's name after
    /// the attempts it was allowed, by the fill and by its cross-check.
    #[test]
    fn a_room_nothing_can_fill_ends_stuck_after_the_attempts_allowed() {
        let shape = Shape { checks: 60, ..Shape::hostile(Hostile::Chain) };
        let c = room_with(&shape, 5, 3, |_, inst| {
            inst.excluded = inst.locations.iter().take(50).cloned().collect();
        })
        .unwrap();
        for fresh in [false, true] {
            match filled(&c, 1, 3, fresh).map(|x| x.0.placed) {
                Err(crate::FillError::Stuck { item, attempts: 3 }) => {
                    assert!(item.starts_with('k'))
                }
                other => panic!("fresh {fresh}: {other:?}"),
            }
        }
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
        let (a, b) = (room(&shape, 7, 3).unwrap(), room(&shape, 7, 3).unwrap());
        for seed in 1..=3 {
            let x = fill(&a, seed);
            assert_eq!(x, fill(&b, seed), "seed {seed}");
            let crossed =
                x.0.iter()
                    .enumerate()
                    .any(|(p, r)| r.iter().flatten().any(|g| usize::from(g.to) != p));
            let fresh = fill_on(&a, seed, true);
            assert!(
                crossed && x.0 == fresh.0,
                "seed {seed}: items cross, by a solve per round too"
            );
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
            let c = room(&shape, 1, players).unwrap();
            let classes = c.plans[0].bit_rules(&c.worlds[0]).0;
            assert!(classes[2] > 15 && classes[4] > 30, "counted and bytecode rules: {classes:?}");
            let plain = Compiled {
                plans: c.plans.iter().cloned().map(crate::Plan::interpreted).collect(),
                worlds: c.worlds.clone(),
                progression: c.progression.clone(),
            };
            for seed in 1..=3 {
                let (a, sa, _) = outcome(&c, seed, false);
                let (b, sb, _) = outcome(&plain, seed, false);
                assert!(a.as_ref().is_ok_and(|f| f.4.iter().all(|b| *b)), "seed {seed}: {a:?}");
                assert_eq!((a, crate::Stats { bits: 0, ..sa }), (b, sb), "seed {seed}");
                assert!(sa.bits > 0 && sa.bits < sa.evals, "{sa:?}");
            }
        }
    }

    /// Everything a fill decides, or the error it ends with as text.
    type Outcome =
        Result<(Vec<Vec<Option<Given>>>, RoomSpheres, u32, u32, Vec<bool>, Vec<usize>), String>;

    fn outcome(c: &Compiled, seed: u64, fresh: bool) -> (Outcome, crate::Stats, u32) {
        match filled(c, seed, config::FILL_MAX_ATTEMPTS, fresh) {
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
    /// outcome, by bit rules and by bytecode, by the fill and by its cross-check.
    #[test]
    fn folding_the_start_items_changes_nothing_a_fill_decides() {
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let cases = [(small(false), "excluded+locked+start"), (heavy, "confined+locked+start")];
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
                    for fresh in [false, true] {
                        for seed in 1..=3 {
                            let (a, _, _) = outcome(&folded, seed, fresh);
                            let (b, _, _) = outcome(&unfolded, seed, fresh);
                            let at = format!("{what}, {players} players, bits {bits}, {fresh}");
                            assert_eq!(a, b, "{at}, seed {seed}");
                        }
                    }
                }
            }
        }
    }

    /// A plan's bit rules answer what its bytecode does: fills with them, and with every rule
    /// left as bytecode (`Plan::interpreted`), agree in everything they decide and in every work
    /// counter but the one that says who answered. Both profiles, clocks on and off, start
    /// items, locked, excluded and confined locations; one player and rooms; by the fill and by
    /// its cross-check.
    #[test]
    fn bit_rules_decide_what_the_bytecode_does() {
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
                    for fresh in [false, true] {
                        let (a, sa, solves) = outcome(&c, seed, fresh);
                        let (b, sb, plainly) = outcome(&plain, seed, fresh);
                        assert_eq!(sb.bits, 0, "{at}: bytecode only");
                        assert_eq!(
                            (a, crate::Stats { bits: 0, ..sa }, solves),
                            (b, sb, plainly),
                            "{at}, fresh {fresh}"
                        );
                        bits += sa.bits;
                        evals += sa.evals;
                    }
                }
            }
        }
        assert!(bits * 2 > evals, "bit rules answered {bits} of {evals} evaluations");
    }

    /// A fill's states never evaluate the checks of locations that do not exist this seed and
    /// grant nothing. Against the same plans with those checks evaluated
    /// (`Plan::with_absent_checks`): everything a fill decides is the same, by the fill and by
    /// its cross-check, with
    /// fewer rule evaluations on the heavy profile (about every second
    /// location of it is absent) and the same count on the light one (none is).
    #[test]
    fn skipping_absent_checks_decides_the_same_with_fewer_evaluations() {
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
                    for fresh in [false, true] {
                        let (a, sa, solves) = outcome(&c, seed, fresh);
                        let (b, sb, wholly) = outcome(&whole, seed, fresh);
                        assert_eq!((&a, solves), (&b, wholly), "{at}, fresh {fresh}");
                        assert!(a.is_ok(), "{at}");
                        if shape.absent > 0.0 {
                            assert!(sa.evals < sb.evals, "{at}: {} of {}", sa.evals, sb.evals);
                        } else {
                            assert_eq!(sa, sb, "{at}");
                        }
                    }
                }
            }
        }
        // One state, lean and whole: the same regions, counters and existing locations.
        let c = room(&heavy, 23, 1).unwrap();
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

    /// A room of one solves once and undoes what it added for its rounds.
    #[test]
    fn a_room_solves_once_and_undoes_what_it_added_for_its_rounds() {
        let c = room(&small(false), 5, 1).unwrap();
        let (f, _) =
            fill_room_spheres(&c.seats(), &mut SpecRng::from_u64(1), config::FILL_MAX_ATTEMPTS)
                .unwrap();
        assert!(f.solves == 1, "solves {}", f.solves);
        assert!(f.profile.undone > 0, "nothing was undone");
    }

    #[test]
    fn a_small_synthetic_room_is_beatable_and_the_same_for_a_seed() {
        for clocks in [false, true] {
            let shape = small(clocks);
            let (x, y) = (world(&shape, 7, 1), world(&shape, 7, 1));
            assert_eq!((x.0, x.1.pool), (y.0, y.1.pool), "same seed, same manifest and pool");
            assert_ne!(world(&shape, 7, 1).0, world(&shape, 7, 2).0, "players differ");
            let a = room(&shape, 7, 3).unwrap();
            let b = room(&shape, 7, 3).unwrap();
            for seed in 1..=3 {
                let x = fill(&a, seed);
                assert_eq!(x, fill(&b, seed), "clocks {clocks}, seed {seed}");
                // A fresh solve of every round places the same.
                let fresh = fill_on(&a, seed, true);
                assert_eq!(x.0, fresh.0, "clocks {clocks}, seed {seed}: a solve per round");
                let crossed =
                    x.0.iter()
                        .enumerate()
                        .any(|(p, row)| row.iter().flatten().any(|g| usize::from(g.to) != p));
                assert!(crossed, "items cross worlds");
            }
        }
    }
}
