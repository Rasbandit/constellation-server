# constellation-server

Rust server for Constellation, compatible with Archipelago clients through a single compat crate (`edge-compat`).
Design lives in the sibling `constellation` repo, `docs/design/05-rust-architecture.md` (workspace, section 6) and `DECISIONS.md` (D-78: all Rust).

Status: first milestone (D-100). One process hosts one room from a seed bundle and serves clients compatible with Archipelago; the log is in memory (Postgres follows, D-98). Internal, not for external use; text mentioning Archipelago needs the review in the design repo's `CLAUDE.md`. What the compatible edge supports, with the test that proves each part: [`docs/compat-status.md`](docs/compat-status.md).

## Layout
- `crates/proto`: wire ids and the error registry (doc 01 D1.7). Leaf crate, no IO. Also `Command`, `Event` and the id newtypes (`Cid`, `RoomId`, `Epoch`, `Timestamp`).
- `crates/spec-rng`: the pinned random stream and sampling rules (doc 01 D3.2), with golden vectors from an independent Python reference.
- `crates/config`: the only home of limits and the only crate that reads environment variables (doc 05 6.2 rule 5). `no_std` without the default `std` feature (only `load` needs std).
- `crates/room-core`: the pure room state machine (`RoomState`, `apply`, `fold`), `no_std` + alloc. No clock, no rng, no IO.
- `crates/log`: the `LogStore` seam with epoch-fenced append (doc 05 7.2) and `MemLogStore` for tests and the simulator.
- `crates/sim`: the deterministic simulator. `cargo test -p sim` runs `SIM_SEEDS` seeds (default 64); `SIM_SEED=<n> cargo test -p sim` replays one.
- `crates/seed-bundle`: the JSON seed bundle schema (one room's slots, placements, data packages, `slot_data`, options) and its validation on load.
- `crates/runtime`: the room actor on tokio (D-99: an in-process seam). Applies command batches, appends them in one fenced write, publishes, then replies (the hold rule).
- `crates/edge-compat`: the Archipelago wire protocol as serde types and the per-connection session; the only crate that knows that protocol (doc 05 6.2 rule 2).
- `crates/server`: the `constellation-server` binary (axum for HTTP, yawc for the WebSocket, D-97).
- `crates/e2e`: end-to-end tests: the real binary over real WebSocket connections on loopback.
- `crates/e2e-ap`: the interop test with `archipelago_rs` (MIT), used as a path dependency from the sibling `Archipela-Go` checkout and not copied. Its own workspace, excluded from the main one, because cargo resolves every path dependency (even an optional one) and CI does not have that checkout.
- `tools/ap-import`: Python importer from a seed made by Archipelago's generator to our bundle.

Next crates, in doc 05 section 20 order: `edge-native`, then Postgres behind `LogStore` (D-98).

## Run the server
```sh
cargo run -p server --bin constellation-server -- --bundle crates/e2e/fixtures/two-slot.json --port 38281
```
Then point a client compatible with Archipelago at `ws://localhost:38281` with slot `Alice` or `Bob` and game `Archipela-Go 2: Electric Boogaloo`. Flags: `--host <ip>` (default 127.0.0.1), `--password <pw>` (overrides the bundle's). `--port 0` picks a free port; the bound address is logged. `GET /healthz` answers `ok`. SIGTERM or Ctrl-C stops it.

## Regenerate the fixture
The fixture is a two-slot seed of the owner's game (player options in `crates/e2e/fixtures/players/`), made with Archipelago's own generator and the Archipela-Go apworld, then converted by `tools/ap-import`. The raw `.archipelago` file is not committed.
```sh
ROOT=$PWD; AG=$(realpath ../Archipela-Go); PY=$AG/apworld/.venv/bin/python; OUT=$(mktemp -d)
cd $AG/.ap
PYTHONPATH=. $PY -c 'import runpy,sys,ModuleUpdate; ModuleUpdate.update_ran=True; sys.argv=sys.argv[1:]; runpy.run_path(sys.argv[0], run_name="__main__")' \
  Generate.py --player_files_path $ROOT/crates/e2e/fixtures/players --outputpath $OUT --seed 7
PYTHONPATH=. $PY $ROOT/tools/ap-import/ap_import.py $OUT/AP_*.zip $ROOT/crates/e2e/fixtures/two-slot.json
cd $ROOT
```
Generator seed 7 reproduces the committed fixture exactly (checked 2026-11-16). Importer tests: `python -m unittest discover -s tools/ap-import` (no Archipelago needed). `seed-bundle` has a test that loads the fixture, so a schema drift fails there.

## Known deviations from doc 05 7.1 and 7.2
- `Item` events carry `from` (the checking slot) and no `item_uid`, and events have no ULID id (`seq` is the id). Location and item ids are `i64`, and a location id is only unique within its slot's world, so the placement table is keyed by (slot, location).
- `Command::Goal` and `EventKind::Goal` record a slot's goal (D-100 needs it); not in the doc 05 sketch.
- `fold(snapshot, limits, tail)` takes the limits and returns `Result`; `Ctx::limits` is a borrow.
- `Applied` has one `reply` plus `depends_on` (the hold rule: do not release a reply until that `seq` is durable), and no `effects`.
- `LogStore` has `read_cursors` and no outbox rows yet. `Cid` and `RoomId` are integers.

## Rules (doc 05 6.2)
1. L0/L1 crates never depend on tokio, sqlx, axum, a clock or an OS RNG.
2. `unsafe` is forbidden workspace-wide; exceptions go in `docs/unsafe-register.md`.
3. Only `config` reads env vars.

## Testing
Rules, layers and the end-to-end plan: [`docs/TESTING.md`](docs/TESTING.md).
- `just check`: the gate (fmt, clippy, typos, unit and integration tests without e2e, docs, cargo-deny).
- `just e2e`: the end-to-end scenarios (builds the server binary itself).
- `just e2e-ap`: the `archipelago_rs` interop scenario; needs `../Archipela-Go`.

## Dev setup
Run `just setup` once. It enables the hooks in `.githooks/`: pre-commit (fmt, clippy, typos, unit tests), commit-msg (conventional commits, subject at most 50 chars), pre-push (`just check`, the same gate as CI). Needs `just`, `cargo-deny`, `typos`.
Lints: every rustc and clippy group is `deny` (see `[workspace.lints]` in `Cargo.toml`), plus warnings as errors in CI. A new `allow` needs a `reason = "..."`.
