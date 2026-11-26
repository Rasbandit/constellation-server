//! `synth_room [--profile light|heavy] [--path auto|carry|forward] [--workers 4]
//! [--players 1,10,20,50,100]
//! [--checks 1000] [--layers 6] [--branching 2] [--progression 0.19] [--clocks on|off]
//! [--seed 1] [--reps 5]`: room fill scaling on synthetic worlds ([`data_fill::synth`]; the
//! heavy profile has clocks on). `--path` forces one way of placing rounds
//! ([`data_fill::room::Path`]); `auto` is what a room of that size takes. One JSON object per player count, then one with the
//! exponent of fill time fitted over the player counts (least squares on the logs).
//!
//! `setup_ms` draws, compiles and resolves every player's world. `fill_ms` is the median of
//! `reps` fills of the same seed (the work is identical, so the spread is timing noise only).
//! With feature `memory-harness`, `peak_heap_bytes` is the most live heap one fill allocated
//! (`allocation-counter`); without it, `null`.

use std::time::Instant;

use data_fill::room::{Compiled, Path, cross_world_share, fill_room_on};
use data_fill::synth::{Shape, room};
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

fn one(
    shape: &Shape,
    seed: u64,
    players: usize,
    reps: usize,
    path: Option<Path>,
) -> Result<Value, String> {
    let path = path.unwrap_or(Path::of(players, config::ROOM_FORWARD_WORKERS));
    let t = Instant::now();
    let c: Compiled = room(shape, seed, players, config::FILL_SETUP_WORKERS)?;
    let setup_ms = ms(t);
    let seats = c.seats();
    let fill =
        || fill_room_on(&seats, &mut SpecRng::from_u64(seed), config::FILL_MAX_ATTEMPTS, path);
    let (first, heap) = peak(fill);
    let (f, sp) = first.map_err(|e| format!("{players} players: {e:?}"))?;
    if !f.all_beatable() {
        return Err(format!("{players} players: not beatable: {:?}", f.beatable));
    }
    let mut times: Vec<f64> = (0..reps.max(1))
        .map(|_| {
            let t = Instant::now();
            drop(fill());
            ms(t)
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let fill_ms = times[times.len() / 2];
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
        "peak_heap_bytes": heap,
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
    let workers = config::ROOM_FORWARD_WORKERS;
    let path_arg = arg(&args, "--path", "auto".to_owned())?;
    let path = match path_arg.as_str() {
        "auto" => None,
        "carry" => Some(Path::Carry),
        "forward" => Some(Path::Forward { workers: arg(&args, "--workers", workers)? }),
        x => return Err(format!("--path: auto, carry or forward, got {x}")),
    };
    let seed: u64 = arg(&args, "--seed", 1)?;
    let reps: usize = arg(&args, "--reps", 5)?;
    let players: Vec<usize> = arg(&args, "--players", "1,10,20,50,100".to_owned())?
        .split(',')
        .map(|p| p.trim().parse().map_err(|_| format!("--players: bad count {p:?}")))
        .collect::<Result<_, _>>()?;
    let mut points = Vec::new();
    for n in players {
        let mut row = one(&shape, seed, n, reps, path)?;
        row["profile"] = json!(profile);
        points.push((n as f64, row["fill_ms"].as_f64().unwrap_or(0.0)));
        println!("{row}");
    }
    let fitted = exponent(&points);
    println!(
        "{}",
        json!({"profile": profile, "path": path_arg, "fitted_exponent": fitted, "points": points})
    );
    Ok(())
}
