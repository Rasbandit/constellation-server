//! Fairness statistics over many fills: item x location frequencies, spread (entropy), a
//! two-sample chi-square against a baseline, seed independence and coverage gaps.
//!
//! Used by `port-check` (port sanity over one instance) and `fill-fairness` (engine fairness
//! against an unbiased baseline on a small world).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::room::{RoomFilled, fill_room_spheres};
use crate::spheres::RoomSpheres;

#[allow(clippy::cast_precision_loss)] // counts here are far below 2^53
fn f(x: u64) -> f64 {
    x as f64
}

/// How often each item lands at each location over a number of samples.
#[derive(Debug, Clone, Default)]
pub struct Freq {
    /// Samples added.
    pub samples: u64,
    /// `(item, location)` to count.
    pub counts: BTreeMap<(u16, u32), u64>,
}

impl Freq {
    /// Add one placement (`placed[location] = item`), counting only `keep(item)`.
    pub fn add(&mut self, placed: &[Option<u16>], keep: impl Fn(u16) -> bool) {
        self.samples += 1;
        for (l, i) in placed.iter().enumerate() {
            if let Some(i) = i
                && keep(*i)
            {
                *self.counts.entry((*i, u32::try_from(l).unwrap_or(u32::MAX))).or_default() += 1;
            }
        }
    }

    /// Share of samples with `item` at `loc`.
    #[must_use]
    pub fn p(&self, item: u16, loc: u32) -> f64 {
        f(self.counts.get(&(item, loc)).copied().unwrap_or(0)) / f(self.samples.max(1))
    }

    /// Items seen.
    #[must_use]
    pub fn items(&self) -> BTreeSet<u16> {
        self.counts.keys().map(|(i, _)| *i).collect()
    }

    /// Location counts for one item.
    #[must_use]
    pub fn row(&self, item: u16) -> BTreeMap<u32, u64> {
        self.counts.range((item, 0)..=(item, u32::MAX)).map(|((_, l), n)| (*l, *n)).collect()
    }

    /// Distinct locations and Shannon entropy (bits) of one item's location distribution.
    #[must_use]
    pub fn spread(&self, item: u16) -> (usize, f64) {
        let row = self.row(item);
        let total = f(row.values().sum());
        let h =
            row.values().map(|n| f(*n) / total).filter(|q| *q > 0.0).map(|q| -q * q.log2()).sum();
        (row.len(), h)
    }
}

/// Expected distinct locations when `draws` placements fall uniformly over `allowed` locations.
#[must_use]
pub fn uniform_distinct(allowed: u64, draws: u64) -> f64 {
    if allowed == 0 {
        return 0.0;
    }
    let a = f(allowed);
    a * (1.0 - (1.0 - 1.0 / a).powf(f(draws)))
}

/// Upper tail `P(X >= x)` for chi-square with `k` degrees of freedom (Wilson-Hilferty
/// approximation: good to about two digits for `k >= 3`, which is all a report needs).
#[must_use]
pub fn chi2_upper(x: f64, k: f64) -> f64 {
    if k <= 0.0 {
        return 1.0;
    }
    let z = ((x / k).cbrt() - (1.0 - 2.0 / (9.0 * k))) / (2.0 / (9.0 * k)).sqrt();
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

/// Complementary error function (Numerical Recipes `erfcc`, relative error below 1.2e-7).
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87
                                        + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

/// Two-sample chi-square of `a` against `b`, per item over the locations either side used.
/// Returns `(chi2, degrees of freedom, upper-tail p)`.
#[must_use]
pub fn chi2(a: &Freq, b: &Freq) -> (f64, f64, f64) {
    let (na, nb) = (f(a.samples.max(1)), f(b.samples.max(1)));
    let (ka, kb) = ((nb / na).sqrt(), (na / nb).sqrt());
    let (mut x, mut dof) = (0.0, 0.0);
    for item in a.items().union(&b.items()) {
        let (ra, rb) = (a.row(*item), b.row(*item));
        let locs: BTreeSet<&u32> = ra.keys().chain(rb.keys()).collect();
        for l in &locs {
            let (oa, ob) =
                (f(ra.get(*l).copied().unwrap_or(0)), f(rb.get(*l).copied().unwrap_or(0)));
            if oa + ob > 0.0 {
                x += (ka * oa - kb * ob).powi(2) / (oa + ob);
            }
        }
        dof += f(locs.len() as u64).max(1.0) - 1.0;
    }
    (x, dof, chi2_upper(x, dof))
}

/// The largest `|p_a - p_b|` over all item/location pairs: `(deviation, item, location)`.
#[must_use]
pub fn max_deviation(a: &Freq, b: &Freq) -> (f64, u16, u32) {
    let keys: BTreeSet<&(u16, u32)> = a.counts.keys().chain(b.counts.keys()).collect();
    keys.into_iter()
        .map(|(i, l)| ((a.p(*i, *l) - b.p(*i, *l)).abs(), *i, *l))
        .fold((0.0, 0, 0), |best, x| if x.0 > best.0 { x } else { best })
}

/// Pairs the baseline reaches often (expected count in `a` at least `min_expected`) that `a`
/// almost never produces (below a tenth of that): `(item, location, p_a, p_baseline)`.
#[must_use]
pub fn coverage_gaps(a: &Freq, baseline: &Freq, min_expected: f64) -> Vec<(u16, u32, f64, f64)> {
    baseline
        .counts
        .keys()
        .filter_map(|(i, l)| {
            let (pa, pb) = (a.p(*i, *l), baseline.p(*i, *l));
            let expected = pb * f(a.samples);
            (expected >= min_expected && pa * f(a.samples) < expected / 10.0)
                .then_some((*i, *l, pa, pb))
        })
        .collect()
}

/// Share of locations holding the same item in two placements (over locations either fills).
#[must_use]
pub fn match_rate(x: &[Option<u16>], y: &[Option<u16>]) -> f64 {
    let (mut same, mut n) = (0u64, 0u64);
    for (a, b) in x.iter().zip(y) {
        if a.is_some() || b.is_some() {
            n += 1;
            same += u64::from(a == b);
        }
    }
    f(same) / f(n.max(1))
}

/// Seed independence: match rate of adjacent seeds against seeds half the run apart, with a
/// z-score of the difference (above 3 means adjacent seeds look alike).
#[must_use]
pub fn independence(fills: &[Vec<Option<u16>>]) -> Value {
    let n = fills.len();
    if n < 4 {
        return json!({"skipped": "needs at least 4 seeds"});
    }
    let half = n / 2;
    let adj: Vec<f64> = (0..n - 1).map(|s| match_rate(&fills[s], &fills[s + 1])).collect();
    let far: Vec<f64> = (0..n - half).map(|s| match_rate(&fills[s], &fills[s + half])).collect();
    let stat = |v: &[f64]| {
        let m = v.iter().sum::<f64>() / f(v.len() as u64);
        let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>()
            / f(v.len().saturating_sub(1).max(1) as u64);
        (m, var / f(v.len() as u64))
    };
    let ((ma, va), (mf, vf)) = (stat(&adj), stat(&far));
    let se = (va + vf).sqrt();
    let z = if se > 0.0 { (ma - mf) / se } else { 0.0 };
    json!({"adjacent_match": ma, "distant_match": mf, "z": z, "flag": z > 3.0})
}

/// Counts of each value, as `[[value, count], ...]` in value order.
#[must_use]
pub fn histogram(values: impl IntoIterator<Item = usize>) -> Value {
    let mut h: BTreeMap<usize, u64> = BTreeMap::new();
    for v in values {
        *h.entry(v).or_default() += 1;
    }
    Value::Array(h.into_iter().map(|(v, n)| json!([v, n])).collect())
}

/// A spec RNG for seed `seed` in stream `tag` (so fills, baselines and samples never share draws).
#[must_use]
pub fn seeded(seed: u64, tag: &[u8; 8]) -> spec_rng::SpecRng {
    spec_rng::SpecRng::from_u64_tagged(seed, tag, 0)
}

/// Fills and baseline samples for one world instance.
#[derive(Debug, Clone, Default)]
pub struct Sampled {
    /// Our fill's frequencies.
    pub fill: Freq,
    /// The unbiased baseline's frequencies (empty when not requested).
    pub baseline: Freq,
    /// Baseline candidates kept / drawn.
    pub accepted: u64,
    /// Baseline candidates drawn.
    pub attempts: u64,
    /// Every fill, in seed order (for the independence check).
    pub fills: Vec<Vec<Option<u16>>>,
    /// Fills that failed.
    pub failed: u64,
}

/// Run `seeds` fills (fill seeds `1..=seeds`) of the product's fill, a room of this one player
/// ([`fill_room_spheres`]), and draw up to `baseline` accepted uniform placements: the pool
/// shuffled onto the free locations, kept only if the placement rules hold, the goal is
/// reachable and every progression item can be collected. Gives up on the baseline after
/// `max_attempts` candidates.
#[must_use]
pub fn sample(
    w: &crate::World,
    plan: &crate::Plan,
    pool: &[u16],
    progression: &[bool],
    seeds: u64,
    baseline: u64,
    max_attempts: u64,
) -> Sampled {
    let mut out = Sampled::default();
    let seat = [crate::Seat { w, plan, progression }];
    for s in 1..=seeds {
        match one_fill(&seat, s) {
            Ok((filled, _)) => {
                let placed: Vec<Option<u16>> =
                    filled.placed[0].iter().map(|g| g.map(|g| g.item)).collect();
                out.fill.add(&placed, |_| true);
                out.fills.push(placed);
            }
            Err(_) => out.failed += 1,
        }
    }
    let base = plan.locked_placed(w);
    let free: Vec<u32> =
        plan.included.iter().copied().filter(|l| base[*l as usize].is_none()).collect();
    let mut rng = seeded(0, b"baseline");
    let mut items = pool.to_vec();
    while out.accepted < baseline && out.attempts < max_attempts {
        out.attempts += 1;
        rng.shuffle(&mut items);
        let mut placed = base.clone();
        let mut ok = items.len() == free.len();
        for (l, i) in free.iter().zip(&items) {
            ok &= plan.permits(*i, *l, progression[*i as usize]);
            placed[*l as usize] = Some(*i);
        }
        if !ok {
            continue;
        }
        let s = crate::spheres::spheres(w, plan, &placed, progression);
        let stranded = s
            .unreachable
            .iter()
            .any(|l| placed[*l as usize].is_some_and(|i| progression[i as usize]));
        if s.beatable && !stranded {
            out.accepted += 1;
            out.baseline.add(&placed, |_| true);
        }
    }
    out
}

/// The fairness report for a [`Sampled`] run.
#[must_use]
pub fn report(w: &crate::World, s: &Sampled) -> Value {
    report_named(&|i| w.item_key(i).to_owned(), &|l| w.loc_key(l).to_owned(), s)
}

/// [`report`] with the names of items and locations given by the caller (a room names them
/// with the player).
#[must_use]
pub fn report_named(
    name: &dyn Fn(u16) -> String,
    loc: &dyn Fn(u32) -> String,
    s: &Sampled,
) -> Value {
    let spread: Vec<Value> = s
        .fill
        .items()
        .into_iter()
        .map(|i| {
            let (d, h) = s.fill.spread(i);
            let (bd, bh) = s.baseline.spread(i);
            json!({"item": name(i), "fill_locations": d, "fill_entropy_bits": h,
                   "baseline_locations": bd, "baseline_entropy_bits": bh})
        })
        .collect();
    let mut v = json!({
        "fills": s.fill.samples, "fill_failures": s.failed,
        "independence": independence(&s.fills),
        "spread": spread,
    });
    if s.baseline.samples > 0 {
        let (x, dof, p) = chi2(&s.fill, &s.baseline);
        let (dev, di, dl) = max_deviation(&s.fill, &s.baseline);
        let gaps: Vec<Value> = coverage_gaps(&s.fill, &s.baseline, 10.0)
            .into_iter()
            .map(|(i, l, pa, pb)| json!({"item": name(i), "location": loc(l), "fill_p": pa, "baseline_p": pb}))
            .collect();
        v["baseline"] = json!({
            "accepted": s.accepted, "attempts": s.attempts,
            "acceptance": f(s.accepted) / f(s.attempts.max(1)),
            "chi2": x, "dof": dof, "p_value": p,
            "max_deviation": {"value": dev, "item": name(di), "location": loc(dl),
                              "fill_p": s.fill.p(di, dl), "baseline_p": s.baseline.p(di, dl)},
            "coverage_gaps": gaps,
        });
    } else {
        v["baseline"] = json!({"accepted": 0, "attempts": s.attempts,
                               "note": "no beatable uniform placement found; bias not measured"});
    }
    v
}

/// A room's placements as one flat placement, so the statistics above (one item per location
/// index) serve a room unchanged: location `l` of player `p` is `loc_off[p] + l`, item `i` of
/// owner `o` is `item_off[o] + i`.
#[derive(Debug, Clone)]
pub struct Flat {
    item_off: Vec<usize>,
    loc_off: Vec<usize>,
    item_names: Vec<String>,
    loc_names: Vec<String>,
}

impl Flat {
    /// The flat numbering of a room.
    ///
    /// # Errors
    /// When the room has more items than a flat item number holds (16 bits).
    pub fn new(seats: &[crate::Seat<'_>]) -> Result<Self, String> {
        let (mut item_off, mut loc_off) = (Vec::new(), Vec::new());
        let (mut item_names, mut loc_names) = (Vec::new(), Vec::new());
        for (p, s) in seats.iter().enumerate() {
            item_off.push(item_names.len());
            loc_off.push(loc_names.len());
            let p = p + 1;
            item_names.extend(
                (0..s.w.items())
                    .map(|i| format!("p{p}:{}", s.w.item_key(u16::try_from(i).unwrap_or(0)))),
            );
            loc_names.extend(
                (0..s.w.locations())
                    .map(|l| format!("p{p}:{}", s.w.loc_key(u32::try_from(l).unwrap_or(0)))),
            );
        }
        if item_names.len() > usize::from(u16::MAX) {
            return Err(format!(
                "{} items in the room: more than a flat item number holds",
                item_names.len()
            ));
        }
        Ok(Flat { item_off, loc_off, item_names, loc_names })
    }

    /// The room's placements, flat.
    #[must_use]
    pub fn placed(&self, placed: &[Vec<Option<crate::Given>>]) -> Vec<Option<u16>> {
        let mut out = vec![None; self.loc_names.len()];
        for (p, row) in placed.iter().enumerate() {
            for (l, g) in row.iter().enumerate() {
                if let Some(g) = g {
                    let item = self.item_off[usize::from(g.to)] + usize::from(g.item);
                    out[self.loc_off[p] + l] = u16::try_from(item).ok();
                }
            }
        }
        out
    }

    /// Name of a flat item (`p<player>:<key>`).
    #[must_use]
    pub fn item(&self, i: u16) -> String {
        self.item_names[usize::from(i)].clone()
    }

    /// Name of a flat location (`p<player>:<key>`).
    #[must_use]
    pub fn loc(&self, l: u32) -> String {
        self.loc_names[l as usize].clone()
    }
}

/// [`sample`] for a room: `seeds` room fills, and up to `baseline` accepted uniform placements
/// of every player's pool over every player's free locations (kept only if the placement
/// rules hold, every goal is reachable and every progression item can be collected).
///
/// # Errors
/// [`crate::FillError::Instance`] when the seats do not fit the room.
pub fn sample_room(
    seats: &[crate::Seat<'_>],
    flat: &Flat,
    seeds: u64,
    baseline: u64,
    max_attempts: u64,
) -> Result<Sampled, crate::FillError> {
    use crate::room::{Given, check_placed, locked_placed, permits};
    let mut out = Sampled::default();
    crate::room::check(seats)?;
    check_placed(seats, &locked_placed(seats))?;
    for s in 1..=seeds {
        match one_fill(seats, s).map(|x| x.0) {
            Ok(filled) => {
                let f = flat.placed(&filled.placed);
                out.fill.add(&f, |_| true);
                out.fills.push(f);
            }
            Err(_) => out.failed += 1,
        }
    }
    let base = locked_placed(seats);
    let mut free: Vec<(u16, u32)> = Vec::new();
    let mut items: Vec<Given> = Vec::new();
    for (p, s) in seats.iter().enumerate() {
        let p = u16::try_from(p).unwrap_or(0);
        free.extend(
            s.plan
                .included
                .iter()
                .filter(|l| base[usize::from(p)][**l as usize].is_none())
                .map(|l| (p, *l)),
        );
        let pool = s.plan.progression.iter().chain(&s.plan.rest);
        items.extend(pool.map(|i| Given { to: p, item: *i }));
    }
    let mut rng = seeded(0, b"baseline");
    while out.accepted < baseline && out.attempts < max_attempts {
        out.attempts += 1;
        rng.shuffle(&mut items);
        let mut placed = base.clone();
        let mut ok = items.len() == free.len();
        for ((p, l), g) in free.iter().zip(&items) {
            ok &= permits(seats, *g, *p, *l);
            placed[usize::from(*p)][*l as usize] = Some(*g);
        }
        if !ok {
            continue;
        }
        let s = crate::spheres::room_spheres(seats, &placed)?;
        let stranded = s.unreachable.iter().any(|(p, l)| {
            placed[usize::from(*p)][*l as usize]
                .is_some_and(|g| crate::room::is_progression(seats, g))
        });
        if s.all_beatable() && !stranded {
            out.accepted += 1;
            out.baseline.add(&flat.placed(&placed), |_| true);
        }
    }
    Ok(out)
}

/// The stream of the fairness runs' fills.
pub const FILLS: &[u8; 8] = b"fill\0\0\0\0";

/// Fill seed `s` of stream [`FILLS`].
fn one_fill(
    seats: &[crate::Seat<'_>],
    s: u64,
) -> Result<(RoomFilled, RoomSpheres), crate::FillError> {
    fill_room_spheres(seats, &mut seeded(s, FILLS), rando_limits::FILL_MAX_ATTEMPTS)
}

/// What the fill's seeds look like over a room. Sphere 0 is what is reachable from the start; "progression" is every progression item at a
/// filled location.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    /// Seeds filled, and seeds that failed.
    pub seeds: u64,
    /// Fills that failed (every attempt stuck). Every number below is over the seeds filled.
    pub failed: u64,
    /// Items moved in place by the fill's repair, summed.
    pub swaps: u64,
    /// Rule evaluations, summed.
    pub evals: u64,
    /// Spheres per seed.
    pub spheres: Vec<usize>,
    /// Progression items by the sphere they lie in.
    pub depth: Vec<u64>,
    /// Filled locations by sphere.
    pub open: Vec<u64>,
    /// Sum over progression items of sphere / last sphere of the seed, and their number.
    pub relative: (f64, u64),
    /// Seeds with progression at each location (item 0 of a [`Freq`], flat locations).
    pub at: Freq,
    /// Per seed and player: the share of the rounds up to their last location in which no
    /// location of theirs opened (they wait for others).
    pub stuck: Vec<f64>,
    /// Per seed looked at: spheres that hold an item no goal can be reached without.
    pub required: Vec<usize>,
    /// Fill time per filled seed, ms (not comparable across machines).
    pub ms: Vec<f64>,
}

/// [`Shape`] of the fill over fill seeds `1..=seeds` of stream [`FILLS`]. For the first `required` seeds each
/// progression item is taken out in turn and the room solved again, to count the spheres that
/// hold a required item (one solve per item: keep it small).
///
/// # Errors
/// [`crate::FillError::Instance`] when the seats do not fit the room.
pub fn shape(
    seats: &[crate::Seat<'_>],
    seeds: u64,
    required: u64,
) -> Result<Shape, crate::FillError> {
    crate::room::check(seats)?;
    let mut out = Shape { seeds, ..Shape::default() };
    let progression = |g: &crate::Given| crate::room::is_progression(seats, *g);
    let offsets: Vec<u32> = seats
        .iter()
        .scan(0u32, |at, s| {
            let here = *at;
            *at += u32::try_from(s.w.locations()).unwrap_or(0);
            Some(here)
        })
        .collect();
    for s in 1..=seeds {
        let t = std::time::Instant::now();
        let (filled, sp) = match one_fill(seats, s) {
            Ok(x) => x,
            Err(_) => {
                out.failed += 1;
                continue;
            }
        };
        out.ms.push(t.elapsed().as_secs_f64() * 1e3);
        out.swaps += u64::from(filled.swaps);
        out.evals += filled.stats.evals;
        out.at.samples += 1;
        let rounds = sp.spheres.len();
        out.spheres.push(rounds);
        if out.depth.len() < rounds {
            out.depth.resize(rounds, 0);
            out.open.resize(rounds, 0);
        }
        let mut last = vec![0usize; seats.len()];
        let mut active = vec![0usize; seats.len()];
        let mut needed = vec![false; rounds];
        for (k, sphere) in sp.spheres.iter().enumerate() {
            let mut here = vec![false; seats.len()];
            for (p, l) in sphere {
                let (pi, li) = (usize::from(*p), *l as usize);
                out.open[k] += 1;
                here[pi] = true;
                if !filled.placed[pi][li].is_some_and(|g| progression(&g)) {
                    continue;
                }
                out.depth[k] += 1;
                out.relative.0 += f(k as u64) / f(rounds.saturating_sub(1).max(1) as u64);
                out.relative.1 += 1;
                *out.at.counts.entry((0, offsets[pi] + *l)).or_default() += 1;
                if s <= required && !needed[k] {
                    let mut without = filled.placed.clone();
                    without[pi][li] = None;
                    let goals = crate::room::validate_room(seats, &without)?.0;
                    needed[k] = !goals.iter().all(|b| *b);
                    // Taking the item out must not be mistaken for a goal that never held.
                    needed[k] &= filled.beatable.iter().all(|b| *b);
                }
            }
            for (p, opened) in here.iter().enumerate() {
                if *opened {
                    last[p] = k + 1;
                    active[p] += 1;
                }
            }
        }
        out.stuck.extend(
            last.iter()
                .zip(&active)
                .filter(|(n, _)| **n > 0)
                .map(|(n, a)| 1.0 - f(*a as u64) / f(*n as u64)),
        );
        if s <= required {
            out.required.push(needed.iter().filter(|x| **x).count());
        }
    }
    Ok(out)
}

fn mean(v: impl IntoIterator<Item = f64>) -> Option<f64> {
    let (sum, n) = v.into_iter().fold((0.0, 0u64), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| sum / f(n))
}

/// The value `share` of the way up the sorted values (0.5: the median).
fn quantile(v: &[f64], share: f64) -> Option<f64> {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let at = (0..v.len()).find(|i| f(*i as u64) >= f(v.len().saturating_sub(1) as u64) * share);
    at.and_then(|i| v.get(i).copied())
}

/// A [`Shape`] as a report.
#[must_use]
pub fn shape_report(s: &Shape) -> Value {
    let share =
        |v: &[u64], upto: usize| f(v.iter().take(upto).sum()) / f(v.iter().sum::<u64>().max(1));
    let filled = s.seeds - s.failed;
    let total = f(s.at.counts.values().sum());
    let entropy: f64 =
        s.at.counts
            .values()
            .map(|n| f(*n) / total.max(1.0))
            .map(|p| if p > 0.0 { -p * p.log2() } else { 0.0 })
            .sum();
    let places = f(s.open.iter().sum::<u64>() / filled.max(1)).max(2.0);
    json!({
        "seeds": s.seeds, "failed": s.failed, "swaps": s.swaps,
        "swaps_per_seed": f(s.swaps) / f(filled.max(1)),
        "rule_evals_per_seed": f(s.evals) / f(filled.max(1)),
        "spheres_mean": mean(s.spheres.iter().map(|n| f(*n as u64))),
        "progression_share_sphere_0": share(&s.depth, 1),
        "progression_share_first_two_spheres": share(&s.depth, 2),
        "location_share_first_two_spheres": share(&s.open, 2),
        "progression_by_sphere": s.depth, "locations_by_sphere": s.open,
        "progression_relative_depth_mean": s.relative.0 / f(s.relative.1.max(1)),
        "progression_location_entropy_normalized": entropy / places.log2(),
        "stuck_share_mean": mean(s.stuck.iter().copied()),
        "stuck_share_worst_5_percent": quantile(&s.stuck, 0.95),
        "required_spheres_mean": mean(s.required.iter().map(|n| f(*n as u64))),
        "required_spheres_seeds": s.required.len(),
        "fill_ms_median": quantile(&s.ms, 0.5), "fill_ms_worst_1_percent": quantile(&s.ms, 0.99),
    })
}
