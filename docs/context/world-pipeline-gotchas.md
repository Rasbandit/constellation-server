# Context Doc: World generation pipeline and repo gotchas

_Last verified: 2026-11-18_

## Status
Working on main.

Generation builds on Archipelago's own generator and world format (https://archipelago.gg); credit for both goes to the Archipelago project and its world authors.

## What This Is
How a world goes from uploaded files to a room, plus the non-obvious traps found while building and running it.

## Environment
Fedora Linux, shared box. Claude Code shell. GitHub Actions CI.

## Connection
Pipeline on main:
1. Upload `.apworld` and `.yaml` with `PUT /api/apworlds` and `PUT /api/yamls` (admin listener, bearer token).
2. `POST /api/worlds`.
3. Sandboxed Python sidecar (`tools/ap-gen`) runs Archipelago's `Generate.py` inside bwrap, with a per-job `systemd-run --user --scope` memory cap.
4. `tools/ap-import` converts the output to v1 JSON.
5. v2 encode into the store (Postgres `game_packages` and `room_seeds`, migration 0003).
6. A room is created.

Proof: `just e2e-full`. It uses real files in `~/Downloads`; never commit them.

## Auth
Admin listener uses a bearer token.

## Key Commands / Patterns
- `just e2e-full` runs the whole pipeline end to end.
- Memory harness: `allocation-counter` crate behind the `memory-harness` feature.
- Sandbox suite runs in nightly through `.github/actions/sandbox-host`.

## Failed Approaches / Dead Ends
- Skipping patch files but keeping multidata: Archipelago has no such flag (`--skip_output` drops both), so patches are written and ignored.
- mmap or a custom global allocator: the workspace forbids `unsafe`, so neither is available.
- Sending a pre-compressed DataPackage frame: yawc has no public API for it, so the reply is serialized once but compressed on each send.
- Sharing one cargo `target/` between agents broke each other's builds; each worktree keeps its own.
- Relying on a prefix deny rule to block pushes: `Bash(git push:*)` in `.claude/settings.local.json` did not block `git -C <dir> push`, because `git -C` was on the allow list. Prefix rules can be sidestepped, so main is protected by a GitHub ruleset ("protect main": PR required, `check` CI required, no force push, no deletion, no bypass).

## Gotchas
- Generation time for the MM world is mostly the world's own fill (about 14 s of 16 s). Importing all 80 core worlds costs about 4 s. The sandbox shows only `worlds/generic` plus the chosen apworlds.
- In Claude Code's shell, `grep` is a wrapper around ugrep. `grep -qv` gave wrong answers. Use `command grep` to test shell logic that CI will run with GNU grep.
- The sandbox tests deliberately exceed per-job memory caps, so their OOM kills set off the box's memory alert. This is expected.
- GitHub job logs are unavailable until the whole workflow run finishes. Cancel the run, or fetch the job log through the API, to read a finished job early.
- Nightly `mutants` jobs hit the 60-minute job limit at 4 shards, so they now use 6.

## References
- `tools/ap-gen`, `tools/ap-import`
- `.github/actions/sandbox-host`
- migration 0003
