# Experiment: a large legacy Python world as data, and a fill over it

Status: experiment, not a standard. Sections marked **PROPOSAL** are suggested addenda to design doc 07 (`constellation/docs/design/07-data-only-world-format.md`) and doc 01 D2.5; nothing here changes either doc. The rule-language additions are tracked in #71.

Question: can a large legacy Python world be expressed as a data-only world, with no world code at generation time, and how fast is a fill over that data?

Short answer: **the logic, yes; the whole world, partly.** Every one of its 3685 rule sites translates. 3206 use only D2.5 combinators; the rest need three small additions (time masks, event-driven counters, a seeded roll). Pool composition, the location filter and a few seeded draws are still legacy code; each maps to a primitive doc 07 already lists or a small new one. A fill over the data takes 18 to 25 ms, against 17 to 29 s for the legacy generation of the same world.

## The test world

2641 locations (1516 exist for the seed measured), 237 items, 318 regions, 45 time slices, 3685 rule sites (checks 2807, connections 394, exits 327, events 120, stays 37) and 107 named defs, 16 of which read time. The manifest is 830 KB of JSON.

The world's source has no license that allows redistribution, so nothing derived from it is committed: the converter and instance dumper that read it stay out of this repository, and every committed fixture (`crates/data-fill/tests/fixture.rs`, `crates/data-fill/tests/fixtures/fairness_world.json`) is hand-made.

## What was built

| Piece | Where | Runs world code? |
|---|---|---|
| Converter: the world's generated logic to a doc-07 style JSON manifest | outside this repository | No. Parses the generated modules and reads literals only |
| Instance dumper: one seed's options, rolls, location set, pool and confinement, plus the reference generator's own solver as an oracle | outside this repository | Yes, stopped before any item is placed, under a memory cap |
| Engine: compile, incremental reachability, assumed fill, validation | `crates/data-fill` | No |
| Equivalence and fairness checks | `crates/data-fill` (`port-check`, `fill-fairness`), `tools/port-check`; see `docs/port-check.md` | Only through a world adapter you write |

## Rule fit

Counted by the converter, one entry per rule site:

| Rule sites | Count |
|---|---|
| Total | 3685 |
| Only D2.5 combinators | 3206 |
| Need `x.event_counter` only | 253 |
| Need `x.time_slices` (alone or with others) | 198, of which 37 are the stay rules themselves |
| Need `x.roll` (alone or with others) | 38 |
| Untranslatable | **0** |

The world's hand-written helper methods (about 30) became rule trees over items, counts, `min`/`max`/`*`/`div` and option comparisons. The one `not` over state became an equivalent monotone `or` of an item and an option. Every remaining `not` is over an option comparison and folds away (D2.7).

## PROPOSAL: three rule-language additions (#71)

### 1. `x.time_slices@0`: regions carry a time mask
The world models a day cycle as 45 time slices. A region is reached *at a set of slices*, not just reached.

- `x.time: {slices: N (1..64), start_mask, owned?}`. Each reached region holds a mask of N bits; the start region starts at `start_mask`.
- New rule leaf `{"x.time_any": mask}`: true when the mask of the region the rule sits in meets `mask`. Valid only in region-local rules (exits, checks, events, stays). "At", "before", "after", "between" and "at night" are all this leaf with a constant mask.
- An open exit passes the **whole** source mask to the target. A night-only exit therefore also passes the day slices. That is coarser than a region-times-slice graph, and it is what the world's own logic does, so the data must say so; expanding to 318 x 45 nodes would give different, stricter answers.
- `can_stay: true` regions expand their mask forward: from each held slice, waiting into each later slice is allowed unless that slice has a stay rule that fails, which stops the wait. Without stay rules this is a forward fill. The start region never waits.
- `owned` (clocks as items): when its `when` rule (options only) holds, reachable time is limited to the half-days whose `rule` holds (an item rule); expansion cannot cross an unowned slice; `none_owned_mask` applies when nothing is owned.
- Cost: one `u64` per region and a bit test per time leaf. Monotone in items and masks, so the fixed point is unique.

### 2. `x.event_counter@0`: events increment counters
D2.5 has `counter` ("derived, monotone increments") and `reach_event`. Here an event is declared per region (`events: [{event, rule}]`); each declared instance fires at most once and adds 1 to the counter of that name. Rules read `{"counter": name}` (one rule needs a counter of at least 7). This clarifies where increments come from; it is not a new combinator.

### 3. `x.roll@0`: a seeded integer per location
Some prices are drawn per location at seed setup (an integer range) and rules compare against them. `{"x.roll": "price", "at": loc}` reads the drawn value, folded to a constant before the fill. The draw itself belongs in the `select` family of doc 07 section 2.3 (integer range, named stream, one value per row). Until that primitive exists, the instance supplies the values.

### No addition needed
- Option-disabled checks that still hand out their vanilla item when reached: the location carries `vanilla`, and a location absent from the seed grants it on reach (doc 07's event-location idea).
- Items confined to their own area (for example keys): an item group may only go to its location group (doc 07 `item_rule`).

## Not converted (still legacy code in this experiment)

| Legacy part | Size | What it does | Primitive it needs |
|---|---|---|---|
| Location filter | about 160 lines | Which locations exist: by check type and option, plus a seeded pick of N locations per group | `when` on location tables (data) plus `select@1` |
| Item pool | about 360 lines | Pool from vanilla items of existing locations, plentiful and trap counts | Pool arithmetic and `weighted_filler` (doc 07 lists both); not assessed line by line |
| Starting items | about 70 lines | Starting items from option toggles | An option-to-start-inventory table (data) |
| Early seed setup | about 70 lines | Price rolls, option clamps, the starting clock pick | `x.roll` / `select`, option `requires` or clamp |
| Static hints | about 40 lines | Lists where certain items landed | A post-fill output primitive ("placements of item X"); not in doc 07 yet |

None of these needed code that a primitive could not plausibly cover, but that is a reading, not a port. `tools/port-check/setup_tables.py` exports the static tables these parts read, as the first step of porting seed setup.

## Correctness check

The instance dumper runs the reference generator's solver for a seed and records reached checks, every reached region with its time mask, and every event counter. The engine solves the same inventories and compares all three.

| Instance (seed 1) | States compared | Result |
|---|---|---|
| Options as given (clocks not shuffled) | no items, all items, 40 random sub-inventories | 42 of 42 identical: 605 and 2641 checks, 50 and 318 regions, masks and counters equal |
| Same options with clocks shuffled | same 42 | 42 of 42 identical |

`port-check` (`docs/port-check.md`) then ran on the same world against the legacy side through an adapter: reachability, replay in both directions (legacy seeds judged by the port, port seeds judged by the legacy side) and determinism all pass.

Known differences that no sampled state hit:
- With clocks shuffled, the legacy solver fixes the start mask at the start of a solve and its expansion can drop unowned bits; the engine keeps masks monotone and grows the start mask when more half-days become owned. The two disagree in principle only when a clock is granted mid-solve by an option-disabled check.
- The engine's fill is not the reference generator's fill. Same pool and constraints, a different RNG (ChaCha8, doc 01 D3.2) and order, so seeds differ; beatability and the checks above are compared instead. `crates/data-fill/src/parity_rng.rs` reproduces the reference generator's RNG bit for bit, for replaying legacy seeds only.

## Measurements

Box: shared 16-core VM, every run under `systemd-run --user --scope -p MemoryMax=4G -p CPUQuota=200%`. Release build of `data-fill` only. Times vary about 20% run to run.

| What | Result |
|---|---|
| One seed's generation in the engine (load the compiled package, bind the instance, fill, validate) | about 50 ms |
| Fill (one full solve, then truth maintenance per step) | 18 to 25 ms |
| Engine peak heap | about 715 KB |
| One full-inventory solve: legacy solver versus engine | 15 to 28 ms versus 0.4 ms |
| Legacy generation of the same world | 17 to 29 s, 89% of it in its solver |

Every engine fill was beatable, reached all 1516 existing locations and needed one attempt. Each optimization that must not change placements (compiled section, fixed solve buffers, carrying the fixed point, saturation, truth maintenance, per-region mask growths) was checked byte-identical against the previous engine on 40 recorded seeds (the `golden` example). How the fill keeps one solve state exact across steps, and the per-phase numbers, are in `docs/world-rules-section.md`.
