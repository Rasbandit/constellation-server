//! `data-fill <manifest.json | package> <instance.json> [seed] [placements-out.json]`: load the
//! world (compiling a manifest, or reading a compiled package as is), compare reachability with
//! the instance's oracle (if present), fill once, validate, print a report.
//! `data-fill compile <manifest.json> <package>`: compile once per world version.
//! `data-fill decode <package>`: print the manifest the package's rules section encodes.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::Instant;

use data_fill::{Instance, Plan, World, decode, fill, placements, section, validate};
use serde::Deserialize;
use serde_json::{Value, json};
use spec_rng::SpecRng;

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 1e6).round() / 1e3
}

/// Compare one oracle solve (checks, regions with masks, event counts) with ours.
fn compare(w: &World, plan: &Plan, oracle: &Value, inventory: &[String]) -> Result<Value, String> {
    let extra = inventory.iter().map(|k| w.item(k)).collect::<Result<Vec<_>, _>>()?;
    let t = Instant::now();
    let s = plan.solve(w, &extra);
    let took = ms(t);
    let want: Vec<&str> = oracle["checks"]
        .as_array()
        .ok_or("oracle checks")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let got: Vec<&str> = (0..u32::try_from(w.locations()).unwrap_or(0))
        .filter(|l| s.reached(*l))
        .map(|l| w.loc_key(l))
        .collect();
    let only_py: Vec<&&str> = want.iter().filter(|k| !got.contains(k)).collect();
    let only_rs: Vec<&&str> = got.iter().filter(|k| !want.contains(k)).collect();
    let want_regions: BTreeMap<&str, u64> = oracle["regions"]
        .as_object()
        .ok_or("oracle regions")?
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_u64().unwrap_or(0)))
        .collect();
    let got_regions: BTreeMap<&str, u64> =
        s.reached_regions().map(|(r, m)| (w.region_key(r), m)).collect();
    let region_diff: Vec<String> = want_regions
        .keys()
        .chain(got_regions.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|k| want_regions.get(*k) != got_regions.get(*k))
        .map(|k| format!("{k}: py {:?} rs {:?}", want_regions.get(k), got_regions.get(k)))
        .collect();
    let want_events: BTreeMap<&str, i64> = oracle["events"]
        .as_object()
        .ok_or("oracle events")?
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_i64().unwrap_or(0)))
        .collect();
    let got_events: BTreeMap<&str, i64> = s
        .counters
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(c, n)| (w.counter_key(c), *n))
        .collect();
    Ok(json!({
        "checks_python": want.len(), "checks_engine": got.len(),
        "only_python": only_py, "only_engine": only_rs,
        "regions_python": want_regions.len(), "regions_engine": got_regions.len(),
        "region_mask_diffs": region_diff,
        "events_match": want_events == got_events,
        "python_solve_ms": oracle["seconds"].as_f64().map(|s| s * 1e3),
        "engine_solve_ms": took,
    }))
}

/// A world from a manifest (`.json`, compiled here) or a compiled package.
fn load(path: &str) -> Result<World, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if path.ends_with(".json") {
        let m: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))?;
        return World::load(&m);
    }
    World::from_package(&bytes)
}

fn run(args: &[String]) -> Result<Value, String> {
    match args {
        [_, cmd, manifest, out] if cmd == "compile" => {
            let text = std::fs::read(manifest).map_err(|e| format!("{manifest}: {e}"))?;
            let m: Value = serde_json::from_slice(&text).map_err(|e| format!("{manifest}: {e}"))?;
            let bytes = section::compile(&m)?;
            let w = World::from_package(&bytes)?;
            std::fs::write(out, &bytes).map_err(|e| format!("{out}: {e}"))?;
            return Ok(json!({"package_bytes": bytes.len(), "rules_bytes": w.rules_bytes()}));
        }
        [_, cmd, package] if cmd == "decode" => return Ok(decode::manifest(&load(package)?)),
        _ => {}
    }
    let [_, manifest, instance, rest @ ..] = args else {
        return Err(
            "usage: data-fill <manifest.json> <instance.json> [seed] [placements-out.json]".into(),
        );
    };
    let seed: u64 = rest.first().map_or(Ok(1), |s| s.parse().map_err(|e| format!("seed: {e}")))?;
    let t_all = Instant::now();
    let t = Instant::now();
    let inst_raw: Value =
        serde_json::from_str(&std::fs::read_to_string(instance).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let inst = Instance::deserialize(&inst_raw).map_err(|e| e.to_string())?;
    let parse_ms = ms(t);
    let t = Instant::now();
    let w = load(manifest)?;
    let plan = Plan::new(&w, &inst).map_err(|e| format!("{e:?}"))?;
    let compile_ms = ms(t);

    let mut oracle = json!(null);
    if let Some(o) = inst_raw.get("oracle") {
        let inv = |k: &str| -> Vec<String> {
            o[k].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        let mut sampled = (0, 0, Vec::new());
        for (k, smp) in
            o["samples"].as_array().map(Vec::as_slice).unwrap_or_default().iter().enumerate()
        {
            let inv: Vec<String> = smp["inventory"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            // The sample inventory already holds the start inventory.
            let r = compare(&w, &plan, smp, &inv)?;
            sampled.0 += 1;
            let same = r["only_python"].as_array().is_some_and(Vec::is_empty)
                && r["only_engine"].as_array().is_some_and(Vec::is_empty)
                && r["region_mask_diffs"].as_array().is_some_and(Vec::is_empty)
                && r["events_match"] == json!(true);
            if same {
                sampled.1 += 1;
            } else {
                sampled.2.push(json!({"sample": k, "diff": r}));
            }
        }
        oracle = json!({
            "no_items": compare(&w, &plan, &o["no_items"], &inst.start_inventory)?,
            "all_items": compare(&w, &plan, &o["all_items"], &inv("all_inventory"))?,
            "samples": {"total": sampled.0, "identical": sampled.1, "mismatches": sampled.2},
        });
    }

    let mut rng = SpecRng::from_u64(seed);
    let t = Instant::now();
    let filled =
        fill(&w, &plan, &mut rng, config::FILL_MAX_ATTEMPTS).map_err(|e| format!("{e:?}"))?;
    let fill_ms = ms(t);
    let t = Instant::now();
    let (beatable, reached) = validate(&w, &plan, &filled.placed);
    let validate_ms = ms(t);
    let progression = inst.progression(&w);
    let placed = placements(&w, &filled, &progression);
    if let Some(out) = rest.get(1) {
        std::fs::write(out, serde_json::to_string(&placed).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    Ok(json!({
        "seed": seed,
        "world": {"items": w.items(), "locations": w.locations(), "regions": w.regions(),
                  "defs": w.defs(), "rules_bytes": w.rules_bytes(),
                  "existing_locations": plan.included.len()},
        "rules": plan.rules_summary(&w),
        "oracle": oracle,
        "fill": {"placed": placed.len(), "attempts": filled.attempts, "solves": filled.solves,
                 "rule_evals": filled.stats.evals,
                 "work": filled.stats.summary(1, u64::from(filled.solves)), "beatable": beatable,
                 "reachable_locations": reached, "existing_locations": plan.included.len()},
        "ms": {"parse": parse_ms, "compile": compile_ms, "fill": fill_ms, "validate": validate_ms, "total": ms(t_all)},
    }))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("data-fill: {e}");
            ExitCode::FAILURE
        }
    }
}
