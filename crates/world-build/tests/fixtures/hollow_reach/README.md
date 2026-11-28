# Hollow Reach (genre example: interconnected-map exploration platformer)

An exploration platformer on one large map, invented as a genre example. Every name in it is made up; no game's names, data or logic. 19 regions, 44 checks, 12 items, 3 options. Tests: `tests/trials/mod.rs`.

Files: `world.rando` (the world, options and goal), `items.rando`, `logic.rando` (the movement defs every zone reads), and one file per zone in `zones/`: the surface, the caverns, the archive, the spire and the core.

Conventions it shows:

- **Movement abilities are the progression.** Each opens a kind of obstacle anywhere on the map, through a def per obstacle (`can_reach_high`, `can_climb_shaft`, `can_cross_gap`, `can_swim`, `can_see_dark`). Health and energy upgrades are `useful`: they never open a path.
- **One map, joined both ways, with backtracking.** Zones lead into each other in several places (the gate's arch to the spire, an underwater tunnel from the caverns to the archive), and many exits back need a different move than the way in (`crystal_pit` up to `dripstone_hall` needs a climb).
- **A one-way drop.** `windy_bluff` falls into `crystal_pit` with no exit back.
- **One location reached two ways.** `gallery_ledge` is declared in `archive_stacks` and again in `spire_lift` with `also`, each with its own rule.
- **Trick difficulty that loosens logic.** `tricks` (`none`, `basic`, `advanced`) adds ways through inside the defs: a `match` in `can_climb_shaft`, and `can_wall_dash`, a def that only `advanced` makes true. The zones never read the option.
- **Guardians counted at the end.** Each guardian's lair fires `guardian_down`, and the core opens at `counter(guardian_down) >= opt.guardians_needed`.

Invented: the world, its zones, rooms, abilities (`vault_boots`, `rush_core`, `grip_claws`, `hook_line`, `tide_lung`, `shatter_charge`, `lumen_moth`) and guardians.

What the language cannot express yet (listed on server issue 226):

- **Tricks as declarations** (`trick`). Each trick here is a choice value plus the defs that read it; a declared trick would carry its own name, description and level for the options screen.
- **A named number.** `def guardians_down = counter(...)` is refused because a def is a rule, so the guardian count is one event fired in three regions instead.
