# Roadmap: the proof of concept

This repo is a proof of concept. It has to answer three questions, with tests as the evidence:

1. **Does it work with what already exists?** Clients and games built for Archipelago connect to our server and play a real multiworld without noticing a difference.
2. **Does the Constellation standard work?** A client written against our own protocol (CWP, doc 01 in the `constellation` design repo) plays a room, and plays in the *same* room as a client compatible with Archipelago.
3. **Can real games move to the standard?** A few existing games are described as Constellation game manifests (CGM, data only, doc 07) and generate seeds that match what they generate today.

Every pitfall we hit on the way is a result, not a failure. Write it down (see "Findings" at the end).

Design references point at the `constellation` repo: `docs/design/NN-*.md`, decisions as `D-NN` in `docs/design/DECISIONS.md`.

## Where we are (2026-11-16)

- One process hosts one room from a seed bundle. The Archipelago-compatible edge (`crates/edge-compat`) handles connect, checks, items, sync, scouts, chat, bounce, goal. The full table, with the test behind each line, is [`compat-status.md`](compat-status.md).
- The room log is in Postgres and survives `kill -9`.
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

Four lanes, one owner each. A lane owns its crates, so two people rarely edit the same files. With three people, merge lanes C and D: D's tasks are short and mostly at the end.

### Lane A: compatibility with existing clients (M1)

Owns `crates/edge-compat`, `crates/seed-bundle`, `tools/ap-import`, `docs/compat-status.md`.

| Task | What | Needs |
|---|---|---|
| A-1. Client matrix | Pick 5 existing games and tools across client styles (a text client, a tracker, an emulator-based game, a PC mod, Archipela-Go). Record in `compat-status.md` which connect, play and goal. | Nothing; start here |
| A-2. Data storage | `Set`, `SetNotify`, `SetReply`, and `Get` for real keys. Many clients and trackers use it. | Room-core command for key writes |
| A-3. Hints | `CreateHints`, `UpdateHint`, `LocationScouts` with `create_as_hint`, hint points spending, `_read_hints_*` | A-2 (hints are stored keys) |
| A-4. Release, collect, remaining | The three commands and their permissions in `RoomInfo` | Room-core commands |
| A-5. Client statuses and `TagsChanged` | Statuses other than goal; the `TagsChanged` print | Nothing |
| A-6. Starting inventory and item links | Importer and bundle accept them instead of refusing the seed | Seed-bundle schema change |
| A-7. Importer coverage | Run `ap-import` over seeds from every game in A-1; fix what it refuses | A-1 |
| A-8. Upstream report | Report the `soketto` / `flate2` deflate finding (bottom of `compat-status.md`) to the `archipelago_rs` maintainers | Nothing; see the rules below first |

### Lane B: the native standard (M2)

Owns `crates/proto`, the new `crates/edge-native`, the new `crates/sdk-core`, `crates/runtime`.

| Task | What | Needs |
|---|---|---|
| B-1. CWP v1 schema | JSON schema generated from `proto` types, a snapshot test, golden transcripts (doc 01 section 7 step 1) | Nothing; start here |
| B-2. Applied cursors | Persist each slot's applied cursor (the known deviation in `compat-status.md`), batched into the room's append | Nothing |
| B-3. Token auth | Per-slot tokens (doc 01 D1.2) | B-1 |
| B-4. `edge-native` | WebSocket edge speaking CWP: hello and capabilities, event log with `seq`, one ack (D1.3, D1.4) | B-1, B-2, B-3 |
| B-5. `sdk-core` (Rust) and a demo bot | The minimal client from doc 01 D5.4; the bot plays a room end to end | B-4 |
| B-6. Mixed room test | Native bot and an existing client in one room, in `crates/server/tests`, with a `kill -9` in the middle | B-5, lane A's e2e helpers |
| B-7. JS SDK (stretch) | Same minimal client in TypeScript, same golden transcripts | B-5 |

### Lane C: games on the standard (M3)

Owns the new `crates/cgm` (manifest types and validator), `crates/rules`, later `crates/fill`.

| Task | What | Needs |
|---|---|---|
| C-1. Manifest types and validator | CGM v1 types, the ID ledger rules (doc 01 D2.1 to D2.6), the Moss Cave sample from doc 01 section 4.1 as the first fixture | Nothing; start here |
| C-2. Rule evaluator | The closed rule language (D2.5) in Rust, with shared test vectors | C-1 |
| C-3. Archipela-Go manifest | Our own game first: write its manifest, check IDs and pools against its apworld | C-1, C-2 |
| C-4. Two more worlds | Two of the three from doc 07 section 8.2 (musedash, faxanadu, lingo), ported by hand on a branch. Record every construct as "data", "needs a primitive" or "residue" | C-3 |
| C-5. Equivalence harness | Compare a port to the original: names, IDs, options, pool multisets over 50 option sets, beatability | C-1; runs the original via Archipelago's generator like the fixture script in `README.md` |
| C-6. Fill v1 (stretch) | Generate a seed from a manifest without Python (CFill v1) and load it as a bundle | C-2, C-4 |

C-4 reads other people's worlds. Describe what they do in our own words; do not copy code or data into this repo without an owner decision and a license check (rules below).

### Lane D: reliability and hosting (M1 playtests, M4)

Owns `crates/sim`, `.github/workflows`, deploy files, `docs/TESTING.md`.

| Task | What | Needs |
|---|---|---|
| D-1. Friends server | One small box, TLS terminator in front, systemd unit from `README.md`, Postgres with nightly backup | Owner approves the spend |
| D-2. Playtest loop | One session every week or two on D-1; each pitfall becomes an issue labeled `finding` | D-1, A-1 |
| D-3. Chaos run | `kill -9` every 30 s for one hour with zero acknowledged loss and zero duplicates (gate 0 c) | Nothing (runs in CI or on D-1, never on a shared dev box) |
| D-4. Restore drill | Restore D-1's backup to a fresh database and replay the room (gate 0 d) | D-1 |
| D-5. Simulator scale | 10M schedules clean (gate 0 a), run nightly in CI | Nothing |
| D-6. Load numbers | Measured connection memory and room rehydrate time, replacing design values | B-4 for native connections |

## Start order for four people

Each person can start on day one without waiting for anyone:

| Person | First task | Then |
|---|---|---|
| 1 | A-1 client matrix | A-2, A-3 |
| 2 | B-1 schema, B-2 cursors | B-3, B-4 |
| 3 | C-1 validator | C-2, C-3 |
| 4 | D-3 chaos run, D-5 simulator | D-1 once approved, then A-4, A-5 or C-5 wherever is behind |

Cross-lane handoffs to watch: B-6 needs lane A's test helpers (`crates/e2e`); C-6 needs lane B's bundle loading unchanged; A-2 and B-2 both touch `room-core` and `runtime`, so agree on the command shape first in the issue.

## Findings

A finding is anything that surprised us: a client that expects something the protocol document does not say, a world construct the manifest cannot express, a number far from the design value.

- Interop findings go in `compat-status.md`. Design findings become an issue labeled `finding` and, when they change a decision, a note in the design repo.
- A suspected bug in someone else's code stays internal and marked suspected until reproduced and reported to its maintainers (rules below).

## Rules that apply to every lane

- The design repo's `CLAUDE.md` is binding for anything that mentions Archipelago: be accurate, credit them, critique design and never people, report bugs upstream before mentioning them anywhere, no copied code, text or data. This repo is public, so issues and PR text count as external.
- `just check` before a PR. Postgres tests run in CI. No em dashes in docs.
