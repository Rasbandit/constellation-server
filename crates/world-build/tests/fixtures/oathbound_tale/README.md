# Oathbound Tale (genre example: turn-based party RPG)

A turn-based party RPG invented as a genre example. Every name in it is made up; no game's names, data or logic. 18 regions, 54 checks (46 at the default options, 38 at one shelf a shop), 16 items, 3 options. Tests: `tests/trials/mod.rs`.

Files: `world.rando`, `items.rando`, `logic.rando` (the party's abilities), and `regions/` split as a world map is: `towns.rando`, `wilds.rando`, `dungeons.rando`.

Conventions it shows:

- **Companions are progression.** They come from a table and a family, each opens something only they can do (Tamsin reads old script, Rourke picks locks, Ilse endures cold and poison, Bram clears rubble), and `party(n)` counts them through a group.
- **Key items open the map**: a ferry pass, a skiff, a lamp, a rune and a war writ.
- **Level gates through an item.** `veteran_rank x 6` stands for the levels a party would gain by fighting, and `rank(n)` gates the far roads, the dungeons and the bosses.
- **Shop shelves are locations.** A town table and a family write one shop region per town, each with `shop_slots` shelves (a family's `where`); `safe_shops` keeps progression off them with `no_progression opt.safe_shops`. Each town has one exit into its shop.
- **A boss that holds a story item**: `goblin_chief` always holds the ferry pass (`holds`).
- **A final gate on party size**: the keep wants `party(opt.party_needed)` and the war writ, and the last fight wants rank 6 and Ilse in particular (`can_endure`), so even `party_needed = 1` asks for her.

Invented: the world, its towns, companions, key items, bosses and every number.

What the language cannot express yet (listed on server issue 226):

- **Prices.** Buying from a shelf spends gold, and spent resources are out of the language's first version (doc 18, section 6). Shelves here are free checks.
- **Levels as a number.** Experience is not a thing a rule can read; `veteran_rank` stands in for it.
