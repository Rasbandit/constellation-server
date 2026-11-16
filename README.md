# constellation-server

Rust server for Constellation, compatible with Archipelago clients through a single compat crate (`edge-compat`, not yet created).
Design lives in the sibling `constellation` repo, `docs/design/05-rust-architecture.md` (workspace, section 6) and `DECISIONS.md` (D-78: all Rust).

Status: foundation only. Internal, not for external use; text mentioning Archipelago needs the review in the design repo's `CLAUDE.md`.

## Layout
- `crates/proto`: wire ids and the error registry (doc 01 D1.7). Leaf crate, no IO.
- `crates/config`: the only home of limits and the only crate that reads environment variables (doc 05 6.2 rule 5).

Next crates, in doc 05 section 20 order: `spec-rng`, `room-core`, `log`, `runtime`, `edge-native`, `sim`.

## Rules (doc 05 6.2)
1. L0/L1 crates never depend on tokio, sqlx, axum, a clock or an OS RNG.
2. `unsafe` is forbidden workspace-wide; exceptions go in `docs/unsafe-register.md`.
3. Only `config` reads env vars.
