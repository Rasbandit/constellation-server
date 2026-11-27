# Contributing

Pick a task from [`docs/ROADMAP.md`](docs/ROADMAP.md) or an open issue, say in the issue that you are on it, and open one PR per task.

## License and contributor terms
This repo is under [PolyForm Strict 1.0.0](LICENSE.md): source-available for noncommercial use, no redistribution, no derived works. The owner plans to make it private later.

By opening a pull request you agree that:
- The owner (Rasbandit) gives you permission to clone this repo, change it, and push your changes to a fork or branch on GitHub, only to prepare and submit contributions to it. You may not publish or share your changes anywhere else.
- You grant the owner a perpetual, worldwide, royalty-free, irrevocable license to use, change, sublicense and relicense your contribution under any terms, including commercial and closed-source ones.
- The contribution is your own work, or you have the right to grant that license, and it contains no code, text or data copied from other projects without the owner's written OK.

## Setup
- Rust from `rust-toolchain.toml` (rustup picks it up), `just`, `cargo-deny`, `typos`.
- `just setup` once: it turns on the git hooks (a fmt check before a commit, the commit message format).
- Postgres is optional locally. CI (`.github/workflows/ci.yml`) runs format, unit, web, version and Postgres tests on every PR; sandbox (bwrap) tests and mutation tests are not run in CI for now (see #185). To run them yourself, set `CONSTELLATION_DATABASE_URL` and run `just test-pg`.
- The design docs live in the sibling `constellation` repo. Clone it next to this one.

## Workflow
- Every to-do is a GitHub issue. No `TODO` or `FIXME` in code or docs; open an issue and write `see #<n>` where the work is deferred.
- Branch off `main`: `feat/...`, `fix/...`, `docs/...`.
- Conventional commits, subject at most 50 characters (the hook checks).
- Tests first: a test that fails without your change and passes with it. Say which one in the PR.
- The engine is in early development. The same seed must give the same result within one version on every machine and on every path that exists; results may change between versions. A test that pins exact output is a snapshot: when a change to generation moves it, update it on purpose in a commit that says so ("re-pin after ..."), and list it in the PR. Nobody has to prove that no pin changed.
- CI must be green before merge. `just check` is optional while this is a POC.
- One reviewer who is not the author approves each PR.
- Lints are default (POC mode). No `unsafe`.
- Limits and environment variables live only in `crates/config`.

## Concurrency
Write all new code as if it runs on many cores, so scaling out never needs a rewrite. Existing code is not rewritten for this; it is brought in line only when an issue already changes that code.
- Shared data is immutable and shared by reference (`&T`, `Arc<T>`). Mutable state is owned by one task and passed in, never global. No `static mut`, no `Rc`/`RefCell` in library types; core types must be `Send + Sync`.
- Results never depend on thread count or scheduling. Each task gets its own RNG stream from `spec_rng::derive`, and parallel results are merged in a fixed order.
- Parallelise across independent units (seeds, rooms, worlds, checks), not inside one inherently sequential step such as the loop of a fill that picks a location for each item. Inside a room's fill, the reachability solves of the players' worlds are independent units and may run in parallel, when what they produce is merged in a fixed order (player order) and a test proves the result identical for 1 worker and for several; the picking stays sequential, on one random stream. Code that can run units in parallel takes its worker count from `crates/config`. Generation's one-job-at-a-time cap is still hardcoded in `crates/server/src/worlds.rs` (see #77). Its tests run with 1 worker and with several and expect the same result.
- CPU-heavy work (generation, compiling worlds, bulk checks) runs in a subprocess (as generation does) or on `spawn_blocking` / a worker pool, never on the async runtime's I/O threads.
- Locks guard small, short critical sections only. Never hold a `std::sync` lock across an `.await`. An async lock held across an `.await` only to order effects needs a comment saying why (see `edge-compat/src/presence.rs`).

## Talking about Archipelago
We build on Archipelago and want to stay good neighbours. Read the design repo's `CLAUDE.md` before writing any issue, PR or doc that mentions it. In short: be accurate and cite sources, credit their work, critique designs and never people, report a suspected bug to its maintainers before mentioning it anywhere public, and never copy their code, text or data into this repo. Text that mentions Archipelago gets a review against that file by someone other than its author before it is posted.
