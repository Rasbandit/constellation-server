# constellation-server

Rust server for Constellation, compatible with Archipelago clients through a single compat crate (`edge-compat`, not yet created).
Design lives in the sibling `constellation` repo, `docs/design/05-rust-architecture.md` (workspace, section 6) and `DECISIONS.md` (D-78: all Rust).

Status: foundation only. Internal, not for external use; text mentioning Archipelago needs the review in the design repo's `CLAUDE.md`.

## Layout
- `crates/proto`: wire ids and the error registry (doc 01 D1.7). Leaf crate, no IO. Also `Command`, `Event` and the id newtypes (`Cid`, `RoomId`, `Epoch`, `Timestamp`).
- `crates/spec-rng`: the pinned random stream and sampling rules (doc 01 D3.2), with golden vectors from an independent Python reference.
- `crates/config`: the only home of limits and the only crate that reads environment variables (doc 05 6.2 rule 5). `no_std` without the default `std` feature (only `load` needs std).
- `crates/room-core`: the pure room state machine (`RoomState`, `apply`, `fold`), `no_std` + alloc. No clock, no rng, no IO.
- `crates/log`: the `LogStore` seam with epoch-fenced append (doc 05 7.2) and `MemLogStore` for tests and the simulator.
- `crates/sim`: the deterministic simulator. `cargo test -p sim` runs `SIM_SEEDS` seeds (default 64); `SIM_SEED=<n> cargo test -p sim` replays one.

Next crates, in doc 05 section 20 order: `runtime`, `edge-native`. Decision D-99: start as one process, so the edge talks to the room through an in-process seam and there is no network code yet.

## Known deviations from doc 05 7.1 and 7.2
- `Item` events carry no `item_uid`, and events have no ULID id (`seq` is the id).
- `fold(snapshot, limits, tail)` takes the limits and returns `Result`; `Ctx::limits` is a borrow.
- `Applied` has one `reply` plus `depends_on` (the hold rule: do not release a reply until that `seq` is durable), and no `effects`.
- `LogStore` has `read_cursors` and no outbox rows yet. `Cid` and `RoomId` are integers.

## Rules (doc 05 6.2)
1. L0/L1 crates never depend on tokio, sqlx, axum, a clock or an OS RNG.
2. `unsafe` is forbidden workspace-wide; exceptions go in `docs/unsafe-register.md`.
3. Only `config` reads env vars.

## Testing
Rules, layers and the end-to-end plan: [`docs/TESTING.md`](docs/TESTING.md).

## Dev setup
Run `just setup` once. It enables the hooks in `.githooks/`: pre-commit (fmt, clippy, typos, unit tests), commit-msg (conventional commits, subject at most 50 chars), pre-push (`just check`, the same gate as CI). Needs `just`, `cargo-deny`, `typos`.
Lints: every rustc and clippy group is `deny` (see `[workspace.lints]` in `Cargo.toml`), plus warnings as errors in CI. A new `allow` needs a `reason = "..."`.
