//! `synth_room [--profile light|heavy] [--path auto|carry|forward] [--workers 4]
//! [--mode exact|level|shuffle[,depth=N][,goal][,noskip]] [--players 1,10,20,50,100]
//! [--checks 1000] [--layers 6] [--branching 2] [--progression 0.19] [--clocks on|off]
//! [--seed 1] [--reps 5] [--worlds each|one]`: room fill scaling on synthetic worlds ([`data_fill::synth`]; the
//! heavy profile has clocks on). `--path` forces one way of placing rounds
//! ([`data_fill::room::Path`]); `auto` is what a room of that size takes. One JSON object per player count, then one with the
//! exponent of fill time fitted over the player counts (least squares on the logs).
//!
//! What is done once per world version is timed apart from what every seed pays. Once per
//! world: `prepare_ms` compiles every world of the room into its stored ready form
//! ([`data_fill::prepared::build`], on one thread), and `load_ms` loads those stored forms
//! ([`Prepared::open`]). Per seed: `setup_ms` resolves every player's instance against their
//! loaded world (on `--workers` threads; drawing the synthetic manifests is not timed), and
//! `fill_ms` is the median of `reps` fills of the same seed (the work is identical, so the
//! spread is timing noise only).
//!
//! `--worlds each` (the default) draws a world per player, the scaling measurement; `--worlds
//! one` is a room as the server generates it, every player on one world, loaded once. Only
//! then is a seed assembled: `assemble_ms` builds the bundle on the prepared tables
//! ([`data_fill::seed::assemble_on`]), `encode_ms` checks, hashes and encodes it as the seed
//! file ([`data_fill::seed::encode_on`]), and `import_v1_ms` is what the path before did in
//! its place (the bundle as JSON text, then `seed_bundle::import_v1`); medians of `reps`. Two
//! parts of the encode are also timed alone: `validate_ms` (the check), and `hash_whole_ms`,
//! the hash with the game's names and ids written again, which the encode does not pay.
//! With feature `memory-harness`, `peak_heap_bytes` is the most live heap one fill allocated on
//! the calling thread (`allocation-counter` counts per thread, so what worker threads allocate
//! is left out: read it from a `--workers 1` run); without the feature, `null`.
//! `peak_rss_bytes` is the whole process's resident high-water mark when the row is printed
//! (`VmHWM` of `/proc/self/status`, Linux only): it never falls, so it covers setup and every
//! smaller room before the row; `null` where the file is not there.
//!
//! `--mode` picks how progression is placed ([`data_fill::room::Algorithm`]): the row then
//! also says how many of the reps' seed the mode placed itself, its repairs, swaps and
//! fallbacks, and the rule evaluations of the mode's own solves.

use std::time::Instant;

use data_fill::prepared::{Prepared, build};
use data_fill::room::{Algorithm, Compiled, Path, cross_world_share, fill_room_by};
use data_fill::seed::{Player, assemble_on, encode_on};
use data_fill::synth::{Shape, world};
use data_fill::{Instance, World};
use serde_json::{Value, json};
use spec_rng::SpecRng;

fn arg<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> Result<T, String> {
    match args.iter().position(|a| a == name) {
        None => Ok(default),
        Some(i) => args
            .get(i + 1)
            .ok_or(format!("{name}: missing value"))?
            .parse()
            .map_err(|_| format!("{name}: bad value")),
    }
}

/// Most live heap while `f` runs, when the counting allocator is linked.
#[cfg(feature = "memory-harness")]
fn peak<T>(f: impl FnOnce() -> T) -> (T, Option<u64>) {
    let mut out = None;
    let a = allocation_counter::measure(|| out = Some(f()));
    (out.unwrap_or_else(|| unreachable!("measure runs its closure")), Some(a.bytes_max))
}

#[cfg(not(feature = "memory-harness"))]
fn peak<T>(f: impl FnOnce() -> T) -> (T, Option<u64>) {
    (f(), None)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// The process's resident high-water mark, bytes (Linux).
fn peak_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?;
    kb.trim().trim_end_matches("kB").trim().parse::<u64>().ok().map(|kb| kb * 1024)
}

/// The median of `reps` timings of `f`, and every timing, ascending (ms).
fn median(
    reps: usize,
    mut f: impl FnMut() -> Result<(), String>,
) -> Result<(f64, Vec<f64>), String> {
    let mut times = Vec::with_capacity(reps.max(1));
    for _ in 0..reps.max(1) {
        let t = Instant::now();
        f()?;
        times.push(ms(t));
    }
    times.sort_by(f64::total_cmp);
    Ok((times[times.len() / 2], times))
}

/// To a hundredth of a millisecond.
fn fine(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

/// The seed cap of the encodes here.
const SEED_MAX_BYTES: u64 = config::Limits::DESIGN.bundle_max_bytes;

fn one(
    shape: &Shape,
    seed: u64,
    players: usize,
    reps: usize,
    (path, workers, mode, one_world): (Option<Path>, usize, Algorithm, bool),
) -> Result<Value, String> {
    let path = path.unwrap_or(Path::of(players, workers));
    let count = if one_world { 1 } else { players };
    let drawn: Vec<(Value, Instance)> = (0..count as u64)
        .map(|p| {
            let (mut manifest, instance) = world(shape, seed, p);
            manifest["game"] = json!("Synth");
            (manifest, instance)
        })
        .collect();
    let t = Instant::now();
    let files: Vec<Vec<u8>> =
        drawn.iter().map(|(m, _)| build(m).map(|b| b.0)).collect::<Result<_, _>>()?;
    let prepare_ms = ms(t);
    let t = Instant::now();
    let loaded: Vec<Prepared> =
        files.iter().map(|f| Prepared::open(f)).collect::<Result<_, _>>()?;
    let load_ms = ms(t);
    let insts: Vec<&Instance> = (0..players).map(|p| &drawn[p % count].1).collect();
    // A room owns its worlds: the copy is part of what a seed pays, as in `seed::generate_on`.
    let t = Instant::now();
    let worlds: Vec<World> = loaded.iter().map(|p| p.world().clone()).collect();
    let c = Compiled::resolve(worlds, &insts, workers)?;
    let setup_ms = ms(t);
    let seats = c.seats();
    let fill = || {
        let mut rng = SpecRng::from_u64(seed);
        fill_room_by(&seats, &mut rng, config::FILL_MAX_ATTEMPTS, path, mode)
    };
    let (first, heap) = peak(fill);
    let (f, sp) = first.map_err(|e| format!("{players} players: {e:?}"))?;
    if !f.all_beatable() {
        return Err(format!("{players} players: not beatable: {:?}", f.beatable));
    }
    let (fill_ms, times) = median(reps, || {
        drop(fill());
        Ok(())
    })?;
    // The seed of a room on one world, on the tables its world was prepared with.
    let mut seed_row = json!({});
    if let (true, Some(on)) = (one_world, loaded.first()) {
        let named: Vec<Player> = (0..players)
            .map(|p| Player { name: format!("P{p}"), instance: drawn[0].1.clone() })
            .collect();
        let assemble = || assemble_on(on, "bench", &named, &seats, &f.placed, &sp);
        let mut bundle = assemble()?;
        let seed_file = encode_on(on, &mut bundle, SEED_MAX_BYTES)?;
        let (assemble_ms, _) = median(reps, || assemble().map(drop))?;
        let (encode_ms, _) = median(reps, || encode_on(on, &mut bundle, SEED_MAX_BYTES).map(drop))?;
        let (validate_ms, _) = median(reps, || bundle.validate().map_err(|e| e.to_string()))?;
        let (hash_whole_ms, _) = median(reps, || bundle.hash().map(drop))?;
        let (import_v1_ms, _) = median(reps, || {
            let text = serde_json::to_vec(&bundle).map_err(|e| e.to_string())?;
            let (old, _) = seed_bundle::import_v1(&text, SEED_MAX_BYTES)?;
            if old.bytes() != seed_file {
                return Err("the seed file differs from the v1 import's".to_owned());
            }
            Ok(())
        })?;
        seed_row = json!({"assemble_ms": fine(assemble_ms), "encode_ms": fine(encode_ms),
                          "validate_ms": fine(validate_ms), "hash_whole_ms": fine(hash_whole_ms),
                          "import_v1_ms": fine(import_v1_ms), "seed_bytes": seed_file.len()});
    }
    let (crossed, all) = cross_world_share(&seats, &f.placed)
        .map_err(|e| format!("{e:?}"))?
        .iter()
        .fold((0, 0), |(x, a), (c, n)| (x + c, a + n));
    let s = &f.stats;
    let work = s.summary(1, u64::from(f.solves));
    Ok(json!({
        "players": players,
        "path": if path == Path::Carry { "carry" } else { "forward" },
        "workers": match path {
            Path::Carry => 1,
            Path::Forward { workers } => workers,
        },
        "solves": f.solves,
        "checks": shape.checks,
        "clocks": shape.clocks,
        "world": if one_world { "one" } else { "each" },
        "worlds": count,
        "prepare_ms": fine(prepare_ms),
        "load_ms": fine(load_ms),
        "prepared_bytes": files.iter().map(Vec::len).sum::<usize>(),
        "loaded_bytes": loaded.iter().map(Prepared::held_bytes).sum::<usize>(),
        "seed": seed_row,
        "setup_ms": (setup_ms * 10.0).round() / 10.0,
        "fill_ms": (fill_ms * 10.0).round() / 10.0,
        "fill_ms_runs": times.iter().map(|t| (t * 10.0).round() / 10.0).collect::<Vec<_>>(),
        "rule_evals": s.evals,
        "ns_per_eval": (fill_ms * 1e6 / s.evals.max(1) as f64 * 10.0).round() / 10.0,
        "removals": s.removals,
        "retractions": s.retractions,
        "retractions_per_removal": work["retractions_per_removal"],
        "churn": work["churn"],
        "rounds": f.rounds,
        "spheres": sp.spheres.len(),
        "cross_world_share": (crossed as f64 / all.max(1) as f64 * 1e3).round() / 1e3,
        "attempts": f.attempts,
        "beatable": f.all_beatable(),
        "placed_by_the_mode": mode == Algorithm::Exact || f.fast,
        "repairs": f.repairs,
        "swaps": f.swaps,
        "fallbacks": f.fallbacks,
        "mode_rule_evals": f.mode_evals,
        "peak_heap_bytes": heap,
        "peak_rss_bytes": peak_rss(),
    }))
}

/// Least-squares slope of `ln y` over `ln x`: `y` grows about as `x` to this power.
fn exponent(points: &[(f64, f64)]) -> Option<f64> {
    let pts: Vec<(f64, f64)> = points
        .iter()
        .filter(|(x, y)| *x > 0.0 && *y > 0.0)
        .map(|(x, y)| (x.ln(), y.ln()))
        .collect();
    let n = pts.len() as f64;
    let (mx, my) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x / n, b + y / n));
    let sxx: f64 = pts.iter().map(|(x, _)| (x - mx).powi(2)).sum();
    let sxy: f64 = pts.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
    (sxx > 0.0).then(|| (sxy / sxx * 100.0).round() / 100.0)
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let profile = arg(&args, "--profile", "light".to_owned())?;
    let d = match profile.as_str() {
        "light" => Shape::default(),
        "heavy" => Shape::heavy(),
        x => return Err(format!("--profile: light or heavy, got {x}")),
    };
    let on = |b: bool| if b { "on" } else { "off" }.to_owned();
    let shape = Shape {
        checks: arg(&args, "--checks", d.checks)?,
        layers: arg(&args, "--layers", d.layers)?,
        branching: arg(&args, "--branching", d.branching)?,
        progression: arg(&args, "--progression", d.progression)?,
        clocks: match arg(&args, "--clocks", on(d.clocks))?.as_str() {
            "on" => true,
            "off" => false,
            x => return Err(format!("--clocks: on or off, got {x}")),
        },
        ..d
    };
    let workers: usize = arg(&args, "--workers", config::ROOM_FORWARD_WORKERS)?;
    let path_arg = arg(&args, "--path", "auto".to_owned())?;
    let path = match path_arg.as_str() {
        "auto" => None,
        "carry" => Some(Path::Carry),
        "forward" => Some(Path::Forward { workers }),
        x => return Err(format!("--path: auto, carry or forward, got {x}")),
    };
    let mode_arg = arg(&args, "--mode", "exact".to_owned())?;
    let mode: Algorithm = mode_arg.parse()?;
    let one_world = match arg(&args, "--worlds", "each".to_owned())?.as_str() {
        "each" => false,
        "one" => true,
        x => return Err(format!("--worlds: each or one, got {x}")),
    };
    let seed: u64 = arg(&args, "--seed", 1)?;
    let reps: usize = arg(&args, "--reps", 5)?;
    let players: Vec<usize> = arg(&args, "--players", "1,10,20,50,100".to_owned())?
        .split(',')
        .map(|p| p.trim().parse().map_err(|_| format!("--players: bad count {p:?}")))
        .collect::<Result<_, _>>()?;
    let mut points = Vec::new();
    for n in players {
        let mut row = one(&shape, seed, n, reps, (path, workers, mode, one_world))?;
        row["profile"] = json!(profile);
        row["mode"] = json!(mode_arg);
        points.push((n as f64, row["fill_ms"].as_f64().unwrap_or(0.0)));
        println!("{row}");
    }
    let fitted = exponent(&points);
    println!(
        "{}",
        json!({"profile": profile, "path": path_arg, "workers": workers, "mode": mode_arg,
               "world": if one_world { "one" } else { "each" },
               "fitted_exponent": fitted, "points": points})
    );
    Ok(())
}
