# Seed bundle v2

One room's seed as compact binary, read in place, plus one package file per game, shared across rooms. The v1 JSON (`tools/ap-import` output, `seed_bundle::SeedBundle`) stays the debugging view: `bundle v2-decode` turns v2 back into it, and `bundle v2-encode` turns v1 into v2. Code: `crates/seed-bundle/src/v2.rs`, `package.rs`, and `crates/bits`.

**Stability.** The room record, slot record and placement records are stable, because rooms run on them. These parts are still unstable and may change before the first release: the package file's reserved sections (id 2 and up), the reserved header field at 14, the hint block's entrance indexes (see "Hint block"), and the meaning of a slot's `kind` 2 (group slots, refused today). A package's key is the hash of its whole file, so adding a section (such as rules) makes a new key; seeds name the file they were made with.

All integers are little-endian. Offsets are bytes from the start of the file. A string reference is `u32 offset` into the string table plus `u32 length`. A length of `0xFFFFFFFF` means "no string".

## Seed file

| At | Size | Field |
|---|---|---|
| 0 | 4 | magic `CSB2` |
| 4 | 2 | version, 2 |
| 6 | 1 | race mode (0 or 1) |
| 7 | 1 | `wi`: bits per item index |
| 8 | 1 | `wr`: bits per receiver slot position |
| 9 | 1 | `wf`: bits per flags value |
| 10 | 2 | slots `n` |
| 12 | 2 | games `g` |
| 14 | 2 | reserved, 0 |
| 16 | 4 | connect names `c` |
| 20 | 4 | string table offset |
| 24 | 4 | file length |
| 28 | 4 | hint block offset, or 0 for a seed with no hint data (see "Hint block") |
| 32 | 32 | SHA-256 of the canonical v1 JSON this seed came from (`bundle_hash`: keys sorted, no whitespace). Postgres keeps this per room, so v1 and v2 of one seed are the same room. |
| 64 | 32 | SHA-256 of the whole file except these 32 bytes |

Then, in order:

1. **Room record**, 36 bytes at 96: seed name (ref), password (ref, or none), generator version major, minor and build (3 × `u32`), hint cost `u8`, release, collect and remaining modes (`u8` each: 0 disabled, 1 enabled, 2 goal, 3 auto, 4 auto-enabled), location check points `u32`.
2. **Games**, `g` × 32 bytes: each game's package key, sorted by game name.
3. **Slots**, `n` × 72 bytes, by ascending id. The slot position is the index here.

   | At | Field |
   |---|---|
   | 0 | id `u32` |
   | 4 | team `u32` |
   | 8 | name (ref) |
   | 16 | game index `u16`, kind `u8` (0 spectator, 1 player, 2 group), pad `u8` |
   | 20 | minimum client version, 3 × `u32` |
   | 32 | presence offset: `ceil(L/64)` `u64` words, where `L` is the game's location count. Bit `i` set means location index `i` is placed. A slot that places nothing stores no words (offset 0). |
   | 36 | placed count `p` (equals the number of set presence bits) |
   | 40 | records offset: `p` records of `wi + wr + wf` bits |
   | 44 | `slot_data` offset, 48 its stored length, 52 its inflated length (0 means stored as plain JSON, otherwise raw deflate) |
   | 56 | group members offset, 60 count (`u32` slot ids) |
   | 64 | start inventory offset, 68 count (`max(wi, 1)` bits each, so every entry costs file bytes; item indexes in the slot's own game) |

4. **Connect names**, `c` × 16 bytes, sorted by name: name (ref), team `u32`, slot id `u32`.
5. **Data**: the areas the slot records and the hint block point at, and the hint block itself. Every non-empty area lies after the tables and before the string table; a reader refuses anything else. The reserved header field at 14 must be 0.
6. **String table**: UTF-8. Each distinct string is stored once, so a slot name and its connect name share bytes.

**Bit fields** are packed least significant bit first: bit `k` of the stream is bit `k % 8` of byte `k / 8`. Record `r` starts at bit `r × (wi + wr + wf)`. Its fields are: item index (`wi` bits) in the receiver's game, receiver slot position (`wr` bits), flags (`wf` bits: 1 progression, 2 useful, 4 trap). The encoder sets each width to the fewest bits the seed needs: `ceil(log2(max + 1))`, which is 0 when every value is 0.

**Indexes.** A game's location ids and item ids, sorted ascending, give the dense indexes: index `i` is the `i`th smallest id. Record `r` of a slot belongs to the `r`th set bit of its presence set, so the location is implicit. To find location id `x`: binary search the game's location ids for its index `i`, check presence bit `i`, and count the set bits below `i` to get `r`.

Worked example (the real one-slot MM world): 2641 locations, 237 items and one slot give `wi` = 8 (the largest item index is 236), `wr` = 0 (one receiver) and `wf` = 2 (flags up to 2). So 1516 placements take 10 bits each, 1895 bytes in total, and the presence set takes 42 words (336 bytes).

## Hint block

The hints a room starts with, the generator's spheres and the entrances hints name. It takes header field 28, reserved and 0 before, so the version stays 2: a seed without hint data has no block (field 28 is 0) and is byte for byte what it was, and a reader older than the block refuses a seed that has one ("reserved header fields are not 0") instead of misreading it.

Every location in the block is a **location index**: the position of its id in the game's sorted location ids (section 1 order), the same index as the presence bitset. The block holds no ids; its only names are the seed's entrance names, as references into the seed's string table.

| At | Size | Field |
|---|---|---|
| 0 | 1 | `ws`: bits per sphere (0: the seed records no sphere) |
| 1 | 1 | `wl`: bits per location index |
| 2 | 1 | `we`: bits per entrance index |
| 3 | 1 | `wp`: bits per slot position |
| 4 | 4 | start hints `h` |
| 8 | 4 | start hints offset: `h` records of `wp + wl + 3` bits |
| 12 | 4 | entrance names `m` |
| 16 | 4 | entrance names offset: `m` string references (8 bytes each), sorted by name |
| 20 | `n` × 12 | per slot, in slot order: spheres offset (0: none), entrances offset, entrances count `e` |

- **Start hint** record: finding slot position (`wp` bits), location index in that slot's game (`wl`), status code (3 bits: 0 unspecified, 1 no priority, 2 avoid, 3 priority, 4 found; the status values 0, 10, 20, 30, 40). In the bundle's order; each location is placed in that slot. The finder and the receiver both see it; a room builds them into its fresh state (`seed_bundle::room_state`), never into the log.
- **Spheres**: one value of `ws` bits per location of the slot's game (`L` values, for placed and unplaced locations alike), by location index. The all-ones value at `ws` bits means "unreachable or not needed" (no sphere); the encoder sets `ws` to the fewest bits that hold the largest sphere plus that value, `ceil(log2(max + 2))`, so it never collides. A slot with no sphere stores none (offset 0). This is the only sphere encoding: the importer writes it from the v1 data, and data-fill writes the same field directly.
- **Entrances**: `e` records of `wl + we` bits (at least 1 bit), ascending by location index: location index (`wl` bits), then entrance index (the rest) into the block's entrance names. Each location is placed in that slot. The names are per seed (a world may compose them per generation), so they live in the seed's string table, never in the package: a package key never depends on a seed. Exit indexes will be rekeyed to section 2's exit table once that section exists.

Widths are capped: `ws` 16, `wl` 32, `we` 24, `wp` 16. The encoder sizes the block from its counts before it writes anything (spheres stay sparse until then), so the seed cap refuses an oversized block before it costs memory. A reader checks every area against the data range, every index against its table, every entrance name against the string table, every hinted location against the presence set, and the entrance order, before the seed opens.

## Package file

Package files are blobs keyed by the SHA-256 of the whole file, kept in the server's blob store (`log::BlobStore`: the `game_packages` table, migration 0003, or memory); every put and every read is checked against its key (`log::BlobStore::put` and `get`). Seeds are kept per room (`room_seeds`), written once with the room after its bundle hash check, and every load and export refuses a seed whose bundle hash is not the room's. In a process each key has one in-memory copy, shared by every room that uses it. The `bundle` CLI reads and writes `<hex key>.pkg` files in a folder, for debugging only.

| At | Size | Field |
|---|---|---|
| 0 | 4 | magic `CSPK` |
| 4 | 2 | version, 1 |
| 6 | 2 | section count `s` |
| 8 | `s` × 48 | table of contents: id `u16`, flags `u16` (bit 0: deflated), offset `u32`, stored length `u32`, inflated length `u32`, SHA-256 of the inflated bytes |

Each id appears once and no two sections share bytes (a reader refuses the file otherwise), and a reader inflates only the section it asks for, each under the caller's cap. Section ids: 1 is the game, the canonical JSON of `{"data": <v1 GameData>, "game": <name>}`. `GameData` carries the game's `hint_blacklist` (names `!hint` refuses, the same for every seed of the game), left out when empty, so a game without one keeps the key it had. 2 is reserved for rules and regions. A reader skips sections it does not know. A section stored without deflate (flag bit 0 clear) can be read in place (`package::stored`); `package::section` loads any section from the file on request. A room keeps only the game section's id tables and JSON in memory, never other sections.

**Index stability.** A game's location and item indexes are the positions of its ids, sorted ascending, in section 1. The key hashes the file, section 1 included, so for a given package key the indexes never change, whatever order the names were listed in. Other sections, and seeds, may refer to them by index (test: `indexes_are_fixed_by_the_package_key`).

## Decoding by hand

Check the magic and the file hash. Read `n`, `g`, the widths and the string table offset. For each slot record, load its game's package and sort the location and item ids. Walk the presence words: for every set bit `i`, take the next record. Location `i`'s id is the location id; the record's item index looks up the receiver slot's game items; the receiver position looks up the slot table. That gives the v1 `{location, item, receiver, flags}`.
