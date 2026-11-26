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

/// A prepared world's own count of the bytes it holds is never under what loading it left
/// allocated, and not far over: a cache that counts with it keeps its cap in real bytes.
#[test]
fn a_prepared_world_counts_the_bytes_it_holds() {
    use data_fill::prepared::{Prepared, build};
    use data_fill::synth::{Shape, world};
    for shape in [Shape { checks: 200, ..Shape::default() }, Shape::default(), Shape::heavy()] {
        let (mut manifest, _) = world(&shape, 1, 0);
        manifest["game"] = "Synth".into();
        let (file, _) = build(&manifest).expect("prepares");
        let mut loaded = None;
        let left = measure(|| loaded = Some(Prepared::open(&file).expect("opens")));
        let held = loaded.as_ref().map_or(0, Prepared::held_bytes) as i64;
        let real = left.bytes_current;
        println!(
            "checks {}: file {} bytes, counted {held}, allocated {real}",
            shape.checks,
            file.len()
        );
        assert!(real <= held && held <= real * 3 / 2, "counted {held}, allocated {real}");
    }
}
