# One entry point for local checks. CI runs fmt and the unit tests (see .github/workflows/ci.yml).
default: check

setup:
    git config core.hooksPath .githooks
    @command -v cargo-deny >/dev/null || echo "install: cargo install cargo-deny --locked"
    @command -v typos >/dev/null || echo "install: cargo install typos-cli --locked"

fmt:
    cargo fmt --all

check: fmt-check lint typos test doc deny

fmt-check:
    cargo fmt --all --check

lint:
    cargo clippy --workspace --all-targets --all-features --locked

# Everything that needs no database. The Postgres half is `just test-pg`.
test:
    cargo test --workspace --locked

# The Postgres tests too: the log store contract suite, the G-DB race tests and the restart
# scenarios. Needs CONSTELLATION_DATABASE_URL (a database you may create schemas in; each test
# makes its own). Fails, never skips, without it. The manual nightly `properties` job runs this.
test-pg:
    @test -n "${CONSTELLATION_DATABASE_URL:-}" || (echo "set CONSTELLATION_DATABASE_URL" >&2; exit 1)
    cargo test --workspace --locked --features log/pg-tests,server/pg-tests

# The upload and generation API through the real bubblewrap sandbox, with a fake sidecar (no
# Archipelago). Needs bwrap and python3; fails, never skips, without them. Local only for now.
test-sandbox:
    cargo test -p server --features sandbox-tests --test worlds --locked

# The sidecar's Python tests (zip and yaml checks, the bundle converter). Light; CI runs them.
test-py:
    python3 -m unittest discover -s tools/ap-gen
    python3 -m unittest discover -s tools/ap-import

# Gate G-DB throughput benchmark (manual nightly workflow). Heavy: never on a shared developer box.
bench-pg:
    @test -n "${CONSTELLATION_DATABASE_URL:-}" || (echo "set CONSTELLATION_DATABASE_URL" >&2; exit 1)
    cargo test --release --locked -p log@0.0.0 --features pg-bench --test throughput -- --nocapture

# The end-to-end scenarios alone: the real server binary over real WebSocket connections
# (integration tests of the server package; cargo builds the binary for them).
e2e:
    cargo test -p server --test compat --locked

# The same core scenario driven by archipelago_rs, a real Archipelago client library. Needs the
# sibling Archipela-Go checkout, so it is a separate crate outside the workspace (see README).
e2e-ap:
    cargo build -p server --bin constellation-server --locked
    CONSTELLATION_SERVER_BIN={{justfile_directory()}}/target/debug/constellation-server cargo test --manifest-path crates/e2e-ap/Cargo.toml --target-dir target --locked --features ap-interop

# Fast unit tests only.
unit:
    cargo test --workspace --lib --locked

doc:
    cargo doc --workspace --no-deps --all-features --locked

deny:
    cargo deny check

typos:
    typos

# Archipelago's own CommonClient (from the sibling Archipela-Go checkout and its venv) against
# the real server: connect, SetNotify, Set, Get, start items, client status. Local only.
ap-client-check:
    cargo build -p server --bin constellation-server --locked
    {{justfile_directory()}}/../Archipela-Go/apworld/.venv/bin/python tools/ap-client-check/check_common_client.py --ap {{justfile_directory()}}/../Archipela-Go/.ap --server {{justfile_directory()}}/target/debug/constellation-server --bundle {{justfile_directory()}}/crates/e2e/fixtures/two-slot.json

# A real seed generation through the admin API and the sandbox: one apworld and one player yaml
# (yours; they are never committed), the sibling Archipela-Go checkout and its venv. Local only.
ap-gen-check apworld=(home_directory() / "Downloads/mm_2ship.apworld") yaml=(home_directory() / "Downloads/Majoras Mask.yaml"):
    cargo build -p server --bin constellation-server --locked
    tools/ap-gen/check.sh "{{apworld}}" "{{yaml}}" {{justfile_directory()}}/../Archipela-Go/.ap {{justfile_directory()}}/../Archipela-Go/apworld/.venv/bin/python

# The whole path a user takes, local only: upload an apworld and a yaml through the admin API,
# generate in the sandbox, wait until the server hosts the world as a room (--rooms), then
# Archipelago's own CommonClient joins it by room key as `slot` and checks a location. The
# files are never copied into the repo.
e2e-full apworld=(home_directory() / "Downloads/mm_2ship.apworld") yaml=(home_directory() / "Downloads/Majoras Mask.yaml") slot="Rasbandit-MM" game="2 Ship 2 Harkinian (MM)":
    cargo build -p server --bin constellation-server -p seed-bundle --bin bundle --locked
    tools/ap-gen/check.sh "{{apworld}}" "{{yaml}}" {{justfile_directory()}}/../Archipela-Go/.ap {{justfile_directory()}}/../Archipela-Go/apworld/.venv/bin/python \
        {{justfile_directory()}}/../Archipela-Go/apworld/.venv/bin/python tools/ap-client-check/play_generated.py --ap {{justfile_directory()}}/../Archipela-Go/.ap --slot "{{slot}}" --game "{{game}}"
