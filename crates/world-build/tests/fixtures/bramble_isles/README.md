# Bramble Isles (genre example: collect-a-thon 3D platformer)

A collect-a-thon 3D platformer invented as a genre example. Every name in it is made up; no game's names, data or logic. 12 regions, 75 checks (65 at the default options, 45 at the fewest seed pods), 11 items, 2 options. Tests: `tests/trials/mod.rs`.

Files: `world.rando`, `items.rando`, `hub.rando` (the hub island and its world doors), and `worlds.rando` (every world, as tables and one family).

Conventions it shows:

- **Worlds that share a shape, written as data.** Every world is an entrance and an inner area behind a move. `isle` is a table of worlds (the marbles their door asks for, the move to their inner area) and `marble` a table of every lucky marble (its world, its area, what it needs). One family writes all ten regions; a `where` with a name comparison (`g.home == w.key`) puts each marble in its world.
- **A hub with world doors opened by a count**: a family over `isle`, `exit {w.key}: count(lucky_marble) >= w.marbles`. The summit's cost is an option.
- **A main collectible as many copies of one item**: `lucky_marble x 30`, one per marble check.
- **Moves gate places in every world**: a ground pound, a glide, fins, a bounce and a roll, and `can_reach_high` over a group of the two moves that get up a ledge.
- **Many minor collectibles.** Each world has eight seed pods, four by its entrance and four further in; `pods_per_world` sets how many of them are checks (a family's `where`). Filler fills what the marbles and moves leave.

Invented: the world, the five worlds, every marble, move and collectible, and every cost.

What the language cannot express yet: nothing this world needed. A collect-a-thon whose doors count collectibles picked up at their places (rather than items placed anywhere) would need a rule to read checks done, which rules never do by design.
