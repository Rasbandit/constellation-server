# Experiment: a large legacy Python world as data, and a fill over it

Status: experiment, not a standard. Sections marked **PROPOSAL** are suggested addenda to design doc 07 (`constellation/docs/design/07-data-only-world-format.md`) and doc 01 D2.5; nothing here changes either doc. The rule-language additions are tracked in #71.

Question: can a large legacy Python world be expressed as a data-only world, with no world code at generation time, and how fast is a fill over that data?

Short answer: **the logic, yes; the whole world, partly.** Every rule site translates. Most use only D2.5 combinators; the rest need three small additions (time masks, event-driven counters, a seeded roll). Pool composition, the location filter and a few seeded draws are still legacy code; each maps to a primitive doc 07 already lists or a small new one.

## The test world

A large legacy world with a day cycle of time slices. Its counts and the measurements made on it are not published here.

The world's source has no license that allows redistribution, so nothing derived from it is committed: the converter and instance dumper that read it stay out of this repository, and every committed fixture (`crates/data-fill/tests/fixture.rs`, `crates/data-fill/tests/fixtures/fairness_world.json`) is hand-made.

## What was built

| Piece | Where | Runs world code? |
|---|---|---|
| Converter: the world's generated logic to a doc-07 style JSON manifest | outside this repository | No. Parses the generated modules and reads literals only |
| Instance dumper: one seed's options, rolls, location set, pool and confinement, plus the reference generator's own solver as an oracle | outside this repository | Yes, stopped before any item is placed, under a memory cap |
| Engine: compile, incremental reachability, assumed fill, validation | `crates/data-fill` | No |
| Equivalence and fairness checks | `crates/data-fill` (`port-check`, `fill-fairness`), `tools/port-check`; see `docs/port-check.md` | Only through a world adapter you write |

## Rule fit

Counted by the converter, one entry per rule site: no site was untranslatable. Most need only D2.5 combinators; the others need `x.event_counter`, `x.time_slices` or `x.roll` (alone or together).

The world's hand-written helper methods became rule trees over items, counts, `min`/`max`/`*`/`div` and option comparisons. The one `not` over state became an equivalent monotone `or` of an item and an option. Every remaining `not` is over an option comparison and folds away (D2.7).

## PROPOSAL: three rule-language additions (#71)

### 1. `x.time_slices@0`: regions carry a time mask
The world models a day cycle as time slices. A region is reached *at a set of slices*, not just reached.

- `x.time: {slices: N (1..64), start_mask, owned?}`. Each reached region holds a mask of N bits; the start region starts at `start_mask`.
- New rule leaf `{"x.time_any": mask}`: true when the mask of the region the rule sits in meets `mask`. Valid only in region-local rules (exits, checks, events, stays). "At", "before", "after", "between" and "at night" are all this leaf with a constant mask.
- An open exit passes the **whole** source mask to the target. A night-only exit therefore also passes the day slices. That is coarser than a region-times-slice graph, and it is what the world's own logic does, so the data must say so; expanding to one node per region and slice would give different, stricter answers.
- `can_stay: true` regions expand their mask forward: from each held slice, waiting into each later slice is allowed unless that slice has a stay rule that fails, which stops the wait. Without stay rules this is a forward fill. The start region never waits.
- `owned` (clocks as items): when its `when` rule (options only) holds, reachable time is limited to the half-days whose `rule` holds (an item rule); expansion cannot cross an unowned slice; `none_owned_mask` applies when nothing is owned.
- Cost: one `u64` per region and a bit test per time leaf. Monotone in items and masks, so the fixed point is unique.

### 2. `x.event_counter@0`: events increment counters
D2.5 has `counter` ("derived, monotone increments") and `reach_event`. Here an event is declared per region (`events: [{event, rule}]`); each declared instance fires at most once and adds 1 to the counter of that name. Rules read `{"counter": name}` (one rule needs a counter of at least 7). This clarifies where increments come from; it is not a new combinator.

### 3. `x.roll@0`: a seeded integer per location
Some prices are drawn per location at seed setup (an integer range) and rules compare against them. `{"x.roll": "price", "at": loc}` reads the drawn value, folded to a constant before the fill. The draw itself belongs in the `select` family of doc 07 section 2.3 (integer range, named stream, one value per row). Until that primitive exists, the instance supplies the values.

### 4. `shuffles`: things assigned to slots (first slice of design doc 17)
One construct for randomization that is not item placement (`crates/data-fill/src/shuffle.rs`). A shuffle gives each slot one thing under a permission table that rows only narrow. Built so far: two kinds, with everything else in doc 17 refused at load by name.

```json
"regions": [{"key": "village", "exits": [
    {"to": "cave", "rule": {"has": "lantern"}, "key": "cave_mouth", "door": "cave_mouth", "group": "cave"},
    {"to": "pond", "rule": {"true": {}}, "key": "well_drop", "group": "drop"}]}],
"shuffles": {
  "entrances": {"check": "layout", "coupled": true,
    "slots": {"type": "exit", "names": ["cave_mouth", "mill_door", "well_drop", "chute"]},
    "allow": [{"slots": {"groups": ["cave"]}, "only": {"groups": ["cave"]}}]},
  "guardians": {"check": "none",
    "slots": {"type": "name", "names": ["marsh_hall", "spire_hall"]},
    "things": {"golem": {"rule": {"has": "hammer"}}, "hydra": {"rule": {"has": "spear"}, "group": "large"}},
    "allow": [{"slots": {"names": ["spire_hall"]}, "deny": {"groups": ["large"]}}]}}
```

- **Exits.** An exit a shuffle names carries a `key`, and may carry a `group` and a `door`. Two exits with the same `door` are the two sides of one door and must be each other's way back; an exit without a second side is one-way. A world without `shuffles` may not carry these fields.
- **`layout` (slots of type `exit`).** Every listed exit is a slot (leaving through it) and a thing (arriving where it leads unshuffled). The exit's own rule stays on the exit. One-way exits trade only with one-way exits. With `coupled: true` the shuffle lists one side of each door, the side that leads away from the start, and the other side follows whatever its own group is, so a door leads back where it was entered; a door listed from its far side is a load error that names it. Without `coupled` every listed side is its own slot. Not allowed on a world with a time context (more than one slice, or clocks) until exits that narrow time are settled (design doc 17 section 9).
- **`none` (slots of type `name`).** Slots are names the shuffle lists; things may carry a `rule` and a `group`. A rule reads the result as `{"at": [shuffle, slot]}`: where a rule is expected it is the rule of the thing there, and inside `eq` or `ne` against a thing name it tests which thing is there. Both fold to constants per seed. A slot that is read as a rule compiles to one shared def that lists every thing with its rule; every read refers to it, and the load-time dependency index therefore lists every candidate's items for a reading site.
- **Rows.** `{"slots": S, "only": T}`, `{"slots": S, "deny": T}`, `{"things": T, "only_in": S}`, `{"things": T, "deny_in": S}`, where `S` and `T` are `{"names": [...]}`, `{"groups": [...]}` or both.
- **Order.** A shuffle runs after every shuffle its things' rules read, ties by key; a cycle is a load error that lists each read. The layout is solved once per world, after them. `data_fill::shuffle::order` gives the order by name. Reads are the only edges so far (see #210).
- **Per seed.** `data_fill::shuffle::stage` runs before the fill: the `none` draws, one joint layout solve, then the whole-world check (holding the whole pool, every region of the manifest and every check that exists is reached and the goal holds). A failed attempt starts again, at most `config::SHUFFLE_MAX_ATTEMPTS` times, then the generation fails naming the player, the world and the shuffle. What no layout can change (a region this seed's options close) is an error at the first attempt. A world without `shuffles` is not staged and draws nothing. A staged world shares the loaded world and owns only where its exits lead.
- **Output.** Each slot's `slot_data` gets the key `shuffles`: `{shuffle: {slot: thing}}`, both sides of a coupled door included. Any client of the room can read any slot's `slot_data`.
- **Not yet.** The world's `require` and `kinds`, and a shuffle's `when`, `pin`, `pool`, `kind`, `after`, `scope`, `vanilla`, counts, weights, repeats and payloads other than a rule are refused at load by name (see #211). The CLI, `fill-fairness` and `port-check` refuse a world with shuffles.
- Limits are in `crates/config` (`SHUFFLE_*`, `SHUFFLES_MAX`). A slot of a `none` shuffle takes one of the 256 options a world's rules can read.

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

The instance dumper runs the reference generator's solver for a seed and records reached checks, every reached region with its time mask, and every event counter. The engine solves the same inventories and compares all three; `port-check` (`docs/port-check.md`) then ran on the same world against the legacy side through an adapter.

Known differences that no sampled state hit:
- With clocks shuffled, the legacy solver fixes the start mask at the start of a solve and its expansion can drop unowned bits; the engine keeps masks monotone and grows the start mask when more half-days become owned. The two disagree in principle only when a clock is granted mid-solve by an option-disabled check.
- The engine's fill is not the reference generator's fill. Same pool and constraints, a different RNG (ChaCha8, doc 01 D3.2) and order, so seeds differ; beatability and the checks above are compared instead. `crates/data-fill/src/parity_rng.rs` reproduces the reference generator's RNG bit for bit, for replaying legacy seeds only.
