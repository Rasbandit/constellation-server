//! `port-check`: prove a data-only port of a legacy Python world equivalent to the original.
//! See `docs/port-check.md` for the adapter interface and how to run it.
//!
//! Checks: (1) reachability differential over random inventories, (2) legacy seeds replayed
//! sphere by sphere in our engine, (3) our seeds judged by the legacy side, (4) determinism of
//! our output files; plus informational seed-shape statistics. Writes one JSON report.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio};
use std::time::Instant;

use data_fill::check::{
    Rules, SeedView, Shape, SolveView, compare_shapes, diff_solve, diff_spheres, fnv1a,
};
use data_fill::fairness::{histogram, sample, uniform_distinct};
use data_fill::spheres::{Spheres, spheres};
use data_fill::{Instance, Plan, Seat, Stats, World, fill_room_spheres};

#[path = "port-check/room.rs"]
mod room;
use serde_json::{Value, json};
use spec_rng::SpecRng;

type R<T> = Result<T, String>;
/// One sphere replay: passed, the divergence if not, the legacy spheres, legacy beatable.
type SphereCheck = (bool, Option<Value>, Vec<Vec<String>>, bool);
/// A per-key test over manifest keys, as the shape report reads them.
type KeyFn<'a, T> = Box<dyn Fn(&str) -> T + 'a>;

/// Output format version; part of the determinism contract with the crate version.
const FORMAT: &str = "port-check/1";

const USAGE: &str = "usage: port-check --manifest <manifest.json> --legacy '<command>' --out <dir>
    [--seeds K] [--first-seed S] [--ours K] [--inventories N] [--fill-seed-base B] [--sanity-seeds N]";

struct Legacy {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seconds: f64,
}

impl Legacy {
    fn spawn(cmd: &str) -> R<Self> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("legacy command: {e}"))?;
        let stdin = child.stdin.take().ok_or("legacy stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("legacy stdout")?);
        Ok(Legacy { child, stdin, stdout, seconds: 0.0 })
    }

    fn call(&mut self, req: &Value) -> R<Value> {
        let t = Instant::now();
        writeln!(self.stdin, "{req}").map_err(|e| format!("legacy write: {e}"))?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        self.stdout.read_line(&mut line).map_err(|e| format!("legacy read: {e}"))?;
        self.seconds += t.elapsed().as_secs_f64();
        if line.is_empty() {
            return Err(format!("legacy side exited during {}", req["op"]));
        }
        let v: Value = serde_json::from_str(&line).map_err(|e| format!("legacy reply: {e}"))?;
        if v["ok"] == json!(true) {
            Ok(v["result"].clone())
        } else {
            Err(format!("legacy {} failed: {}", req["op"], v["error"]))
        }
    }
}

impl Drop for Legacy {
    fn drop(&mut self) {
        // Closing stdin ends the driver loop; wait so no orphan keeps memory.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Args {
    manifest: PathBuf,
    legacy: String,
    out: PathBuf,
    seeds: u64,
    first_seed: u64,
    ours: u64,
    inventories: usize,
    fill_seed_base: u64,
    sanity_seeds: u64,
}

fn parse(args: &[String]) -> R<Args> {
    let mut m: HashMap<&str, &str> = HashMap::new();
    let mut it = args.iter().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().ok_or_else(|| format!("{k}: missing value\n{USAGE}"))?;
        m.insert(k.as_str(), v.as_str());
    }
    let num = |k: &str, d: u64| -> R<u64> {
        m.get(k).map_or(Ok(d), |v| v.parse().map_err(|e| format!("{k}: {e}")))
    };
    let req = |k: &str| m.get(k).copied().ok_or_else(|| format!("{k} is required\n{USAGE}"));
    Ok(Args {
        manifest: req("--manifest")?.into(),
        legacy: req("--legacy")?.to_owned(),
        out: req("--out")?.into(),
        seeds: num("--seeds", 20)?,
        first_seed: num("--first-seed", 1)?,
        ours: num("--ours", 20)?,
        inventories: usize::try_from(num("--inventories", 200)?).map_err(|e| e.to_string())?,
        fill_seed_base: num("--fill-seed-base", 1_000_000)?,
        sanity_seeds: num("--sanity-seeds", 200)?,
    })
}

fn read_json(p: &Path) -> R<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?)
        .map_err(|e| format!("{}: {e}", p.display()))
}

fn write(p: &Path, bytes: &[u8]) -> R<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}

/// Pass/fail tally for one check, keeping the first failure.
#[derive(Default)]
struct Tally {
    total: u64,
    passed: u64,
    first: Option<Value>,
}

impl Tally {
    fn record(&mut self, ok: bool, detail: impl FnOnce() -> Value) {
        self.total += 1;
        if ok {
            self.passed += 1;
        } else if self.first.is_none() {
            self.first = Some(detail());
        }
    }
    fn json(&self, extra: Value) -> Value {
        let mut v = json!({"pass": self.total > 0 && self.passed == self.total,
                           "passed": self.passed, "total": self.total, "first_divergence": self.first});
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        v
    }
}

/// One seed's compiled instance.
struct Seed<'w> {
    seed: u64,
    inst: Instance,
    w: &'w World,
    plan: Plan,
    progression: Vec<bool>,
}

impl Seed<'_> {
    fn prog_key(&self, key: &str) -> bool {
        self.inst.pool_class.get(key).is_some_and(|c| c == "progression")
    }

    fn placed(&self, p: &BTreeMap<String, String>) -> R<Vec<Option<u16>>> {
        let mut v = vec![None; self.w.locations()];
        for (l, i) in p {
            v[self.w.loc(l)? as usize] = Some(self.w.item(i)?);
        }
        Ok(v)
    }

    fn keys(&self, s: &[Vec<u32>]) -> Vec<Vec<String>> {
        s.iter().map(|x| x.iter().map(|l| self.w.loc_key(*l).to_owned()).collect()).collect()
    }

    /// Progression item keys with multiplicity: the pool plus locked items.
    fn progression_items(&self) -> Vec<String> {
        let mut v: Vec<String> =
            self.inst.pool.iter().filter(|k| self.prog_key(k)).cloned().collect();
        let mut locked: Vec<(&String, &String)> = self.inst.locked.iter().collect();
        locked.sort();
        v.extend(locked.into_iter().filter(|(_, i)| self.prog_key(i)).map(|(_, i)| i.clone()));
        v
    }
}

fn str_map(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .map(|m| m.iter().filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_owned()))).collect())
        .unwrap_or_default()
}

fn sphere_lists(v: &Value) -> Vec<Vec<String>> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            s.as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .collect()
}

pub(crate) struct Ctx {
    rules: Rules,
    scope_all: bool,
    has_regions: bool,
    has_events: bool,
    engine_seconds: f64,
    /// Work counters over every fill of ours (the complexity contract).
    work: (Stats, u64, u64),
}

impl Ctx {
    /// Compare one legacy sphere answer with ours for the same placements.
    fn spheres_check(
        &mut self,
        s: &Seed,
        placements: &BTreeMap<String, String>,
        legacy: &Value,
        known: Option<&Spheres>,
    ) -> R<SphereCheck> {
        let placed = s.placed(placements)?;
        let t = Instant::now();
        let fresh;
        let ours = match known {
            Some(k) => k,
            None => {
                fresh = spheres(s.w, &s.plan, &placed, &s.progression);
                &fresh
            }
        };
        self.engine_seconds += t.elapsed().as_secs_f64();
        let ours_keys = s.keys(&ours.spheres);
        let legacy_keys = sphere_lists(&legacy["spheres"]);
        let legacy_beatable = legacy["beatable"] == json!(true);
        let violations = legacy["violations"].as_array().cloned().unwrap_or_default();
        let diff = diff_spheres(&legacy_keys, &ours_keys);
        let ok = diff.is_none() && legacy_beatable == ours.beatable && violations.is_empty();
        let detail = (!ok).then(|| {
            let mut d = json!({"seed": s.seed, "legacy_beatable": legacy_beatable,
                               "port_beatable": ours.beatable, "violations": violations});
            if let Some((round, loc, side)) = &diff {
                // What was held going into that round: progression collected in earlier rounds.
                let held: Vec<&String> = legacy_keys
                    .iter()
                    .take(*round)
                    .flatten()
                    .filter_map(|l| placements.get(l))
                    .filter(|i| s.prog_key(i))
                    .collect();
                d["sphere"] = json!(round + 1);
                d["location"] = json!(loc);
                d["reached_by"] = json!(side);
                d["item_there"] = json!(placements.get(loc));
                d["held_before_round"] = json!(held);
                d["rules"] = self.rules.of(loc);
            }
            d
        });
        Ok((ok, detail, legacy_keys, legacy_beatable))
    }

    fn shape_view<'a>(
        &'a self,
        s: &'a Seed,
    ) -> (KeyFn<'a, bool>, KeyFn<'a, bool>, KeyFn<'a, Option<String>>) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for k in s.progression_items() {
            *counts.entry(k).or_default() += 1;
        }
        (
            Box::new(move |k: &str| s.prog_key(k)),
            Box::new(move |k: &str| counts.get(k).is_some_and(|n| *n <= 2)),
            Box::new(move |l: &str| self.rules.owner(l).map(str::to_owned)),
        )
    }
}

/// How many players the adapter reports (1 when it says nothing). The number comes from another
/// program, so it is checked against the room limit before any instance is fetched.
fn players_of(hello: &Value) -> R<u64> {
    let n = hello["players"].as_u64().unwrap_or(1);
    if n == 0 || n > config::ROOM_MAX_PLAYERS as u64 {
        return Err(format!(
            "adapter reports {n} players; a room takes 1 to {}",
            config::ROOM_MAX_PLAYERS
        ));
    }
    Ok(n)
}

/// A request for one player of a room (`player` from 1); a single-player world sends none.
fn for_player(mut req: Value, player: Option<u64>) -> Value {
    if let (Some(p), Some(o)) = (player, req.as_object_mut()) {
        o.insert("player".to_owned(), json!(p));
    }
    req
}

/// Fetch one seed's instance (of one player of a room) from the legacy side, without compiling.
fn fetch_instance(legacy: &mut Legacy, out: &Path, seed: u64, player: Option<u64>) -> R<Instance> {
    let raw = legacy.call(&for_player(json!({"op": "instance", "seed": seed}), player))?;
    let name = player.map_or(String::new(), |p| format!("_p{p}"));
    write(&out.join(format!("legacy/instance_{seed}{name}.json")), raw.to_string().as_bytes())?;
    serde_json::from_value(raw).map_err(|e| format!("instance {seed}{name}: {e}"))
}

/// Fetch one seed's instance from the legacy side and compile it.
fn load_seed<'w>(
    legacy: &mut Legacy,
    w: &'w World,
    out: &Path,
    seed: u64,
    player: Option<u64>,
) -> R<Seed<'w>> {
    let inst = fetch_instance(legacy, out, seed, player)?;
    let plan = Plan::new(w, &inst).map_err(|e| format!("{e:?}"))?;
    let progression = inst.progression(w);
    Ok(Seed { seed, inst, w, plan, progression })
}

/// Port sanity: many of our fills on one instance, engine only. Informational.
fn sanity(s: &Seed, rules: &Rules, n: u64) -> Value {
    let t = Instant::now();
    let pool: Vec<u16> = s.inst.pool.iter().filter_map(|k| s.w.item(k).ok()).collect();
    let run = sample(s.w, &s.plan, &pool, &s.progression, n, 0, 0);
    let mut copies: HashMap<String, u64> = HashMap::new();
    for k in s.progression_items() {
        *copies.entry(k).or_default() += 1;
    }
    let locked = s.plan.locked_placed(s.w);
    let free: Vec<u32> =
        s.plan.included.iter().copied().filter(|l| locked[*l as usize].is_none()).collect();
    let (mut sphere_counts, mut depths) = (Vec::new(), Vec::new());
    let mut region: BTreeMap<String, u64> = BTreeMap::new();
    let mut prog_total = 0u64;
    for placed in &run.fills {
        let sp = spheres(s.w, &s.plan, placed, &s.progression);
        sphere_counts.push(sp.spheres.len());
        let mut seen: BTreeSet<u16> = BTreeSet::new();
        for (round, sphere) in sp.spheres.iter().enumerate() {
            for l in sphere {
                let Some(i) = placed[*l as usize] else { continue };
                let key = s.w.item_key(i);
                if copies.get(key).is_some_and(|c| *c <= 2) && seen.insert(i) {
                    depths.push(round + 1);
                }
            }
        }
        for (l, i) in placed.iter().enumerate() {
            if let Some(i) = i
                && s.progression[*i as usize]
            {
                prog_total += 1;
                let owner = rules
                    .owner(s.w.loc_key(u32::try_from(l).unwrap_or(0)))
                    .unwrap_or("(none)")
                    .to_owned();
                *region.entry(owner).or_default() += 1;
            }
        }
    }
    let mut items: Vec<Value> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    for (key, c) in copies.iter().filter(|(_, c)| **c <= 2).collect::<BTreeMap<_, _>>() {
        let Ok(i) = s.w.item(key) else { continue };
        // An item that is only ever locked has exactly the places it is locked at.
        let pooled = s.inst.pool.iter().any(|k| k == key);
        let allowed = if pooled {
            free.iter().filter(|l| s.plan.permits(i, **l, true)).count() as u64
        } else {
            locked.iter().filter(|x| **x == Some(i)).count() as u64
        };
        let (distinct, h) = run.fill.spread(i);
        let expected = uniform_distinct(allowed, c * run.fill.samples);
        let flagged = allowed >= 10 && (distinct as f64) < 0.25 * expected;
        if flagged {
            flags.push(format!(
                "{key}: {distinct} distinct locations over {} fills, rules allow {allowed}",
                run.fill.samples
            ));
        }
        items.push(json!({"item": key, "distinct_locations": distinct, "entropy_bits": h,
                          "allowed_locations": allowed, "uniform_expected_distinct": expected, "flag": flagged}));
    }
    let mut shares: Vec<(String, f64)> =
        region.into_iter().map(|(r, n)| (r, n as f64 / prog_total.max(1) as f64)).collect();
    shares.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    json!({
        "what": "our fills on one instance, engine only; informational",
        "instance_seed": s.seed, "fills": run.fill.samples, "fill_failures": run.failed,
        "sphere_count_histogram [spheres, fills]": histogram(sphere_counts),
        "key_item_depth_histogram [sphere, placements]": histogram(depths),
        "progression_share_top_regions": shares.iter().take(15).map(|(r, x)| json!([r, x])).collect::<Vec<_>>(),
        "key_item_spread": items,
        "independence": run.fills.len().ge(&4).then(|| data_fill::fairness::independence(&run.fills)),
        "flags": flags,
        "seconds": t.elapsed().as_secs_f64(),
    })
}

/// (1) Reachability differential for one instance (of one player of a room): no items, all
/// items, and `share` random inventories.
fn reachability(
    legacy: &mut Legacy,
    cx: &mut Ctx,
    reach: &mut Tally,
    s: &Seed,
    share: usize,
    player: Option<u64>,
) -> R<()> {
    let seed = s.seed;
    let scope: Option<BTreeSet<String>> =
        (!cx.scope_all).then(|| s.inst.locations.iter().cloned().collect());
    let all = s.progression_items();
    let mut rng = SpecRng::from_u64_tagged(seed, b"inventor", player.unwrap_or(0));
    let mut invs: Vec<(String, Vec<String>)> =
        vec![("no items".into(), Vec::new()), ("all items".into(), all.clone())];
    for j in 0..share {
        // Sweep the share of items held from 0 to 1 so early, middle and late states all appear.
        let frac = if share > 1 { j as f64 / (share - 1) as f64 } else { 0.5 };
        let cut = (frac * f64::from(u32::MAX)) as u64;
        let pick: Vec<String> =
            all.iter().filter(|_| rng.next_u64() >> 32 <= cut).cloned().collect();
        invs.push((format!("random {j} ({:.0}% held)", frac * 100.0), pick));
    }
    for (label, inv) in invs {
        let req = for_player(json!({"op": "solve", "seed": seed, "inventory": inv}), player);
        let lv = SolveView::legacy(&legacy.call(&req)?);
        let t = Instant::now();
        let extra = inv.iter().map(|k| s.w.item(k)).collect::<R<Vec<_>>>()?;
        let r = s.plan.solve(s.w, &extra);
        let ov = SolveView::engine(
            s.w,
            &r,
            scope.as_ref(),
            cx.has_regions && lv.regions.is_some(),
            cx.has_events && lv.events.is_some(),
        );
        cx.engine_seconds += t.elapsed().as_secs_f64();
        let d = diff_solve(&lv, &ov, &cx.rules);
        reach.record(d.is_none(), || {
            let mut d = d.clone().unwrap_or_default();
            d["seed"] = json!(seed);
            if let Some(p) = player {
                d["player"] = json!(p);
            }
            d["inventory_label"] = json!(label);
            d["inventory"] = json!(inv);
            d
        });
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // the run is one sequence of checks
fn run(a: &Args) -> R<Value> {
    let t_all = Instant::now();
    let m = read_json(&a.manifest)?;
    // The compiled section is per world version: one compile shared by every seed.
    let world = World::load(&m)?;
    if world.has_shuffles() {
        // The comparison solves and fills the world as loaded.
        return Err(data_fill::shuffle::UNSTAGED.to_owned());
    }
    let mut legacy = Legacy::spawn(&a.legacy)?;
    let hello = legacy.call(&json!({"op": "hello"}))?;
    let mut cx = Ctx {
        rules: Rules::new(&m),
        scope_all: hello["location_scope"] == json!("all"),
        has_regions: hello["has_regions"] == json!(true),
        has_events: hello["has_events"] == json!(true),
        engine_seconds: 0.0,
        work: (Stats::default(), 0, 0),
    };
    let players = players_of(&hello)?;
    if players > 1 {
        return room::run(a, &world, &mut legacy, &mut cx, hello, players, t_all);
    }
    let version =
        json!({"engine": format!("data-fill {}", env!("CARGO_PKG_VERSION")), "format": FORMAT});
    let (mut reach, mut replay, mut judged, mut determinism) =
        (Tally::default(), Tally::default(), Tally::default(), Tally::default());
    let (mut shape_legacy, mut shape_ours) = (Shape::default(), Shape::default());
    let (mut replay_beatable, mut judged_beatable) = (0u64, 0u64);
    let mut hashes: Vec<Value> = Vec::new();
    let mut legacy_failures: Vec<Value> = Vec::new();
    let k = a.seeds.max(1);
    for n in 0..k {
        let seed = a.first_seed + n;
        eprintln!("port-check: seed {seed} ({}/{k})", n + 1);
        let t = Instant::now();
        let s = load_seed(&mut legacy, &world, &a.out, seed, None)?;
        cx.engine_seconds += t.elapsed().as_secs_f64();
        let share = a.inventories / usize::try_from(k).unwrap_or(1)
            + usize::from((n as usize) < a.inventories % usize::try_from(k).unwrap_or(1));
        reachability(&mut legacy, &mut cx, &mut reach, &s, share, None)?;

        // (2) The legacy generator's own seed, replayed in our engine.
        let generated = match legacy.call(&json!({"op": "generate", "seed": seed})) {
            Ok(g) => g,
            Err(e) => {
                // A legacy seed that fails to generate is the legacy side's problem, not a divergence.
                legacy_failures.push(json!([seed, e]));
                continue;
            }
        };
        let placements = str_map(&generated["placements"]);
        write(
            &a.out.join(format!("legacy/placements_{seed}.json")),
            serde_json::to_string(&placements).unwrap_or_default().as_bytes(),
        )?;
        let lsph =
            legacy.call(&json!({"op": "spheres", "seed": seed, "placements": placements}))?;
        let (ok, detail, lkeys, lbeat) = cx.spheres_check(&s, &placements, &lsph, None)?;
        replay.record(ok, || detail.unwrap_or_default());
        replay_beatable += u64::from(lbeat);
        {
            let (p, key, owner) = cx.shape_view(&s);
            shape_legacy.add(&SeedView {
                spheres: &lkeys,
                placements: &placements,
                progression: &p,
                key_item: &key,
                owner: &owner,
            });
        }

        // (3) + (4) Our seed for this instance: three fills must give the same bytes, then the
        // legacy side judges the placements.
        if n < a.ours {
            let fill_seed = a.fill_seed_base + seed;
            let mut files: Vec<Vec<u8>> = Vec::new();
            let mut ours: BTreeMap<String, String> = BTreeMap::new();
            let mut model = None;
            for _ in 0..3 {
                let t = Instant::now();
                // Spheres come from the fill's own final state, not a fresh sweep.
                // A room of this one player: the product's fill.
                let seat = [Seat { w: s.w, plan: &s.plan, progression: &s.progression }];
                let (filled, sp) = fill_room_spheres(
                    &seat,
                    &mut SpecRng::from_u64(fill_seed),
                    config::FILL_MAX_ATTEMPTS,
                )
                .map_err(|e| format!("fill {seed}: {e:?}"))?;
                model = Some(Spheres::from(sp));
                cx.engine_seconds += t.elapsed().as_secs_f64();
                cx.work.0.absorb(&filled.stats);
                cx.work.1 += 1;
                cx.work.2 += u64::from(filled.solves);
                ours = filled.placed[0]
                    .iter()
                    .enumerate()
                    .filter_map(|(l, g)| {
                        Some((
                            s.w.loc_key(u32::try_from(l).ok()?).to_owned(),
                            s.w.item_key((*g)?.item).to_owned(),
                        ))
                    })
                    .collect();
                let file = json!({"version": version, "instance_seed": seed, "fill_seed": fill_seed, "placements": ours});
                files.push(serde_json::to_vec_pretty(&file).unwrap_or_default());
            }
            let same = files.windows(2).all(|p| p[0] == p[1]);
            let hash = format!("{:016x}", fnv1a(&files[0]));
            determinism.record(same, || json!({"seed": seed, "hashes": files.iter().map(|f| format!("{:016x}", fnv1a(f))).collect::<Vec<_>>()}));
            hashes.push(json!([fill_seed, hash]));
            write(&a.out.join(format!("ours/placements_{seed}.json")), &files[0])?;
            let lsph = legacy.call(&json!({"op": "spheres", "seed": seed, "placements": ours}))?;
            let (ok, detail, lkeys, lbeat) = cx.spheres_check(&s, &ours, &lsph, model.as_ref())?;
            judged.record(ok, || detail.unwrap_or_default());
            judged_beatable += u64::from(lbeat);
            let (p, key, owner) = cx.shape_view(&s);
            shape_ours.add(&SeedView {
                spheres: &lkeys,
                placements: &ours,
                progression: &p,
                key_item: &key,
                owner: &owner,
            });
        }
    }
    let tier1 = if a.sanity_seeds > 0 {
        let s = load_seed(&mut legacy, &world, &a.out, a.first_seed, None)?;
        sanity(&s, &cx.rules, a.sanity_seeds)
    } else {
        Value::Null
    };
    let checks = json!({
        "reachability": reach.json(json!({"what": "legacy solver vs port engine: locations, regions with time masks, event counts"})),
        "legacy_seeds_replayed": replay.json(json!({"what": "legacy placements: spheres and beatability agree", "beatable_by_legacy": replay_beatable, "legacy_generation_failures": legacy_failures})),
        "port_seeds_judged_by_legacy": judged.json(json!({"what": "port placements: legacy finds them valid, beatable, same spheres", "beatable_by_legacy": judged_beatable})),
        "determinism": determinism.json(json!({"what": "same seed + same version = same bytes (3 fills each)", "version": version, "hashes": hashes})),
    });
    let pass =
        ["reachability", "legacy_seeds_replayed", "port_seeds_judged_by_legacy", "determinism"]
            .iter()
            .all(|c| checks[c]["pass"] == json!(true));
    Ok(json!({
        "pass": pass,
        "checks": checks,
        "seed_shape (informational)": compare_shapes(&shape_legacy, &shape_ours),
        "port_sanity (informational)": tier1,
        "work": cx.work.0.summary(cx.work.1, cx.work.2),
        "seconds": {"total": t_all.elapsed().as_secs_f64(), "legacy": legacy.seconds, "port_engine": cx.engine_seconds},
        "port_engine_peak_rss_kb": peak_rss_kb(),
        "legacy_adapter": hello,
    }))
}

/// This process's peak resident memory (Linux `VmHWM`); the legacy side is a separate process.
fn peak_rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let a = match parse(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("port-check: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&a) {
        Ok(report) => {
            let text = serde_json::to_string_pretty(&report).unwrap_or_default();
            if let Err(e) = write(&a.out.join("report.json"), text.as_bytes()) {
                eprintln!("port-check: {e}");
            }
            println!("{text}");
            if report["pass"] == json!(true) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(e) => {
            eprintln!("port-check: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_players_an_adapter_reports_are_checked_before_any_fetch() {
        let n = |v: Value| players_of(&json!({"players": v}));
        assert_eq!(players_of(&json!({})), Ok(1));
        assert_eq!(n(json!(3)), Ok(3));
        let max = config::ROOM_MAX_PLAYERS as u64;
        assert_eq!(n(json!(max)), Ok(max));
        for bad in [json!(0), json!(max + 1), json!(u64::MAX)] {
            assert!(n(bad.clone()).is_err(), "{bad}");
        }
        // Not a number: no players reported, a single-player world.
        assert_eq!(n(json!("many")), Ok(1));
    }
}
