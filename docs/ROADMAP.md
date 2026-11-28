# Roadmap: the proof of concept

This repo is a proof of concept. It has to answer three questions, with tests as the evidence:

1. **Does it work with what already exists?** Clients and games built for Archipelago connect to our server and play a real multiworld without noticing a difference.
2. **Does our native protocol work?** A client written against our own protocol (doc 01 in the `constellation` design repo) plays a room, and plays in the *same* room as a client compatible with Archipelago.
3. **Can real games move to data-only worlds?** A few existing games are written as `.rando` worlds (data only: doc 18, D-141) and generate seeds that match what they generate today on IDs, item pools and beatability.

Every pitfall we hit on the way is a result, not a failure. Write it down (see "Findings" at the end).

Design references point at the `constellation` repo: `docs/design/NN-*.md`, decisions as `D-NN` in `docs/design/DECISIONS.md`.

## Where we are (2026-11-28)

- One server hosts many rooms, each reached by its room key in the path (#55, #105). The Archipelago-compatible edge (`crates/edge-compat`) handles connect, checks, items, sync, scouts, chat, bounce, goal. The full table, with the test behind each line, is [`compat-status.md`](compat-status.md).
- With a database URL set, the room log is in Postgres and survives `kill -9`; without one it is in memory.
- A seed made by Archipelago's own generator converts to our bundle (`tools/ap-import`).
- The fill: `crates/data-fill` makes a seed from a data-only world manifest (#81), for several players in one room (#95), with one fill method on one path (#222). Rules are compiled into a bytecode rules section and evaluated inside `data-fill` (#81, #205); the separate evaluator crate with shared test vectors (#28) is not built. `crates/rule-ops` holds only the comparison operators that `data-fill` and `world-build` share.
- The language and CLI: the `.rando` authoring language and the `rando` tool with `check`, `fmt`, `build` and `generate` (#223), then a second grammar round (#228); the lexer is its own crate, `crates/rando-syntax` (#232). The validator issue #27 is closed, by hand, with a comment that #223 did the work.
- Packaging: `rando.toml`, `rando.lock` and `rando pack` (#230).
- The settings schema: option scope and labels, and `settings.json` in the package (#231).
- Not started: the native edge (`edge-native`) and the client SDK (no such crates on main).

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
- **Lane C: games on the standard (M3).** Owns `crates/world-build` (the `rando` tool), `crates/rando-syntax`, `crates/data-fill` (the fill) and `crates/rule-ops`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:games)
- **Lane D: reliability and hosting (M1 playtests, M4).** Owns `crates/sim`, `.github/workflows`, deploy files, `docs/TESTING.md`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:reliability)
- **Lane E: web UI (M1, M2 stretch).** Owns the `web/` app (Vite and React) and the web routes in `crates/server`. [Issues](https://github.com/Rasbandit/constellation-server/issues?q=is:open+label:lane:web)

## Projects

Projects are goals that cut across the five lanes. Each has a label `project:<name>` and a board that mirrors it; the issues are the source of truth, and this file holds the goals and the order. Issues marked `decision-needed` are decided by the claiming dev and recorded in the issue.

**Connection standard** (`project:connection-standard`: #133 to #146, plus #13, #14, #15, #16, #17, #18, #49, #50, #114; lane native, M2 then M3). Goal: one simple way for any game mod to connect, with SDKs for the languages games need.
1. Research: #133 (how mods connect today), #135 (room to connected game), #136 (how other mod ecosystems ship SDKs).
2. The game contract (#134) and the shared core (#16).
3. Server side of the connect flow (#137), reconnect (#138), settings sync (#139), SDKs (#140), the player-facing flow (#141) and the mod kit (#142).
4. Ports of the first games (#144 picks them, #145 ports them), then lessons (#146). #143 is a research question on world packages.

**Developer portal and games site** (`project:developer-portal`: #148 to #161; lane web, M3). Goal: an easy path for developers to get a game listed, and a place for players to find it. Decided: game code and releases live in the dev's own repo; the only gate is automated checks.
1. Research: #149 (how a dev submits) and #150 (how other registries work), with #148 defining the game entry.
2. The site order and split decision (#151).
3. Registry (#152), automated checks (#153), listing states (#154) and the takedown path (#155).
4. Then the portal (#156), sandbox (#157), quick start (#158), maintainer dashboard (#159), games site (#160) and game health signal (#161).

**World standard** (`project:world-standard`: #164 to #172, plus #28 to #31, #71, #91 to #94 and #226; #27, the validator, is closed; lane games, M3). Goal: convert existing worlds into the data-only standard in a separate worlds repo, and write down best practice from what hurts.
1. Setup: the worlds repo (#164), credit and copying rules (#165), the definition of a converted world (#166).
2. Pick the first three worlds (#167), then the folder template (#168).
3. Convert them (#29, #30), checked by the equivalence harness (#31).
4. Learn: the gap log (#169), the best-practices guide (#170), the tooling decision (#171) and the registry link-up (#172).

**Platform** (`project:platform`: #174 to #184, plus #56, #74, #88, #96, #112, #120, #127, #131; lane reliability, M1 to M4). Goal: admin tools, monitoring and usage transparency.
1. Decide what usage is and who sees it (#174), what owners and operators need (#175), and the privacy rules (#176).
2. Build owner tools (#177), metrics (#180), alerting (#181) and the retention policy (#184).
3. Then the operator console (#178), audit log (#179), public status page (#183) and cost per room (#182).

**Features and web app** (`project:features-web`: #42, #73, #82, #111, and #225 as a stretch; lane web). Goal: user-facing features and the web app flow. The website cleanup is parked until it has issues.

### Started since, with no label or board yet

These four serve question 3 (lane games, M3). Each follows a decided design doc in the `constellation` repo and has merged work here; none has a `project:` label or a board yet, so the issues and pull requests below are the record.

**The `.rando` authoring language** (doc 18, D-141). Goal: worlds written as `.rando` files and built by the `rando` tool in `crates/world-build` ([`world-build.md`](world-build.md)). Done: the tool core (#223), two grammar rounds (#228 and the trial worlds of #227), five genre example worlds (#229), the lexer in its own crate, `crates/rando-syntax` (#232). The trial worlds ran on 2026-11-27 and led to nine grammar and tool changes (doc 18 section 2.13, built in #228); the grammar is still provisional (doc 18). Open: the doc 18 features the tool still refuses (#226).

**World packaging** (doc 19, D-142). Goal: one package per world version for the developer portal. Done in the tool: `rando.toml`, `rando.lock` and `rando pack` (#230). Not built yet: the server side, which checks the package as untrusted input and rebuilds its source in the sandbox (D-142).

**The settings language** (doc 20, D-143, D-144). Goal: player and room settings as a small `.rando` file type. Done: option scope and labels, and `settings.json` in the package (#231), and the shared lexer with a settings file kind (#232). Open: the checker and tools of doc 20 sections 7 and 8.

**Generic shuffles, entrance and room shuffle first** (doc 17, D-137). Goal: one system for randomization that is not item placement. Done: the first slice in `data_fill::shuffle`, with entrance shuffle (#207). Open: the write edges in the run order (#210) and the rest of doc 17 (#211).

## Tech choices

The web UI is a Vite and React app in `web/`, served by the server binary on the room port (#107). The server stays one binary to deploy; contributors working on the UI need Node tooling (`web/package.json`).

## Findings

A finding is anything that surprised us: a client that expects something the protocol document does not say, a world construct the manifest cannot express, a number far from the design value.

- Interop findings go in `compat-status.md`. Design findings become an issue labeled `finding` and, when they change a decision, a note in the design repo.
- A suspected bug in someone else's code stays internal and marked suspected until reproduced and reported to its maintainers (rules below).

## Rules that apply to every lane

- The design repo's `CLAUDE.md` is binding for anything that mentions Archipelago: be accurate, credit them, critique design and never people, report bugs upstream before mentioning them anywhere, no copied code, text or data. This repo is public, so issues and PR text count as external: text that mentions Archipelago gets a review against that file by someone other than its author before it is posted.
- CI (format, unit, web, version and Postgres tests) gates merge; sandbox and mutation tests are not run in CI for now (see #185). No em dashes in docs.
