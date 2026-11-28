# The rules section of a game package

Internal to the engine and unstable (D-126): it may change in any release. Status: implemented in `crates/data-fill` (`section.rs` compiles, `world.rs` loads and checks, `reach.rs` solves, `fold.rs` folds per seed, `decode.rs` reads back). It describes a binary form of the data manifest that `crates/data-fill` reads today as JSON (doc 07 shape plus the `x.` extensions of the data-only world experiment, archived in the design repo, see #71), so that one world generation fits in under 1 MB of heap. It is written for section 2 (`SECTION_RULES`) of the per-game package in the seed bundle v2 design (#68) and uses the shared index types in `crates/bits` (`BitSet`, `Packed`, the LSB-first bit stream). The package around it is the v2 package file (`seed_bundle::package::file_bytes` and `stored`, uncompressed so the sections are read in place); the game section inside it is still this crate's stand-in (below).

## Two layers: authors and the machine

| Layer | Who reads it | What it is |
|---|---|---|
| The manifest (JSON, doc 07 shape) | World authors and porters | Names, readable rules, `has_all`, sums, `ref` to named defs. Nothing in it is shaped for the engine, and the engine never asks an author to change it |
| The compiled rules section (this document) | The engine only | Dense indexes, bytecode, dependency indexes, group counters. Every performance concession lives here |
| The per-seed fold | The engine only, rebuilt each seed | The bytecode with this seed's options and rolls folded in |

The compiler (`section::compile`) runs once per world version and is the only code that reads the manifest. Its errors name the manifest path in author terms, for example `regions["menu"].checks["gift"].rule.has: unknown item "nope"`, never a byte offset or an index. `data-fill decode <package>` (`decode::manifest`) prints the manifest the section encodes, names restored from the game section and the names subsection, so a porter can see what the engine sees; compiling that output gives the same section bytes (tested).

## Why: where a generation spends time and memory

Measured on a large converted world on 2026-11-25 (the figures are not published here, since that world's source has no redistribution licence), with the JSON manifest as the engine read it then:

1. **Memory: the JSON tree.** The manifest parsed into `serde_json::Value` was many times its text and was the whole engine peak; most of the text was location records whose names and types the engine never reads. The compiled world after it was a small fraction of that.
2. **Memory: what stays.** After the inputs are dropped, the engine held compiled rule trees as boxed enums (most nodes behind their own allocation), key strings, key-to-index hash maps and the dependency index as one `Vec` per item. The fill itself was already small; the floor was the representation of the world.
3. **Time: the fill.** Most of the engine time. Each assumed-fill step solved from scratch, a tree walk through `Box` pointers and `defs` references.

## Shape

The section's bytes are inflated into one heap buffer when the package is loaded (no mmap). Everything inside is addressed by byte offsets from the start of the section; nothing is decoded into a tree. A solve reads rules straight from that buffer.

The section is per game and shared by every seed of that game version, so it is **not** folded by options. Option reads and seeded values stay operands that a solve looks up in a small per-seed table; the per-seed plan may precompute which sites fold to a constant (below).

Items and locations are named by their **dense index** in the game section's sorted id tables (the same indexes the seed file's placement records use). Regions, counters, defs and sites are dense indexes local to this section. No strings are needed to generate; names live in an optional subsection for diagnostics.

All integers are little-endian. Packed lists use the `crates/bits` layout: `width`-bit fields, LSB first. A packed list in this section is stored as `u32 count`, `u8 width` (at least 1, so a list's length is bounded by its bytes and a damaged count cannot make a reader loop), then `bits::bytes_for(count, width)` bytes.

Item and location indexes are manifest order in this implementation, and the game section is a stand-in (per table `u16` count, the ids as `i64`, then the keys). Moving to the v2 game package means remapping both to its id-sorted order in the compiler; the engine does not care which order it is.

### Header

| Field | Size |
|---|---|
| format version | `u16` |
| flags (bit 0: names subsection present) | `u16` |
| item count, location count (must match the game section) | `u16`, `u16` |
| region count, counter count, def count, site count | `u16` each |
| start region | `u16` |
| directory: offset and length of each subsection below, in this order | 15 x (`u32`, `u32`) |

### Subsections

| # | Subsection | Layout |
|---|---|---|
| 1 | options | per option: default value `i32` (values come from the seed; the key is in names) |
| 2 | roll slots | packed roll ids, then packed location indexes, one pair per distinct roll read; the seed supplies one value per slot |
| 3 | time | slices `u8` (1 to 64), `start_mask u64`, owned flag `u8`; when owned: `when` rule offset `u32` (options only, checked), `none_owned_mask u64`, half-day count `u8`, per half-day its slice mask `u64` and rule offset `u32` |
| 4 | mask table | distinct `x.time_any` masks, `u64` each; rules refer to them by index |
| 5 | regions | flags `u8` per region (bit 0: time can pass), then one packed list of `4 x regions + 1` first sites (exits, events, checks, stays of region 0, then region 1, ...), so a region's sites of a kind are a contiguous range |
| 6 | sites | target width `u8`, rule width `u8`, then one packed record per site: target (region for an exit, counter for an event, location for a check, slice for a stay) and rule offset. Kind is implied by the region's ranges |
| 7 | defs | packed list of rule offsets, one per def |
| 8 | bytecode | the rules (next section) |
| 9 | goal | rule offset `u32` |
| 10 | item to sites | CSR form: packed list of `item count + 1` start positions, then the packed list of site indexes. A site is listed under every item its rule reads, through defs and groups included, deduplicated. Indexes `site count + h` stand for half-day `h`'s clock rule |
| 11 | counter to sites | the same, per counter |
| 12 | time-dependent sites | `BitSet` words over sites: the rule reads time (directly or through a def) |
| 13 | locations | per location: vanilla item (packed, width of `item count`, the value `item count` meaning none) |
| 14 | names | string lists (`u16` count, then `u16` length and UTF-8 per key) of region, counter, def, option and roll keys. Needed to bind a seed given by key (options, rolls) and for reports; never read by a solve |
| 15 | item groups | packed group starts, packed member items, packed caps (0 = no cap, else cap + 1), then packed weights (one per member) only when some weight is not 1; without the list every weight is 1, so a world with no weighted sum keeps its bytes. A group names each item once (terms of one item are merged, below). The loader refuses a weights list of another length than the members or of only 1s (one encoding per world), bytes after the lists, an item named twice in one group, a group whose weights add up to more than `config::RULE_WEIGHT_MAX` x `config::RULE_GROUP_MAX_MEMBERS`, and a group of more than `config::RULE_GROUP_MAX_MEMBERS` members: the item sets a `GROUP` sum reads |

Rules for writers: every offset points inside the section; every index is below its count; defs form a DAG (no def reaches itself); no rule nests deeper than `config::RULES_MAX_DEPTH` (256) nodes, counting the defs it reaches through `ref`, so the recursive evaluators stay inside a thread stack (the compiler applies the same cap); every rule is monotone (below). `World::from_package` rejects a section that breaks any of these before a solve starts, in one pass (each def and each shared rule offset is checked once), and never panics on damaged bytes (tested by flipping every byte of a package).

### Bytecode

Prefix order, byte aligned (rules are small on large worlds, so bit packing them would save little and cost a slower decoder). One opcode byte, then fixed operands. Item, location and def operands are 1 byte when their count is at most 256 and 2 bytes otherwise (read from the header, fixed per section).

| Opcode | Operands | Meaning |
|---|---|---|
| `TRUE`, `FALSE` | | Constants. Offsets 0 and 1 of the bytecode hold these two, so a constant site points there and costs no bytes |
| `ALL`, `ANY` | `u8` count, `u16` byte length | Short-circuit over `count` children; the length lets a reader skip the rest |
| `HAS` | item | At least one copy |
| `HAS_N` | item, `u8` n | At least `n` copies |
| `HAS_ANY` | `u8` count, items | Any of these |
| `CMP` | `u8` op (eq, ne, lt, le, gt, ge), then two integer expressions | Comparison |
| `TIME_ANY` | `u8` mask index | The evaluating region's mask meets the mask |
| `REF` | def | Evaluate a def |
| `REACH` | region (`u16`) | A region is reached (goal only) |
| `INT` | `i16` | Integer constant |
| `COUNT` | item | Copies held |
| `COUNTER` | `u8` or `u16` counter | Counter value |
| `OPT` | `u8` option | Seed's option value |
| `ROLL` | `u16` roll slot | Seed's rolled value |
| `ADD`, `MIN`, `MAX` | `u8` count | Over `count` integer expressions |
| `MUL`, `DIV` | | Over two integer expressions; `DIV` needs a positive constant divisor (checked at build) |
| `GROUP` | `u16` group | Sum over the group's members of `weight x min(copies, cap)`. The compiler emits it for an authored sum of two or more terms whose every term is `{"count": k}` or `{"int": ["min", {"count": k}, cap]}` ("has N of these items"), each alone (weight 1) or as a weighted term (below); a solve keeps the value up to date as items are added and taken back, so reading it costs O(1) |

**Weighted group sums.** A weighted term is `{"int": ["*", copies, w]}` (the canonical form, which the decoder writes; `["*", w, copies]` compiles the same), where `copies` is `{"count": k}` or `{"int": ["min", {"count": k}, cap]}` and `w` is a JSON integer from 0 to `config::RULE_WEIGHT_MAX` (32,767, the largest integer a rule holds). "The army's strength is at least 12", with a squire worth 1, a knight worth 3 counted at most twice and a dragon worth 10, is:

```json
{"ge": [{"int": ["+",
  {"count": "squire"},
  {"int": ["*", {"int": ["min", {"count": "knight"}, 2]}, 3]},
  {"int": ["*", {"count": "dragon"}, 10]}]}, 12]}
```

Such a rule is one `GROUP` atom on the bit rule path (`bitrules.rs`, kind `GROUP`), like a plain count sum. Weights are non-negative and the comparison is "at least", so the rule is monotone. A collected or taken-back copy changes each group it is in by `weight x` the change of `min(copies, cap)`: constant time per group, never a rescan. A weight that is negative, fractional or past the cap is not a group term: it falls to the plain `MUL` path, which refuses a negative factor and an integer past 16 bits, and a fraction is not an integer node. A sum of more than `config::RULE_GROUP_MAX_MEMBERS` (65,535) terms is refused. Terms of one item and one cap merge into one member whose weight is their sum (so a stored weight can pass 32,767, up to the group's bound), which keeps a copy at one step per group however often a manifest names the item; the decoder writes a merged weight back as terms of at most 32,767 that merge again. One item under two caps (`count(a) + min(count(a), 1)`) is no single member, so that sum stays bytecode, still correct. A comparison with a threshold of 0 or less holds from the start and is not a bit rule atom (atoms test "at least 1" or more); it stays bytecode. A sum over counters (events) is not a group and stays bytecode, still correct. Bound (estimated from the limits, not measured): a group's weights add up to at most 65,535 x 32,767, below 2^31, so a sum is below 2^31 times the copies of one item, and copies are bounded by a room's locations: far inside the `i64` it is kept in.

Every rule is monotone: it can only turn from false to true as items, counters and time grow. The compiler rejects, with the manifest path: a `not` over anything but options; a comparison with items or counters on the side that must be smaller (`lt`/`le` with state on the left, `gt`/`ge` with state on the right, `eq`/`ne` with state anywhere), which would test for the absence of an item; and a product of state with anything but a non-negative constant. The loader checks the same.

The only `not` the data may hold is over option comparisons. Since options are not folded in the section, `NOT` over a subtree that reads only options is allowed (`NOT` opcode, checked at build to read no item, counter, time or region); anything else is rejected, as today.

### Per seed (not in the package)

The seed brings option values, roll values and the instance (existing locations, excluded, locked, pool with classes, start items, placement groups). Resolved once into a plan:

- option values `i32` per option, roll values `i16` per roll slot;
- `BitSet`s over locations: exists, excluded; placement group per location and per item (packed);
- the fold (`fold.rs`): one pass over the bytecode with options and rolls known writes a folded copy (constant parts removed, a site or def that folds to a constant points at offset 0 or 1) and a rule offset per site, def and half-day. Small on a large world; a solve reads only the folded copy.

### Per solve

| State | Form |
|---|---|
| copies per item, counter values, group sums | `i64` each (narrower types would save little) |
| time mask per region | `u64` per region (a `BitSet` of at most 64 slices in one word); 0 = not reached |
| reached locations | `BitSet` over locations |
| what a location gives when reached | borrowed from the fill's placement array, not copied per solve |
| regions seen, sites done | `BitSet`s; queued and pending are `bool` flags (cleared one by one as entries leave the queue, which `bits::BitSet` has no method for) |
| work queues | regions and sites `u32`, sized once (each entry is queued at most once) |
| def cache | per def: known true, or known false at an epoch. Rules are monotone, so a true def stays true for the solve; the epoch moves when an item or counter some def reads changes. Defs that read time are not cached |

The fill keeps one solve state and resets it between steps: a fill step allocates nothing (tested). Items added before a run do not walk their dependency lists, because the first visit of a region evaluates all its sites anyway; the clock rules, which run before any visit, are rechecked once at the start of the run.

The loader also unpacks the indexes a solve walks every step (site targets, region ranges, item and counter to sites, time-dependent sites) into plain arrays; the packed bytes stay the source of truth for checks and decoding.

## Complexity contract

The format makes a bad big-O impossible rather than unlikely:

- **Per solve: O(total rule size + exits x mask width).** A site's rule flips at most once per solve (monotone rules), so it is done after it holds. It is evaluated when its region is first reached, when the region's mask grows and the rule reads time (a mask has at most 64 slices, so at most 64 growths per region), and when an item or counter it reads changes (only the sites listed under that item, never a scan). An open exit passes its mask once per growth of its source.
- **No absence tests** (monotone rules, checked by compiler and loader): this is what lets a site stop being rechecked once true.
- **Defs form a DAG** (checked), and timeless defs are cached per solve, so a shared helper costs one evaluation per epoch, not one per reference. Within one evaluation (one rule, one state or rank, one mask) every def is evaluated at most once, time-dependent ones and the take-back path (a fact checked as of its rank) included, so a rule costs at most its own size plus the defs it reaches, never one evaluation per path through them. The compiler keeps the read lists of a def as sets for the same reason.
- **Group sums are counters**: "N of these items", weighted or not, costs O(groups the item is in) per collected item and O(1) per read in a solve, never a scan of the group.
- **No name lookups at generation time**: every key is a dense index in the section; keys are resolved once per seed when the instance is bound (`Plan::new`), never in a solve.
- **Per fill: O(progression items x per-solve cost)** for the assumed fill (at most one solve per placed progression item), plus O(locations) for the filler.
- **Per group, no fresh solve.** The fill reaches each group's state from one solve by adding to it and taking the additions back (`Reach::mark`, `Reach::undo_to`): rules are monotone, so only additions follow a mark, and undoing them restores that fixed point value for value. Earlier engines (truth maintenance, a derivation log, a fresh solve per step) are in `git` history.

### Several players (a room)

The fill has one method, the exact (assumed) fill (D-138), on one path for every room size (`room.rs`, `Steps::split`); one player fills as a room of one. A fresh solve of every group's state is a test-only cross-check (`room::fill_room_fresh`, feature `cross-check`; `adding_to_a_room_and_taking_it_back_matches_a_fresh_solve`). Same seed, settings and engine version give the same result on every machine; a new engine version may change it (D-140).

- **One state per player, linked by messages.** Each player has their own `Reach` over their own world and plan. A location that holds an item of another player does not collect it; reaching it sends the owner's state a copy. The owner's state continues incrementally, and what it reaches may send copies on.
- **One rank sequence for the room.** The states take ranks from one counter that the room hands from state to state, so a rank orders every derivation in the room.
- **Groups.** Items are placed in groups (one confined item, or a round), each against a state that holds none of the group: the room is solved once with none of the groups' items, then each group's state is reached by adding what it holds and taking it back after.
- **Per room, per solve: O(sum over players of the single-player cost)**; messages are one per reached location holding another player's item.
- **Memory is linear**: the world and its section are shared; each added player adds a plan and a state (`measure_room` measures it).
- **Placing is sequential**: one walk with one random stream, on one thread.

- **Rounds**: as in the reference multiplayer fill, progression is placed in rounds of one item per player (the last of each player's own pile of the shuffled pool, piles in the order players first appear), and a round is placed against a state that holds none of its items, which favours earlier locations; confined items go one at a time. With one player a round is one item. A round whose state does not reach a free location for each of its items is divided in halves, down to single items. An item that still has nowhere to go takes the location of a placed item that is reached without it, and that item is placed again against the state that has neither (the assumed fill's own step, so the seed is beatable for the same reason); at most `config::FILL_DISPLACE_TRIES` placed items are tried, spread over the order they were placed in, one fresh solve each, and a fill's repair (the moves' solves, one solve's worth for the pass over the rounds after each move, and one solve's worth per divided round) spends at most `config::FILL_REPAIR_EVALS` rule evaluations over all its attempts (a budget of work, the same on every machine). A room that fills without either is placed exactly as before these repairs. Only when a repair fails does the fill start over, at most `config::FILL_MAX_ATTEMPTS` times, and a restart orders each player's pile deepest first (`depth.rs`): one forward solve of the world alone adds, step by step, every copy that would open something, and the step is the copy's depth. That keeps a room of worlds where one key hides everything behind it from falling back to its start regions (issue 212), at a cost: shorter item chains and seeds further from uniform (counted on the bench), which is why it is only the recovery. Setup reports a world whose early capacity ratio (`data_fill::capacity`) is under 1: such a world leaves fewer places early on than items to seat, and leans on these repairs.
- **Choices where the reference multiplayer fill has options**: one shared pool and one shuffled location list over all players; an item with a placement group stays in its owner's world and group; no progression on an excluded location; every player's goal must be reachable, and a location may be unreachable (see #92); local-item limits are open (#91).

## Measured on the converted world

The compiled section, then truth maintenance, cut fill time and engine peak heap by large factors against the tree-walking engine on the converted world (2026-11-25; figures not published here, for the licence reason above). Fills stayed byte-identical to the tree-walking engine on the recorded seeds (the `golden` example), and `port-check` passed every check. Current numbers on committed synthetic worlds come from the `bench-room` CI job (`docs/port-check.md`).

## Prepared worlds

A manifest is compiled once, when it is uploaded, and kept in its ready form (`crates/data-fill/src/prepared.rs`): the package file above with one more section, `SECTION_SEED_GAME` (3), which holds the game's package file as seeds of that world name it (names, ids and checksum, `seed_bundle::GamePackage::file`). The server stores that file as a blob by content hash and finds it by the manifest's id and the engine that made it (table `prepared_worlds`): the file's layout (`data_fill::prepared::layout`: this section's format version, the package file version and `config::PREPARED_WORLD_LAYOUT`) and the server build (`GET /version`). A generation sends the stored file to its fill child, which loads it, fills and prints the seed file (v2) itself.

A stored world is prepared again from the stored manifest, by the generation that needs it, when there is none for the running build (so each deploy prepares each world once more, on its first generation, and a compiler fix reaches every world), when its blob is missing or torn, or when the fill child cannot load it. Builds that share a version text (local and test builds carry no commit in theirs) share stored worlds: there, raise `config::PREPARED_WORLD_LAYOUT` with any change to the file.

## What is not decided here

- Whether the per-seed instance (pool, existing locations, groups) also becomes a package or seed section, or keeps arriving as JSON. On the converted world the typed JSON instance alone fit the budget but was most of it.
- Whether the dependency index stays in the section. The converter computes it and the loader rebuilds it from the rules and refuses a section whose index differs (a missing reader would never be rechecked); the item group membership index is computed at load. `reach` is allowed in the goal only, by compiler and loader, so no index of region readers is needed.
- Versioning: a change to any layout here bumps the header's format version; section 2 is listed as unstable in the v2 document until the first release.
