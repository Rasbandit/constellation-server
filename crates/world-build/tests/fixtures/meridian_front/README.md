# Meridian Front (genre example: real-time strategy campaign)

A sci-fi real-time strategy campaign invented as a genre example. Every name in it is made up; no game's names, data or logic. 18 regions, 51 checks (51 at the default options, 18 at the smallest campaign), 14 items, 3 options. Tests: `tests/trials/mod.rs`.

Files: `world.rando` (the world, its options and the goal), `army.rando` (units, upgrades and the strength formula), `campaign.rando` (planets, the mission table and its regions), `finales.rando` (the chapter finales, a table of their own), `war_room.rando` (the hub every mission is launched from).

Conventions it shows:

- **A campaign is a hub and a tree.** The war room is the start region; every mission is a region with an exit from it. A mission's exit needs its planet's coordinates and the mission before it in the tree (`m.after`, a `rule` column that reads other missions' `_won` events).
- **Objectives are the checks.** Each mission has a victory, a bonus and a mastery objective, written once in a family over the mission table. Finales have a victory, a feat named in their row, and a no-losses objective.
- **Three kinds of items.** Coordinates open a planet's missions, unit items unlock a unit type, upgrade copies add research tiers (2 targeting tiers, 4 plating tiers).
- **Army strength as a weighted sum.** `def army(n)` adds the units with weights 1 to 6 plus one per upgrade tier, and a mission's victory asks for `army(m.power)`. The first two missions ask for 0, which folds to `true`; the rest ask for 3 to 18.
- **Campaign subsets.** `chapters` cuts the campaign short and `side_ops` drops the optional missions. Both reach the missions through one def, `in_campaign(chapter, side)`, that reads options only, used as the mission and finale families' `where`. The war room's exits into a mission left out go with it, so they need no filter of their own.
- **Campaign order.** `campaign_order` is `tree` or `open` (any mission of a reached planet), through `opt.campaign_order == open or ...` on each exit.
- **A goal counted over chapters**: each finale fires `chapter_won`, and the goal is `counter(chapter_won) >= opt.chapters`.

Invented: the world, the four planets (`halvar`, `velkas`, `brannoc`, `ilvane`), every mission, unit, upgrade and objective, and every number.

What the language cannot express yet (listed on server issue 226):

- **A shuffled mission order that an option turns on.** Dealing missions to the tree's slots each seed is an exit shuffle, but a shuffle cannot have a `when`, so it could not be an option. `campaign_order` offers the tree or an open order instead.
- **A campaign subset drawn per seed** (`pick n from mission`). Subsets here are fixed by options.
- **A weighted sum as a named number** (`score`). `army(n)` works as a def with arithmetic, but a def is always a rule, so the sum cannot be named once and compared in different ways, and `fmt` writes the sum on one long line: it breaks rules at `and` and `or` only.
