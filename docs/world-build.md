# The world build tool (`crates/world-build`)

Turns a world as an author wrote it into the manifest JSON the fill engine loads (`crates/data-fill`), and into the per-seed instance the engine fills from. The design is doc 16 (the world file standard) and doc 17 (generic shuffles) in the `constellation` docs repo. Issue #27.

**Status.** The owner has chosen an authoring language of its own, with its own file extension, over YAML; its design is being written (docs repo, draft doc 18). So everything here is built on a syntax-neutral model, and the front end that reads a file is one function (`world_build::front::read`) that is not built yet and refuses every input. A world is built today from its model, in Rust.

Two things the model is shaped for: a front end that expands location families and tables into many plain checks, each keeping the place of the declaration it came from (a test builds 20,000 generated checks in about 2 s in a debug build, measured once on a shared machine); and a ledger of permanent ids, held apart from names (`id` on items and checks).

## The parts

| Part | Where | What it does |
|---|---|---|
| Model | `src/model.rs` | The world as written, every value with its line and column. What a front end produces. |
| Rule parser | `src/rule.rs` | Rule strings to a tree, by the precedence of doc 16 section 5.0. Recursive descent with a depth cap. |
| Checks and lowering | `src/world.rs`, `src/lower.rs` | Names, duplicates, the format rules, the shuffle slice; rules to the engine's trees. Collects every error. |
| Canonical JSON | `src/canon.rs` | Sorted keys by bytes, integers only, no whitespace, one newline at the end. |
| Instance | `src/instance.rs` | Per option values: which checks exist, the pool, classes, start items, fixed placements, placement sets. Also the build-time pool test. |
| Front end | `src/front.rs` | Source text to model. Not built. |

```rust
let built = world_build::build_model(&world, "tidewell.world")?; // Err: every error, file order
built.json();                       // the manifest, canonical text
built.default_instance()?;          // data_fill::Instance at every option's default
built.instance(&options)?;          // for other option values
```

Every built manifest goes through the engine's own compiler (`data_fill::section::compile`) before it is returned, as a last check.

## The example world

"Tidewell Isle", invented for the tests: `crates/world-build/tests/support/mod.rs` (`tidewell()`). 9 regions, 17 checks, 7 items, 4 options (a toggle for tricky climbs, a range for pearls needed, a toggle for market stalls, a choice of start gift), one def, one item group, one start item, a fixed placement, an excluded check, an event, a coupled entrance shuffle over the three doors from the dock (`doors`, check `layout`), and a shuffle of two wardens whose rules the exits behind them read (`wardens`, check `none`). The same file has doc 16's smallest world (`two_rooms()`), whose manifest bytes are pinned.

Run the tests: `cargo test -p world-build`. The end-to-end test builds Tidewell, loads it in the engine and generates seeds 1 to 50 at 1 and 4 players; `data_fill::seed::generate` refuses a world a player cannot finish, so every seed passing means every seed is beatable.

## The rule language this version builds

| Written | Engine form | Notes |
|---|---|---|
| `true`, `false` | `{"true": {}}` | |
| `a and b`, `a or b` | `all`, `any` | At most 255 parts. Mixing without parentheses is a warning. |
| `not a` | `not` | Options and shuffle name tests only (D-132). |
| `has(x)`, `has(x, n)` | `has`, with `n` only when it is not 1 | `n` a literal, 0 to 255 (engine limit). |
| `has_all(..)`, `has_any(..)` | same, items sorted | |
| a def's name | `ref` | Cycles are errors. In a field settled from options, the def must read options only; the per-seed step evaluates each def once. |
| an event's name | `counter >= 1` | |
| `opt.k` (toggle) | `opt.k != 0` | Other option types must be compared. |
| `opt.k == value` (choice) | `eq` with the value's position | Also `!=` and `opt.k in [a, b]`. |
| `>= > <= < == !=` | `ge gt le lt eq ne` | Monotone only: state on the larger side; `==`, `!=` never over state. |
| `count(x)`, `level(x)`, `counter(e)`, `group_count(g)` | `count`, `count`, `counter`, a sum of counts | |
| `+`, `min`, `max`, `*`, `div` | `int` | `*` over state needs a literal factor of at least 0; `div` a positive literal. Literals fit 16 bits. |
| `reach(region)` | `reach` | Goal only. |
| `at.s.slot` | `at` | As a rule (every thing needs a rule) or tested by thing name. Not on an exit shuffle. |

Refused with a message that names them: `-` and `mod` (the engine cannot evaluate them), `during` (no time contexts yet), `roll` (its draw waits on D-47), any dotted name other than `opt.` and `at.` (a rule reads only its own world, D-133).

## What is checked

Each error names the line, the column and what to do.

- Names: unknown names, a name used as the wrong kind ("`lamp` is an item; did you mean `has(lamp)`?"), duplicates across the one namespace (events may repeat), reserved words, name shape.
- Regions: an exit to a missing region, two exits to one region, a region no exit leads to from the start.
- Format rule a (D-132): `not` over state, comparisons that can turn false, products and divisions that can.
- Format rule b (D-133): dotted names that are not options or shuffle reads.
- Format rule c (D-134): check and region `when`, `no_progression`, `start_items` and `assumed` conditions read options only, never state or a shuffle result.
- Items: class, count bounds, `fill_with` items and weights, `holds` with no copy left, start items, placement sets (an item in one set).
- Pool (doc 16 section 3.4): every combination of the options the counts read; past 4,096, every combination of each option's lowest and highest value; past that, each option alone at its two ends, with a warning. Too many items for the checks names the settings. Counts are checked before any copy is written out, and start items are capped.
- Shuffles, the engine's first slice: exit shuffles (`layout`) and free-name shuffles (`none`). Refused: `when`, `pin`, `vanilla`, `pool`, `after`, `kind`, `scope`, thing fields other than `rule` and `group`, row fields `again` and `when`, `check: reach`, a `coupled` that reads options, coupling on a name shuffle, the reserved names `layout` and `own_fill`, and any name a row or slot list does not have.
- Nesting: a rule string nests at most 64 deep as written (a sum is one node however long), and lowering counts the defs a rule reaches against the engine's own cap of 256, so no input exhausts the stack. The engine counts a few short forms as one or two nodes more, so near the cap its own check may be the one that refuses; both messages say what to do.
- Shuffles whose things' rules read each other in a cycle, a shuffle reading itself included.
- Fields the model carries but this version does not build (`rolls`, `contexts`, `require`, `stays`, ...) are reported as not supported yet.

Limits are in `crates/config`: `WORLD_RULE_MAX_DEPTH` (64), `WORLD_BUILD_MAX_ERRORS` (200 errors and 200 warnings, the rest counted in one last line), `RULE_MAX_PARTS` (255, the engine's), `WORLD_POOL_TEST_MAX_COMBINATIONS` (4,096), `WORLD_ITEM_COPIES_MAX` (32,767), `WORLD_OPTIONS_MAX` (256, the engine's), `WORLD_START_COPIES_MAX` (1,024). All design values except the engine's.

## Choices made where doc 16 is silent

- **Output is the engine's manifest shape, not all of doc 16's canonical JSON.** The engine reads a subset (doc 16 section 3.5 table); fields it would ignore are not written. Doc 16's world-side fields (class, count, pool, `when`, `holds`, `no_progression`, placement, start items, assumed) go into the per-seed instance instead.
- **Canonical text is compact**: no whitespace, a newline at the end.
- **Ids**: an item's or check's `id` from the ledger when the model has one (1 to 2^53, each used once per table); the rest are numbered on from the largest given, in file order, so a world with no ledger is numbered from 1. Reading and writing the ledger file is the front end's job and is not built.
- **`nothing`** is added as an item (filler, the id after the largest) only when the world has no `fill_with`.
- **Free slots are split by weight deterministically** (largest remainder, ties by key), since the seeded draw waits on D-47.
- **A `holds` copy of a check that does not exist this seed stays in the pool.**
- **`class` is required**; the doc's proposed default by use is not built.
- **Names** are `[a-z_][a-z0-9_]*`; a game key also allows `-`. Exit keys are `region:to`.
- **A region's `when`** is joined with `and` into its exits' and events' rules and its checks' `when`.
- **A rule error in a rule written over several lines** reports where the rule starts.

## What the manifest cannot express (gaps)

- `format`, `name` and `requires` (no header fields beyond `game` and `version`).
- Item class, count, the pool and `fill_with`, check `when`, `holds`, `no_progression`, placement sets, start items and assumed items: all per-seed instance fields, which this tool computes, but a manifest alone does not carry them.
- An option's type, values and range (the manifest holds a key and an integer default).
- Item groups as named lists (a group count is written out as its sum).
- A thing's `count`, `min`, `weight`, payloads, shuffle `when`, pins and the other later fields of doc 17.
