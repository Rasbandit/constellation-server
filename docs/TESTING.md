# Testing rules

Hard rules for every agent and human writing code here. They sit under the lint rules in `Cargo.toml` and the layers in design doc 05 section 16.2. CI enforces what it can; the rest is review.

## The rules

1. **Test first.** Write the failing test, watch it fail for the right reason, then write the code. A bug fix starts with a test that fails without the fix and passes with it; the PR says which.
2. **Never bend a test to fit code.** If a test and the code disagree, find out which is wrong. Changing an expected value needs a stated reason in the commit.
3. **Name the behaviour.** `the_server_never_warns_a_compressing_client`, not `test_3`. One behaviour per test. A failing name should say what broke.
4. **Cover the edges every time:** empty, one, max, zero, overflow, duplicate, reordered, truncated, hostile input, and the retry or timeout path. A test list with only the happy path is unfinished.
5. **No flakiness, ever.** No `sleep` to wait for something (use the simulated clock or an explicit signal). No real network except loopback on an ephemeral port. No shared global state, no test order dependence, no `#[ignore]` without a linked issue, no automatic retries. A flaky test is a bug in the test or the code: fix it the day it appears.
6. **Expected values come from outside the code under test.** For wire formats, crypto, randomness and rules: an independent implementation, a published spec vector, or a hand calculation, written down with its source (see `crates/spec-rng/reference/ref.py`). Never paste the output of the function you are testing as the expected value.
7. **Determinism is printed.** Anything random uses a seed that is logged on failure. A failing property or simulation seed becomes a permanent regression test.
8. **Tests obey the lints.** No `unwrap` shortcuts outside tests (they are allowed inside `#[cfg(test)]` by `clippy.toml`). No `allow` without a `reason`. Test helpers are not published.
9. **No heavy work on a developer machine.** Local runs are targeted. The full gate, real Postgres, load, soak and fuzz runs belong to CI or a named test rig.

## Layers

| Layer | Tool | Where it lives | When it runs |
|---|---|---|---|
| Unit | `cargo test`, `#[cfg(test)] mod tests` in the crate | next to the code | every commit (pre-commit runs `--lib`) |
| Golden vectors | fixed inputs with outputs from an independent source | `src/tests.rs` plus the reference in the crate's `reference/` | every commit |
| Property | `proptest` | `mod properties` in the same file | every PR with default cases; nightly with `PROPTEST_CASES=50000` |
| Snapshot | `insta` (JSON schema, OpenAPI, headers) | crate tests | every PR |
| Integration | real crate boundaries, in-memory fakes (`MemLogStore`) | crate `tests/` only when a crate boundary is the thing under test | every PR |
| Deterministic simulation | the `sim` crate: fake clock, network, store, seeded scheduler, crash injection | `crates/sim` | every PR bounded; nightly 10M schedules |
| **End to end** | real server binary, real Postgres, real WebSocket clients | `crates/e2e` (created with the first `edge-native` route) | CI only |
| Conformance | Autobahn suite, real Archipelago, Python `websockets`, browser clients | CI jobs (gate G-WS) | CI only |
| Fuzz | `cargo-fuzz` on every parser and decoder | `fuzz/` per crate | continuous in CI, stored corpus |
| Mutation | `cargo-mutants` on leaf crates | CI | nightly |
| Load, soak, chaos | `sim` load generator, `kill -9` script | test rig | nightly or on demand |

## End-to-end rules

- The e2e crate starts the **real server** on an ephemeral port against a real Postgres (a CI service container) and drives it with real WebSocket clients built on `sdk-core`. No mocks of our own code at this layer.
- Each scenario owns a fresh database schema and room; scenarios run in parallel and never share state.
- First scenarios, in order: connect and resume with a cursor; check a location and receive the item; duplicate `cid` is applied once; `kill -9` the server mid-batch and resume with zero loss or duplicates (gate 0 (c)); an old Archipelago client plays through `edge-compat`; compression negotiation (gate G-WS).
- Browser end to end (host console, lobby, pairing) uses Playwright against the running server, once the web UI exists.
- An e2e test that needs a sleep is a missing signal in the product: add the signal (an event, a metric), then wait on it.

## Quality bars (CI)

- **Coverage:** line coverage with `cargo-llvm-cov`, floor 90 percent on the workspace; leaf crates (`proto`, `spec-rng`, `rules`, `room-core`) aim for 100 percent of branches that are reachable. A coverage drop needs a reason in the PR.
- **Mutation score:** nightly `cargo-mutants` on leaf crates; surviving mutants are triaged, not ignored. A survivor usually means a missing assertion.
- **Test time:** a unit test over 1 second is a smell; the full unit suite should stay under about a minute.
- **Nightly runs** use larger property case counts and longer simulations than PR runs. A nightly failure blocks the next release, not just the next PR.

## Hooks

Run `just setup` once. Pre-commit: fmt, clippy, typos and unit tests (`cargo test --lib`). Commit-msg: conventional commit, subject at most 50 characters. Pre-push: `just check`, the same gate as CI minus the CI-only jobs. Never use `--no-verify`; fix the finding.
