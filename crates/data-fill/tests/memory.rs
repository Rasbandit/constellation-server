//! Allocation checks for a solve, counted by `allocation-counter`'s global allocator
//! (feature `memory-harness`).

use allocation_counter::measure;
use data_fill::{Plan, Reach, World};
use serde_json::Value;

mod support;

#[test]
fn a_reused_solve_state_allocates_nothing() {
    let v = support::fixture("fairness_world");
    let (manifest, mut insts) = data_fill::room::from_doc(&v).expect("instance");
    let inst = insts.remove(0);
    let w = World::load(manifest).expect("world");
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

/// A world heavy in what is built at load: the keys world with one definition of sixteen ways
/// of thirty items each (overlapping runs of its items), read by every check beside the
/// check's own rule, so the bit rules written out per check are large
/// (`config::RULE_BITS_MAX_WORLD_TESTS` bounds them).
fn shared_definition() -> Value {
    use data_fill::synth::{Hostile, Shape, world};
    let (mut manifest, _) = world(&Shape::hostile(Hostile::Keys), 1, 0);
    let items: Vec<Value> = manifest["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| serde_json::json!({"has": i["key"]}))
        .collect();
    assert!(items.len() >= 100, "the keys world has items for sixteen different ways");
    let ways: Vec<Value> = (0..16)
        .map(|way| {
            let all: Vec<&Value> = items.iter().cycle().skip(way * 7).take(30).collect();
            serde_json::json!({"all": all})
        })
        .collect();
    manifest["defs"] = serde_json::json!({"big": {"any": ways}});
    for region in manifest["regions"].as_array_mut().expect("regions") {
        for check in region["checks"].as_array_mut().expect("checks") {
            check["rule"] = serde_json::json!({"all": [{"ref": "big"}, check["rule"]]});
        }
    }
    manifest
}

/// A prepared world's own count of the bytes it holds is never under what loading it left
/// allocated, and not far over: a cache that counts with it keeps its cap in real bytes. On
/// the synthetic profiles and on a world with large bit rules ([`shared_definition`]). What a
/// world holds per byte of its file differs between them by more than two times, so no
/// multiple of the file length would count them all.
#[test]
fn a_prepared_world_counts_the_bytes_it_holds() {
    use data_fill::prepared::{Prepared, build};
    use data_fill::synth::{Hostile, Shape, world};
    let synth = |shape: Shape| world(&shape, 1, 0).0;
    let worlds = [
        ("small", synth(Shape { checks: 200, ..Shape::default() })),
        ("light", synth(Shape::default())),
        ("heavy", synth(Shape::heavy())),
        ("wide", synth(Shape::wide())),
        ("keys", synth(Shape::hostile(Hostile::Keys))),
        ("shared definition", shared_definition()),
        ("doors", synth(Shape { doors: true, timeless: true, ..Shape::heavy() })),
    ];
    let mut ratios = Vec::new();
    for (name, mut manifest) in worlds {
        manifest["game"] = "Synth".into();
        let (file, _) = build(&manifest).expect("prepares");
        let mut loaded = None;
        let left = measure(|| loaded = Some(Prepared::open(&file).expect("opens")));
        let held = loaded.as_ref().map_or(0, Prepared::held_bytes) as i64;
        let real = left.bytes_current;
        println!("{name}: file {} bytes, counted {held}, allocated {real}", file.len());
        assert!(real <= held && held <= real * 3 / 2, "{name}: counted {held}, allocated {real}");
        ratios.push(held as f64 / file.len() as f64);
    }
    let (least, most) =
        ratios.iter().fold((f64::MAX, 0.0_f64), |(lo, hi), r| (lo.min(*r), hi.max(*r)));
    println!("held bytes per file byte: {least:.1} to {most:.1}");
    assert!(most > 2.0 * least, "no one multiple of the file length fits: {ratios:?}");
}
