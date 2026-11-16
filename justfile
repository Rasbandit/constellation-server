# One entry point for local checks. CI runs the same commands (see .github/workflows/ci.yml).
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
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Everything that needs no database. The Postgres half is `just test-pg`.
test:
    cargo test --workspace --locked

# The Postgres tests too: the log store contract suite, the G-DB race tests and the restart
# scenarios. Needs CONSTELLATION_DATABASE_URL (a database you may create schemas in; each test
# makes its own). Fails, never skips, without it. CI runs this in the `check` job.
test-pg:
    @test -n "${CONSTELLATION_DATABASE_URL:-}" || (echo "set CONSTELLATION_DATABASE_URL" >&2; exit 1)
    cargo test --workspace --locked --features log/pg-tests,server/pg-tests

# Gate G-DB throughput benchmark (nightly CI). Heavy: never on a shared developer box.
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

# Fast unit tests only; what the pre-commit hook runs.
unit:
    cargo test --workspace --lib --locked

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

deny:
    cargo deny check

typos:
    typos
