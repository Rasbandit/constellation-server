//! The memory harness on a bundle file of your own (never commit third-party bundles):
//! `cargo run -p server --features memory-harness --example measure -- <v1.json> [<load>]`.
//! `<load>` is the file the server loads (v1 or v2, default the v1 file); the v1 file names
//! the slots and locations the harness drives.
#![allow(
    unused_crate_dependencies,
    reason = "an example sees every dependency of the server and the e2e helpers"
)]

#[path = "../tests/support/memory.rs"]
mod harness;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(v1) = args.first() else {
        eprintln!("usage: measure <v1.json> [<file the server loads>]");
        std::process::exit(2);
    };
    let load = args.get(1).unwrap_or(v1);
    let text = std::fs::read_to_string(v1).unwrap();
    let b = seed_bundle::SeedBundle::from_json(&text).unwrap();
    let slots = i64::try_from(b.slots.len()).unwrap_or(i64::MAX);
    let n = harness::measure_bundle(std::path::Path::new(load), &b);
    println!("{}", n.row(load));
    println!("per active room = {} bytes", n.per_room(slots));
}
