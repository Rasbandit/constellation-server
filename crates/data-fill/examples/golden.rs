//! `golden <world-dir> <recorded-dir>`: placements byte-identical to the engine before the
//! compiled rules section, for seeds 1 to 20 of `instance_s1` and `instance_clock_s1`.
//! `<world-dir>` holds `manifest.json` and the instance dumps, `<recorded-dir>` the recorded
//! `<instance>_<seed>.json` placements. The test world cannot be committed (see
//! `docs/data-only-world-experiment.md`, source licenses), so this is an example run by hand.

use std::process::ExitCode;

use data_fill::{Instance, Plan, World, fill, placements};
use serde_json::Value;
use spec_rng::SpecRng;

fn run(dir: &str, golden: &str) -> Result<usize, String> {
    let e = |x: &dyn std::fmt::Display| x.to_string();
    let text = std::fs::read(format!("{dir}/manifest.json")).map_err(|x| e(&x))?;
    let m: Value = serde_json::from_slice(&text).map_err(|x| e(&x))?;
    let w = World::load(&m)?;
    drop((m, text));
    let mut differ = 0;
    for name in ["instance_s1", "instance_clock_s1"] {
        let f = std::fs::File::open(format!("{dir}/{name}.json")).map_err(|x| e(&x))?;
        let inst: Instance =
            serde_json::from_reader(std::io::BufReader::new(f)).map_err(|x| e(&x))?;
        let plan = Plan::new(&w, &inst).map_err(|x| format!("{x:?}"))?;
        let progression = inst.progression(&w);
        for seed in 1u64..=20 {
            let mut sb = [0u8; 32];
            sb[..8].copy_from_slice(&seed.to_le_bytes());
            let filled =
                fill(&w, &plan, &mut SpecRng::from_seed(sb), 20).map_err(|x| format!("{x:?}"))?;
            let got =
                serde_json::to_string(&placements(&w, &filled, &progression)).map_err(|x| e(&x))?;
            let want = std::fs::read_to_string(format!("{golden}/{name}_{seed}.json"))
                .map_err(|x| e(&x))?;
            let same = got == want;
            differ += usize::from(!same);
            println!("{name} seed {seed}: {}", if same { "identical" } else { "DIFFERS" });
        }
    }
    Ok(differ)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, golden] = args.as_slice() else {
        eprintln!("usage: golden <world-dir> <recorded-dir>");
        return ExitCode::from(2);
    };
    match run(dir, golden) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(n) => {
            eprintln!("golden: {n} fills differ");
            ExitCode::FAILURE
        }
        Err(x) => {
            eprintln!("golden: {x}");
            ExitCode::from(2)
        }
    }
}
