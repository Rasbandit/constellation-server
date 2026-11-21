use super::*;

fn add(total: &mut Stats, s: Stats) {
    total.events = total.events.saturating_add(s.events);
    total.crashes = total.crashes.saturating_add(s.crashes);
    total.takeovers = total.takeovers.saturating_add(s.takeovers);
    total.fenced_writes = total.fenced_writes.saturating_add(s.fenced_writes);
    total.deduped = total.deduped.saturating_add(s.deduped);
    total.held_replies = total.held_replies.saturating_add(s.held_replies);
    total.lost_events = total.lost_events.saturating_add(s.lost_events);
}

#[test]
fn bounded_seeds_hold_every_invariant() {
    let limits = config::load().unwrap();
    let seeds: Vec<u64> =
        limits.sim_replay_seed.map_or_else(|| (0..limits.sim_seeds).collect(), |s| vec![s]);
    let mut total = Stats::default();
    for seed in seeds {
        add(&mut total, run_seed(seed, &limits).unwrap_or_else(|f| panic!("{f}")));
    }
    eprintln!("sim totals: {total:?}");
}

#[test]
fn the_default_seeds_really_inject_every_fault() {
    let limits = Limits::default();
    let mut total = Stats::default();
    for seed in 0..8 {
        add(&mut total, run_seed(seed, &limits).unwrap_or_else(|f| panic!("{f}")));
    }
    assert!(total.events > 0 && total.crashes > 0 && total.takeovers > 0, "{total:?}");
    assert!(total.fenced_writes > 0 && total.deduped > 0, "{total:?}");
    // Batching: replies were held for a later append, and crashes threw uncommitted events away.
    assert!(total.held_replies > 0 && total.lost_events > 0, "{total:?}");
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

#[test]
fn releasing_a_reply_before_its_event_is_durable_is_caught() {
    // The planted bug: the caller ignores `depends_on`. The sim must notice on some seed.
    let limits = Limits::default();
    let caught = (0..16).find_map(|seed| {
        let mut w = World::new(seed, limits).unwrap().release_early();
        (0..STEPS).find_map(|_| w.step().err())
    });
    let msg = caught.expect("the planted early release went unnoticed");
    assert!(msg.contains("before durable"), "{msg}");
}
