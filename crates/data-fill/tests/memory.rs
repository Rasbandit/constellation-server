//! Allocation checks for a solve, counted by `allocation-counter`'s global allocator
//! (feature `memory-harness`).

use allocation_counter::measure;
use data_fill::{Instance, Plan, Reach, World};
use serde::Deserialize;
use serde_json::Value;

#[test]
fn a_reused_solve_state_allocates_nothing() {
    let v: Value =
        serde_json::from_str(include_str!("fixtures/fairness_world.json")).expect("fixture parses");
    let inst = Instance::deserialize(&v["instance"]).expect("instance");
    let w = World::load(&v["manifest"]).expect("world");
    let plan = Plan::new(&w, &inst).expect("plan");
    let pool: Vec<u16> = inst.pool.iter().map(|k| w.item(k).expect("item")).collect();
    let locked = plan.locked_placed(&w);
    let mut r = Reach::new(&w, &plan);
    plan.solve_into(&mut r, &[], &locked);
    // Every buffer is sized once from the world; a fill step only resets them.
    for extra in [&[][..], &pool[..], &pool[..1]] {
        assert_eq!(measure(|| plan.solve_into(&mut r, extra, &locked)).count_total, 0);
    }
}
