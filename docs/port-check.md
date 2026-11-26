# Porting guide: proving a data-only port matches its legacy Python world

Two tools in `crates/data-fill`:

- `port-check`: is the data-only port equivalent to the legacy Python world? Pass/fail.
- `fill-fairness`: is our fill engine fair against an unbiased baseline? Meant for nightly CI.

Both are generic. The only world-specific code is a small Python adapter you write for the legacy world, plus the data-only manifest of your port.

## What you need

1. The manifest of your port (doc 07 shape plus the `x.` extensions in `data-only-world-experiment.md`, see #71). Location and item keys in the manifest are the keys every exchange below uses.
2. A Python environment where the legacy world runs.
3. An adapter file (next section). Keep it next to your world, not in this repository, if the legacy world's license does not allow redistribution.

## The adapter

`tools/port-check/legacy_driver.py` runs your adapter and speaks JSON lines on stdio. Your file defines `make(args) -> adapter`; `args` are whatever you put after the adapter path on the command line. The adapter has five methods. Every key is a manifest key.

| Method | Returns |
|---|---|
| `hello()` | `{"name": str, "location_scope": "all" or "existing", "has_regions": bool, "has_events": bool}` |
| `instance(seed)` | The legacy world's setup for that seed, before any item is placed (schema below) |
| `solve(seed, inventory)` | What the legacy logic reaches holding the instance's start items plus `inventory` (a list of item keys, repeats allowed): `{"locations": [...], "regions": {key: time_mask}, "events": {key: count}}`. `regions` and `events` are optional; send them if your world has them |
| `generate(seed)` | A full legacy generation for that seed: `{"placements": {location: item}}`, every filled location including locked ones |
| `spheres(seed, placements)` | Load `placements` into a fresh legacy setup for that seed and collect sphere by sphere with the legacy world's own state: `{"spheres": [[location, ...], ...], "beatable": bool, "unreachable": [...], "violations": [...]}` |

`location_scope`: `"all"` when `solve` reports every location of the world (also ones that do not exist this seed), `"existing"` when it reports only the seed's locations.

`spheres` semantics, which both sides follow: round 1 is every filled location reachable from the start state; then everything found so far is collected (only progression items count as held) and the next round is what became newly reachable. Rounds that contain only event locations (no key in the manifest) are dropped. `violations` lists anything the legacy world would reject about the placements: a location that does not exist, an item not in the pool, progression on an excluded location, an item outside its placement group, pool items left over. It must be empty for legacy-generated placements.

`instance(seed)` schema (the engine's `Instance`, `crates/data-fill/src/fill.rs`):

| Key | Meaning |
|---|---|
| `options` | Option values the manifest's rules read |
| `rolls` | Seeded values per location (`{roll name: {location: value}}`) |
| `locations` | Locations that exist this seed |
| `excluded` | Locations that may not hold progression |
| `locked` | Location to the item fixed there |
| `pool` | Items to place, repeats allowed |
| `pool_class` | `progression`, `useful`, `filler` or `trap` for every item in the pool, the locked items and the start inventory |
| `start_inventory` | Items the player starts with |
| `logic_start` | Item counts the logic assumes from the start without them being real items |
| `item_groups`, `location_groups` | An item in a group may go only to a location of the same group |

Write `solve` against the legacy world's own solver or state, not a re-implementation: the point is to compare against the original. Print anything you like; the driver sends stdout to stderr so it cannot break the protocol.

## Exporting seed-setup tables

A port that runs seed setup from data needs the legacy world's static tables (location lists, filters, option declarations, constants) in its manifest. `tools/port-check/setup_tables.py` exports them once per world version: it imports the world module (nothing is generated) and writes the tables a spec file names into one manifest section.

```sh
python -I tools/port-check/setup_tables.py --path /path/to/legacy/root --module <dotted world module> \
  --spec my_world.setup-spec.json --manifest my_world.manifest.json --out my_world.setup.json \
  [--prelude my_world_setup_prelude.py] [--section x.setup]
```

| Argument | Meaning |
|---|---|
| `--module` | Dotted name of the legacy world module (the package that declares the world) |
| `--path` | Put a directory on `sys.path` first; repeat as needed |
| `--spec` | JSON: `{"tables": {output key: path}, "options": path}`. A path is dotted from the world module; submodules are imported on demand and dict keys work as segments (`tables.by_scene.north`) |
| `--prelude` | A Python file run before the import: put the legacy framework's bootstrap here (working directory, caches, loaders). It may define `extra(world) -> dict` for tables that need computing; its keys are added to the section |
| `--section` | Manifest key to write (default `x.setup`); the rest of the manifest is kept |

Values become JSON as follows: an enum member becomes its lowercased member name (the manifest key convention), a set a sorted list, a tuple or named tuple a list, a dataclass an object; anything else stops the export with the path of the value. `options` reads the option declarations of the world class at that path (its options dataclass's type hints) and writes one row per option with `key`, `default` and `kind` (`range` with `range` and special `names`; `toggle` when the names are exactly false/true; `choice` with names plus aliases; otherwise `other`). Output keys are sorted, so the file is stable across runs. Keep the spec and prelude next to your adapter, not in this repository. Tests: `python -m unittest discover -s tools/port-check`.

## Running the equivalence check

Build once (capped, data-fill only):

```sh
CARGO_TARGET_DIR=$PWD/target systemd-run --user --scope -p MemoryMax=4G -p CPUQuota=200% \
  cargo build -j2 -p data-fill --release
```

Run (the legacy command is run through `sh -c`; it inherits the memory cap of the scope):

```sh
systemd-run --user --scope -p MemoryMax=4G -p CPUQuota=200% target/release/port-check \
  --manifest my_world.manifest.json \
  --legacy "python -I tools/port-check/legacy_driver.py /path/to/my_adapter.py <adapter args>" \
  --out port-check-out --seeds 20 --ours 20 --inventories 200 --sanity-seeds 200
```

Options: `--seeds K` legacy seeds (from `--first-seed`, default 1), `--ours K` of our fills checked by the legacy side, `--inventories N` random inventories spread over the K instances (each instance also gets "no items" and "all items"), `--fill-seed-base B` (our fill seed is `B + instance seed`), `--sanity-seeds N` (tier 1 below; 0 turns it off).

It writes `port-check-out/report.json` (also printed), the instances and legacy placements under `legacy/`, and our placements under `ours/`. Exit code 0 means every check passed, 1 means one failed, 2 means the run itself failed (bad arguments, the legacy side crashed).

## The checks

| Check | Passes when |
|---|---|
| `reachability` | For every inventory, the same locations are reached, and (when reported) the same regions with the same time masks and the same event counts |
| `legacy_seeds_replayed` | For each legacy generation, our engine finds the same spheres round by round, agrees on beatability, and the legacy side reports no violations |
| `port_seeds_judged_by_legacy` | For each of our fills, the legacy side finds no violations, the same spheres and the same beatability |
| `determinism` | Three fills of the same seed write byte-identical files |

On failure, `first_divergence` says where to look:
- reachability: the seed, the inventory (label and items), the first location (or region, or event) that differs, which side reached it, and every manifest rule that can reach that location;
- spheres: the seed, the round, the location, which side reached it, the item there, the progression items held going into that round, and the location's rules.

A legacy seed that fails to generate is listed under `legacy_generation_failures` and skipped; that is the legacy world's own failure, not a divergence.

### Determinism contract

Same instance, same fill seed and same engine version give the same bytes. Every placements file records `version` (`data-fill <crate version>` and the output format `port-check/1`); a change to either is allowed to change the bytes. The report lists one content hash per fill seed.

### Seed shape (informational)

Our fills will not match the legacy generator seed for seed (different RNG and order), so the report compares distributions instead, over the K legacy seeds and the K of ours: number of spheres, progression items per sphere, the sphere where key items (progression items with at most two copies) land, and each region's share of progression. Large gaps are listed under `flags`. Nothing here fails the run; a flag is a prompt to look, not a verdict.

## Several players

A room is one or more players of one world, each with their own settings, filled together; items cross between worlds (design in `docs/world-rules-section.md`, "Several players"). `port-check` runs the room mode when the adapter's `hello()` reports `"players": N` with N above 1; everything above stays as is for one player.

The adapter gains these (every player number is 1 based; the keys are manifest keys):

| Method | Returns |
|---|---|
| `hello()` | also `"players": N` |
| `instance(seed, player)` | the player's instance (their options, the pool they own, their start inventory, their locked items; an item they own may lie in another world) |
| `solve(seed, inventory, player)` | what that player's legacy logic reaches, as above |
| `room_generate(seed)` | `{"placements": {"1": {location: {"item": key, "owner": 2}, ...}, "2": {...}}}`: for each player's world, every filled location, the item there and the player it belongs to |
| `room_spheres(seed, placements)` | `{"spheres": [[[player, location], ...], ...], "beatable": bool, "unreachable": [[player, location], ...], "violations": [...]}`: the placements loaded into a fresh room and collected round by round over the whole room (round 1 is every filled location of every world reachable from the start states; then everything found is collected). `beatable` means every player's goal. `violations` as above, plus an item with a placement group outside its owner's world |

`tools/port-check/legacy_driver.py` forwards the new methods. The checks are the same four, over rooms: `reachability` (per player: every inventory, one request per player), `legacy_seeds_replayed` (our engine finds the same spheres over the whole room and the same beatability for the legacy room), `port_seeds_judged_by_legacy` (the legacy side judges our room), `determinism` (three fills of the same seed write byte-identical room files). `--inventories` is spread over seeds and players.

The seed-shape column gains `cross_world_progression_share`: per player, the share of their progression items that lie in other players' worlds, for the legacy rooms and ours (a flag when a player's mean differs by more than 0.15). The tier 1 sanity fills are not run for rooms; `fill-fairness` measures a room (below).

Run (K = 5, three players of the 1516-location test world: clock shuffle off, on, off; two further logic-affecting options each; the settings are in the adapter's yaml files, kept next to it):

```sh
target/release/port-check --manifest my_world.manifest.json \
  --legacy "python -I tools/port-check/legacy_driver.py /path/to/my_room_adapter.py <adapter args>" \
  --out port-check-room --seeds 5 --ours 5 --inventories 150 --sanity-seeds 0
```

Result (K = 5 legacy rooms, 5 of ours, 150 inventories; the legacy generation takes 64 to 96 s per room, so the run is about 7 minutes; our engine's share of it is 4 s):

| Check | Result |
|---|---|
| `reachability` | 180 of 180 inventories identical (per player: locations, regions with time masks, events) |
| `legacy_seeds_replayed` | 5 of 5: same spheres over the whole room (20 to 34 spheres), all beatable |
| `port_seeds_judged_by_legacy` | 5 of 5: no violations, same spheres, all beatable |
| `determinism` | 5 of 5 seeds, three fills each, byte-identical |

Seed shape (informational), legacy rooms against ours, 5 and 5 rooms, player 1 / 2 / 3:

| | Legacy | Ours |
|---|---|---|
| Spheres per room, mean (min to max) | 25.4 (20 to 34) | 25.6 (20 to 33) |
| Progression items per sphere, mean | 34.8 | 34.5 |
| Key item depth, mean fraction of the room's spheres | 0.43 | 0.51 |
| Share of a player's progression in other worlds | 0.632 / 0.640 / 0.633 | 0.637 / 0.652 / 0.648 |
| Share if placed uniformly over three worlds | 0.667 | 0.667 |

The 178 per-item and per-region flags the report lists are single-item means over five rooms and are noise at this sample size; the sphere counts and shares above are the numbers to watch. Before the rounds below, the same run had 30.8 spheres and a key item depth of 0.47 (rooms that were longer than the legacy ones); a fill of one item at a time is the more accurate assumed fill, and the legacy default of one item per player at a time is biased towards earlier locations by design.

Two things this run found. The start inventory of the player with shuffled clocks was a night clock, and the engine owned the first time slice with it (the solver does not): fixed, with a unit test, in this PR (a related case, issue #94, stays open). And the first run of ours was one item at a time; the rounds of one item per player come from reading the legacy fill.


### Scaling with players

`measure_room` (feature `memory-harness`) fills a room of N players of the test world (the two instances in turn, so the 10 and 20 player rooms repeat settings), release build, one fill of seed 1, `systemd-run -p CPUQuota=200%`; the world is loaded once and shared; fill time is the median of five fills; engine peak heap is the counting allocator's peak, world load included.

| Players | Setup, 1 thread (ms) | Fill, median of 5 (ms) | Fill per player (ms) | Rule evaluations (millions) | Retractions per removal (mean) | Rounds (spheres) | Share in other worlds | Engine peak heap (MB) |
|---|---|---|---|---|---|---|---|---|
| 1 | 22.1 | 19.3 | 19.3 | 0.096 | 252 | 14 | 0.00 | 0.71 |
| 2 | 40.5 | 70.5 | 35.3 | 0.227 | 309 | 14 | 0.51 | 1.01 |
| 3 | 60.0 | 130.0 | 43.3 | 0.456 | 429 | 19 | 0.66 | 1.37 |
| 10 | 203.5 | 1 060 | 106 | 3.78 | 1 083 | 49 | 0.87 | 3.83 |
| 20 (extra) | 386.6 | 3 597 | 180 | 10.6 | 1 551 | 77 | 0.92 | 7.37 |

- **Memory per added player: about 350 KB** (a plan and a solve state; the world and its rules section are shared). From 1 to 20 players the peak grows 0.71 to 7.37 MB, linear: 291 KB for the second player, then 346 to 351 KB each.
- **Setup per added player: about 20 ms** on one thread (parse the instance, resolve the plan), and a generation in the product runs it on `config::GENERATION_WORKERS` threads (one; the tools use `config::FILL_SETUP_WORKERS`).
- **Fill time per added player grows with the room on the carried path** (rooms under 8 players since the forward path below): 19, 35, 43, 106 and 180 ms per player at 1, 2, 3, 10 and 20 players; total time grows about as players^1.7 (rule evaluations as players^1.6). This is **worse than linear and flagged**: one removal retracts more as the room grows (mean 252 to 1 551 facts), and a room has more removals. Marginal cost of the 2nd player is 51 ms, of the 3rd 60 ms, of each of players 4 to 10 about 133 ms, of players 11 to 20 about 254 ms. Issue #93 tracks it. For scale, a 10 player room fills in 1.1 s.
- The cheap changes (a min-heap of suspects over players, a worklist of players with something to run, piles indexed by owner) are in: placements are byte-identical to before them (61 fills over 1, 2, 3 and 10 players, clock on and off, and the three-player fixture) and the time dropped 10 to 12 percent at 10 and 20 players. What is left is rule evaluation (the evaluations column is unchanged), not bookkeeping.
- Every fill in this table is one full solve, then incremental steps (`solves` 1 in all rows). That holds for rooms under `config::ROOM_FORWARD_MIN_PLAYERS` (8) players; larger rooms solve once per round (next section).


### Rooms of 8 players or more: a forward solve per round

The table above is the carried path (`room::Path::Carry`). A room of `config::ROOM_FORWARD_MIN_PLAYERS` (8) players or more now places its rounds on `room::Path::Forward`: the room is solved afresh for each round with none of the round's items held, forward only, each player's state on one of `config::ROOM_FORWARD_WORKERS` (4) threads. The placements are the same on both paths: a round's state is the unique least fixed point for what is held and placed, and placing reads only what it reaches (tests `a_forward_solve_per_round_places_what_truth_maintenance_does` and `the_forward_path_is_the_same_with_one_worker_and_with_many`).

The test world cannot be committed, so CI measures synthetic worlds of 1,000 checks (`data_fill::synth`, `examples/synth_room.rs`). The heavy profile follows the test world's counts per check (0.21 regions, 2.4 rule sites, 0.07 defs, 45 time slices, clocks on, 2.4 nodes per rule site against about 2.6). Measured on one GitHub-hosted runner by the `bench-room` job (release build, one seed per size, fill ms as the median of 5, 2026-11-26, commit 7944108); runs on other runners differed by about 20 percent. "Carried" is this branch's carried path on one thread. Rule evaluations are the same for any thread count.

| Profile | Players | Carried: fill ms | Carried: rule evaluations | Forward, 1 thread: fill ms | Forward, 4 threads: fill ms | Forward: rule evaluations |
|---|---|---|---|---|---|---|
| light | 1 | 3.4 | 43,855 | 6.5 | 6.9 | 253,458 |
| light | 10 | 112.0 | 1,117,203 | 91.7 | 78.5 | 2,687,618 |
| light | 20 | 278.0 | 2,553,983 | 207.2 | 140.9 | 5,361,691 |
| light | 50 | 1,351.2 | 11,441,303 | 558.3 | 332.4 | 13,425,303 |
| light | 100 | 3,707.2 | 27,459,539 | 1,193.6 | 651.0 | 26,840,369 |
| heavy | 1 | 18.1 | 149,132 | 26.1 | 26.3 | 488,289 |
| heavy | 10 | 587.0 | 4,204,093 | 356.8 | 236.3 | 5,364,638 |
| heavy | 20 | 1,636.9 | 11,060,637 | 707.1 | 412.4 | 10,518,266 |
| heavy | 50 | 8,095.2 | 48,917,059 | 1,856.4 | 1,031.1 | 26,224,539 |
| heavy | 100 | 28,567.6 | 145,327,726 | 4,137.5 | 2,406.7 | 52,251,328 |

Fitted exponent of fill time over players: carried 1.52 (light) and 1.59 (heavy); forward 1.13 and 1.09 on one thread, 0.98 and 0.96 on four. At 100 players of the heavy profile the forward solve alone is 6.9 times faster than the carried path, and four threads add 1.7 times. Peak heap in the job's table counts the calling thread only; read it from the one-thread rows.

- **How close the heavy profile is to the test world.** The test world's fills above, scaled by checks (1,000 of 1,516), are 12.7, 699 and 2,373 ms at 1, 10 and 20 players. The heavy profile's carried fills are 18.1, 587 and 1,637 ms in this run (43 percent above, 16 and 31 percent below); two earlier runs gave 18.5, 582 and 1,606 ms and 21.8, 702 and 1,913 ms (72 percent above, level, 19 percent below). The rooms are near; one player is heavier than the test world per check, and the profile grows more slowly from 1 to 10 players (about 32 times against 55). The two were measured on different machines, so the comparison is rough. Rule evaluations, which do not depend on the machine: 149 thousand, 4.2 million and 11.1 million against the test world's 96 thousand, 3.78 million and 10.6 million (not scaled).
- **Where the two paths cross is not measured** between 1 and 10 players; the forward path is slower for one player and faster at 10 on both profiles. 8 is a cautious pick.
- **Confined items** are placed one at a time before the rounds and stay on the carried path. The synthetic rooms have none, so a room with many confined items per player is not covered by these numbers.
- The forward path evaluates every rule site of every world once per round. Its cost per evaluation is what is left to cut.

## Opt-in fill modes: level fill and shuffle fill

The fill described above (the exact fill, `room::Algorithm::Exact`) stays the default and the reference; nothing in it changed (the pinned placements test still holds). Two further modes place the progression items that have no placement group, each with about one forward solve instead of one per item or round. They are asked for by name (`fill_room_by`, or `"algorithm"` in a fill request: `"exact"`, `{"level": {}}`, `{"shuffle": {}}`); confined items before them and everything else after them are placed as always. Neither mode is presented as a new idea. Which existing fills they resemble is not stated here: the paragraph that says so, with sources, waits for the review and sign-off `CONTRIBUTING.md` asks for.

**Level fill** (`Algorithm::Level`, `level.rs`). The shuffled progression list is read as the order a player finds the items. One forward solve adds them one at a time and labels every location with its level: the number of order items held when it is first reached. Item `k` may sit only at a level below `k`, so the seed is beatable by construction. Placing all items is possible exactly when, for every `k`, at least `k` free locations have a level below `k`; items are placed last first, each among the levels that keep that true for the items before it. An order that fails the count is reordered without drawing (the first later item that opened a location in the last solve moves forward) and solved again, at most `config::LEVEL_FILL_MAX_REPAIRS` times; then the attempt is the exact fill's, on the same draws, so a fallback seed is byte for byte the exact fill's seed.

**Shuffle fill** (`Algorithm::Shuffle`, shuffle then unstick). Every item goes to a uniformly random permitted location. One forward solve holds progression only. While progression is out of reach, each player with an unreached item that would open a rule on their frontier right now (an exit, event or check of a reached region that does not hold yet, evaluated with one more copy; failing that an item such a rule reads; any unreached item when no frontier names one) has one such item change places with a non-progression item at a reached location, and the same solve goes on: rules are monotone and what moves out is not held, so nothing reached is lost, and nothing is retracted or restarted. A first shuffle that is beatable is the seed as drawn (test `a_shuffle_that_is_beatable_as_drawn_is_kept_as_drawn`). With no reached location left to move to, the attempt is the exact fill's, on the same draws. It runs on one thread.

Both end with a fresh solve of the finished room; a seed whose progression that solve does not reach is `FillError::Invalid`, in release builds too, and is not retried (test `a_seed_whose_progression_the_final_solve_misses_is_invalid`). That solve holds progression items only, as the sphere sweep does; `validate_room` and the exact fill's last solve hold every placed item. The two agree as long as no rule reads an item that is not progression, which nothing enforces and no fixture exercises: where one did, a mode would report less reached than `validate_room`, never more.

**Dials** (`room::Dials`; the defaults in `crates/config` change nothing): `depth` (`FILL_DEPTH_PERCENT`, 0 uniform; above 0 more draws per pick and the location opened latest wins, below 0 the earliest; refused beyond `FILL_DEPTH_MAX_PERCENT` either way, since a fill request carries it), `goal_only` (`FILL_GOAL_ONLY`: only every goal has to be reachable; the engine had no accessibility setting before, see #92), and `skip_empty` (`FILL_SKIP_EMPTY_CHECKS`, a work switch, not a feel dial).

**Checks of empty locations.** Before this, a solve evaluated every check of a reached region when the region was first visited and again whenever an item or counter its rule reads changed, until it held (the dependency index, `World::item_sites`); a check that holds is never evaluated again. So an empty location cost at least one evaluation per solve. With `skip_empty`, the opt-in modes' solves leave the check of a location that holds no progression asleep: it is evaluated when a shuffle fill needs it as a place to move an item to, in level fill's labelling solve when an item may take it (the level is the product there, so those stay awake), and otherwise in one pass at the end. Under `goal_only` the last pass is skipped only for a caller that asks for no spheres; the product always asks, since a seed records a sphere per location. Placements, reports and spheres are the same with and without (tests `skipping_empty_checks_gives_the_same_seed_with_fewer_evaluations` and, for rooms where a round moves several items, `skipping_empty_checks_places_the_same_in_rooms_of_several_moves_a_round`; the bench job compares a placements hash).

### The modes, measured

Measured by the `bench-room` job on one GitHub-hosted `ubuntu-latest` runner (release build, commit 20d879c, 2026-11-25; one room seed per size, fill ms is the median of 5). This runner was slower than the one behind the table further up (the exact fill's heavy 100 took 4,603 ms here against 3,428 to 4,138 ms on other runs), so compare rows of this table with each other only. The modes ran on one thread; "exact" is the exact fill on the forward path on one thread, with its carried path in brackets. Rule evaluations and counts do not depend on the machine. Peak heap is the counting allocator's peak for one fill on the calling thread; peak RSS is the whole process's high-water mark when the row was printed (setup and the smaller rooms included).

| Profile | Players | Mode | Fill ms | Rule evaluations | Solves | Swaps | Fell back | Peak heap MB | Peak RSS MB |
|---|---|---|---|---|---|---|---|---|---|
| light | 1 | exact | 8.9 (carried 4.4) | 253,458 (43,855) | 190 | | | 0.09 | 10.6 |
| light | 1 | level | 0.4 | 3,244 | 2 | | no | 0.10 | 10.6 |
| light | 1 | shuffle | 0.3 | 2,323 | 2 | 3 | no | 0.12 | 10.6 |
| light | 100 | exact | 1,329.6 (carried 4,329) | 26,840,369 | 190 | | | 10.7 | 325 |
| light | 100 | level | 56.3 | 332,538 | 2 | | no | 11.0 | 325 |
| light | 100 | shuffle | 49.5 | 224,633 | 2 | 1 | no | 13.1 | 325 |
| heavy | 1 | exact | 33.8 (carried 21.4) | 488,289 (149,132) | 190 | | | 0.13 | 23.7 |
| heavy | 1 | level | 1.3 | 7,155 | 2 | | no | 0.14 | 23.7 |
| heavy | 1 | shuffle | 1.5 | 8,030 | 2 | 15 | no | 0.16 | 23.7 |
| heavy | 10 | exact | 414.1 | 5,364,638 | 190 | | | 1.6 | 97.7 |
| heavy | 10 | level | 13.3 | 73,893 | 2 | | no | 1.5 | 97.6 |
| heavy | 10 | shuffle | 14.6 | 64,627 | 2 | 70 | no | 1.7 | 97.6 |
| heavy | 100 | exact | 4,602.9 (carried 30,113) | 52,251,328 | 190 | | | 15.1 | 822 |
| heavy | 100 | level | 170.4 | 743,472 | 2 | | no | 15.0 | 822 |
| heavy | 100 | shuffle | 177.9 | 709,583 | 2 | 800 | no | 17.5 | 822 |

On this runner both modes are 24 to 27 times faster than the exact fill at 100 players on one thread, and 14 to 16 times faster than the carried path for one player of the heavy profile. Per world at 100 players of the heavy profile: 1.7 ms (level) and 1.8 ms (shuffle) against 46 ms. Fill ms includes the final solve that checks the seed and the sphere sweep the seed records; of level fill's evaluations at heavy 100, 443,623 are its own solve.

Skipping the checks of empty locations (the `noskip` rows of the job; the job fails if a `noskip` row's placements hash differs from its skipping row's, and none did): at heavy 100, level fill 1,017,391 to 743,472 evaluations (27 percent fewer, 187.0 to 170.4 ms) and shuffle fill 1,089,781 to 709,583 (35 percent fewer, 200.9 to 177.9 ms); at light 100, 343,670 to 332,538 (3 percent) and 328,495 to 224,633 (32 percent, 51.0 to 49.5 ms).

**Seed shape**, counted by the same job (`fill-fairness --modes`). The reference is the exact fill on fill seeds 1 to N of one stream; every row is filled on seeds 1 to N of another stream, and the `exact` row is therefore the noise floor: what two independent samples of the same fill look like at this sample size. A seed that fell back to the exact fill is counted in its own column and left out of the row's shape. "Distance" is the total variation distance between the row's and the reference's distribution of progression over spheres (0 the same, 1 disjoint). "Location chi2/dof" compares how often each location holds progression with the reference. Required spheres are counted on the first 5 seeds only and show a direction, not a value.

| Room | Mode | Placed by the mode | Fell back (seeds) | Swaps or repairs per seed | Spheres, mean | Progression in first two spheres (locations there) | Required spheres (5 seeds) | Stuck share, mean | Distance | Location chi2/dof |
|---|---|---|---|---|---|---|---|---|---|---|
| light, 1 player, 200 seeds | exact (noise floor) | 200 | | | 11.6 | 0.026 (0.021) | 3.8 | | 0.063 | 0.78 |
| | level | 200 | 0 | 0 | 6.6 | 0.120 (0.045) | 0 | | 0.836 | 3.58 |
| | level, depth 100 | 200 | 0 | 0 | 9.4 | 0.041 (0.023) | 1.0 | | 0.406 | 1.20 |
| | level, depth 300 | 200 | 0 | 0 | 14.1 | 0.025 (0.018) | 5.4 | | 0.403 | 1.38 |
| | level, depth -100 | 200 | 0 | 0 | 5.5 | 0.173 (0.054) | 0 | | 0.877 | 8.62 |
| | shuffle | 200 (11 percent untouched) | 0 | 2.8 swaps | 11.4 | 0.032 (0.023) | 6.6 | | 0.081 | 0.80 |
| | shuffle, depth 300 | 200 | 0 | 2.7 swaps | 12.1 | 0.026 (0.019) | 6.0 | | 0.042 | 0.80 |
| | shuffle, depth -100 | 200 | 0 | 2.9 swaps | 11.1 | 0.034 (0.024) | 4.6 | | 0.125 | 0.81 |
| heavy, 1 player, 200 seeds | exact (noise floor) | 200 | | | 20.1 | 0.020 (0.006) | 12.4 | | 0.085 | 0.77 |
| | level | 193 | 7 | 0.56 repairs | 10.8 | 0.034 (0.007) | 4.2 | | 0.830 | 3.44 |
| | level, depth 300 | 193 | 7 | 0.56 repairs | 19.1 | 0.028 (0.006) | 9.6 | | 0.194 | 1.29 |
| | level, depth -100 | 193 | 7 | 0.56 repairs | 9.8 | 0.034 (0.007) | 3.0 | | 0.843 | 7.04 |
| | shuffle | 191 (none untouched) | 9 | 11.4 swaps | 16.7 | 0.029 (0.007) | 9.0 | | 0.488 | 0.86 |
| | shuffle, depth 300 | 187 | 13 | 11.7 swaps | 18.6 | 0.021 (0.006) | 8.6 | | 0.280 | 0.85 |
| | shuffle, depth -100 | 189 | 11 | 11.8 swaps | 15.7 | 0.033 (0.008) | 7.8 | | 0.603 | 0.90 |
| light, 10 players, 20 seeds | exact (noise floor) | 20 | | | 20.5 | 0.013 (0.011) | not counted | 0.311 | 0.267 | 0.85 |
| | level | 20 | 0 | 0 | 7.2 | 0.086 (0.029) | | 0.008 | 0.962 | 1.06 |
| | level, depth 300 | 20 | 0 | 0 | 19.4 | 0.009 (0.009) | | 0.227 | 0.356 | 0.87 |
| | shuffle | 20 (15 percent untouched) | 0 | 8.2 swaps | 17.6 | 0.014 (0.013) | | 0.218 | 0.332 | 0.85 |
| heavy, 10 players, 20 seeds | exact (noise floor) | 20 | | | 35.1 | 0.013 (0.003) | not counted | 0.396 | 0.274 | 0.84 |
| | level | 20 | 0 | 0 | 14.1 | 0.019 (0.004) | | 0.165 | 0.922 | 1.04 |
| | level, depth 300 | 20 | 0 | 0 | 24.1 | 0.014 (0.003) | | 0.196 | 0.819 | 0.86 |
| | shuffle | 20 (none untouched) | 0 | 79.8 swaps | 25.4 | 0.019 (0.005) | | 0.219 | 0.706 | 0.84 |

The two small fixture worlds, 10,000 seeds a row, where the comparison has power, also against 10,000 uniform beatable placements (rejection sampling; "max gap" is the largest difference between how often the row and the baseline put an item at a location):

| World | Mode | Fell back (seeds) | Spheres, mean | Location chi2/dof against exact | Distance | Chi-square against uniform | Max gap from uniform | Coverage gaps |
|---|---|---|---|---|---|---|---|---|
| single player (dof 15 against uniform) | exact (noise floor) | | 3.5 | 0.64 | 0.005 | 278 | 0.074 | 0 |
| | level | 0 | 3.1 | 460 | 0.161 | 7,620 | 0.220 | 0 |
| | level, depth 100 | 0 | 3.2 | 261 | 0.114 | 4,550 | 0.165 | 0 |
| | level, depth 300 | 0 | 3.3 | 126 | 0.058 | 3,373 | 0.222 | 0 |
| | shuffle | 0 | 3.3 | 63 | 0.064 | 909 | 0.080 | 0 |
| | shuffle, depth 300 | 0 | 3.3 | 45 | 0.054 | 615 | 0.061 | 0 |
| room of three (dof 176 against uniform) | exact, rounds (noise floor) | | 4.1 | 0.59 | 0.004 | 9,218 | 0.136 | 6 |
| | level | 0 | 3.6 | 312 | 0.136 | 27,384 | 0.128 | 0 |
| | level, depth 100 | 0 | 4.0 | 153 | 0.053 | 10,501 | 0.080 | 0 |
| | level, depth 300 | 0 | 4.6 | 175 | 0.071 | 3,081 | 0.057 | 0 |
| | shuffle | 38 | 4.3 | 154 | 0.034 | 4,120 | 0.055 | 0 |
| | shuffle, depth 300 | 38 | 4.3 | 160 | 0.032 | 3,683 | 0.055 | 0 |

What the numbers say, and what they do not:
- **Neither mode spreads items like the exact fill.** Where the test has power (the fixtures, 10,000 seeds) both differ from it far beyond the noise floor: location chi2/dof 63 and 154 for shuffle fill, 460 and 312 for level fill, against 0.6 for the exact fill itself. On the synthetic rooms 200 and 20 seeds over about a thousand locations cannot tell: every row's location chi2/dof there is at the noise floor (0.8 to 0.9) except level fill at depth 0 and below (3.4 to 8.6), so those columns show that level fill's location bias is large, and nothing about shuffle fill's.
- **Sphere counts.** Shuffle fill's are near the exact fill's on the light profile (11.4 against 11.6) and shorter on the heavy one (16.7 against 20.1, 25.4 against 35.1 with 10 players; distance 0.49 and 0.71 against floors of 0.09 and 0.27). Level fill at depth 0 has about half the spheres (6.6 against 11.6, 10.8 against 20.1); depth 300 brings the count near (14.1, 19.1) with a distribution that still differs.
- **Against uniform** (fixtures only): on the single player world the exact fill is the closest by far (chi-square 278; shuffle 909, level 7,620). On the room of three the exact fill's rounds are not the closest: level fill at depth 300 (3,081) and shuffle fill (4,120) are closer than it (9,218), and level fill at depth 0 is the furthest (27,384). The max gap column agrees on the room and cannot separate the exact fill from shuffle fill on the single world (0.074 against 0.080).
- **Fallbacks.** On one player of the heavy profile 9 of 200 shuffle seeds and 7 of 200 level seeds fell back to the exact fill (so those cost the exact fill's time: worst 1 percent 25 and 29 ms against medians of 1.6 ms); none on the light profile or with 10 players; 38 of 10,000 shuffle rooms on the room fixture. Before shuffle fill looked one step ahead for an item that opens a rule now (commit cba2df5), 85 of 200 heavy seeds fell back (counted on the run at c744aa6, same stream as the reference).
- **Dials.** `depth` moves level fill a long way (5.5 to 14.1 spheres on light). It moves shuffle fill a little and only through the items it swaps (11.1 to 12.1 spheres on light, 15.7 to 18.6 on heavy; on the room fixture not at all). `goal_only` changed nothing measurable on these worlds: the goal of a synthetic world is its last layer, which needs nearly everything.

`fill-fairness --modes "exact level level,depth=100 shuffle" (--world w.json | --synth light|heavy --players N)` prints, per mode on the same room (the exact fill reference on one stream of seeds, every row on another, seeds that fell back counted apart): spheres, progression per sphere against locations per sphere, how often each location holds progression (normalized entropy, and a chi-square against the exact fill), the share of rounds a player waits with nothing new, spheres that hold a required item (`--required N` seeds: one solve per progression item, so keep N small), repairs, swaps, fallbacks, and fill time (median and worst 1 percent); with `--baseline N` on a small world also the comparison with uniform beatable placements.

## Fairness

### Tier 1: port sanity (part of `port-check`)

`--sanity-seeds N` (default 200) fills one instance N times with our engine only and reports: per key item, the distinct locations it landed in and the entropy of its location distribution next to how many locations the placement rules allow and how many distinct locations a uniform spread would reach; the progression share of the top regions; a key-item depth histogram; a sphere-count histogram; and seed independence (below). An item that lands in fewer than a quarter of the distinct locations a uniform spread would reach, while the rules allow at least 10, is flagged. Logic often forces an item early, so a flag can be legitimate; it tells you which items to look at.

### Tier 2: engine fairness (`fill-fairness`)

Runs on a small world where uniform random placement is often beatable, so an unbiased baseline exists: draw uniform random placements, keep the ones that respect the placement rules, are beatable and leave no progression unreachable. Then it compares our fill's item x location frequencies against that baseline:

- two-sample chi-square over every item's location histogram, with an upper-tail p-value (Wilson-Hilferty approximation, about two digits);
- the largest frequency gap and the item/location pair it is on;
- a coverage map: pairs the baseline reaches often (expected count at least 10) that our fill produces less than a tenth as often;
- seed independence: the share of locations holding the same item in adjacent fill seeds against seeds half the run apart, as a z-score (above 3 is flagged);
- per item spread (distinct locations, entropy) next to the baseline's.

Assumed fill is not uniform over beatable seeds by design (it places items reachable without themselves, which favors earlier locations), so a small p-value on its own is expected. The numbers to watch over time are the largest gap and the coverage map.

```sh
target/release/fill-fairness --world crates/data-fill/tests/fixtures/fairness_world.json \
  --seeds 2000 --out fairness.json
```

Also accepts `--manifest m.json --instance i.json` for a real world (an instance file as `port-check` writes under `legacy/`); on a large world the uniform baseline is almost never beatable, so only spread and independence are reported (`baseline.note` says so).

`--fail-deviation` and `--fail-p` turn the report into a gate (for example in a scheduled job that keeps `fairness.json` as an artifact), exit code 1 when exceeded:

```sh
target/release/fill-fairness --world crates/data-fill/tests/fixtures/fairness_world.json \
  --seeds 10000 --baseline 10000 --fail-deviation 0.10 --out fairness.json
```

### Fairness of a room

`fill-fairness --world crates/data-fill/tests/fixtures/room_world.json` (a world file with an `instances` list is a room: three players of the small fixture world, one a harder hall door, one a goal that stops at the hall and one location fewer) measures the room fill against uniform random rooms that are beatable for every player, the same two tests as for one player:

| World | Fill | Baseline | Max gap (item, location) | Coverage gaps | Chi-square (dof), p | Seed independence |
|---|---|---|---|---|---|---|
| single player fixture (10 000 fills, 10 000 baseline of 40 091 candidates) | one item at a time | uniform, beatable | 0.074 (coin, l6) | 0 | 277.6 (15), 1e-42 | z = -0.13, not flagged |
| room, three players (10 000, 10 000 of 54 388) | rounds | uniform, beatable for all | 0.136 (player 1's coin, player 2's vault) | 6, all player 2's vault | 9 218 (176), 0 | z = 1.18, not flagged |
| room, three players (4 000, 4 000 of 21 897), one item at a time | one item at a time | same | 0.044 (player 1's coin, player 2's vault) | 0 | 519 (176), 9e-35 | z = -1.53, not flagged |

As for one player, a small p-value on its own is expected: assumed fill is not uniform over beatable rooms. The room numbers to watch are the largest gap and the coverage map. The six gaps are one location: player 2's vault needs all three of player 2's progression items (hall door with the lamp, then the vault door), and in a round every player's last item is taken out of the state, so while each of the three players still has an item to place that location is unreachable for every item. The baseline puts another player's item there in 4 to 8 percent of rooms; the fill never does. That is the bias towards earlier locations the rounds are meant to have, magnified by a fixture whose players have only three progression items each; on the test world (hundreds of items per player) the sphere counts and shares above match the legacy rooms.


## Limits

- The checks compare behaviour on the seeds and inventories drawn. They do not prove equivalence for every state; more seeds and inventories make a miss less likely.
- Spheres compare which locations open in which round, not the order inside a round.
- `progression` in `pool_class` is what both sides count as held. An item the legacy logic reads but classifies as non-progression is never held on either side, matching the legacy generator.
- A room is checked for the players the legacy side reports. Reachability is per player, with the other players' items not taken into account (the legacy solver is per world); the cross-world effects are checked through the room's spheres.
