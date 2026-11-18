# The rules section of a game package

Status: implemented in `crates/data-fill` (`section.rs` compiles, `world.rs` loads and checks, `reach.rs` solves, `fold.rs` folds per seed, `decode.rs` reads back). It describes a binary form of the data manifest that `crates/data-fill` reads today as JSON (doc 07 shape plus the `x.` extensions in `data-only-world-experiment.md`, see #71), so that one world generation fits in under 1 MB of heap. It is written for section 2 (`SECTION_RULES`) of the per-game package in the seed bundle v2 design (#68) and uses the shared index types in `crates/bits` (`BitSet`, `Packed`, the LSB-first bit stream). Until the shared types land here (see #68), `crates/data-fill/src/section_io.rs` stands in for `crates/bits` and the package reader under the same method names (table at the end).

## Two layers: authors and the machine

| Layer | Who reads it | What it is |
|---|---|---|
| The manifest (JSON, doc 07 shape) | World authors and porters | Names, readable rules, `has_all`, sums, `ref` to named defs. Nothing in it is shaped for the engine, and the engine never asks an author to change it |
| The compiled rules section (this document) | The engine only | Dense indexes, bytecode, dependency indexes, group counters. Every performance concession lives here |
| The per-seed fold | The engine only, rebuilt each seed | The bytecode with this seed's options and rolls folded in |

The compiler (`section::compile`) runs once per world version and is the only code that reads the manifest. Its errors name the manifest path in author terms, for example `regions["menu"].checks["gift"].rule.has: unknown item "nope"`, never a byte offset or an index. `data-fill decode <package>` (`decode::manifest`) prints the manifest the section encodes, names restored from the game section and the names subsection, so a porter can see what the engine sees; compiling that output gives the same section bytes (tested).

## Why: where a generation spends time and memory today

Measured with `cargo run --release -p data-fill --features memory-harness --example measure -- <manifest> <instance> 1` on the experiment's test world (manifest 830 KB of JSON: 2641 locations of which 1516 exist this seed, 237 items, 318 regions, 45 time slices, 3685 rule sites, 107 defs). "Peak" is live heap above the phase start, "held" what the phase leaves live. Allocator overhead per block is not counted. Two instances (clock shuffle off and on); times vary about 20% run to run on the shared box.

| Phase | Time | Allocated | Peak | Held |
|---|---|---|---|---|
| read manifest file | 1 ms | 0.83 MB | 0.83 MB | 0.83 MB |
| parse manifest into `serde_json::Value` | 23 to 24 ms | 13.2 MB | 12.9 MB | 12.9 MB |
| parse instance (typed, the engine part) | 4 to 6 ms | 0.44 MB | 0.28 MB | 0.27 MB |
| compile (`World::load`: fold options, build rule trees) | 8 ms | 1.8 MB | 0.93 MB | 0.91 MB |
| drop the JSON inputs | 7 ms | | | -16.7 MB |
| dependency index (`Index::new`) | 1 ms | 0.36 MB | 0.15 MB | 0.14 MB |
| plan (`Plan::new`) | 0.5 ms | 0.09 MB | 0.06 MB | 0.06 MB |
| fill (288 to 293 solves) | 110 to 230 ms | 12.3 MB | 0.07 MB | 0.01 MB |
| validate (one solve) | 0.5 to 1.4 ms | 0.04 MB | 0.04 MB | 0 |
| write placements JSON | 0.3 ms | 0.36 MB | 0.25 MB | 0 |
| **engine process peak** | | | **17.9 MB** | |
| test oracle: read and parse the recorded solves | 13 to 19 ms | 14.8 MB | 10.7 MB | |
| test oracle: 42 solves | 9 to 20 ms | 1.8 MB | 0.04 MB | |

What it means:

1. **Memory: the JSON tree.** The manifest as a `Value` is 12.9 MB, 15.6 times its text, and it is the whole engine peak. About half of the text is location records whose names and types the engine never reads. The compiled world after it is 0.91 MB.
2. **Memory: what stays.** After the inputs are dropped, the engine holds 1.39 MB: compiled rule trees as boxed enums (24 bytes per node, about 9 500 nodes after folding, most behind their own allocation), key strings (96 KB), key-to-index hash maps, and the dependency index as one `Vec` per item. A solve adds 42 KB, the fill 69 KB. So the fill itself is already small; the floor is the representation of the world.
3. **Time: the fill.** 85 to 90% of the engine time. Each assumed-fill step solves from scratch (about 3 900 rule evaluations, about 100 ns each, a tree walk through `Box` pointers and `defs` references). 15% of a solve is setup (fresh buffers, start items, holding every placed item). Clock shuffle doubles the fill because time masks grow in more steps.

The test oracle (recorded legacy solves in the instance file, 2.8 of its 2.9 MB) costs more memory than the engine and is kept out of every number above the line.

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
| 15 | item groups | packed group starts, packed member items, packed caps (0 = no cap, else cap + 1): the item sets a `GROUP` sum reads |

Rules for writers: every offset points inside the section; every index is below its count; defs form a DAG (no def reaches itself); every rule is monotone (below). `World::from_package` rejects a section that breaks any of these before a solve starts, in one pass (each def and each shared rule offset is checked once), and never panics on damaged bytes (tested by flipping every byte of a package).

### Bytecode

Prefix order, byte aligned (rules are about 27 KB here, so bit packing them would save little and cost a slower decoder). One opcode byte, then fixed operands. Item, location and def operands are 1 byte when their count is at most 256 and 2 bytes otherwise (read from the header, fixed per section).

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
| `GROUP` | `u16` group | Sum over the group's members of `min(copies, cap)`. The compiler emits it for an authored sum whose every term is `{"count": k}` or `{"int": ["min", {"count": k}, cap]}` ("has N of these items"); a solve keeps the value up to date as items are added, so reading it costs O(1) |

Every rule is monotone: it can only turn from false to true as items, counters and time grow. The compiler rejects, with the manifest path: a `not` over anything but options; a comparison with items or counters on the side that must be smaller (`lt`/`le` with state on the left, `gt`/`ge` with state on the right, `eq`/`ne` with state anywhere), which would test for the absence of an item; and a product of state with anything but a non-negative constant. The loader checks the same.

The only `not` the data may hold is over option comparisons. Since options are not folded in the section, `NOT` over a subtree that reads only options is allowed (`NOT` opcode, checked at build to read no item, counter, time or region); anything else is rejected, as today.

### Per seed (not in the package)

The seed brings option values, roll values and the instance (existing locations, excluded, locked, pool with classes, start items, placement groups). Resolved once into a plan:

- option values `i32` per option, roll values `i16` per roll slot;
- `BitSet`s over locations: exists, excluded; placement group per location and per item (packed);
- the fold (`fold.rs`): one pass over the bytecode with options and rolls known writes a folded copy (constant parts removed, a site or def that folds to a constant points at offset 0 or 1) and a rule offset per site, def and half-day. About 40 KB on this world; a solve reads only the folded copy.

### Per solve

| State | Form |
|---|---|
| copies per item, counter values, group sums | `i64` each (2 KB here; narrower types would save little) |
| time mask per region | `u64` per region (a `BitSet` of at most 64 slices in one word); 0 = not reached |
| reached locations | `BitSet` over locations |
| what a location gives when reached | borrowed from the fill's placement array, not copied per solve |
| regions seen, sites done | `BitSet`s; queued and pending are `bool` flags (cleared one by one as entries leave the queue, which `bits::BitSet` has no method for) |
| work queues | regions and sites `u32`, sized once (each entry is queued at most once) |
| def cache | per def: known true, or known false at an epoch. Rules are monotone, so a true def stays true for the solve; the epoch moves when an item or counter some def reads changes. Defs that read time are not cached |

The fill keeps one solve state and resets it between steps: a fill step allocates nothing (tested). Items added before a run do not walk their dependency lists, because the first visit of a region evaluates all its sites anyway; the clock rules, which run before any visit, are rechecked once at the start of the run.

The loader also unpacks the indexes a solve walks every step (site targets, region ranges, item and counter to sites, time-dependent sites, about 70 KB here) into plain arrays; the packed bytes stay the source of truth for checks and decoding.

## Complexity contract

The format makes a bad big-O impossible rather than unlikely:

- **Per solve: O(total rule size + exits x mask width).** A site's rule flips at most once per solve (monotone rules), so it is done after it holds. It is evaluated when its region is first reached, when the region's mask grows and the rule reads time (a mask has at most 64 slices, so at most 64 growths per region), and when an item or counter it reads changes (only the sites listed under that item, never a scan). An open exit passes its mask once per growth of its source.
- **No absence tests** (monotone rules, checked by compiler and loader): this is what lets a site stop being rechecked once true.
- **Defs form a DAG** (checked), and timeless defs are cached per solve, so a shared helper costs one evaluation per epoch, not one per reference.
- **Group sums are counters**: "N of these items" costs O(groups the item is in) per collected item and O(1) per read, never a scan of the group.
- **No name lookups at generation time**: every key is a dense index in the section; keys are resolved once per seed when the instance is bound (`Plan::new`), never in a solve.
- **Per fill: O(progression items x per-solve cost)** for the assumed fill (at most one solve per placed progression item), plus O(locations) for the filler.
- **One solve state per fill, kept exact by truth maintenance.** Assumed fill takes one item back per step, which is not monotone, so the naive way is a fresh solve per step. Instead every fact the state holds carries a rank, unique and increasing in derivation order: a site that held (an exit, event or check), a half-day rule that held, and a growth of a region's mask (the slices one support added at once: the start, an exit passing its source's slices, or waiting forward). A collected copy has the rank of the check that reached its location. The state as of rank `k` is every fact of rank below `k`, and each fact holds in the state as of its own rank, so the ranks are a derivation. A step goes like this:
  1. *Carry*: placing the previous item at a reached location only adds, so hold it and run the worklist from the current fixed point (only the sites that read it are rechecked).
  2. *Saturated*: `Plan::new` computes per progression item the held count past which no rule reads more copies (the fewest copies that make every test reading it true with nothing else held; "never" when none up to the copies that exist does). Rules are monotone, so with that many still held, taking a copy back changes nothing: take it back and go on.
  3. *Retract* (`Reach::remove`): otherwise check the facts that read the item, lowest rank first, each against the state as of its own rank minus the copy. Checking as of the fact's own rank, not against the current state, is what keeps this exact: support that runs through the fact itself (an item found behind the site) is of higher rank and never counts. A fact that still holds keeps its place; one that does not is retracted, and the facts that read it are checked in turn, along the item, counter, location, exit and region indexes.
  4. *Re-derive*: the retracted sites and half-days are the only ones that can hold again, because the new fixed point is inside the old one. The worklist evaluates only those (and the regions that lost a slice); what holds again gets a new rank.

  Mask growths are kept as a short list per region (one entry per support and rank), not a rank per slice: support checks, cascades and "the mask as of a rank" walk growths. The lists reserve a few entries per region and keep their capacity across resets (fixed per-slice arrays would cost 114 KB per state on the test world).

  The state carries over between placement phases, so a fill runs one full solve; beatability and the sphere report are read off the final state. Placements are byte-identical to a fresh solve per step: the `golden` example matches 40 of 40 recorded fills of the test world, and the unit test `carrying_the_fixed_point_keeps_every_placement` fills 200 seeds both ways on a world with thresholds, an `any` held up from inside, and clocks. `removal_does_not_keep_support_that_runs_through_itself` and `removal_retracts_only_what_depended_on_the_item` pin the two halves of the retraction.

Counters that check the contract (`Stats`, printed by `measure` and in `port-check`'s report under `work`), seed 1 of the test world, both instances (clock shuffle off / on):

| Counter | Tree-walking engine | Compiled section, fresh solve per step | Derivation log (since replaced) |
|---|---|---|---|
| full solves per fill | 288 / 293 | 288 / 293 | 2 / 2 (one per placement phase) |
| rule evaluations per fill (all steps) | 1 132 039 / 1 203 715 | 1 116 824 / 1 170 006 | 61 931 / 88 012 |
| rule evaluations per full solve | 3 931 / 4 108 | 3 878 / 3 993 | |
| worklist pushes per fill | 692 068 / 978 242 | 439 384 / 688 326 (about 2 350 per solve with clocks) | 61 298 / 84 264 |
| dependency entries walked per collected item, mean / max | 4.1 / 600, 4.8 / 600 | 4.1 / 601, 5.1 / 601 | |
| dependency entries walked for start items (before the first run) | 2 165 515 / 2 111 601 | 0 | 0 |

The derivation log was an intermediate step between a fresh solve per step and truth maintenance: it recorded each firing, mask change and visit, replayed the sites that read a taken-back item and, on a failed replay, undid and rewalked the log suffix. It cut fill time from 98 to 101 ms to 26 to 28 ms (clock shuffle on: 174 to 197 ms to 46 to 52 ms), but most of what remained was log bookkeeping, and the log (24 bytes per entry, about 115 / 170 KB per solve state) raised engine peak heap to 1.0 / 1.5 MB. Truth maintenance retracts along the dependency indexes instead of walking the log suffix: one full solve per fill instead of two, fill 18 to 25 ms, engine peak heap 715 / 718 KB on the test world. The per-step counters in the table were not re-measured for it. The saturation pass adds about 1 ms to the plan.

The max of 601 is the most-read item (one entry per site that reads it); it is walked once per copy collected, and sites already done are skipped at the push.

## Measured on the test world

`measure` (feature `memory-harness`), seed 1, both instances, release build, shared box under `systemd-run -p CPUQuota=200%` at load 6 to 9 (times vary about 20% run to run). "Before" is the tree-walking engine, "After" the compiled section with a fresh solve per step, "Now" the same with truth maintenance (only the rows it changes were re-measured).

| Phase | Before | After | Now |
|---|---|---|---|
| once per world version: parse manifest JSON, compile | 31 ms, 13.2 MB peak | 45 to 58 ms, 14.9 MB peak (JSON only here) |  |
| package on disk (rules + game stand-in, not deflated) | | 185 KB; rules section 68 021 bytes |  |
| load the package (read, check, unpack hot indexes) | | 1.5 ms, 0.33 MB held |  |
| parse the instance (typed, streamed, recorded oracle skipped) | 4 to 6 ms (text held: 2.9 MB) | 15 to 21 ms (nothing but the typed instance held) |  |
| plan and per-seed fold | 0.5 ms | 3.5 ms, 83 KB held |  |
| fill | 116 / 218 ms, 3 482 allocations | 108 / 184 ms, 40 allocations | 18 to 25 ms |
| one solve | 0.51 / 0.71 ms | 0.39 / 0.43 ms |  |
| **engine peak heap** | **17.9 / 17.8 MB** | **688 / 691 KB** | **715 / 718 KB** |

The section is 68 KB against the 66 KB estimate; the peak is above the 160 KB estimate because the typed instance (0.27 MB, the first open question below) and the instance parser's buffers are live while the plan is built, and the game stand-in keeps every key. Fills are byte-identical to the tree-walking engine for seeds 1 to 20 on both instances (the `golden` example), and `port-check` (5 legacy seeds, 50 inventories, 200 sanity fills) passes every check.

## Standing in for the shared types

`crates/data-fill/src/section_io.rs` holds the only bit and package reading this crate does, under the shared names:

| `section_io` | `crates/bits` / seed-bundle `package` |
|---|---|
| `width`, `read`, `bytes_for` | `bits::width`, `bits::read`, `bits::bytes_for` |
| `Packer::{push, finish}` | `bits::Packer::{push, finish}` |
| `PackedRef::{from_bytes, get}` | `bits::PackedRef::{from_bytes, get}` |
| `BitSet::{new, contains, insert, clear}` | `bits::BitSet::{new, contains, insert, clear}` |
| `stored(file, id)` | `package::stored(file, id)` (returns `BundleError` instead of `String`) |
| `file_bytes(&[(id, bytes)])` | `package::file_bytes(&[Section])` (adds a magic, a version and a hash per section) |

`read` here takes one unaligned eight-byte load when eight bytes remain, instead of copying; `bits::read` should do the same before the swap, or the solve loses about a third of its speed (measured).

## What is not decided here

- Whether the per-seed instance (pool, existing locations, groups) also becomes a package or seed section, or keeps arriving as JSON. Today's typed JSON instance is 0.27 MB held, which alone fits the budget but is most of it.
- Whether the converter or the loader computes the dependency index. It is shipped here (the converter computes it); the item group membership index is computed at load.
- Versioning: a change to any layout here bumps the header's format version; section 2 is listed as unstable in the v2 document until the first release.
