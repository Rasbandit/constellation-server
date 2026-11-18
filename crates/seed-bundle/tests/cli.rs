//! The `bundle` binary's exit status (its commands are unit-tested in `src/bin/bundle.rs`).
use std::process::Command;

#[test]
fn a_bad_command_prints_the_usage_and_fails() {
    let out = Command::new(env!("CARGO_BIN_EXE_bundle")).arg("nonsense").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("usage: bundle"));
}

#[test]
fn a_good_command_prints_its_result_and_succeeds() {
    let dir = std::env::temp_dir().join(format!("bundle-exit-{}", std::process::id()));
    let input = concat!(env!("CARGO_MANIFEST_DIR"), "/../e2e/fixtures/cross-games.json");
    let out = Command::new(env!("CARGO_BIN_EXE_bundle"))
        .args(["v2-encode", input])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("bytes="));
}
