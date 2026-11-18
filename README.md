# constellation-server

Rust server for Constellation, compatible with Archipelago clients through a single compat crate (`edge-compat`).
Design lives in the sibling `constellation` repo, `docs/design/05-rust-architecture.md` (workspace, section 6) and `DECISIONS.md` (D-78: all Rust).

Status: first milestone (D-100). One process hosts one room from a seed bundle, or one room per bundle in a folder, and serves clients compatible with Archipelago; the log is in Postgres when a database URL is set (the room survives a restart or a crash, D-98), otherwise in memory. A proof of concept, not for production use; the plan is [`docs/ROADMAP.md`](docs/ROADMAP.md) and how to help is [`CONTRIBUTING.md`](CONTRIBUTING.md). Text mentioning Archipelago needs the review in the design repo's `CLAUDE.md`. What the compatible edge supports, with the test that proves each part: [`docs/compat-status.md`](docs/compat-status.md).

## Layout
- `crates/proto`: wire ids and the error registry (doc 01 D1.7). Leaf crate, no IO. Also `Command`, `Event` and the id newtypes (`Cid`, `RoomId`, `Epoch`, `Timestamp`). `proto::cwp` holds the CWP v1 wire types (design doc 01 D1.10, proposal D-123): the frames, validated names, the integer range, the type and path registries, typed decode and encode, and a message codec that still answers a frame it cannot decode. The generated schema is committed at `crates/proto/schema/cwp-v1.json` (validate against `#/$defs/ClientFrame` and `#/$defs/ServerFrame`; the file's root also carries the registry, paths and error table under keys that are not JSON Schema keywords); a snapshot test compares it as JSON and, on a change, writes the new one to the system temp directory for review. Golden transcripts in `crates/proto/golden/` are replayed by a checker that enforces the D1.10 rules (ordering, dedupe, audience, replay, results), and a set of wrong transcripts proves each rule is enforced.
- `crates/spec-rng`: the pinned random stream and sampling rules (doc 01 D3.2), with golden vectors from an independent Python reference.
- `crates/config`: the only home of limits and the only crate that reads environment variables (doc 05 6.2 rule 5). `no_std` without the default `std` feature (only `load` needs std).
- `crates/room-core`: the pure room state machine (`RoomState`, `apply`, `fold`), `no_std` + alloc. No clock, no rng, no IO.
- `crates/log`: the `LogStore` seam with epoch-fenced append (doc 05 7.2), `MemLogStore` for tests and the simulator, and `PgLogStore` (feature `postgres`, sqlx, D-98). One contract suite (`src/contract.rs`) runs against both stores. Migrations are plain SQL in `migrations/`, applied by the server at start.
- `crates/sim`: the deterministic simulator. `cargo test -p sim` runs `SIM_SEEDS` seeds (default 64); `SIM_SEED=<n> cargo test -p sim` replays one.
- `crates/seed-bundle`: the JSON seed bundle schema (one room's slots, placements, data packages, `slot_data`, options) and its validation on load.
- `crates/runtime`: the room actor on tokio (D-99: an in-process seam). Applies command batches, appends them in one fenced write, publishes, then replies (the hold rule).
- `crates/edge-compat`: the Archipelago wire protocol as serde types and the per-connection session; the only crate that knows that protocol (doc 05 6.2 rule 2).
- `crates/server`: the `constellation-server` binary (axum for HTTP, yawc for the WebSocket, D-97).
- `crates/e2e`: end-to-end helpers (start the real binary, a yawc client, the fixture) and the fixture itself. The scenarios are integration tests of `server` (`crates/server/tests/compat.rs`), so cargo builds the binary and hands its path over (`CARGO_BIN_EXE_constellation-server`).
- `crates/e2e-ap`: the interop test with `archipelago_rs` (MIT), used as a path dependency from the sibling `Archipela-Go` checkout; no source of it is in this repo. Its own workspace, excluded from the main one, because cargo resolves every path dependency (even an optional one); CI does not have that checkout, so CI does not run it (run `just e2e-ap` locally before merging edge changes). Its lint table is a copy of the workspace's, kept equal by a test in `crates/e2e`.
- `tools/ap-import`: Python importer from a seed made by Archipelago's generator to our bundle.
- `tools/ap-client-check`: drives the real server with Archipelago's own `CommonClient` from the sibling checkout (connect, `SetNotify`, `Set`, `Get`, start items, client status). Local only: `just ap-client-check`.

Next crates, in doc 05 section 20 order: `edge-native`.

## Run the server
```sh
cargo run -p server --bin constellation-server -- --bundle crates/e2e/fixtures/two-slot.json --port 38281
```
Then point a client compatible with Archipelago at `ws://localhost:38281` with slot `Alice` or `Bob` and game `Archipela-Go 2: Electric Boogaloo`. Flags: `--host <ip>` (default 127.0.0.1).

### Several rooms
`--rooms <folder>` in place of `--bundle` starts one room per `*.json` seed bundle in the folder (at most `max_rooms`, 1 000, in `crates/config`). Each room gets a random room key (D-12: 10 characters, `a` to `z` and `2` to `7`) and logs it: `room ready key=<key> seed=<seed name>`. With Postgres the key is stored with the room (`migrations/0002_room_key.sql`), so a restart keeps it; in memory a restart draws new keys. `CONSTELLATION_ROOM_PASSWORD` applies to every room.

The folder is watched while the server runs: every `CONSTELLATION_ROOMS_SCAN_MS` (2 000) it starts a room for each new bundle, so adding a room is copying a file in. Write it under another name and rename it to `*.json`, so a scan never reads half a file. The folder may start empty. One room's trouble does not stop the others: a bundle that does not make a room (invalid, a seed name that already runs, a stored room made from another bundle, past `max_rooms`) is skipped with a warning and tried again when the file changes, and a room whose task fails is started again from its log on the next scan, under the same key. A fenced room still ends the process. A running room's file is not reread.

**Temporary:** a room from the folder is removed `CONSTELLATION_ROOM_LIFETIME_SECS` (86 400, 24 hours) after this process started it. Its clients are closed, new connections get 404, and its file is renamed `<name>.json.expired` so it does not come back; its data stays in the log store. This is a stopgap until rooms have a real lifecycle, see #56.

A client reaches a room through a hostname whose first label is its key (doc 01 section 3.8.1, hostname form): `ws://<key>.<host>:<port>`. The server reads the `Host` header of the WebSocket request; case does not matter and the port is ignored. With one room, every host reaches it (so `--bundle` and `ws://localhost:38281` keep working). With several, a host that names no room, or an unknown key, gets HTTP 404.

To try it on one machine without DNS, `sslip.io` names resolve to the IP inside them: `ws://<key>.127.0.0.1.sslip.io:38281` reaches a local server. A real deployment needs a wildcard DNS record (`*.<host>`) and a wildcard TLS certificate in front. `--port 0` picks a free port; the bound address is logged. `GET /healthz` answers `ok`. SIGTERM or Ctrl-C stops it.
The room password is never a flag (it would show in `ps`): set `CONSTELLATION_ROOM_PASSWORD`, or `CONSTELLATION_ROOM_PASSWORD_FILE` to a file whose first line is the password; it overrides the seed's. Limits come from `crates/config` (design values), some overridable by environment: `CONSTELLATION_MAX_CONNECTIONS` (10 000; over it, HTTP 503), `CONSTELLATION_CONNECT_TIMEOUT_MS` (30 000: a socket with no slot connected by then is closed), `CONSTELLATION_IDLE_TIMEOUT_MS` (60 000) and `CONSTELLATION_PING_INTERVAL_MS` (20 000).

### With Postgres (the room survives restarts)
Set `CONSTELLATION_DATABASE_URL` (or `CONSTELLATION_DATABASE_URL_FILE`, a file whose first line is the URL; never a flag, since the URL can hold a password) to a database the server may create tables in, for example `postgres://constellation@localhost/constellation`. At Stage 0 and 1 Postgres runs on the same box (doc 12), no pooler in between. On start the server applies the migrations, claims the room (bumping its owner epoch, so an older process still running can no longer write), and rebuilds it from the log before it listens; the log line `room loaded ... events=<n>` says how much it read. The room id comes from the seed name; a restart with a different bundle under the same seed name is refused. Without a URL the server logs `log store: IN MEMORY` as a warning and a restart loses the room. Pool size and session timeouts come from `crates/config` (design values): `db_pool_connections` 8, `statement_timeout` 10 s, `lock_timeout` 5 s, `idle_in_transaction_session_timeout` 10 s. A URL parameter sqlx does not recognise is refused at start (sqlx would log it with its value). If the room stops (its log refused or failed a write) the process exits non-zero so a supervisor restarts it and the room reloads; `GET /healthz` answers 503 `room stopped` meanwhile.

Exit status: 0 after SIGTERM or Ctrl-C, also during startup (it abandons startup); 3 when a newer owner took the room (the log line says `fenced by a newer owner current=<epoch>`): do not restart it, or two instances on one database fence each other in a loop; 1 for every other failure, including a room whose log write failed: restart it, it reloads from the log. A systemd unit (example, not shipped yet):
```ini
[Service]
ExecStart=/usr/local/bin/constellation-server --bundle /srv/constellation/seed.json --port 38281
EnvironmentFile=/etc/constellation/env
Restart=on-failure
RestartPreventExitStatus=3
```

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
Generator seed 7 reproduces the committed fixture exactly (checked 2026-11-17; Bob has a starting inventory). Importer tests: `python -m unittest discover -s tools/ap-import` (no Archipelago needed). `seed-bundle` has a test that loads the fixture, so a schema drift fails there.

## Licences
This repo is source-available, not open source: [PolyForm Strict 1.0.0](LICENSE.md). You may read, run and test it for noncommercial purposes; commercial use, redistribution and derived works need a written license from the owner. Contributors get the extra permissions in [`CONTRIBUTING.md`](CONTRIBUTING.md).

We depend on `yawc` (MPL-2.0, D-97) and on MIT and Apache-2.0 crates as cargo dependencies; none of their source is in this repo. `THIRD_PARTY_NOTICES.md` is not written yet: it becomes required when we distribute a binary (it must then carry the notices of every bundled crate), and is created then.

## Known deviations from doc 05 7.1 and 7.2
- `Item` events carry `from` (the checking slot) and no `item_uid`, and events have no ULID id (`seq` is the id). Location and item ids are `i64`, and a location id is only unique within its slot's world, so the placement table is keyed by (slot, location).
- `Command::Goal` and `EventKind::Goal` record a slot's goal (D-100 needs it); not in the doc 05 sketch.
- `fold(snapshot, limits, tail)` takes the limits and returns `Result`; `Ctx::limits` is a borrow.
- `Applied` has one `reply` plus `depends_on` (the hold rule: do not release a reply until that `seq` is durable), and no `effects`.
- `LogStore` has `read_cursors` and no outbox rows yet. `Cid` and `RoomId` are integers.
- The Postgres schema (`migrations/0001_log.sql`) is doc 02 section 5.5 cut down: no foreign keys, unencrypted event bodies, the latest snapshot only, cursors in `slot_cursors`. `seq`, `item_seq` and epochs above `i64::MAX` are refused by `PgLogStore` (Postgres has no unsigned 64-bit type).

## Rules (doc 05 6.2)
1. L0/L1 crates never depend on tokio, sqlx, axum, a clock or an OS RNG.
2. `unsafe` is forbidden workspace-wide; exceptions go in `docs/unsafe-register.md`.
3. Only `config` reads env vars.

## Testing
Rules, layers and the end-to-end plan: [`docs/TESTING.md`](docs/TESTING.md).
- `just check`: the gate (fmt, clippy, typos, every test that needs no database including the end-to-end scenarios, docs, cargo-deny).
- `just test-pg`: adds the Postgres tests (the store contract suite on Postgres, the gate G-DB fence race and cancel tests, and the `kill -9` restart scenarios in `crates/server/tests/restart.rs`). Needs `CONSTELLATION_DATABASE_URL`; each test makes its own schema. Without the URL they fail, they never skip. CI runs them against a Postgres 18 service container.
- `just bench-pg`: the gate G-DB throughput benchmark (the manual `nightly` workflow; not for a shared developer box).
- `just e2e`: the end-to-end scenarios alone (`cargo test -p server --test compat`).
- `just e2e-ap`: the `archipelago_rs` interop scenario; needs `../Archipela-Go`. It builds the server in the main workspace and passes its path at compile time (`CONSTELLATION_SERVER_BIN`).
- `just ap-client-check`: Archipelago's `CommonClient` against the real server; needs `../Archipela-Go` and its `apworld/.venv`. Run it before merging edge changes, like `just e2e-ap`.

## Dev setup
Run `just setup` once. It enables the hooks in `.githooks/`: pre-commit (fmt check), commit-msg (conventional commits, subject at most 50 chars). `just check` also needs `cargo-deny` and `typos`.
Lints: POC mode, compiler and clippy defaults plus `unsafe_code = "forbid"` (see `[workspace.lints]` in `Cargo.toml`). Warnings do not fail CI.
