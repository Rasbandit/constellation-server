//! `fill-fairness`: engine fairness of our fill against an unbiased baseline (uniform random
//! placements, unbeatable ones discarded). Meant for a small world and many seeds (nightly).
//! See `docs/port-check.md`.

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Instant;

use data_fill::fairness::{Flat, report, report_named, sample, sample_room};
use data_fill::room;
use data_fill::{Instance, Plan, World};
use serde_json::{Value, json};

const USAGE: &str = "usage: fill-fairness (--world <world.json> | --manifest <m.json> --instance <i.json>)
    [--seeds N] [--baseline N] [--max-attempts N] [--fail-p P] [--fail-deviation D] [--out report.json]";

fn read(p: &str) -> Result<Value, String> {
    serde_json::from_str(&std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?)
        .map_err(|e| format!("{p}: {e}"))
}

fn run(args: &[String]) -> Result<(Value, bool), String> {
    let mut m: HashMap<&str, &str> = HashMap::new();
    let mut it = args.iter().skip(1);
    while let Some(k) = it.next() {
        m.insert(
            k.as_str(),
            it.next().ok_or_else(|| format!("{k}: missing value\n{USAGE}"))?.as_str(),
        );
    }
    let num = |k: &str, d: u64| {
        m.get(k).map_or(Ok(d), |v| v.parse::<u64>().map_err(|e| format!("{k}: {e}")))
    };
    // A room: `--world` with an `instances` list is one world played by several players.
    if let Some(w) = m.get("--world") {
        let v = read(w)?;
        if let Some(list) = v["instances"].as_array() {
            let (seeds, baseline) =
                (num("--seeds", 2000)?, num("--baseline", num("--seeds", 2000)?)?);
            let max_attempts = num("--max-attempts", baseline.saturating_mul(1000))?;
            return room_report(&v["manifest"], list, (seeds, baseline, max_attempts), &m);
        }
    }
    let (manifest, inst_raw) = match (m.get("--world"), m.get("--manifest"), m.get("--instance")) {
        (Some(w), _, _) => {
            let v = read(w)?;
            (v["manifest"].clone(), v["instance"].clone())
        }
        (None, Some(a), Some(b)) => (read(a)?, read(b)?),
        _ => return Err(USAGE.into()),
    };
    let seeds = num("--seeds", 2000)?;
    let baseline = num("--baseline", seeds)?;
    let max_attempts = num("--max-attempts", baseline.saturating_mul(1000))?;
    let inst: Instance = serde_json::from_value(inst_raw).map_err(|e| format!("instance: {e}"))?;
    let w = World::load(&manifest)?;
    let plan = Plan::new(&w, &inst).map_err(|e| format!("{e:?}"))?;
    let pool = inst.pool.iter().map(|k| w.item(k)).collect::<Result<Vec<_>, _>>()?;
    let prog = inst.progression(&w);
    let t = Instant::now();
    let s = sample(&w, &plan, &pool, &prog, seeds, baseline, max_attempts);
    finish(report(&w, &s), s.failed == 0, t, &m)
}

/// The fairness run of one room: the same report, over every player's world.
fn room_report(
    manifest: &Value,
    list: &[Value],
    (seeds, baseline, max_attempts): (u64, u64, u64),
    m: &HashMap<&str, &str>,
) -> Result<(Value, bool), String> {
    let insts = room::instances(list)?;
    let insts: Vec<&Instance> = insts.iter().collect();
    let room = room::Compiled::shared(World::load(manifest)?, &insts, config::FILL_SETUP_WORKERS)?;
    let seats = room.seats();
    let flat = Flat::new(&seats)?;
    let t = Instant::now();
    let s =
        sample_room(&seats, &flat, seeds, baseline, max_attempts).map_err(|e| format!("{e:?}"))?;
    let mut r = report_named(&|i| flat.item(i), &|l| flat.loc(l), &s);
    r["players"] = json!(seats.len());
    finish(r, s.failed == 0, t, m)
}

/// Add timing and the gates, write `--out`.
fn finish(
    mut r: Value,
    mut ok: bool,
    t: Instant,
    m: &HashMap<&str, &str>,
) -> Result<(Value, bool), String> {
    let real =
        |k: &str| m.get(k).map(|v| v.parse::<f64>().map_err(|e| format!("{k}: {e}"))).transpose();
    r["seconds"] = json!(t.elapsed().as_secs_f64());
    if let Some(p) = real("--fail-p")? {
        ok &= r["baseline"]["p_value"].as_f64().is_none_or(|x| x >= p);
    }
    if let Some(d) = real("--fail-deviation")? {
        ok &= r["baseline"]["max_deviation"]["value"].as_f64().is_none_or(|x| x <= d);
    }
    r["pass"] = json!(ok);
    if let Some(out) = m.get("--out") {
        std::fs::write(out, serde_json::to_string_pretty(&r).unwrap_or_default())
            .map_err(|e| format!("{out}: {e}"))?;
    }
    Ok((r, ok))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok((r, ok)) => {
            println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
            if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(e) => {
            eprintln!("fill-fairness: {e}");
            ExitCode::from(2)
        }
    }
}
