# Ember Spire (genre example: run-based roguelike)

A run-based roguelike invented as a genre example. Every name in it is made up; no game's names, data or logic. 1 region, 86 checks (44 at the default options, 21 at the smallest), 9 items, 3 options. Tests: `tests/trials/mod.rs`.

One file, `world.rando`: a roguelike's world is a list of goals with no map, so one region and a few families say all of it.

Conventions it shows:

- **Locations are counted progress**, not places: `clear_floor_{i}`, `{hero}_act_{a}_boss`, `win_with_{hero}` and `{hero}_ascension_{k}`.
- **Options set how many locations exist.** `floors` and `ascensions` are a family's `where` (`for i in 1..50 where i <= opt.floors`), so the checks past them are left out of the seed.
- **Item copies that follow an option.** `ember_relic` has `opt.floors div 5 + opt.ascensions` copies, so a taller tower has the relics its rules ask for.
- **Characters as items.** Heroes come from a table and a family; the first is a start item.
- **A pool that is mostly filler.** 5 to 19 progression items (by the options) and the rest gold, potions, card packs and a curse.
- **A goal over several characters**: each hero's win fires `run_won`, and the goal is `counter(run_won) >= opt.wins_needed`.

Invented: the world, the heroes, the relics, and every number.

What the language cannot express yet (listed on server issue 226):

- **A warning for a check no inventory can reach.** A draft of this world gave ascensions more relics to ask for than there were copies. `check` and `generate --check-seeds` both passed: the fill only needs the goal, so those checks got filler. Nothing flags a check that can never be reached.
