# Crownless Saga (genre example: turn-based party RPG)

A turn-based party RPG invented as a genre example. Every name in it is made up; no game's names, data or logic. 14 regions, 54 checks (46 at the default options, 38 at one shelf a shop), 16 items, 3 options. Tests: `tests/trials/mod.rs`.

Files: `world.rando`, `items.rando`, `logic.rando` (the party's abilities), and `regions/` split as a world map is: `towns.rando`, `wilds.rando`, `dungeons.rando`.

Conventions it shows:

- **Companions are progression.** They come from a table and a family, each opens what only they can do (`can_pick_locks` and the others), and `party(n)` counts them through a group.
- **Key items open the map**: a ferry pass, a skiff, a lamp, a rune and a seal.
- **Level gates through an item.** `veteran_rank x 6` stands for the levels a party would gain by fighting, and `rank(n)` gates each road, dungeon and boss.
- **Shop shelves are locations.** `shop_slots` sets how many shelves each town's shop has (a family's `where`), and `safe_shops` keeps progression off them with `no_progression opt.safe_shops`.
- **A boss that holds a story item**: `goblin_chief` always holds the ferry pass (`holds`).
- **A final gate on party size**: the keep wants `party(opt.party_needed)` and the seal.

Invented: the world, its towns, companions, key items, bosses and every number.

What the language cannot express yet (listed on server issue 226):

- **Prices.** Buying from a shelf spends gold, and spent resources are out of the language's first version (doc 18, section 6). Shelves here are free checks.
- **Levels as a number.** Experience is not a thing a rule can read; `veteran_rank` stands in for it.
- **One family over several hand-written regions.** The shop shelves are the same three lines in each town; a family cannot add members to regions declared elsewhere, so each town repeats them.
