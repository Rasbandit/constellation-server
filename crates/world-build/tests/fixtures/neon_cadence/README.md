# Neon Cadence (trial world 2 of doc 18)

A rhythm game invented for the grammar trials (doc 18, section 8, question 9). No game's names, data or logic. 64 songs in a catalog, 66 regions, 383 checks (257 at the default options), 16 items. Tests: `tests/trials/mod.rs`.

What it exercises:

- **A generated base**: `catalog.rando` is written by a generator (the `catalog` function in the tests) from `data/songs.csv`, with a `generated` header: the source's hash, the content hash and `expect song >= 60`. The test checks the committed file is exactly what the generator writes.
- **A hand overlay**: `fixes.rando` replaces a check's rule and an exit's rule, removes a check, removes a song (its region and the exit to it) and extends a generated region.
- **A catalog as a table and location families**: each song by each chart by each objective, `for d in chart where d.focus <= 1 or opt.expert_charts, g in objective`, with a `rule` column (`objective.need`) and name pieces from fields (`has({s.pack}_pack)`).
- **Unlock items per tier**: `tier_pass x 3`, `count(tier_pass) >= s.tier - 1` (tier 1 folds to `true`), and one pack item per pack from a family over a hand-written table.
- **A goal of "beat N songs"**: every song region fires `song_cleared`, and the goal reads `counter(song_cleared) >= opt.songs_needed`.
- **Weighted sums**: stars worth 1, 2 and 5, `count(bronze_star) + 2 * count(silver_star) + 5 * count(gold_star) >= n`, through a def with a parameter.
- **The id ledger across a regeneration** (test only): rows reordered and one inserted in the middle keep every id, the new song gets new ids, a song removed from the source makes the overlay that names it fail with `overlay.target_missing`, and a hand edit of the generated file is refused.

`ledger.json` is what `rando build` writes; the test holds it fixed.
