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

test:
    cargo test --workspace --all-features --locked --exclude e2e

# The real server binary over real WebSocket connections (crates/e2e; builds the server itself).
e2e:
    cargo test -p e2e --locked

# The same core scenario driven by archipelago_rs, a real Archipelago client library. Needs the
# sibling Archipela-Go checkout, so it is a separate crate outside the workspace (see README).
e2e-ap:
    cargo test --manifest-path crates/e2e-ap/Cargo.toml --locked --features ap-interop

# Fast unit tests only; what the pre-commit hook runs.
unit:
    cargo test --workspace --lib --locked

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

deny:
    cargo deny check

typos:
    typos
