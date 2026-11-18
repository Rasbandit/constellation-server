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

## Limits

- The checks compare behaviour on the seeds and inventories drawn. They do not prove equivalence for every state; more seeds and inventories make a miss less likely.
- Spheres compare which locations open in which round, not the order inside a round.
- `progression` in `pool_class` is what both sides count as held. An item the legacy logic reads but classifies as non-progression is never held on either side, matching the legacy generator.
- Multiworld: not yet supported. Both tools check one player's world at a time.
