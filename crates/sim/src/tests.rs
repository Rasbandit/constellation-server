use super::*;

#[test]
fn bounded_seeds_hold_every_invariant() {
    let limits = config::load();
    let seeds: Vec<u64> =
        limits.sim_replay_seed.map_or_else(|| (0..limits.sim_seeds).collect(), |s| vec![s]);
    let mut total = Stats::default();
    for seed in seeds {
        match run_seed(seed, &limits) {
            Ok(s) => {
                total.crashes += s.crashes;
                total.takeovers += s.takeovers;
                total.fenced_writes += s.fenced_writes;
                total.deduped += s.deduped;
                total.events += s.events;
            }
            Err(f) => panic!("{f}"),
        }
    }
    eprintln!("sim totals: {total:?}");
}

#[test]
fn the_default_seeds_really_inject_every_fault() {
    let limits = Limits::default();
    let mut total = Stats::default();
    for seed in 0..8 {
        let s = run_seed(seed, &limits).unwrap_or_else(|f| panic!("{f}"));
        total.crashes += s.crashes;
        total.takeovers += s.takeovers;
        total.fenced_writes += s.fenced_writes;
        total.deduped += s.deduped;
        total.events += s.events;
    }
    assert!(total.events > 0 && total.crashes > 0 && total.takeovers > 0, "{total:?}");
    assert!(total.fenced_writes > 0 && total.deduped > 0, "{total:?}");
}

#[test]
fn a_seed_replays_to_the_same_result() {
    let limits = Limits::default();
    assert_eq!(run_seed(3, &limits), run_seed(3, &limits));
}

#[test]
fn a_failure_prints_its_seed_and_the_replay_command() {
    let f = SimFailure { seed: 42, step: 7, message: "boom".into() };
    let s = f.to_string();
    assert!(s.contains("seed 42") && s.contains("SIM_SEED=42"), "{s}");
}
