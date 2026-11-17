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
- `just setup` once: it turns on the git hooks (fmt, clippy, typos and unit tests before a commit; the full gate before a push).
- Postgres is optional locally. CI runs the Postgres tests. To run them yourself, set `CONSTELLATION_DATABASE_URL` and run `just test-pg`.
- The design docs live in the sibling `constellation` repo. Clone it next to this one.

## Workflow
- Every to-do is a GitHub issue. No `TODO` or `FIXME` in code or docs; open an issue and write `see #<n>` where the work is deferred.
- Branch off `main`: `feat/...`, `fix/...`, `docs/...`.
- Conventional commits, subject at most 50 characters (the hook checks).
- Tests first: a test that fails without your change and passes with it. Say which one in the PR.
- `just check` passes before you push. CI must be green before merge.
- One reviewer who is not the author approves each PR.
- Lints are strict (`deny`). A new `allow` needs a `reason = "..."`. No `unsafe`.
- Limits and environment variables live only in `crates/config`.

## Talking about Archipelago
We build on Archipelago and want to stay good neighbours. Read the design repo's `CLAUDE.md` before writing any issue, PR or doc that mentions it. In short: be accurate and cite sources, credit their work, critique designs and never people, report a suspected bug to its maintainers before mentioning it anywhere public, and never copy their code, text or data into this repo. Text that mentions Archipelago gets a review against that file by someone other than its author before it is posted.
