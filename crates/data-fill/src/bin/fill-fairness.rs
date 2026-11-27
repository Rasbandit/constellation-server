//! `fill-fairness`: engine fairness of our fill against an unbiased baseline (uniform random
//! placements, unbeatable ones discarded). Meant for a small world and many seeds (nightly).
//! See `docs/port-check.md`.

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Instant;

use data_fill::fairness::{Flat, report, report_named, sample, sample_room, shape, shape_report};
use data_fill::room;
use data_fill::{Plan, World};
use serde_json::{Value, json};

const USAGE: &str = "usage: fill-fairness (--world <world.json> | --manifest <m.json> --instance <i.json>)
    [--seeds N] [--baseline N] [--max-attempts N] [--fail-p P] [--fail-deviation D] [--out report.json]
  or, for the shape of the fill's seeds on one room:
    fill-fairness (--world <world.json> | --synth light|heavy|mixed [--players N] [--checks N] [--room-seed S])
    --shape on [--seeds N] [--required N] [--baseline N]";

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
    if m.get("--shape") == Some(&"on") {
        return shape_of(&m, &num);
    }
    let seeds = num("--seeds", 2000)?;
    let baseline = num("--baseline", seeds)?;
    let max_attempts = num("--max-attempts", baseline.saturating_mul(1000))?;
    let doc = match (m.get("--world"), m.get("--manifest"), m.get("--instance")) {
        (Some(w), _, _) => read(w)?,
        (None, Some(a), Some(b)) => json!({"manifest": read(a)?, "instance": read(b)?}),
        _ => return Err(USAGE.into()),
    };
    // A room: `--world` with an `instances` list is one world played by several players.
    if doc["instances"].is_array() {
        return room_report(&doc, (seeds, baseline, max_attempts), &m);
    }
    let (manifest, mut insts) = room::from_doc(&doc)?;
    let inst = insts.remove(0);
    let w = World::load(manifest)?;
    let plan = Plan::new(&w, &inst)?;
    let pool = inst.pool.iter().map(|k| w.item(k)).collect::<Result<Vec<_>, _>>()?;
    let prog = inst.progression(&w);
    let t = Instant::now();
    let s = sample(&w, &plan, &pool, &prog, seeds, baseline, max_attempts);
    finish(report(&w, &s), s.failed == 0, t, &m)
}

/// The fairness run of one room: the same report, over every player's world.
fn room_report(
    doc: &Value,
    (seeds, baseline, max_attempts): (u64, u64, u64),
    m: &HashMap<&str, &str>,
) -> Result<(Value, bool), String> {
    let room = room::Compiled::from_doc(doc)?;
    let seats = room.seats();
    let flat = Flat::new(&seats)?;
    let t = Instant::now();
    let s = sample_room(&seats, &flat, seeds, baseline, max_attempts)?;
    let mut r = report_named(&|i| flat.item(i), &|l| flat.loc(l), &s);
    r["players"] = json!(seats.len());
    finish(r, s.failed == 0, t, m)
}

/// The fill's seeds over one room: their shape (spheres, where progression lies, waiting,
/// required spheres for the first `--required` seeds), and with `--baseline N` (a small world)
/// their distance from N uniform beatable rooms.
fn shape_of(
    m: &HashMap<&str, &str>,
    num: &dyn Fn(&str, u64) -> Result<u64, String>,
) -> Result<(Value, bool), String> {
    let c = match (m.get("--world"), m.get("--synth")) {
        (Some(w), _) => room::Compiled::from_doc(&read(w)?)?,
        (None, Some(profile)) => {
            use data_fill::synth::{Hostile, Shape};
            let checks = usize::try_from(num("--checks", 1000)?).map_err(|e| e.to_string())?;
            let sized = |d: Shape| Shape { checks, ..d };
            // `mixed`: chain, light, keys and heavy worlds in turn (piles of different
            // lengths and shapes in one room).
            let shapes = match *profile {
                "light" => vec![sized(Shape::default())],
                "heavy" => vec![sized(Shape::heavy())],
                "mixed" => vec![
                    Shape::hostile(Hostile::Chain),
                    sized(Shape::default()),
                    Shape::hostile(Hostile::Keys),
                    sized(Shape::heavy()),
                ],
                x => return Err(format!("--synth: light, heavy or mixed, got {x}")),
            };
            let players = usize::try_from(num("--players", 1)?).map_err(|e| e.to_string())?;
            data_fill::synth::mixed(&shapes, num("--room-seed", 1)?, players)?
        }
        _ => return Err(USAGE.into()),
    };
    let seats = c.seats();
    let flat = Flat::new(&seats)?;
    let (seeds, required, baseline) =
        (num("--seeds", 200)?, num("--required", 0)?, num("--baseline", 0)?);
    let max_attempts = num("--max-attempts", baseline.saturating_mul(1000))?;
    let t = Instant::now();
    let err = |e| format!("{e:?}");
    let mut out = shape_report(&shape(&seats, seeds, required).map_err(err)?);
    if baseline > 0 {
        let sampled = sample_room(&seats, &flat, seeds, baseline, max_attempts).map_err(err)?;
        out["against_uniform"] =
            report_named(&|i| flat.item(i), &|l| flat.loc(l), &sampled)["baseline"].clone();
    }
    out["players"] = json!(seats.len());
    finish(out, true, t, m)
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
