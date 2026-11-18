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
8. **Test helpers are not published.**
9. **No heavy work on a developer machine.** Local runs are targeted. The full gate, real Postgres, load, soak and fuzz runs belong to CI or a named test rig.

## Layers

| Layer | Tool | Where it lives | When it runs |
|---|---|---|---|
| Unit | `cargo test`, `#[cfg(test)] mod tests` in the crate | next to the code | every PR (CI) |
| Golden vectors | fixed inputs with outputs from an independent source | `src/tests.rs` plus the reference in the crate's `reference/` | every commit |
| Property | `proptest` | `mod properties` in the same file | every PR with default cases; manual `nightly` with `PROPTEST_CASES=50000` |
| Snapshot | `insta` (JSON schema, OpenAPI, headers) | crate tests | every PR |
| Integration | real crate boundaries, in-memory fakes (`MemLogStore`) | crate `tests/` only when a crate boundary is the thing under test | every PR |
| Deterministic simulation | the `sim` crate: fake clock, network, store, seeded scheduler, crash injection | `crates/sim` | every PR bounded; manual `nightly` 10M schedules |
| **End to end** | real server binary, real Postgres, real WebSocket clients | `crates/server/tests/` with helpers in `crates/e2e` | every PR (loopback only, about a second) |
| Conformance | Autobahn suite, real Archipelago, Python `websockets`, browser clients | CI jobs (gate G-WS) | CI only |
| Fuzz | `cargo-fuzz` on every parser and decoder | `fuzz/` per crate | continuous in CI, stored corpus |
| Mutation | `cargo-mutants` on leaf crates | manual `nightly` workflow | on demand |
| Load, soak, chaos | `sim` load generator, `kill -9` script | test rig | on demand |

## End-to-end rules

- The e2e crate starts the **real server** on an ephemeral port against a real Postgres (a CI service container) and drives it with real WebSocket clients built on `sdk-core`. No mocks of our own code at this layer.
- Each scenario owns a fresh database schema and room; scenarios run in parallel and never share state.
- First scenarios, in order: connect and resume with a cursor; check a location and receive the item; duplicate `cid` is applied once; `kill -9` the server mid-batch and resume with zero loss or duplicates (gate 0 (c)); an old Archipelago client plays through `edge-compat`; compression negotiation (gate G-WS).
- Browser end to end (host console, lobby, pairing) uses Playwright against the running server, once the web UI exists.
- An e2e test that needs a sleep is a missing signal in the product: add the signal (an event, a metric), then wait on it.

## Postgres tests

- The cargo feature `pg-tests` (on `log` and `server`) turns on every test that needs Postgres: the `LogStore` contract suite against `PgLogStore`, the gate G-DB fence race and cancel tests (both in `crates/log/src/contract.rs`, so they also run on `MemLogStore`), and `crates/server/tests/restart.rs`. With the feature on and no `CONSTELLATION_DATABASE_URL`, they fail with a message saying so (rule 5: no silent skip). `just test-pg` runs them; The manual `properties` job in `nightly.yml` runs them against a Postgres 18 service container.
- Each test creates its own schema (`log::testing::fresh_schema`) and points its connections at it, so tests share one database and run in parallel. Schemas are not dropped; use a throwaway database.
- The throughput benchmark (feature `pg-bench`, `crates/log/tests/throughput.rs`, gate G-DB) runs in the manual `nightly` workflow only. It gates the shipped writer, `PgLogStore::append` (one transaction per room batch, 1,000 rooms, 20 ms windows), at 1,000 rows per second: 1.5 times the Stage 1 peak of 666 rows per second that doc 12 section 3.3 estimates (an estimate there, not a measurement). Measured on GitHub runners for the same writer code: 4,881, 5,130 and 1,780 rows per second (2026-11-16), so a higher floor would fail on runner variance (this path is one commit, one fsync, per room batch). It also reports a hand-written cross-room group commit with `UNNEST`; that number is the target for the group-commit writer of doc 02 section 5.4, which is not built yet, and is not gated until it is (its bar will be 60 percent of 20,000 rows per second). Group commit measured 65,751 to 91,320 rows per second on the same runs.
- G-DB status: fence (`FOR SHARE` on `room_owner`) and fence race test done; cancel safety (own task per call, connection closed on error) done and tested; session timeouts (`statement_timeout`, `lock_timeout`, `idle_in_transaction_session_timeout` from `config`) set on every connection; store throughput gated at 1,000 rows per second; group-commit writer, PgBouncer and `COPY` open.
- **Deferred:** the PgBouncer test of gate G-DB (writer behind PgBouncer in transaction mode with prepared statements). Stage 0 and 1 run Postgres on the same box with no pooler (doc 12), so there is nothing to test yet; it comes with the first pooler. `COPY` into a temp table is not benchmarked yet either (only `UNNEST`).

## Quality bars (CI)

- **Coverage:** line coverage with `cargo-llvm-cov` (`cargo llvm-cov --workspace`, on demand; not in CI while this is a POC), target 90 percent on the workspace; leaf crates (`proto`, `spec-rng`, `rules`, `room-core`) aim for 100 percent of branches that are reachable. A coverage drop needs a reason in the PR.
- **Mutation score:** `cargo-mutants` in the manual `nightly` workflow, on leaf crates; surviving mutants are triaged, not ignored. A survivor usually means a missing assertion.
- **Mutation runs:** the `nightly` workflow, sharded (`mutants-pg` for `log` and `server` with Postgres, `mutants` for the rest). Each mutant runs the whole workspace's tests under nextest's `mutants` profile (`.config/nextest.toml`), so a test that hangs on a mutant fails instead of timing out; a test that can hang must still fail fast on its own where it can (wait on a signal, not a timer). The harness crates `e2e` and `sim` are not mutated. An equivalent mutant is excluded by name in the workflow, with its reason in a comment at the code.
- **Test time:** a unit test over 1 second is a smell; the full unit suite should stay under about a minute.
- **The `nightly` workflow** (manual while this is a POC) uses larger property case counts and longer simulations than PR runs. A nightly failure blocks the next release, not just the next PR.

## Hooks

Run `just setup` once. Pre-commit: fmt check. Commit-msg: conventional commit, subject at most 50 characters. CI runs fmt and the unit tests (`--lib`); the full suite with Postgres is manual (`nightly.yml`, or `just test-pg`). Never use `--no-verify`; fix the finding.
