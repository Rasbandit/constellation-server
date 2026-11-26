//! Synthetic worlds for scaling measurements (`examples/synth_room.rs`): per player, a manifest
//! and an instance drawn from a seed, compiled through the real path ([`crate::section::compile`]
//! and [`crate::World::from_package`], via [`Compiled::new`]).
//!
//! The start region opens onto `layers` layers of regions; every region of a layer has
//! `branching` gated exits in from regions of the layer before. Gates mix `has` (with and
//! without counts), `any`, `all` with a def, and counters lit by events in the first layer.
//! Every gate can be met with every item held, so the goal (reach every region of the last
//! layer) holds with the whole pool.

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
    /// Clocks restrict time: the later slice is owned only with the first progression item.
    pub clocks: bool,
}

impl Default for Shape {
    fn default() -> Self {
        Shape { checks: 1000, layers: 6, branching: 2, progression: 0.19, clocks: false }
    }
}

/// One player's world and instance. The same `seed` and `player` give the same bytes.
#[must_use]
pub fn world(shape: &Shape, seed: u64, player: u64) -> (Value, Instance) {
    let mut rng = SpecRng::from_u64_tagged(seed, b"synthrm\0", player);
    let mut pick = |n: usize| usize::try_from(rng.below(n as u64).unwrap_or(0)).unwrap_or(0);
    let checks = shape.checks.max(1);
    let regions = (checks / 8).max(2);
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
    let defs = (kinds / 8).max(1);
    let def_rules: serde_json::Map<String, Value> = (0..defs)
        .map(|d| {
            let rule = json!({"any": [has(&mut pick), {"all": [has(&mut pick), has(&mut pick)]}]});
            (format!("d{d}"), rule)
        })
        .collect();
    // Counters, each lit by two events in the first layer, read from the third layer on.
    let counters = (regions / 16).max(1);
    let mut events: Vec<Vec<Value>> = vec![Vec::new(); regions];
    for c in 0..counters {
        for _ in 0..2 {
            let r = in_layer[1][pick(in_layer[1].len())];
            events[r].push(json!({"event": format!("c{c}"), "rule": has(&mut pick)}));
        }
    }
    let gate = |d: usize, pick: &mut dyn FnMut(usize) -> usize| match pick(20) {
        0..=7 => has(pick),
        8..=11 => json!({"any": [has(pick), has(pick)]}),
        12..=15 => json!({"all": [has(pick), {"ref": format!("d{}", pick(defs))}]}),
        16..=17 if d >= 3 => json!({"ge": [{"counter": format!("c{}", pick(counters))}, 2]}),
        _ => has(pick),
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
    for l in 0..checks {
        let r = l % regions;
        let rule = match pick(8) {
            0 if can_stay(r) => json!({"x.time_any": 2}),
            1 | 2 => has(&mut pick),
            _ => json!({"true": {}}),
        };
        region_checks[r].push(json!({"loc": format!("l{l}"), "rule": rule}));
    }
    let regions_json: Vec<Value> = (0..regions)
        .map(|r| {
            let stays = if can_stay(r) && r % 8 == 7 {
                json!([{"slice": 1, "rule": has(&mut pick)}])
            } else {
                json!([])
            };
            json!({"key": format!("r{r}"), "can_stay": can_stay(r),
                   "checks": region_checks[r], "exits": exits[r], "events": events[r],
                   "stays": stays})
        })
        .collect();
    let mut time = json!({"slices": 2, "start_mask": 1});
    if shape.clocks {
        time["owned"] = json!({"when": {"true": {}}, "none_owned_mask": 1, "half_days": [
            {"first": 0, "last": 0, "rule": {"true": {}}},
            {"first": 1, "last": 1, "rule": {"has": p(0)}}]});
    }
    let goal: Vec<Value> =
        in_layer[layers].iter().map(|r| json!({"reach": format!("region:r{r}")})).collect();
    let mut items: Vec<Value> = (0..kinds).map(|k| json!({"key": p(k), "id": k + 1})).collect();
    items.push(json!({"key": "useful", "id": kinds + 1}));
    items.push(json!({"key": "filler", "id": kinds + 2}));
    let manifest = json!({
        "options": [],
        "items": items,
        "locations": (0..checks).map(|l| json!({"key": format!("l{l}"), "id": l + 1}))
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
