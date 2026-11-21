# Roadmap: the proof of concept

This repo is a proof of concept. It has to answer three questions, with tests as the evidence:

1. **Does it work with what already exists?** Clients and games built for Archipelago connect to our server and play a real multiworld without noticing a difference.
2. **Does the Constellation standard work?** A client written against our own protocol (CWP, doc 01 in the `constellation` design repo) plays a room, and plays in the *same* room as a client compatible with Archipelago.
3. **Can real games move to the standard?** A few existing games are described as Constellation game manifests (CGM, data only: doc 01 section 3.2, and doc 07 for how far data can go) and generate seeds that match what they generate today.

Every pitfall we hit on the way is a result, not a failure. Write it down (see "Findings" at the end).

Design references point at the `constellation` repo: `docs/design/NN-*.md`, decisions as `D-NN` in `docs/design/DECISIONS.md`.

## Where we are (2026-11-16)

- One process hosts one room from a seed bundle. The Archipelago-compatible edge (`crates/edge-compat`) handles connect, checks, items, sync, scouts, chat, bounce, goal. The full table, with the test behind each line, is [`compat-status.md`](compat-status.md).
- With a database URL set, the room log is in Postgres and survives `kill -9`; without one it is in memory.
- A seed made by Archipelago's own generator converts to our bundle (`tools/ap-import`).
- Not started: the native edge (`edge-native`), the client SDK, the manifest validator, the rule evaluator, the fill.

## Milestones

| Milestone | Proves | Done when |
|---|---|---|
| **M0. Open for contributors** | People can join | License chosen, `CONTRIBUTING.md`, issues filed from this roadmap, CI green on a fork PR |
| **M1. Real clients play** | Question 1 | Friends play a full multiworld on a hosted server with at least three different existing games, and the remaining gaps in `compat-status.md` that those games hit are closed |
| **M2. Native round trip** | Question 2 | A native client (Rust SDK) and an existing client play in one room; each sees the other's items; restart in the middle loses nothing |
| **M3. Games on the standard** | Question 3 | Archipela-Go plus two other worlds have a manifest that passes the validator and matches the original on IDs, item pools and beatability |
| **M4. Findings report** | The POC verdict | Gate 0 numbers recorded as measured (doc 05 section 20), and a written list of what worked, what broke, and what the design must change |

M1, M2 and M3 run in parallel. M4 collects them.

## Workstreams

**The GitHub issues are the source of truth for tasks.** This file holds the goals; each task, its "Done when" and its "Blocked by" line live in an issue with a lane label and a milestone ([milestones](https://github.com/Rasbandit/constellation-server/milestones)). How to claim one is in [`AGENTS.md`](../AGENTS.md).

Five lanes, one owner each. A lane owns its crates, so two people rarely edit the same files. With three people, merge lanes C and D: D's tasks are short and mostly at the end.

- **Lane A: compatibility with existing clients (M1).** Owns `crates/edge-compat`, `crates/seed-bundle`, `tools/ap-import`, `docs/compat-status.md`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:compat)
- **Lane B: the native standard (M2).** Owns `crates/proto`, the new `crates/edge-native`, the new `crates/sdk-core`, `crates/runtime`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:native)
- **Lane C: games on the standard (M3).** Owns the new `crates/cgm` (manifest types and validator), `crates/rules`, later `crates/fill`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:games)
- **Lane D: reliability and hosting (M1 playtests, M4).** Owns `crates/sim`, `.github/workflows`, deploy files, `docs/TESTING.md`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:reliability)
- **Lane E: web UI (M1, M2 stretch).** Owns a new `crates/web` (or `crates/server/src/web`), `templates/`, `static/`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:web)

## Tech choices

The web UI is server-rendered HTML from the existing server binary: axum (already used) with Askama templates, htmx (one vendored JS file) for interactivity, server-sent events (SSE) from axum for live updates, and plain CSS. Why: it is the fastest path, it ships as one binary, and contributors install nothing beyond Rust (no Node, no build step). This is a POC choice; design decision D-70 (Leptos SSR) stays open for later.

## Start order

Each person can start on day one without waiting for anyone:

1. Lane A: #19 client matrix, then #20 and #21.
2. Lane B: #12 schema and #13 cursors, then #14 and #15.
3. Lane C: #27 validator, then #28 and #29.
4. Lane D: #35 chaos run and #37 simulator, then #33 once approved, then #22, #23 or #31 wherever is behind.
5. Lane E (if a fifth person joins): #39 web skeleton, then #40 and #41.

Cross-lane handoffs to watch: #17 needs lane A's test helpers (`crates/e2e`); #32 needs lane A's bundle loading (`seed-bundle`) unchanged; #20 and #13 both touch `room-core` and `runtime`, so agree on the command shape first in the issue.

## Findings

A finding is anything that surprised us: a client that expects something the protocol document does not say, a world construct the manifest cannot express, a number far from the design value.

- Interop findings go in `compat-status.md`. Design findings become an issue labeled `finding` and, when they change a decision, a note in the design repo.
- A suspected bug in someone else's code stays internal and marked suspected until reproduced and reported to its maintainers (rules below).

## Rules that apply to every lane

- The design repo's `CLAUDE.md` is binding for anything that mentions Archipelago: be accurate, credit them, critique design and never people, report bugs upstream before mentioning them anywhere, no copied code, text or data. This repo is public, so issues and PR text count as external: text that mentions Archipelago gets a review against that file by someone other than its author before it is posted.
- CI (format, unit, web, version and Postgres tests) gates merge; sandbox and mutation tests are not run in CI for now (see #185). No em dashes in docs.
