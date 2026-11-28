# Tidewell Isle (the minimal teaching world)

The smallest world that still uses most of the language, and the first one to read. Invented for the build tool's tests: every name in it is made up; no game's names, data or logic. 8 regions, 17 checks (15 with `market_stalls` off), 7 items, 4 options, two shuffles. Tests: `tests/lang/mod.rs` (its files build to the same manifest as the model in `tests/support/mod.rs`) and `tests/trials/mod.rs`.

Files: the entry `world.rando` (the world line, options, defs, groups, shuffles and the goal), `items.rando`, and one module per area in `regions/`.

Conventions it shows:

- **Modules**: an entry file that names its modules with `use`, and modules that `use items` for the names they read.
- **Three option kinds** (`toggle`, `range`, `choice`), read in a rule (`can_climb`), in a `when` on a check (`market_stalls`), on a start item (`start_gift`) and in the goal (`pearls_needed`).
- **Item classes and copies**: progression, useful, filler and trap items, `pearl x 3`, and a `pool fill` that tops up with filler.
- **A named rule** (`def can_climb`) and **an item group** read with `group_count(tools) >= 2`.
- **An event** (`beacon_lit`) that a check and the goal both read.
- **Placement modifiers**: a fixed item (`holds chart`) and a check that never gets progression (`no_progression`).
- **A small family**: `for s in [fruit, fish]` writes two market stalls.
- **Two shuffles**: coupled door slots (`group outside` and `group inside`, `check layout`), and two wardens drawn per seed and read as `at.wardens.tower_door`.

`rando.lock` is what `rando build` writes: ids in declaration order, items and checks apart. The test holds it fixed.
