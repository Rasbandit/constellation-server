# Cinderfall Wilds (trial world 1 of doc 18)

An adventure invented for the grammar trials (doc 18, section 8, question 9). No game's names, data or logic. 58 regions, 173 checks (164 at the default options), 24 items, 4 options. Tests: `tests/trials/mod.rs`.

What it exercises:

- **Modules**: an entry file, `items` and `logic` shared by every area, six overworld files and five dungeon files, each with its own `use` lines.
- **Progressive items**: `blade x 3`, `gauntlet x 2`, `quiver x 2`, read as `has(blade, 2)` and through `can_lift(n)`.
- **Dungeon keys confined to their dungeon**: four `placement` sets, each dungeon region marked `placement <set>`, small keys counted with `keys(k, n)`.
- **Named conditions with parameters**: `can_lift(n)`, `keys(k, n)`, `can_fight(tier)`.
- **Options that reshape rules**: `difficulty` through `match` (in `can_fight` and `can_see_dark`), `relics_needed` in a rule, `start_lantern` as a conditional start item, and `sunken_vault`, an optional dungeon whose checks carry `when opt.sunken_vault`.
- **An entrance shuffle of interiors**: eight house doors (`group outside` and `group inside`), shuffled coupled with `check layout`.
- **Fixed placements** (`holds relic`), `no_progression`, small families (`for i in 1..4`), groups and `has_any`.

The optional dungeon's rooms are always in the map and only its checks have a `when`: a region `when` there would leave rooms no seed can reach at the default, which a world with a shuffle refuses (engine issue 211). `ledger.json` is what `rando build` writes; the test holds it fixed.
