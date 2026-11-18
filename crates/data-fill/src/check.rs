//! Comparisons for `port-check`: one reachability solve or one sphere list from the legacy side
//! against ours, the first divergence in a form a porter can act on, and seed-shape statistics.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

use crate::reach::Reach;
use crate::world::World;

/// Where each location's rules live in the manifest (for divergence reports).
#[derive(Debug, Clone, Default)]
pub struct Rules {
    by_loc: HashMap<String, Vec<Value>>,
    owner: HashMap<String, String>,
}

impl Rules {
    /// Index a manifest's region checks by location.
    #[must_use]
    pub fn new(m: &Value) -> Self {
        let mut r = Rules::default();
        for reg in m["regions"].as_array().into_iter().flatten() {
            let region = reg["key"].as_str().unwrap_or("").to_owned();
            for c in reg["checks"].as_array().into_iter().flatten() {
                let loc = c["loc"].as_str().unwrap_or("").to_owned();
                r.owner.entry(loc.clone()).or_insert_with(|| region.clone());
                r.by_loc.entry(loc).or_default().push(json!({"region": region, "rule": c["rule"]}));
            }
        }
        r
    }

    /// Every `{region, rule}` that can reach a location.
    #[must_use]
    pub fn of(&self, loc: &str) -> Value {
        Value::Array(self.by_loc.get(loc).cloned().unwrap_or_default())
    }

    /// The first region that lists a location (used to group placements by region).
    #[must_use]
    pub fn owner(&self, loc: &str) -> Option<&str> {
        self.owner.get(loc).map(String::as_str)
    }
}

fn strs(v: &Value) -> BTreeSet<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect()
}

/// One side's answer for one inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SolveView {
    /// Reached locations.
    pub locations: BTreeSet<String>,
    /// Reached regions with time masks, when the side reports them.
    pub regions: Option<BTreeMap<String, u64>>,
    /// Event counts above zero, when the side reports them.
    pub events: Option<BTreeMap<String, i64>>,
}

impl SolveView {
    /// Parse a legacy `solve` answer.
    #[must_use]
    pub fn legacy(v: &Value) -> Self {
        SolveView {
            locations: strs(&v["locations"]),
            regions: v["regions"]
                .as_object()
                .map(|m| m.iter().map(|(k, x)| (k.clone(), x.as_u64().unwrap_or(0))).collect()),
            events: v["events"].as_object().map(|m| {
                m.iter()
                    .map(|(k, x)| (k.clone(), x.as_i64().unwrap_or(0)))
                    .filter(|(_, n)| *n != 0)
                    .collect()
            }),
        }
    }

    /// Our engine's answer, shaped like the legacy one. `scope` limits locations (None: all).
    #[must_use]
    pub fn engine(
        w: &World,
        r: &Reach,
        scope: Option<&BTreeSet<String>>,
        regions: bool,
        events: bool,
    ) -> Self {
        SolveView {
            locations: (0..u32::try_from(w.locations()).unwrap_or(0))
                .filter(|l| r.reached(*l))
                .map(|l| w.loc_key(l))
                .filter(|k| scope.is_none_or(|s| s.contains(*k)))
                .map(str::to_owned)
                .collect(),
            regions: regions.then(|| {
                r.reached_regions().map(|(i, m)| (w.region_key(i).to_owned(), m)).collect()
            }),
            events: events.then(|| {
                r.counters
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| **n > 0)
                    .map(|(c, n)| (w.counter_key(c).to_owned(), *n))
                    .collect()
            }),
        }
    }
}

/// The first difference between two answers, or `None` when they agree.
#[must_use]
pub fn diff_solve(legacy: &SolveView, ours: &SolveView, rules: &Rules) -> Option<Value> {
    if let Some(l) = legacy.locations.symmetric_difference(&ours.locations).next() {
        let side = if legacy.locations.contains(l) { "legacy only" } else { "data-only port only" };
        return Some(
            json!({"kind": "location", "location": l, "reached_by": side, "rules": rules.of(l)}),
        );
    }
    if let (Some(a), Some(b)) = (&legacy.regions, &ours.regions) {
        let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
        if let Some(k) = keys.into_iter().find(|k| a.get(*k) != b.get(*k)) {
            return Some(
                json!({"kind": "region", "region": k, "legacy_mask": a.get(k), "port_mask": b.get(k)}),
            );
        }
    }
    if let (Some(a), Some(b)) = (&legacy.events, &ours.events) {
        let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
        if let Some(k) = keys.into_iter().find(|k| a.get(*k) != b.get(*k)) {
            return Some(
                json!({"kind": "event", "event": k, "legacy": a.get(k), "port": b.get(k)}),
            );
        }
    }
    None
}

/// The first round where two sphere lists differ: `(round, location, side)`. A shorter list
/// diverges at the first round it lacks.
#[must_use]
pub fn diff_spheres(
    legacy: &[Vec<String>],
    ours: &[Vec<String>],
) -> Option<(usize, String, &'static str)> {
    for round in 0..legacy.len().max(ours.len()) {
        let a: BTreeSet<&String> = legacy.get(round).into_iter().flatten().collect();
        let b: BTreeSet<&String> = ours.get(round).into_iter().flatten().collect();
        if let Some(l) = a.symmetric_difference(&b).next() {
            let side = if a.contains(l) { "legacy only" } else { "data-only port only" };
            return Some((round, (*l).clone(), side));
        }
    }
    None
}

/// Seed-shape statistics over several seeds of one side.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    seeds: usize,
    sphere_counts: Vec<usize>,
    prog_per_sphere: Vec<Vec<usize>>,
    key_depth: BTreeMap<String, Vec<usize>>,
    norm_depth: Vec<f64>,
    region_share: BTreeMap<String, f64>,
}

/// One filled seed, as `Shape::add` reads it.
pub struct SeedView<'a> {
    /// Location keys per sphere.
    pub spheres: &'a [Vec<String>],
    /// Location key to item key.
    pub placements: &'a BTreeMap<String, String>,
    /// Item keys that count as progression.
    pub progression: &'a dyn Fn(&str) -> bool,
    /// Item keys that count as key items (progression with at most two copies).
    pub key_item: &'a dyn Fn(&str) -> bool,
    /// Location key to its region.
    pub owner: &'a dyn Fn(&str) -> Option<String>,
}

#[allow(clippy::cast_precision_loss)] // counts here are far below 2^53
fn mean(xs: impl IntoIterator<Item = f64>) -> f64 {
    let (s, n) = xs.into_iter().fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    if n == 0 { 0.0 } else { s / n as f64 }
}

#[allow(clippy::cast_precision_loss)]
fn f(x: usize) -> f64 {
    x as f64
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Relative gap `|a - b| / max(|a|, |b|)`.
fn rel(a: f64, b: f64) -> f64 {
    let m = a.abs().max(b.abs());
    if m == 0.0 { 0.0 } else { (a - b).abs() / m }
}

impl Shape {
    /// Add one seed.
    pub fn add(&mut self, s: &SeedView<'_>) {
        self.seeds += 1;
        self.sphere_counts.push(s.spheres.len());
        let mut per = Vec::new();
        let mut depth: BTreeMap<String, usize> = BTreeMap::new();
        for (round, sphere) in s.spheres.iter().enumerate() {
            let mut n = 0;
            for l in sphere {
                let Some(item) = s.placements.get(l) else { continue };
                if (s.progression)(item) {
                    n += 1;
                }
                if (s.key_item)(item) {
                    depth.entry(item.clone()).or_insert(round + 1);
                }
            }
            per.push(n);
        }
        self.prog_per_sphere.push(per);
        let total = s.spheres.len().max(1);
        self.norm_depth.extend(depth.values().map(|d| f(*d) / f(total)));
        for (k, d) in depth {
            self.key_depth.entry(k).or_default().push(d);
        }
        let prog: Vec<&String> =
            s.placements.iter().filter(|(_, i)| (s.progression)(i)).map(|(l, _)| l).collect();
        let mut by_region: BTreeMap<String, usize> = BTreeMap::new();
        for l in &prog {
            *by_region.entry((s.owner)(l).unwrap_or_else(|| "(none)".into())).or_default() += 1;
        }
        for (r, n) in by_region {
            *self.region_share.entry(r).or_default() += f(n) / f(prog.len().max(1));
        }
    }

    fn share(&self, r: &str) -> f64 {
        self.region_share.get(r).copied().unwrap_or(0.0) / f(self.seeds.max(1))
    }

    fn mean_prog_at(&self, round: usize) -> f64 {
        mean(self.prog_per_sphere.iter().map(|p| f(p.get(round).copied().unwrap_or(0))))
    }
}

/// Side-by-side seed-shape numbers with large gaps flagged (informational, never a failure).
#[must_use]
pub fn compare_shapes(legacy: &Shape, ours: &Shape) -> Value {
    let mut flags: Vec<String> = Vec::new();
    let stat = |s: &Shape| {
        json!({"mean": round3(mean(s.sphere_counts.iter().map(|x| f(*x)))),
               "min": s.sphere_counts.iter().min(), "max": s.sphere_counts.iter().max()})
    };
    let (ls, os) = (
        mean(legacy.sphere_counts.iter().map(|x| f(*x))),
        mean(ours.sphere_counts.iter().map(|x| f(*x))),
    );
    if rel(ls, os) > 0.25 {
        flags.push(format!("sphere count: legacy {ls:.2}, port {os:.2}"));
    }
    let rounds =
        legacy.sphere_counts.iter().chain(&ours.sphere_counts).copied().max().unwrap_or(0).min(15);
    let per: Vec<Value> = (0..rounds)
        .map(|r| json!([r + 1, round3(legacy.mean_prog_at(r)), round3(ours.mean_prog_at(r))]))
        .collect();
    let overall = |s: &Shape| mean(s.prog_per_sphere.iter().flatten().map(|x| f(*x)));
    let (lp, op) = (overall(legacy), overall(ours));
    if rel(lp, op) > 0.25 {
        flags.push(format!("progression per sphere: legacy {lp:.2}, port {op:.2}"));
    }
    let (ld, od) = (mean(legacy.norm_depth.iter().copied()), mean(ours.norm_depth.iter().copied()));
    if (ld - od).abs() > 0.1 {
        flags.push(format!(
            "key item depth (fraction of the seed's spheres): legacy {ld:.2}, port {od:.2}"
        ));
    }
    let mut items: Vec<(String, f64, f64)> = legacy
        .key_depth
        .keys()
        .chain(ours.key_depth.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|k| {
            let m = |s: &Shape| mean(s.key_depth.get(k).into_iter().flatten().map(|x| f(*x)));
            (k.clone(), m(legacy), m(ours))
        })
        .collect();
    items.sort_by(|a, b| (b.1 - b.2).abs().total_cmp(&(a.1 - a.2).abs()).then(a.0.cmp(&b.0)));
    for (k, a, b) in &items {
        if (a - b).abs() >= 2.0 {
            flags.push(format!("key item {k}: mean sphere legacy {a:.1}, port {b:.1}"));
        }
    }
    let mut regions: Vec<(String, f64, f64)> = legacy
        .region_share
        .keys()
        .chain(ours.region_share.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|r| (r.clone(), legacy.share(r), ours.share(r)))
        .collect();
    regions.sort_by(|a, b| b.1.max(b.2).total_cmp(&a.1.max(a.2)).then(a.0.cmp(&b.0)));
    for (r, a, b) in &regions {
        if (a - b).abs() >= 0.02 && rel(*a, *b) >= 0.5 {
            flags.push(format!(
                "region {r}: progression share legacy {:.1}%, port {:.1}%",
                a * 100.0,
                b * 100.0
            ));
        }
    }
    json!({
        "seeds": {"legacy": legacy.seeds, "port": ours.seeds},
        "sphere_count": {"legacy": stat(legacy), "port": stat(ours)},
        "progression_per_sphere": {"overall": [round3(lp), round3(op)],
                                   "by_sphere [sphere, legacy, port]": per},
        "key_item_depth": {"mean_fraction_of_spheres": [round3(ld), round3(od)],
                           "largest_gaps [item, legacy sphere, port sphere]":
                               items.iter().take(15).map(|(k, a, b)| json!([k, round3(*a), round3(*b)])).collect::<Vec<_>>()},
        "region_progression_share": {"top [region, legacy, port]":
            regions.iter().take(15).map(|(r, a, b)| json!([r, round3(*a), round3(*b)])).collect::<Vec<_>>()},
        "flags": flags,
    })
}

/// FNV-1a 64 of some bytes (a stable content hash for determinism reports).
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}
