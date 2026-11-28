# The world build tool (`crates/world-build`)

Turns a world written in the `.rando` language into the manifest JSON the fill engine loads (`crates/data-fill`), and into the per-seed instance the engine fills from. The design is doc 18 (the authoring language), doc 16 (the world file standard) and doc 17 (generic shuffles) in the `constellation` docs repo. Issue #27. `.rando` and `rando` are working names, cleared by a lawyer before anything external.

## Write a world

A world is a folder with a `rando.toml`, an entry file (`world.rando` unless `rando.toml` names another), any modules it names with `use`, and the `rando.lock` the tool writes.

`rando.toml` (design doc 19 section 2) is the one fixed file name. It marks the folder (with no folder given, `rando` walks up from the current one to the nearest `rando.toml`, as Cargo does) and holds what is not game logic, in `[world]`: `name` (must equal the `world` line's game key), `version` (Semantic Versioning, build metadata refused), `entry` (default `world.rando`), `authors`, `license` (an SPDX expression), `description`, `icon`, `homepage`, `repository`, and `[assets] files`. Unknown keys are errors that name the key. `authors`, `license` and `description` are required by `rando pack`; `check` and `build` only warn when one is missing. A folder with only `world.rando` still builds, with a warning that shows a minimal `rando.toml`; that becomes an error with the first portal release. An asset listed twice is refused. Limits are `config::Limits` fields (`world_*`: file size, name and version length, author count and length, description, license and URL length, asset count), read from `Limits::DESIGN`. Not checked yet (each a `shortcut:` in `src/meta.rs`): license identifiers against the SPDX list, icon pixel sizes, and the warning that an email in `authors` is public. The example world, "Tidewell Isle" (invented for the tests: 8 regions, 17 checks, 7 items, 4 options, two shuffles), is in `crates/world-build/tests/fixtures/tidewell/`. Its entry file starts:

```rando
world tidewell_isle "Tidewell Isle" version 1
use items
use regions/coast
use regions/lighthouse
start dock

option tricky_climbs: toggle = off
option start_gift: choice(none, lamp) = none

start_item lamp when opt.start_gift == lamp
def can_climb = has(crowbar) or opt.tricky_climbs
```

and a region, from `regions/coast.rando`:

```rando
region market {
  for s in [fruit, fish] {
    check {s}_stall when opt.market_stalls
  }
  check notice_board
  exit dock door market_door group inside
}
```

Rules are written bare. A check or exit with no rule is open. `when` reads options only. Every declaration starts with a keyword, and a rule ends at the first token it cannot continue with, so newlines matter only inside tables (one row per line).

## Run it

```sh
cargo run -p world-build --bin rando -- check crates/world-build/tests/fixtures/tidewell
cargo run -p world-build --bin rando -- build <dir> -o manifest.json --instance instance.json
cargo run -p world-build --bin rando -- fmt <dir> [--check]
cargo run -p world-build --bin rando -- generate <dir> [--seed N] [--players N] [--option name=value ...] [--options file] [-o out/] [--format text|json]
cargo run -p world-build --bin rando -- generate <dir> --check-seeds K [--seed N] [--players N] [--option name=value ...]
cargo run -p world-build --bin rando -- pack [dir] [-o file] [--no-source]
cargo run -p world-build --bin rando -- stamp <file> --tool "NAME VERSION" [--source PATH ...] [--expect 'TABLE>=N' ...]
```

- `check` reads and checks the world and prints every error and warning as `file:line:col: message` with a `help:` line.
- `build` also writes the canonical manifest (to standard output without `-o`) and writes `rando.lock` back into the folder. `--instance` writes the per-seed instance at every option's default. `--allow-edited` accepts a hand-edited generated file as a warning. `--publish` refuses edited files and any warning.
- `fmt` rewrites every `.rando` file under the folder in the canonical layout. A rule too long for a line of 100 characters (`fmt::WIDTH`) is broken before each `or` of its outer `or` (or each `and` of its outer `and`), one level under its line, and a part still too long is broken the same way one level deeper (a table cell, which ends with its line, and a rule with a `match` are left alone). A line can stay longer when no `and` or `or` is left to break at (one long call, or modifiers after the rule). A generated file's content hash does not count these breaks. `--check` fails when a file would change, for CI. Its layout is linear in the file's size (the parentheses it adds were placed by a scan per token before, which was quadratic; a test holds 4 times the parts to under 10 times the time).
- `generate` builds the world in memory with the same checks as `build` (its errors print exactly as `check` prints them, and nothing is written), makes the per-seed instance from the options (each option's default otherwise), fills a room of `--players` copies (default 1, at most `config::ROOM_MAX_PLAYERS`, 1024) with the engine's own fill (`data_fill::seed::generate_on`) and writes the seed with the engine's seed writer. It writes two files into `-o` (default the current folder): `<world>-<seed>.seed` and `<world>-<seed>-spoiler.txt` (or `.json` with `--format json`). The spoiler holds the seed, the options used, every placement by region and by item, the play order (progression items by sphere; the engine computes spheres, not a minimal path), each player's shuffle results, and the fill counters (attempts and restarts, rounds divided, items moved, and a warning when a world has less room early on than progression to place). A seed is 0 to 2^53 - 1 (`config::SEED_NUMBER_MAX`, so a JSON spoiler holds it exactly). Without `--seed` a random seed is drawn and printed; the same seed, players and options always write the same bytes.
  - Options are `--option name=value` (repeatable) or `--options file`, one `name = value` per line with `#` comments. A toggle takes `on`/`off`, a choice a value's name, a range a whole number in its bounds. An unknown option or a value it does not take is refused by name, with what it takes. When the spoiler cannot be written, the seed file written before it is removed.
  - A fill that cannot finish names the stuck item (and its player in a room of several) and says what to try. The engine returns this as a typed error, `data_fill::seed::GenerateError::Fill(FillError::Stuck { item, player, attempts })`, so nothing reads error text.
  - `--check-seeds K` fills seeds `N` to `N + K - 1` (`N` is `--seed`, default 1) and writes nothing. It prints how many filled, the fill attempts and restarts over the seeds that filled, and the time per seed (fastest, median, slowest; measured on the machine it runs on, and covering the fill, the spoiler and the seed file). A range that would go past the largest seed is refused. On any failure it prints the first failing seed with its error and exits 1.
- `stamp` writes or refreshes the `generated` header of a file a generator wrote, in any language (the library's `front::stamp` and `write_generated` do the same in Rust): the tool, each source with its SHA-256 (paths relative to the world's folder, the nearest folder above the file with a `world.rando`), the table sizes promised, and the content hash. An old header is replaced and the body kept as written. Quote `--expect`, since a shell reads `>` as a redirection.
- `pack` (design doc 19 sections 3 and 4) needs a `rando.toml` with `authors`, `license` and `description`, builds with the rules of `build --publish` (any warning refuses it), and writes `<name>-<version>.randopack` beside `rando.toml` (or to `-o`), then `rando.lock` with the ids marked published. Nothing is written when it is refused. The package is a zip of exactly: `manifest.json` (what `build` writes), `meta.json` (`rando.toml` as canonical JSON with `entry` filled in, asset paths under `assets/`, a `license_file` when the license is a `LicenseRef-`, and `tool`, the `rando` version), `assets/` (the icon, `[assets] files`, and `LICENSE` for a `LicenseRef-` license), `source/` (`rando.toml`, every `.rando` and `.csv` file the build read, and the new `rando.lock`; left out with `--no-source`, which the portal will refuse) and `sha256.txt` (`<hex>  <path>` per other entry, sorted). Entries are in path order with the zip epoch as their time, mode 0644, deflate at level 9 and no extra fields, so one folder packed twice by one `rando` gives the same bytes (a test holds this). Assets are checked by their first bytes (PNG, WebP, or plain UTF-8 text; the icon must be an image) and sizes: `config::Limits` `world_icon_max_bytes` (256 KiB), `world_asset_max_bytes` (2 MiB each), `world_assets_max_bytes` (16 MiB in all); the package caps the server will apply are the `package_max_*` fields of `config::Limits` (source 16 MiB, package 40 MiB, 512 entries, paths of 255 bytes), which `pack` reads from `Limits::DESIGN`, so `pack` never writes a package the server would refuse for size. Not checked yet: icon pixel sizes. The server upload (doc 19 section 5) is not built yet.
- Unknown names, items, options and regions (exit targets and `start`) suggest the nearest declared one of that kind in the help ("did you mean `can_be_goron`?"), within an edit distance of 1 to 3 by length.
- `build` writes `rando.lock` last, and only when the manifest (and `--instance`) were written, so a failed build records no ids. `--publish` marks every declared item and check id `"published": true` in `rando.lock`. A later build refuses to drop a published entry (an error naming it and its id): its id is already in seeds and clients. A `was` rename keeps the id and the mark. Unpublished entries are retired as before.

### The id lock, `rando.lock`

`rando.lock` holds the permanent id of every item and check, keyed by name or written `key`. The tool writes it; commit it, and never delete or hand-edit it. It is one line of canonical JSON: `format` (1 today; any other value is refused with "this rando reads format 1", so update `rando`), a `note` saying the above (JSON has no comments), `world`, `items` and `checks`.

- **No lock**: the build still numbers everything in declaration order and writes one, and prints a warning that new ids were assigned, naming the file to commit. A world whose lock is lost gets the same ids back only if nothing was renamed, reordered across modules or removed since; that is why it is committed. The real guard against renumbering a published world is the server (design doc 19): a published version's ids never change, and an upload that renumbers them is refused.
- **Upgrading from `ledger.json`**: the old name is refused with the fix, `git mv ledger.json rando.lock`. A lock with no `format` field is the old shape and is read as format 1, then rewritten with the field. A `ledger.json` left beside a `rando.lock` is a warning (delete it, so the two cannot drift).
- **Strict reading**: a lock that is there but is not an object of `world` (this world's key), `items` and `checks` (objects of objects with an `id`), optional `format` and `note`, and nothing else, is an error labelled `rando.lock` with the help to restore it from version control. The only way to number afresh is to have no lock at all.
- **File order**: with a lock, the order modules are read in does not move any id (a test swaps the `use` lines). Without one, ids follow the order the files apply, which is fixed by the `use` lines, not by the disk.
- **A merge conflict** in `rando.lock` (two branches each added items or checks) is always the whole line. Keep one side, then run `rando build`: the other branch's new names get fresh ids after the kept side's. If one side was published, keep that side, since its ids are in seeds already. Never hand-merge the JSON.

Tests: `cargo test -p world-build` (`tests/build.rs` for the checks on the model, `tests/lang/` for the language). Every folder in `tests/fixtures` must be a world (a `world.rando`) and is built, checked for warnings, its committed `rando.lock` (required for every fixture) and its canonical layout, loaded in the engine and filled for seeds 1 to 50 at 1 and 4 players (`tests/trials/`), so a new fixture is tested by being there. `data_fill::seed::generate` refuses a world a player cannot finish, so every seed passing means every seed is beatable. Tidewell's `.rando` files build to the same bytes as its model in `tests/support/mod.rs`. The trial worlds of doc 18 (section 8, question 9), `cinderfall` (an adventure: modules, keys, progressive items, a shuffle of interiors) and `neon_cadence` (a catalog: a generated base, an overlay, families, a regeneration), have tests of their own. Five genre examples show how a genre's randomizer conventions read in the language, each with a README of what it shows and what it cannot express yet: `meridian_front` (a real-time strategy campaign), `cobalt_warrens` (an exploration platformer on one map), `gloam_delve` (a roguelike), `oathbound_tale` (a party RPG) and `bramble_isles` (a collect-a-thon platformer).

## The parts

| Part | Where | What it does |
|---|---|---|
| Lexer | `src/lex.rs` | Tokens with file, line and column, glue (no space before) and line breaks; comments kept. |
| Rule parser | `src/rule.rs` | Doc 16 section 5.0 precedence, recursive descent on tokens, depth cap (arithmetic chains count; a sum is one node). |
| Front end | `src/front/` | `parse.rs`: one file to a tree with error recovery and layout marks. `mod.rs`: modules, tables and families, overlays, visibility, generated headers. `ledger.rs`: ids. `fmt.rs`: the formatter. |
| Model | `src/model.rs` | The world as written, syntax-neutral, every value with its place; ledger ids apart from names. |
| Checks and lowering | `src/world.rs`, `src/lower.rs` | Names, the format rules, the shuffle slice, the pool test; rules to the engine's trees. |
| Canonical JSON | `src/canon.rs` | Keys sorted by bytes, integers only, no whitespace, one newline at the end. |
| Instance | `src/instance.rs` | Per option values: checks that exist, the pool, classes, start items, fixed placements, placement sets, as the engine's instance JSON. |
| CLI | `src/main.rs` | `rando check`, `fmt`, `build`. |

The engine is optional: feature `engine` (on by default) adds `data-fill`, for the last check (the engine's own compiler runs on every manifest) and `Built::instance`. `cargo build -p world-build --no-default-features` builds a tool that checks, formats and writes canonical JSON and instance JSON without Constellation. The comparison operators and the monotone rule live in `crates/rule-ops`, which both use. Everything is Rust, with no new dependency except `sha2`, which the workspace already had.

## Grammar decisions (corrections for doc 18)

Where doc 18's sketch was ambiguous, did not parse its own examples, or said nothing, the parser does the following:

1. **Reserved words**: the words that start a declaration or a member (`world start option item pool start_item assumed def group score steps table for placement region shuffle require trick goal extend replace remove generated use check exit event stay`), plus `when`, `match`, `and`, `or`, `not`, `in`, `div`, `mod`, `true`, `false`. A reserved word is written as a name in backticks: `` `group` ``.
2. **Contextual words are not reserved**: `also`, `key`, `was`, `door`, `vanilla`, `holds`, `no_progression`, `can_stay`, `x`, `version`, `fill`, `from`, `where`, `kind`, `slots`, `things`, `thing`, `coupled`, `only`, `deny`, `only_in`, `deny_in`, `again`, `exits`, `tool`, `source`, `content`, `sha256`, `expect`, the option and column types, and `on`/`off`. Each comes only after a complete name or rule, where a rule has already ended. So `key` stays usable as a name (doc 16's example item) and as a table column.
3. **Table rows are one per line.** This is the one place a newline means something. A row with too few or too many values is an error at the end of the line.
4. **Field access** `d.key` is a family variable's column. Dotted names are one token: `opt.k`, `at.s.slot`, `var.field`, and overlay targets `region.name`.
5. **Name pieces** `{d.key}_ante{ante}` are glued tokens: a piece joins the name only with no space before it. `_` alone is the `match` wildcard. `_` glued between pieces (`c{i}_{j}`) is text, and a name may not be `_` alone.
6. **`use PATH`**: `PATH` is names joined by `/`, without quotes, read as `PATH.rando` inside the world folder.
7. **`shuffle NAME [kind NAME] { ... }`**, one item per line: `slots exits(g, ...)`, `things exits(...)`, `slots a, b` (free names), `thing NAME [: rule] [group NAME]`, `coupled [rule]`, `check none|layout`, `when rule`, and rows `slots S only|deny T [when r] [again]` and `things T only_in|deny_in S ...`. `kind`, `when`, row `when` and `again`, `pin`, `vanilla`, `pool`, `after` and `scope` are parsed and refused as not supported yet (the engine's first slice).
8. **Overlay targets** are `region NAME`, `check REGION.NAME`, `exit REGION.TO`, `def NAME` and `item NAME`. `extend` takes a region and a block, `replace` takes `: rule` for a check, exit or def, and `remove` takes any target. There is no syntax for the base hash an overlay was last built against, so the missing-target message cannot name the hashes doc 18 section 2.8 shows.
9. **`generated { ... }`** must be the first thing in its file. `content` is the SHA-256 of the tokens after the block in the canonical layout, comments and blank lines left out (so a note is not an edit), plus the header's `expect` lines and the bytes of every table file the file reads with `from`. `expect NAME >= N` names a table declared in that file. Families have no names to expect. `world_build::front::write_generated` writes a header with the right hash.
10. **Toggles**: their default is `on`/`off` (also `true`/`false`), and `match` on a toggle has arms `on` and `off`. `match` on a range is an error.
11. **`no_progression`** alone means always. A rule after it is read only if a rule starts there, and a modifier word (`key`, `was`, `vanilla`, `holds`, `no_progression`) starts the next modifier, not a rule. So no def may take one of those names (an error that says why); items, checks, events and table columns still may (`no_progression` never reads an event, which is state). Each check modifier (`when`, `vanilla`, `holds`, `no_progression`, `placement`, `key`, `was`) may be given once; a second is an error.
18. **Negative numbers**: where a number is expected (a range's ends, an option default, a table's `int` cell, a copy count), `-` glued before a plain number on the same line is its sign (`-3`, not `- 3` or `- -3`), so `range -3..3` and a row `a -2` read as written; `fmt` writes the sign glued. Inside a rule, `-` after a name or number is subtraction (`i -1` is `i - 1`).
20. **Computed numbers**: a number of copies (`x`), a start or assumed item's copies, a fill weight, a family range's ends (`for i in 1..d.rooms`) and the count in `has(x, n)` take a number expression: written numbers, family fields, def parameters, `opt.<key>` and `+ - * div mod`. What folds at build time is a number. Item copies and `has` counts may also read options: copies are settled per seed (and tried by the pool test at every setting), and `has(x, e)` becomes `count(x) >= e` in the engine's form, which takes `+`, `*` and `div` over options. Start item copies, fill weights and ranges must fold. Nothing that reads what the player holds is a number there.
19. **Always-true comparisons fold**: a comparison that holds whatever the player has (`count(x) >= 0`, `0 <= counter(e)`, a sum or nonnegative multiple of counts against a number at or below its least value) is the rule `true`, and a `true` part drops out of an `and` and settles an `or`. A comparison with an option is never folded (an option can be negative). A folded comparison still counts as reading what the player holds, so `not (count(x) >= 0)` and `when count(x) >= 0` are refused like any state read.
12. **Declarations doc 18 shows but this version does not build** (`score`, `steps`, `trick`, `require`, `stay`, an item `when`, a class `match`) are parsed far enough to be skipped and reported as not supported yet, with their place.
13. **A family's body** holds `item`, `region`, `start_item`, `assumed`, `group`, `placement` and `for` at the top level, and members inside a region; anything else there is an error. Group and placement set names and members may hold name pieces (`placement {d.key}_keys { {d.key}_key }`). **A placement block** `placement NAME { items } { declarations }` holds what a family's body holds; every region in it (its families' regions too) joins the set unless it names its own, so a dungeon's keys and rooms are declared together. Blocks nest at most 32 deep. **A family's `where`** may read options. What folds at build time drops combinations; what is left joins each generated check's (or region's) `when`, and an exit's or event's rule. A `where` cannot switch off declarations that have no `when` (options, defs, shuffles, items).
14. **Ledger keys in a family** are the expanded names. A written `key "..."` applies to one declaration outside a family.
15. **Visibility**: a file may read a def, item, item group, placement set, shuffle or option declared in itself, in a file it reaches by `use`, or in the entry file. Regions and events are visible world-wide (and checks are never read), so region files lead to each other and read each other's events without a `use` each way.
16. **Def parameters**: an argument is a number, a name or `opt.<key>`, after folding. The written def is named `<def>__<arg>__<arg>`, where each argument is written as its length, `_`, then its spelling: a number is its digits (`3`, or `3n` for -3), a constant `0b`/`1b`, an option `0opt_<key>` and a name itself (`need(p_, q)` is `need__2_p___1_q`). The length keeps names that start or end with `_` apart, so two argument lists never share a written def.
17. **Top-level keywords** other than declarations are errors: `check`, `exit`, `event` and `stay` say they belong inside a region. `score`, `steps`, `trick` and `require` are skipped and reported as not supported yet.

**Where this parser and doc 18 (at `d2c302b`, "answer PR 8 review") now differ**, for the two to converge on one form:

- Doc 18 ends a statement at a newline. Here only table rows are line-based, and statements end where their grammar ends.
- Doc 18 starts every file with `language 1`. Here there is no version line.
- Doc 18 writes `use "base.rando"`. Here it is `use base` (names joined by `/`, no quotes, no extension).
- Doc 18 writes `pool fill [coin 9, gust 1]`. Here it is `pool fill { coin 9, gust 1 }`.
- Doc 18 writes templates in backticks, `` `{t.key}_lap{lap}` ``. Here it is bare glued pieces, `{t.key}_lap{lap}`, and backticks only quote one plain name.
- Doc 18 writes `expect dish 40`. Here it is `expect dish >= 40`.
- Doc 18 writes `replace check cliff.summit { clauses }`. Here `replace` takes `: rule` only.
- The reserved and contextual word lists differ. Doc 18 reserves `language`, `context`, `roll`, `requires`, `in`, `where`, `opt`, `at`, and here `where` is contextual. Here `key`, `was`, `door`, `vanilla`, `holds`, `no_progression` and `can_stay` are contextual, as doc 18 now also has them.

## What is checked

Each error names the file, line and column and what to do. All errors are collected; the parser recovers at the next keyword. A block left open (`region x {` with no `}`) is named with the line it opened on, and the members read before the next declaration are kept. A syntax error does not stop the checks: they still run on everything that parsed, in every file, and leave out what may only be a cascade of it (an error about the whole world, a region that cannot be reached, or one naming in backticks a name written anywhere in a file that did not parse, since its declaration may be in the lost part; an overlay whose target is such a name is not reported as missing). No ledger is assigned or written while a file does not parse.

- Syntax, everywhere, with recovery; modules that do not exist; names read without a `use`.
- Families and tables: unknown tables and variables, a row or name piece of the wrong type, CSV header, types and line numbers, duplicate rows (at their own line), ranges that are empty or too long, at most 100,000 declarations written by families, and blocks at most 32 deep. One mistake a family repeats is reported once: a name a template writes is shown as the template, with the first expansion and its binder values and the count of the others ("unknown item `{s.pack}_pak` (as `dawn_pak` for `s = amber`, and 2 more)"); a mistake one expansion makes keeps its name and adds the template and binder values.
- Files: a path or link that leads out of the world's folder is refused, and `fmt` does not follow links.
- Checks in several regions: a check may be declared in more than one region, each declaration with that region's rule. Exactly one declaration is plain and every repeat is marked `also` right after the name (`check drop also: has(lamp)`), so a name copied by mistake is an error naming both places, not a silent merge; `also` with no plain declaration is an error too, and `also` goes right after the name (written after the rule, it is an error that says so). The rule is checked on the files as written, before overlays apply: an overlay that removes the plain declaration (`remove check a.x`, `remove region a`) leaves the `also` declarations as the check, and a declaration an `extend` adds must still be `also` when a plain one exists. It is one location (one id, one place in the pool) with a site in each region in the engine's form, and it exists where at least one of its regions does. Every other modifier (`when`, `vanilla`, `holds`, `no_progression`, `placement`, `key`, `was`) is given in one declaration only; a second is an error naming the region of the first. One region may not declare a check twice, and the regions of one check must agree on their placement set unless the check gives its own.
- Names: unknown names, a name used as the wrong kind ("`lamp` is an item; did you mean `has(lamp)`?"), duplicates across the one namespace of items, defs, events, regions, item groups, placement sets and shuffles (events may repeat), reserved words, name shape. **Checks have their own namespace**: rules never read a check, so a check may share its name with an item, a region or an event (its ledger id is in the ledger's own check table). A rule that names a check is an error that says so.
- Regions: an exit to a missing region, two exits to one region through one door, a region no exit leads to from the start, and what a region `when` would leave dangling (see "Choices").
- Format rule a (D-132): `not` over state, comparisons that can turn false, products and divisions that can.
- Format rule b (D-133): dotted names that are not options, shuffle reads or family fields.
- Format rule c (D-134): check and region `when`, `no_progression`, start item and assumed conditions read options only, never state or a shuffle result, through defs too.
- `match` covers every value; defs with parameters get the right number of fixed arguments.
- Items, the pool test across option values, shuffles in the engine's first slice, read cycles, nesting depth, and table sizes, as before.
- The ledger: ids unique, in 1 to 2^53 - 1, and for this world. Renames with `was` keep ids. Retired ids are never given again. A published entry cannot be dropped.
- Generated files: a content hash that does not match (an error, or a warning with `--allow-edited` when not publishing), sources that changed (a warning), and tables smaller than their `expect`.
- Overlays: a target that is gone (`overlay.target_missing`), and any two overlays that change one target (a replace, a remove, or a check or exit an `extend` adds) where neither file uses the other. A removed check or item is retired in the ledger.
- The ledger also refuses a `was` that names a key still declared, and a written `key` that meets another declaration's key or name.

Limits are in `crates/config` (`WORLD_*`, `RULE_MAX_PARTS`). All are design values except the engine's own.

## Choices made where the docs are silent

- **Output is the engine's manifest shape, not all of doc 16's canonical JSON.** Fields the engine would ignore are not written. The world-side fields go into the per-seed instance.
- **Canonical text is compact**: no whitespace, a newline at the end. The ledger is canonical JSON too.
- **Ids**: from `rando.lock`, keyed by name or written key. A world with no ledger is numbered in declaration order from 1, items and checks apart, and the ledger is then written. Without a ledger, ids follow the order the files apply: each module after the modules it uses.
- **`nothing`** is added as an item (filler, the id after the largest) only when the world has no `pool fill`.
- **Free slots are split by weight deterministically** (largest remainder, ties by key), since the seeded draw waits on D-47.
- **A `holds` copy of a check that does not exist this seed stays in the pool.**
- **`class` is required.**
- **Exit keys are `region:to`.** Several exits from one region to another are several ways in, each with its own rule, keyed `region:to:door` by a door each has, or `region:to:N` by its place among them (from 1, in file order, so reordering them renames these keys) when it has none; two of them may not share a door. An overlay's `exit region.to` changes every exit to that region (there is no target for one of them yet).
- **A region's `when` removes the region** for the seeds where it is false: it joins the condition of its checks, its events and every exit into or out of it, so nothing in it exists and no way leads through it. The engine reaches every region of the manifest in a world with a shuffle (engine issue 211), so the build adds, for each region with a `when`, an exit from the start whose rule is the `when`'s negation: it opens only when the region is gone, and the region then holds nothing. `check` tries every setting of the options the region conditions read (the pool test's settings) and reports, naming the setting: a region with no way in from the start through regions that exist, and a rule (a check's, an event's, an exit's, the goal or a shuffle thing's) that cannot hold without an event only gone regions fire or a `reach` of a gone region (one alternative of an `or` may be gone). The start region cannot have a `when`, and no shuffle may move an exit into or out of a region with one, nor may such an exit have a `door` (a shuffle that changes with the options is not in the engine's first slice). A rule the setting's own options close anyway (`opt.vault and lever`) is not reported.
- **Number-only arithmetic folds** at build time, `-` and `mod` included. Over options or state they stay refused, because the engine cannot evaluate them.
- **`count(a, b)`** is a sum of counts over the items in key order, like a group.
- **Builds run on their own thread** with a stack sized for the depth cap (`WORLD_BUILD_STACK_BYTES`).

## What the manifest cannot express (gaps)

- `format`, the display name, option and player-facing texts, and `requires`. The manifest has no header fields beyond `game` and `version`.
- Item class, count, the pool, check `when`, `holds`, `no_progression`, placement sets, start items and assumed items. These are per-seed instance fields, which this tool computes.
- An option's type, values and range. The manifest holds a key and an integer default.
- Item groups as named lists (a group count is written out as its sum), and the defs with parameters (written out per argument list).
- A thing's `count`, `min`, `weight`, payloads, shuffle `when`, pins and the other later fields of doc 17.
