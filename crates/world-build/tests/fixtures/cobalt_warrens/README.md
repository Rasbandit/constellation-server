# Cobalt Warrens (genre example: interconnected-map exploration platformer)

An exploration platformer on one large map, invented as a genre example. Every name in it is made up; no game's names, data or logic. 18 regions, 44 checks, 13 items, 3 options. Tests: `tests/trials/mod.rs`.

Files: `world.rando` (the world, options and goal), `items.rando`, `logic.rando` (the movement defs every zone reads), and one file per zone in `zones/`: the surface, the mines, the foundry, the aqueduct and the reactor.

Conventions it shows:

- **Movement gear is the progression.** Each piece opens a kind of obstacle anywhere on the map, through a def per obstacle (`can_reach_high`, `can_climb_shaft`, `can_cross_gap`, `can_swim`, `can_see_dark`). Health and battery upgrades are `useful`: they never open a path.
- **One map, joined both ways, with backtracking.** Zones lead into each other in several places (a cable from the airlock to the aqueduct, an underwater drift from the mines to the foundry), and many exits back need different gear than the way in (`collapsed_mine` up to `ore_hall` needs a climb).
- **A one-way drop.** `ridge` falls into `collapsed_mine` with no exit back, and every exit out of the mine is gated. This is sound for the fill only because the game is assumed to have a warp back to the landing pad: the logic checks what is reachable from the start, not whether a player can walk out of where they are. A world whose game has no such warp must not write a one-way exit into a place it cannot leave.
- **One location reached two ways.** `catwalk_ledge` is declared in `conveyor_loft` and again in `intake_lift` with `also`, each with its own rule.
- **Trick difficulty that loosens logic.** `tricks` (`none`, `basic`, `advanced`) adds ways through inside the defs: a `match` in `can_climb_shaft`, and `can_wall_dash` and `can_long_jump`, defs that only `advanced` makes true. The zones never read the option.
- **A final door with a key and a count.** Each boss fires `boss_down`, and the reactor door needs the keycard and `counter(boss_down) >= opt.bosses_needed`.

Invented: the world, its zones, rooms, gear and bosses.

What the language cannot express yet (listed on server issue 226):

- **Tricks as declarations** (`trick`). Each trick here is a choice value plus the defs that read it; a declared trick would carry its own name, description and level for the options screen.
