# Bramble Isles (genre example: collect-a-thon 3D platformer)

A collect-a-thon 3D platformer invented as a genre example. Every name in it is made up; no game's names, data or logic. 12 regions, 75 checks (65 at the default options, 45 at the fewest seed pods), 11 items, 2 options. Tests: `tests/trials/mod.rs`.

Files: `world.rando`, `items.rando`, `hub.rando` (the hub island and its world doors), and one file per world in `worlds/`.

Conventions it shows:

- **A hub with world doors opened by a count.** A table of doors and their sun gem cost, and a family of exits: `exit {d.key}: count(sun_gem) >= d.gems`. The summit's cost is an option.
- **A main collectible as many copies of one item**: `sun_gem x 30`, one per gem check.
- **Moves gate places in every world**: a ground pound, a glide, fins, a bounce and a roll, and `can_reach_high` over a group of the two moves that get up a ledge.
- **Many minor collectibles.** Each world has eight seed pods, four near its entrance and four further in; `pods_per_world` sets how many of them are checks (a family's `where`). The items they hold are mostly filler.
- **The same shape per world.** Each world file is two regions: an entrance and an inner area behind a move.

Invented: the world, the five worlds, every gem, move and collectible, and every cost.

What the language cannot express yet: nothing this world needed. A collect-a-thon whose doors count collectibles picked up at their places (rather than items placed anywhere) would need a rule to read checks done, which rules never do by design.
