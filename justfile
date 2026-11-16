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
    cargo test --workspace --all-features --locked

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

deny:
    cargo deny check

typos:
    typos
