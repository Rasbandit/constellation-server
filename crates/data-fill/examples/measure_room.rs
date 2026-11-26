//! `measure_room <manifest.json | package> <players> <instance.json>... [seed]`: wall time and
//! heap of a room fill for `players` players of one world, instance files taken in turn (so
//! `measure_room m.json 10 a.json b.json` repeats the two). One line of numbers per
//! phase as in `measure`; the last line is the one the scaling table reads.
//!
//! The world is loaded once and shared by every seat. Each player's instance is parsed, resolved
//! into a plan and dropped before the next, so the peak includes one typed instance, as a
//! generation that streams its players would. Setup runs on one thread here because the counting
//! allocator counts per thread.

use std::time::Instant;

use allocation_counter::{AllocationInfo, measure};
use data_fill::room::{Compiled, fill_room_spheres};
use data_fill::{Instance, Plan, World, section};
use serde_json::Value;
use spec_rng::SpecRng;

#[derive(Default)]
struct Run {
    live: i64,
    peak: i64,
}

impl Run {
    fn phase<T>(&mut self, name: &str, f: impl FnOnce() -> T) -> (T, f64) {
        let mut out = None;
        let t = Instant::now();
        let a: AllocationInfo = measure(|| out = Some(f()));
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let peak = self.live + i64::try_from(a.bytes_max).unwrap_or(i64::MAX);
        self.peak = self.peak.max(peak);
        self.live += a.bytes_current;
        println!(
            "{name:<16} {ms:>9.3} ms  total {:>11}  peak {:>10}  held {:>10}  allocs {:>8}  live {:>10}",
            a.bytes_total, a.bytes_max, a.bytes_current, a.count_total, self.live
        );
        (out.unwrap_or_else(|| unreachable!("measure runs its closure")), ms)
    }
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let [_, manifest, players, rest @ ..] = args.as_slice() else {
        return Err(
            "usage: measure_room <manifest.json | package> <players> <instance.json>... [seed]"
                .into(),
        );
    };
    let n: usize = players.parse().map_err(|x| format!("players: {x}"))?;
    let (files, seed) = match rest.split_last() {
        Some((last, files)) if !files.is_empty() && !last.ends_with(".json") => {
            (files, last.parse::<u64>().map_err(|x| format!("seed: {x}"))?)
        }
        _ => (rest, 1),
    };
    if files.is_empty() || n == 0 {
        return Err("at least one player and one instance file".into());
    }
    let mut run = Run::default();
    let bytes = std::fs::read(manifest).map_err(e)?;
    let (w, _) = run.phase("load world", || {
        if manifest.ends_with(".json") {
            let m: Value = serde_json::from_slice(&bytes).map_err(e)?;
            World::from_package(&section::compile(&m)?)
        } else {
            World::from_package(&bytes)
        }
    });
    let w = w?;
    drop(bytes);
    let (setup, setup_ms) = run.phase("setup players", || {
        let (mut plans, mut progression) = (Vec::new(), Vec::new());
        for p in 0..n {
            let f = &files[p % files.len()];
            let inst: Instance = std::fs::File::open(f)
                .map_err(e)
                .and_then(|f| serde_json::from_reader(std::io::BufReader::new(f)).map_err(e))?;
            plans.push(Plan::new(&w, &inst).map_err(|x| format!("{x:?}"))?);
            progression.push(inst.progression(&w));
        }
        Ok::<_, String>((plans, progression))
    });
    let (plans, progression) = setup?;
    // One world for every seat; the players were resolved one by one above (see the top).
    let room = Compiled { worlds: vec![w], plans, progression };
    let seats = room.seats();
    let (filled, fill_ms) = run.phase("fill room", || {
        fill_room_spheres(&seats, &mut SpecRng::from_u64(seed), config::FILL_MAX_ATTEMPTS)
    });
    let (f, sp) = filled.map_err(|x| format!("{x:?}"))?;
    // Times without the counting allocator's bookkeeping in the closure: the median of five fills.
    let mut times: Vec<f64> = (0..5u8)
        .map(|k| {
            let t = Instant::now();
            drop(fill_room_spheres(
                &seats,
                &mut SpecRng::from_u64(seed + 1 + u64::from(k)),
                config::FILL_MAX_ATTEMPTS,
            ));
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let share: Vec<f64> = data_fill::room::cross_world_share(&seats, &f.placed)
        .map_err(|x| format!("{x:?}"))?
        .iter()
        .map(|(x, all)| *x as f64 / (*all).max(1) as f64)
        .collect();
    println!(
        "players {n}  setup_ms {setup_ms:.1}  fill_ms {fill_ms:.1}  fill_median_ms {:.1}  engine_peak {}  \
         attempts {}  solves {}  beatable {}  spheres {}  cross_share_mean {:.3}",
        times[2],
        run.peak,
        f.attempts,
        f.solves,
        f.all_beatable(),
        sp.spheres.len(),
        share.iter().sum::<f64>() / share.len() as f64
    );
    println!("work {}", f.stats.summary(1, u64::from(f.solves)));
    Ok(())
}
