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
}

/// One player's world and instance. The same `seed` and `player` give the same bytes.
#[must_use]
pub fn world(shape: &Shape, seed: u64, player: u64) -> (Value, Instance) {
    let mut rng = SpecRng::from_u64_tagged(seed, b"synthrm\0", player);
    let mut pick = |n: usize| usize::try_from(rng.below(n as u64).unwrap_or(0)).unwrap_or(0);
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
    let inst = Instance {
        locations: (0..checks).map(|l| format!("l{l}")).collect(),
        pool,
        pool_class,
        ..Instance::default()
    };
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
    use crate::room::{Given, fill_room_spheres};

    fn small(clocks: bool) -> Shape {
        Shape { checks: 60, layers: 3, clocks, ..Shape::default() }
    }

    fn fill(c: &Compiled, seed: u64) -> (Vec<Vec<Option<Given>>>, crate::Stats, usize) {
        let (f, sp) =
            fill_room_spheres(&c.seats(), &mut SpecRng::from_u64(seed), config::FILL_MAX_ATTEMPTS)
                .unwrap();
        assert!(f.all_beatable(), "seed {seed}: {:?}", f.beatable);
        (f.placed, f.stats, sp.spheres.len())
    }

    /// The exact placements of fixed seeds, room and single player, with and without clocks,
    /// with `p0` confined to a placement group: pinned, so a change to how the fill scans its
    /// locations or looks up groups must keep every byte.
    #[test]
    fn placements_of_fixed_seeds_are_pinned() {
        let mut out = String::new();
        for clocks in [false, true] {
            let shape = small(clocks);
            let built: Vec<(Value, Instance)> = (0..3)
                .map(|p| {
                    let (m, mut inst) = world(&shape, 11, p);
                    // 60 checks make 7 regions: these are the locations of the first two.
                    inst.item_groups.insert("p0".to_owned(), "home".to_owned());
                    for l in (0..shape.checks).filter(|l| l % 7 < 2) {
                        inst.location_groups.insert(format!("l{l}"), "home".to_owned());
                    }
                    (m, inst)
                })
                .collect();
            let manifests: Vec<&Value> = built.iter().map(|b| &b.0).collect();
            let insts: Vec<&Instance> = built.iter().map(|b| &b.1).collect();
            let c = Compiled::new(&manifests, &insts, 1).unwrap();
            for seed in 1..=4 {
                out += &format!("{:?}\n", fill(&c, seed).0);
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
        let p0 = |row: &str| row.matches("item: 0 }").count();
        assert!(out.lines().any(|r| p0(r) > 0), "the confined item was placed");
        assert_eq!(format!("{:016x}", crate::check::fnv1a(out.as_bytes())), "036064f5ec95175d");
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

    /// A round's state is the unique least fixed point for what is held and placed, and
    /// placement reads only what that state reaches: so a fresh forward solve per round
    /// (`Path::Forward`) must place every item exactly where truth maintenance (`Path::Carry`)
    /// does. Rooms of 1 to 5 players, both profiles, clocks on and off, with and without a
    /// confined item (confined items go one at a time, carried, on both paths).
    #[test]
    fn a_forward_solve_per_round_places_what_truth_maintenance_does() {
        use crate::room::{Path, fill_room_on};
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let (mut rooms, mut fresh, mut retracted) = (0, 0, 0);
        for (shape, confine) in
            [(small(false), false), (small(true), true), (heavy, false), (heavy, true)]
        {
            for players in [1usize, 2, 3, 5] {
                let built: Vec<(Value, Instance)> = (0..players as u64)
                    .map(|p| {
                        let (m, mut inst) = world(&shape, 23, p);
                        if confine {
                            inst.item_groups.insert("p0".to_owned(), "home".to_owned());
                            for l in (0..shape.checks).filter(|l| l % 5 < 2) {
                                inst.location_groups.insert(format!("l{l}"), "home".to_owned());
                            }
                        }
                        (m, inst)
                    })
                    .collect();
                let manifests: Vec<&Value> = built.iter().map(|b| &b.0).collect();
                let insts: Vec<&Instance> = built.iter().map(|b| &b.1).collect();
                let c = Compiled::new(&manifests, &insts, 1).unwrap();
                let seats = c.seats();
                for seed in 1..=4 {
                    let on = |path| {
                        let mut rng = SpecRng::from_u64(seed);
                        fill_room_on(&seats, &mut rng, config::FILL_MAX_ATTEMPTS, path).unwrap()
                    };
                    let ((a, sa), (b, sb)) = (on(Path::Carry), on(Path::Forward));
                    let what =
                        format!("{shape:?}, confined {confine}, {players} players, seed {seed}");
                    assert_eq!(a.placed, b.placed, "{what}");
                    assert_eq!(sa, sb, "spheres, {what}");
                    assert_eq!(
                        (a.attempts, a.rounds, &a.beatable, &a.reached),
                        (b.attempts, b.rounds, &b.beatable, &b.reached),
                        "{what}"
                    );
                    assert!(a.all_beatable(), "{what}");
                    // The forward path solved afresh per round and took nothing back there.
                    assert!(b.solves > a.solves, "{what}");
                    assert!(confine || b.stats.removals == 0, "{what}");
                    rooms += 1;
                    fresh += b.solves;
                    retracted += a.stats.retractions;
                }
            }
        }
        assert!(rooms == 64 && fresh > 0 && retracted > 0, "both paths ran");
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
