//! `measure <manifest.json | package> <instance.json> [seed]`: wall time and heap per phase of
//! one world generation, counted by `allocation-counter`'s global allocator (feature `memory-harness`).
//!
//! Engine phases run first and in the order a generation needs them; the oracle comparison
//! (the instance's recorded solves, a test-only cost) runs after and is reported apart.
//! Per phase: `total` bytes allocated, `peak` live bytes above the phase start, `held` net bytes
//! still live when it ends. `process_peak` is the live heap at the phase start plus its peak.
//! Allocator overhead per block is not included.

use std::time::Instant;

use allocation_counter::{AllocationInfo, measure};
use data_fill::{Instance, Plan, Reach, World, fill, fill_spheres, placements, section};
use serde_json::Value;
use spec_rng::SpecRng;

#[derive(Default)]
struct Run {
    live: i64,
    peak: i64,
}

impl Run {
    fn phase<T>(&mut self, name: &str, f: impl FnOnce() -> T) -> T {
        let mut out = None;
        let t = Instant::now();
        let a: AllocationInfo = measure(|| out = Some(f()));
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let peak = self.live + i64::try_from(a.bytes_max).unwrap_or(i64::MAX);
        self.peak = self.peak.max(peak);
        self.live += a.bytes_current;
        println!(
            "{name:<16} {ms:>9.3} ms  total {:>11}  peak {:>10}  held {:>10}  allocs {:>8}  \
             live {:>10}  process_peak {:>10}",
            a.bytes_total, a.bytes_max, a.bytes_current, a.count_total, self.live, peak
        );
        out.unwrap_or_else(|| unreachable!("measure runs its closure"))
    }
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let [_, manifest, instance, rest @ ..] = args.as_slice() else {
        return Err("usage: measure <manifest.json | package> <instance.json> [seed]".into());
    };
    // A manifest is compiled first (reported apart) into a package next to it.
    let package =
        if manifest.ends_with(".json") { format!("{manifest}.pkg") } else { manifest.clone() };
    let seed: u64 = rest.first().map_or(Ok(1), |s| s.parse().map_err(|e| format!("seed: {e}")))?;
    let mut run = Run::default();

    if manifest.ends_with(".json") {
        println!("-- once per world version");
        let text = run.phase("read manifest", || std::fs::read(manifest)).map_err(e)?;
        let m: Value = run.phase("parse manifest", || serde_json::from_slice(&text)).map_err(e)?;
        let pkg = run.phase("compile section", || section::compile(&m))?;
        run.phase("drop manifest", || drop((m, text)));
        std::fs::write(&package, &pkg).map_err(e)?;
        drop(pkg);
        run = Run::default();
    }

    println!("-- engine");
    let file = run.phase("read package", || std::fs::read(&package)).map_err(e)?;
    let w = run.phase("load section", || World::from_package(&file))?;
    run.phase("drop file", || drop(file));
    // Typed and streamed: the recorded oracle in the file is skipped, never held.
    let inst: Instance = run.phase("parse instance", || {
        std::fs::File::open(instance)
            .map_err(e)
            .and_then(|f| serde_json::from_reader(std::io::BufReader::new(f)).map_err(e))
    })?;
    let plan = run.phase("plan", || Plan::new(&w, &inst)).map_err(|x| format!("{x:?}"))?;
    let progression = run.phase("progression", || inst.progression(&w));
    let all: Vec<u16> = inst.pool.iter().filter_map(|k| w.item(k).ok()).collect();
    let pool_keys = inst.pool.len();
    run.phase("drop instance", || drop(inst));
    let locked = plan.locked_placed(&w);
    run.phase("one solve", || drop(plan.state(&w, &all, &locked)));
    run.phase("288 x solve", || {
        let mut r = Reach::new(&w, &plan);
        for _ in 0..288 {
            plan.solve_into(&mut r, &all, &locked);
        }
    });
    println!("rules_bytes {}  pool {pool_keys}", w.rules_bytes());
    let mut rng = SpecRng::from_u64(seed);
    let filled = run
        .phase("fill", || fill(&w, &plan, &mut rng, config::FILL_MAX_ATTEMPTS))
        .map_err(|x| format!("{x:?}"))?;
    // Beatability and spheres come from the fill's own state, no fresh sweep.
    let (beatable, reached) = (filled.beatable, filled.reached);
    let mut rng2 = SpecRng::from_u64(seed);
    let sp = run.phase("fill + spheres", || {
        fill_spheres(&w, &plan, &mut rng2, config::FILL_MAX_ATTEMPTS, &progression)
    });
    let rounds = sp.map_err(|x| format!("{x:?}"))?.1.spheres.len();
    let bytes = run.phase("write", || {
        serde_json::to_string(&placements(&w, &filled, &progression)).map(|s| s.len())
    });
    println!(
        "engine_peak {}  solves {}  attempts {}  beatable {beatable}  reached {reached}  \
         spheres {rounds}  placements_bytes {}",
        run.peak,
        filled.solves,
        filled.attempts,
        bytes.unwrap_or(0)
    );
    println!("work {}", filled.stats.summary(1, u64::from(filled.solves)));

    println!("-- oracle (test only)");
    let i_text = run.phase("read oracle", || std::fs::read_to_string(instance)).map_err(e)?;
    let raw: Value = run.phase("parse oracle", || serde_json::from_str(&i_text)).map_err(e)?;
    let o = &raw["oracle"];
    let keys = |v: &Value| -> Vec<u16> {
        v.as_array()
            .map(|a| a.iter().filter_map(|k| w.item(k.as_str()?).ok()).collect())
            .unwrap_or_default()
    };
    let mut invs = vec![keys(&raw["start_inventory"]), keys(&o["all_inventory"])];
    invs.extend(
        o["samples"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|s| keys(&s["inventory"])),
    );
    let n = invs.len();
    run.phase("oracle solves", || {
        for inv in &invs {
            drop(plan.solve(&w, inv));
        }
    });
    println!("oracle_solves {n}  process_peak {}", run.peak);
    Ok(())
}
