# Gloam Delve (genre example: run-based roguelike)

A run-based roguelike invented as a genre example. Every name in it is made up; no game's names, data or logic. 1 region, 80 checks (40 at the default options, 15 at the smallest), 10 items, 3 options. Tests: `tests/trials/mod.rs`.

One file, `world.rando`: a roguelike's world is a list of goals with no map, so one region and a few families say all of it.

Conventions it shows:

- **Locations are counted progress**, not places: `reach_depth_{i}`, `{delver}_beats_warden`, `win_with_{delver}` and `{delver}_hardship_{k}`.
- **Options set how many locations exist.** `depths` and `hardships` are a family's `where` (`for i in 1..50 where i <= opt.depths`), so the checks past them are left out of the seed.
- **Item copies that follow an option.** `ember_shard` has `opt.depths div 5 + opt.hardships` copies, so a deeper cave has the shards its rules ask for.
- **Characters as items.** Delvers come from a table and a family; the first is a start item, so it is not in the pool.
- **A def with a parameter for a repeated rule**: `can_win(d)` is a delver's win, read by the win, the hardship wins and the `run_won` event.
- **A pool that is mostly filler.** 5 to 18 progression items in the pool (the 4 delvers to find, and 1 to 14 shards by the options), and the rest coins, tonics, trinkets and a jinx.
- **A goal over several characters**: each delver's win fires `run_won`, and the goal is `counter(run_won) >= opt.wins_needed`.

Invented: the world, the delvers, the shards, and every number.

What the language cannot express yet (listed on server issue 226):

- **A warning for a check no inventory can reach.** A draft of this world asked for more shards than there were copies. `check` and `generate --check-seeds` both passed: the fill only needs the goal, so those checks got filler. Nothing flags a check that can never be reached.
