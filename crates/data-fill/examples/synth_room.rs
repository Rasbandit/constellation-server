//! `synth_room [--profile light|heavy|wide|chain|gated|keys|brink|tight] [--rules bits|bytecode] [--absent skip|check]
//! [--path auto|carry|forward|split] [--workers 4]
//! [--mode exact|level|shuffle[,depth=N][,goal][,noskip]] [--players 1,10,20,50,100]
//! [--checks 1000] [--layers 6] [--branching 2] [--progression 0.19] [--clocks on|off]
//! [--seed 1] [--reps 5] [--worlds each|one] [--failed-row] [--rounds players|single] [--entrances on|off|vanilla] [--guards on|off] [--timeless on|off]`: room fill scaling on synthetic worlds ([`data_fill::synth`]; the
//! heavy profile has clocks on). `--path` forces one way of placing rounds
//! ([`data_fill::room::Path`]); `auto` is what a room of that size takes. One JSON object per player count, then one with the
//! exponent of fill time fitted over the player counts (least squares on the logs).
//!
//! What is done once per world version is timed apart from what every seed pays. Once per
//! world: `prepare_ms` compiles every world of the room into its stored ready form
//! ([`data_fill::prepared::build`], on one thread), and `load_ms` loads those stored forms
//! ([`Prepared::open`]); `bit_rules_ms` is the part of it that builds the worlds' bit rules,
//! measured by building them once more, so `load_ms - bit_rules_ms` is the load without them.
//! Per seed: `setup_ms` resolves every player's instance against their
//! loaded world (on `--workers` threads; drawing the synthetic manifests is not timed), and
//! `fill_ms` is the median of `reps` fills of the same seed (the work is identical, so the
//! spread is timing noise only).
//!
//! `--worlds each` (the default) draws a world per player, the scaling measurement; `--worlds
//! one` is a room as the server generates it, every player on one world, loaded once. Only
//! then is a seed assembled: `assemble_ms` builds the bundle on the prepared tables
//! ([`data_fill::seed::assemble_on`]), `encode_ms` checks, hashes and encodes it as the seed
//! file ([`data_fill::seed::encode_on`]), `open_ms` opens that file on the world's package as
//! the server does (`seed_bundle::Seed::read`), and `import_v1_ms` is what the path before did
//! in place of both (the bundle as JSON text, then `seed_bundle::import_v1`, which encodes and
//! opens); medians of `reps`. Two
//! parts of the encode are also timed alone: `validate_ms` (the check), and `hash_whole_ms`,
//! the hash with the game's names and ids written again, which the encode does not pay.
//! With feature `memory-harness`, `peak_heap_bytes` is the most live heap one fill allocated on
//! the calling thread (`allocation-counter` counts per thread, so what worker threads allocate
//! is left out: read it from a `--workers 1` run); without the feature, `null`.
//! `peak_rss_bytes` is the whole process's resident high-water mark when the row is printed
//! (`VmHWM` of `/proc/self/status`, Linux only): it never falls, so it covers setup and every
//! smaller room before the row; `null` where the file is not there. The same mark is also read
//! after the fills (`peak_rss_after_fill_bytes`) and, with `--worlds one`, after the encode
//! (`peak_rss_after_encode_bytes`), before the comparison import that only this harness runs.
//!
//! `rule_evals` are counted by the solver, `rule_evals_ranked` the removals' support checks among
//! them, and `ns_per_eval` is the fill's wall time over that count, so it carries the solver's
//! bookkeeping. `eval_only` is evaluation alone on the first player's world ([`Reach::replay`]:
//! every exit, event and check rule of the reached regions, timed over `REPLAYS` passes), with
//! nothing held and with every progression item held, and the bytecode nodes and def bodies an
//! evaluation reads (counted).
//!
//! `--rules bytecode` leaves every rule as bytecode (`Plan::interpreted`) and `--absent check`
//! also evaluates the checks of absent locations (`Plan::with_absent_checks`): the engine as it
//! was before each, for a before and after on one runner. The placements hash must not move.
//! `rules_report` is the first player's `Plan::rules_report` (rule sites per class, what the
//! seed's fold leaves and what the steps not built could remove, all counted).
//!
//! `row_s` is the wall time the whole row took (drawing, preparing, every fill and replay), and
//! `fill_reps` how many fills the median is over: `--reps`, or two once a fill takes over a
//! second.
//!
//! `--entrances on` draws the worlds with shuffled two-way doors (`Shape::doors`), `--guards
//! on` with a `none` shuffle that rules read (`Shape::guards`), and `--timeless on` without a
//! time context (`Shape::timeless`, which an exit shuffle needs). With a shuffle, every
//! player's world is staged before the fill ([`data_fill::shuffle::room`]) and the row's
//! `shuffle` holds, as medians of `reps`: `stage_ms` (the shuffles, the fold, the whole-world
//! check and resolving the instances, every player, one thread), `plans_ms` (resolving the
//! instances alone on the staged worlds, which a room without shuffles pays too) and
//! `shuffle_ms`, their difference: what the shuffles add per seed. `restarts` counts attempts
//! after the first over the room, `exits` the shuffled exits of the first world.
//! `--entrances vanilla` draws the same worlds as `--entrances on` and fills them with every
//! door where the manifest has it ([`data_fill::shuffle::unshuffled`]), so the two rows differ
//! in the layout alone; the row's `layout` says which. Every row has `room_bytes`: the heap
//! the room's worlds and plans keep after setup (with feature `memory-harness`; the loaded
//! worlds they share are not in it), `room_bytes_per_player`, and `world_bytes`, the heap of
//! the first world loaded once more.
//!
//! `--failed-row`: a room whose fill ends in an error (stuck on every attempt) prints
//! `{"room": players, "failed": error, ...}` instead of ending the run with that error.
//!
//! `--mode` picks how progression is placed ([`data_fill::room::Algorithm`]): the row then
//! also says how many of the reps' seed the mode placed itself, its repairs, swaps and
//! fallbacks, and the rule evaluations of the mode's own solves.

use std::time::Instant;

use data_fill::prepared::{Prepared, SEED_MAX_BYTES, build, seed_game};
use data_fill::room::{
    Algorithm, Compiled, Path, Seat, cross_world_share, fill_room_by, fill_room_one_item_rounds,
    plans,
};
use data_fill::seed::{Player, assemble_on, encode_on};
use data_fill::synth::{Hostile, Shape, world};
use data_fill::{Instance, Plan, World};
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

/// The heap what `f` returns still holds when `f` is done, when the counting allocator is
/// linked.
#[cfg(feature = "memory-harness")]
fn kept<T>(f: impl FnOnce() -> T) -> (T, Option<i64>) {
    let mut out = None;
    let a = allocation_counter::measure(|| out = Some(f()));
    (out.unwrap_or_else(|| unreachable!("measure runs its closure")), Some(a.bytes_current))
}

#[cfg(not(feature = "memory-harness"))]
fn kept<T>(f: impl FnOnce() -> T) -> (T, Option<i64>) {
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

/// Passes of [`data_fill::Reach::replay`] that `eval_only` times.
const REPLAYS: u32 = 200;

/// Evaluation alone on one seat's world holding `extra`: counted work, and ns per evaluation.
fn eval_only(seat: &Seat<'_>, extra: &[u16]) -> Value {
    let r = seat.plan.state(seat.w, extra, &[]);
    let [evals, held, nodes, bodies] = r.replay();
    let t = Instant::now();
    let again: u64 = (0..REPLAYS).map(|_| std::hint::black_box(&r).replay()[1]).sum();
    let ns = t.elapsed().as_secs_f64() * 1e9 / (f64::from(REPLAYS) * evals.max(1) as f64);
    let per = |x: u64| (x as f64 / evals.max(1) as f64 * 100.0).round() / 100.0;
    json!({"evals": evals, "held": held, "same_every_pass": again == held * u64::from(REPLAYS),
           "ns_per_eval": (ns * 10.0).round() / 10.0,
           "nodes_per_eval": per(nodes), "def_bodies_per_eval": per(bodies)})
}

/// To a hundredth of a millisecond.
fn fine(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

fn one(
    shape: &Shape,
    seed: u64,
    players: usize,
    reps: usize,
    (path, workers, mode, one_world): (Option<Path>, usize, Algorithm, bool),
    (bits, skip, single, vanilla): (bool, bool, bool, bool),
) -> Result<Value, String> {
    let row = Instant::now();
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
    // Of `load_ms`: building every world's bit rules, done once more and timed alone.
    let t = Instant::now();
    let rebuilt: usize = loaded.iter().map(|p| p.world().build_bit_rules()).sum();
    std::hint::black_box(rebuilt);
    let bit_rules_ms = ms(t);
    let insts: Vec<&Instance> = (0..players).map(|p| &drawn[p % count].1).collect();
    let mut shuffle_row = Value::Null;
    let base: Vec<&World> = loaded.iter().map(Prepared::world).collect();
    let t = Instant::now();
    // A room owns its worlds, as in `seed::generate_on`: staged per player when the world
    // declares shuffles, packed copies of the loaded world otherwise.
    let setup = || {
        if vanilla {
            // The doors-shaped worlds with every door where the manifest has it.
            let mut c = Compiled { worlds: Vec::new(), plans: Vec::new(), progression: Vec::new() };
            for (p, inst) in insts.iter().enumerate() {
                let s = data_fill::shuffle::unshuffled(base[p % count], inst)?
                    .ok_or("--entrances vanilla: the world declares no shuffle")?;
                c.progression.push(inst.progression(&s.world));
                c.worlds.push(s.world);
                c.plans.push(s.plan);
            }
            Ok(c)
        } else if shape.doors || shape.guards {
            data_fill::shuffle::room(&base, &insts, seed, 1).map(|x| x.0)
        } else {
            // Several worlds are copied side by side; one is shared as loaded.
            let own = |w: &&World| if count > 1 { w.packed() } else { (*w).clone() };
            Compiled::resolve(base.iter().map(own).collect(), &insts, workers)
        }
    };
    let (c, room_bytes) = kept(setup);
    let mut c = c?;
    let setup_ms = ms(t);
    if !vanilla && (shape.doors || shape.guards) {
        let (stage_ms, _) = median(reps, || setup().map(drop))?;
        let pairs: Vec<(&World, &Instance)> =
            (0..players).map(|p| (c.world(p), insts[p])).collect();
        let (plans_ms, _) =
            median(reps, || plans(&pairs, 1).map(drop).map_err(|e| format!("{e:?}")))?;
        let (mut restarts, mut steps) = (0, [0u64; data_fill::shuffle::STEPS.len()]);
        for (p, inst) in insts.iter().enumerate() {
            let player = u32::try_from(p).map_err(|e| e.to_string())?;
            if let Some(s) = data_fill::shuffle::stage(base[p % count], inst, seed, player)? {
                restarts += s.attempts.saturating_sub(1);
                steps.iter_mut().zip(s.nanos).for_each(|(sum, ns)| *sum += ns);
            }
        }
        // The passing attempts' steps over the room, ms (one more pass, timed inside).
        let steps: serde_json::Map<String, Value> = data_fill::shuffle::STEPS
            .iter()
            .zip(steps)
            .map(|(name, ns)| ((*name).to_owned(), json!(fine(ns as f64 / 1e6))))
            .collect();
        let shuffle_ms = stage_ms - plans_ms;
        shuffle_row = json!({
            "stage_ms": fine(stage_ms), "plans_ms": fine(plans_ms), "shuffle_ms": fine(shuffle_ms),
            "per_world_ms": (shuffle_ms / players as f64 * 1e3).round() / 1e3,
            "restarts": restarts,
            "steps_ms": steps,
            "exits": drawn[0].0["shuffles"]["entrances"]["slots"]["names"].as_array().map(Vec::len),
        });
    }
    let plain = |p: Plan| if bits { p } else { p.interpreted() };
    let whole = |p: Plan| if skip { p } else { p.with_absent_checks() };
    c.plans = c.plans.into_iter().map(|p| whole(plain(p))).collect();
    let seats = c.seats();
    let fill = || {
        let mut rng = SpecRng::from_u64(seed);
        if single {
            return fill_room_one_item_rounds(&seats, &mut rng, config::FILL_MAX_ATTEMPTS);
        }
        fill_room_by(&seats, &mut rng, config::FILL_MAX_ATTEMPTS, path, mode)
    };
    let t = Instant::now();
    let (first, heap) = peak(fill);
    // A fill of over a second is timed twice more, not `reps` times: its noise is small
    // against it, and the job's time goes to these rows.
    let reps = if ms(t) > 1e3 { reps.min(2) } else { reps };
    let (f, sp) = first.map_err(|e| format!("{players} players: {e:?}"))?;
    if !f.all_beatable() {
        return Err(format!("{players} players: not beatable: {:?}", f.beatable));
    }
    let (fill_ms, times) = median(reps, || {
        drop(fill());
        Ok(())
    })?;
    // The seed of a room on one world, on the tables its world was prepared with.
    let rss_after_fill = peak_rss();
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
        // What the server then does with the seed file: open it on the world's package.
        let game = seed_game(&files[0])?.to_vec();
        let (open_ms, _) = median(reps, || {
            seed_bundle::Seed::read(seed_file.clone(), |_| Ok(game.clone()), SEED_MAX_BYTES)
                .map(drop)
                .map_err(|e| e.to_string())
        })?;
        let rss_after_encode = peak_rss();
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
        seed_row = json!({"assemble_ms": fine(assemble_ms), "encode_ms": fine(encode_ms), "open_ms": fine(open_ms),
                          "validate_ms": fine(validate_ms), "hash_whole_ms": fine(hash_whole_ms),
                          "import_v1_ms": fine(import_v1_ms), "seed_bytes": seed_file.len(),
                          "peak_rss_after_encode_bytes": rss_after_encode});
    }
    let (crossed, all) = cross_world_share(&seats, &f.placed)
        .map_err(|e| format!("{e:?}"))?
        .iter()
        .fold((0, 0), |(x, a), (c, n)| (x + c, a + n));
    let s = &f.stats;
    let pool: Vec<u16> = insts[0]
        .pool
        .iter()
        .filter_map(|k| seats[0].w.item(k).ok())
        .filter(|i| seats[0].progression[usize::from(*i)])
        .collect();
    let replay = json!({"nothing_held": eval_only(&seats[0], &[]),
                        "all_held": eval_only(&seats[0], &pool)});
    let work = s.summary(1, u64::from(f.solves));
    let mut out = json!({
        "players": players,
        "path": match path {
            Path::Carry => "carry",
            Path::Forward { .. } => "forward",
            Path::Split => "split",
        },
        "workers": match path {
            Path::Carry | Path::Split => 1,
            Path::Forward { workers } => workers,
        },
        "solves": f.solves,
        "checks": shape.checks,
        "clocks": shape.clocks && !shape.timeless,
        "world": if one_world { "one" } else { "each" },
        "worlds": count,
        "prepare_ms": fine(prepare_ms),
        "load_ms": fine(load_ms),
        "prepared_bytes": files.iter().map(Vec::len).sum::<usize>(),
        "seed": seed_row,
        "setup_ms": (setup_ms * 10.0).round() / 10.0,
        "fill_ms": (fill_ms * 10.0).round() / 10.0,
        "fill_ms_runs": times.iter().map(|t| (t * 10.0).round() / 10.0).collect::<Vec<_>>(),
        "rule_evals": s.evals,
        "rule_evals_ranked": s.ranked,
        "rule_evals_bits": s.bits,
        "rules": if bits { "bits" } else { "bytecode" },
        "absent": if skip { "skip" } else { "check" },
        "rules_report": seats[0].plan.rules_report(seats[0].w),
        "eval_only": replay,
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
        // The same hash in two rows means the same placements (the `noskip` rows against theirs).
        "placements_hash": format!("{:016x}", data_fill::check::fnv1a(format!("{:?}", f.placed).as_bytes())),
        "repairs": f.repairs,
        "swaps": f.swaps,
        "fallbacks": f.fallbacks,
        "mode_rule_evals": f.mode_evals,
        "peak_heap_bytes": heap,
        "peak_rss_after_fill_bytes": rss_after_fill,
        "peak_rss_bytes": peak_rss(),
    });
    let r = f.profile;
    let share = |x: u64| (x as f64 / r.solved.max(1) as f64 * 1e3).round() / 1e3;
    let ms = |ns: u64| (ns as f64 / 1e6 * 10.0).round() / 10.0;
    // One fill's rounds (the first, untimed fill's): counted, and its wall time by phase.
    out["rounds_profile"] = json!({
        "world_solves": r.solved, "changed_share": share(r.changed),
        "set_up_ms": ms(r.nanos[0]), "settle_ms": ms(r.nanos[1]), "pick_ms": ms(r.nanos[2]),
        "undone": r.undone, "journal_peak": r.journaled,
    });
    out["entrances"] = json!(shape.doors);
    out["layout"] = json!(match (shape.doors, vanilla) {
        (false, _) => None,
        (true, true) => Some("vanilla"),
        (true, false) => Some("shuffled"),
    });
    out["guards"] = json!(shape.guards);
    out["timeless"] = json!(shape.timeless);
    out["shuffle"] = shuffle_row;
    // One loaded world's heap: what a room shares, and what a copy per player would add.
    out["world_bytes"] = json!(kept(|| World::from_package(&files[0])).1);
    out["room_bytes"] = json!(room_bytes);
    out["room_bytes_per_player"] = json!(room_bytes.map(|b| b / players.max(1) as i64));
    out["bit_rules_ms"] = json!(fine(bit_rules_ms));
    out["seed"] = json!(seed);
    out["round"] = json!(if single { "one item" } else { "one item per player" });
    out["fill_reps"] = json!(reps);
    out["row_s"] = json!((row.elapsed().as_secs_f64() * 10.0).round() / 10.0);
    Ok(out)
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
        "wide" => Shape::wide(),
        "chain" => Shape::hostile(Hostile::Chain),
        "gated" => Shape::hostile(Hostile::Gated),
        "keys" => Shape::hostile(Hostile::Keys),
        "brink" => Shape::hostile(Hostile::Brink),
        "tight" => Shape::hostile(Hostile::Tight),
        x => {
            return Err(format!(
                "--profile: light, heavy, wide, chain, gated, keys, brink or tight, got {x}"
            ));
        }
    };
    let on = |b: bool| if b { "on" } else { "off" }.to_owned();
    // `--entrances vanilla`: the doors-shaped world, filled with no door moved.
    let vanilla = arg(&args, "--entrances", String::new())? == "vanilla";
    let flag = |name: &str, default: bool| match arg(&args, name, on(default))?.as_str() {
        "on" => Ok(true),
        "off" => Ok(false),
        x => Err(format!("{name}: on or off, got {x}")),
    };
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
        doors: vanilla || flag("--entrances", d.doors)?,
        guards: flag("--guards", d.guards)?,
        timeless: flag("--timeless", d.timeless)?,
        ..d
    };
    let workers: usize = arg(&args, "--workers", config::ROOM_FORWARD_WORKERS)?;
    let path_arg = arg(&args, "--path", "auto".to_owned())?;
    let path = match path_arg.as_str() {
        "auto" => None,
        "carry" => Some(Path::Carry),
        "forward" => Some(Path::Forward { workers }),
        "split" => Some(Path::Split),
        x => return Err(format!("--path: auto, carry, forward or split, got {x}")),
    };
    let mode_arg = arg(&args, "--mode", "exact".to_owned())?;
    let mode: Algorithm = mode_arg.parse()?;
    let one_world = match arg(&args, "--worlds", "each".to_owned())?.as_str() {
        "each" => false,
        "one" => true,
        x => return Err(format!("--worlds: each or one, got {x}")),
    };
    let pick = |name: &str, on: &str, off: &str| match arg(&args, name, on.to_owned())?.as_str() {
        x if x == on => Ok(true),
        x if x == off => Ok(false),
        x => Err(format!("{name}: {on} or {off}, got {x}")),
    };
    // `--rounds single`: for measurements, rounds of one item on the split path (issue 212).
    let single = pick("--rounds", "players", "single").map(|per_player| !per_player)?;
    let engine =
        (pick("--rules", "bits", "bytecode")?, pick("--absent", "skip", "check")?, single, vanilla);
    let seed: u64 = arg(&args, "--seed", 1)?;
    let reps: usize = arg(&args, "--reps", 5)?;
    let players: Vec<usize> = arg(&args, "--players", "1,10,20,50,100".to_owned())?
        .split(',')
        .map(|p| p.trim().parse().map_err(|_| format!("--players: bad count {p:?}")))
        .collect::<Result<_, _>>()?;
    let mut points = Vec::new();
    // `--failed-row`: a room that cannot be filled prints a row saying so, and the run goes on.
    let failed_row = args.iter().any(|a| a == "--failed-row");
    for n in players {
        let row = one(&shape, seed, n, reps, (path, workers, mode, one_world), engine);
        let mut row = match row {
            Err(e) if failed_row => {
                println!(
                    "{}",
                    json!({"room": n, "profile": profile, "path": path_arg, "seed": seed,
                           "round": if single { "one item" } else { "one item per player" },
                           "failed": e})
                );
                continue;
            }
            row => row?,
        };
        row["profile"] = json!(profile);
        row["mode"] = json!(mode_arg);
        points.push((n as f64, row["fill_ms"].as_f64().unwrap_or(0.0)));
        println!("{row}");
    }
    let fitted = exponent(&points);
    println!(
        "{}",
        json!({"profile": profile, "path": path_arg, "workers": workers, "mode": mode_arg,
               "rules": if engine.0 { "bits" } else { "bytecode" },
               "absent": if engine.1 { "skip" } else { "check" },
               "world": if one_world { "one" } else { "each" },
               "fitted_exponent": fitted, "points": points})
    );
    Ok(())
}
