//! The format, engine and settings-language limits (doc 02 3.2, doc 05 6.2 rule 5): the caps
//! the public tools (`rando`, the fill, the seed and world formats, the settings language) and
//! the server must agree on, so a package the tool writes is one the server takes. No env
//! reads and no IO; the server's `config` holds these as `Limits::format` and may not raise
//! them. Every number here is a DESIGN VALUE from the docs, not a measurement; replace after
//! gate 1 (doc 05 section 20).
#![no_std]

/// Format and settings-language limits, all with design-value defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest seed bundle file the server will read.
    pub bundle_max_bytes: u64,
    /// Largest world manifest upload, bytes (design value: the large test world is 830 KB).
    pub manifest_max_bytes: usize,
    /// Largest `rando.toml`, bytes (design doc 19 section 2.2, design value: a full one is
    /// under 1 KiB).
    pub world_toml_max_bytes: usize,
    /// Longest `rando.toml` `name`, bytes (doc 19 section 2.3, design value).
    pub world_name_max_bytes: usize,
    /// Longest `rando.toml` `version`, bytes (doc 19 section 2.3, design value).
    pub world_version_max_bytes: usize,
    /// Most `authors` in `rando.toml` (doc 19 section 2.3, design value).
    pub world_authors_max: usize,
    /// Longest author, characters (doc 19 section 2.3, design value).
    pub world_author_max_chars: usize,
    /// Longest `description`, characters (doc 19 section 2.3, design value).
    pub world_description_max_chars: usize,
    /// Longest `license` expression, bytes (doc 19 section 2.3, design value).
    pub world_license_max_bytes: usize,
    /// Longest `homepage` or `repository` URL, bytes (doc 19 section 2.3, design value).
    pub world_url_max_bytes: usize,
    /// Most files `[assets] files` may list (doc 19 section 2.3, design value).
    pub world_assets_max: usize,
    /// Largest asset file, bytes (doc 19 section 2.3, design value).
    pub world_asset_max_bytes: usize,
    /// Largest sum of a world's assets, bytes (doc 19 section 2.3, design value).
    pub world_assets_max_bytes: usize,
    /// Largest icon, bytes (doc 19 section 2.3, design value).
    pub world_icon_max_bytes: usize,
    /// Largest `source/` of a world package, all files together, bytes (doc 19 section 5.2,
    /// design value: 256 files at the per-file cap would be 2 GiB). `rando pack` holds to the
    /// package caps too, so it never writes a package the server refuses for size.
    pub package_max_source_bytes: usize,
    /// Largest world package, bytes (doc 19 section 5.2, design value: 8 MiB manifest, 16 MiB
    /// source and 16 MiB assets).
    pub package_max_bytes: usize,
    /// Most entries in a world package (doc 19 section 5.2, design value: 256 modules, 64
    /// assets and the fixed entries).
    pub package_max_entries: usize,
    /// Longest entry path in a world package, bytes (doc 19 section 5.2, design value).
    pub package_max_path_bytes: usize,
    /// Largest `settings.json` in a world package, bytes (doc 20 section 5, a design value,
    /// not derived: it repeats the manifest's names and adds labels).
    pub package_max_settings_bytes: usize,
    /// Largest settings file (`.randoset`), bytes (doc 20 section 7.1, design value), checked
    /// before it is lexed.
    pub settings_max_bytes: usize,
    /// Most statements in one settings file, block lines included (doc 20 7.1, design value).
    pub settings_max_statements: usize,
    /// Most entries in one list, and in one argument list of `one_of`, `chance` and `some_of`
    /// (doc 20 7.1, design value).
    pub settings_max_list_entries: usize,
    /// Most item copies one settings file names, summed over every `x` count (doc 20 7.1,
    /// design value).
    pub settings_max_total_copies: u64,
    /// Most names one settings file reaches after expanding groups, counted while expanding
    /// (doc 20 7.1, design value).
    pub settings_max_expanded_names: usize,
    /// Most `place`, `connect` and `pin` lines in one settings file (doc 20 7.1, design value).
    pub settings_max_placements: usize,
    /// Most `when` and `with` blocks in one settings file (doc 20 7.1, design value).
    pub settings_max_blocks: usize,
    /// Most conditions on one `when` (doc 20 7.1, design value).
    pub settings_max_conditions: usize,
    /// Most value combinations the `requires` walk over surprises tries before leaving it to
    /// generation (doc 20 sections 2.10 and 7.1, design value).
    pub settings_max_combinations: u64,
    /// Most comments in one settings file (doc 20 7.1, design value).
    pub settings_max_comments: usize,
    /// Longest name in a settings file, bytes (doc 20 7.1, design value).
    pub settings_max_name_bytes: usize,
    /// Longest string in a settings file, bytes; a `text` option may set less (doc 20 7.1).
    pub settings_max_string_bytes: usize,
    /// Smallest integer a settings file may write (doc 20 7.1: a 32-bit integer).
    pub settings_int_min: i64,
    /// Largest integer a settings file may write (doc 20 7.1: a 32-bit integer).
    pub settings_int_max: i64,
    /// Largest `x` count (doc 20 7.1, design value); the smallest is 1.
    pub settings_max_copies: i64,
    /// Most edits in one `set_options` frame (doc 20 7.1, design value).
    pub settings_max_edits_per_frame: usize,
    /// Longest value text of one edit, bytes (doc 20 7.1, design value).
    pub settings_max_value_bytes: usize,
    /// Longest `rules` field of `set_options`, and one rule given to `add_rule` or `set_rule`,
    /// bytes (doc 20 7.1, design value).
    pub settings_max_rules_bytes: usize,
    /// Most presets taking part in one settings file's layering, counted across lines and
    /// chains (doc 20 section 2.6, doc 15 section 2.6's design value).
    pub settings_max_presets: usize,
    /// Deepest nesting of values in a settings file, checked by the parser before it recurses
    /// (doc 20 section 2.2: a surprise holds `any` or `between` one level down, and a record's
    /// field holds a list; nothing deeper), so a deep file is refused, never a stack overflow.
    pub settings_max_depth: usize,
    /// Longest player name, Unicode scalar values (doc 20 7.1, design value).
    pub settings_player_max_chars: usize,
    /// Longest player name, bytes (doc 20 7.1, design value).
    pub settings_player_max_bytes: usize,
}

impl Limits {
    /// The design values (see each field).
    pub const DESIGN: Self = Self {
        bundle_max_bytes: 64 * 1024 * 1024,
        manifest_max_bytes: 8 * 1024 * 1024,
        world_toml_max_bytes: 64 * 1024,
        world_name_max_bytes: 64,
        world_version_max_bytes: 64,
        world_authors_max: 16,
        world_author_max_chars: 128,
        world_description_max_chars: 1000,
        world_license_max_bytes: 256,
        world_url_max_bytes: 2048,
        world_assets_max: 64,
        world_asset_max_bytes: 2 * 1024 * 1024,
        world_assets_max_bytes: 16 * 1024 * 1024,
        world_icon_max_bytes: 256 * 1024,
        package_max_source_bytes: 16 * 1024 * 1024,
        package_max_bytes: 40 * 1024 * 1024,
        package_max_entries: 512,
        package_max_path_bytes: 255,
        package_max_settings_bytes: 4 * 1024 * 1024,
        settings_max_bytes: 64 * 1024,
        settings_max_statements: 2000,
        settings_max_list_entries: 1000,
        settings_max_total_copies: 20_000,
        settings_max_expanded_names: 50_000,
        settings_max_placements: 256,
        settings_max_blocks: 64,
        settings_max_conditions: 8,
        settings_max_combinations: 4096,
        settings_max_comments: 1000,
        settings_max_name_bytes: 64,
        settings_max_string_bytes: 1024,
        settings_int_min: -(1 << 31),
        settings_int_max: (1 << 31) - 1,
        settings_max_copies: 65_535,
        settings_max_edits_per_frame: 64,
        settings_max_value_bytes: 4 * 1024,
        settings_max_rules_bytes: 32 * 1024,
        settings_max_presets: 2,
        settings_max_depth: 2,
        settings_player_max_chars: 64,
        settings_player_max_bytes: 256,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DESIGN
    }
}

/// Deepest rule nesting a world's rules section may hold, counting every node and every def a
/// rule reaches through `ref`: the loader rejects deeper rules, so the recursive evaluators
/// stay well inside a thread's stack (design value; real worlds nest under 20).
pub const RULES_MAX_DEPTH: u32 = 256;

/// Largest weight of one member of a weighted group sum (`data_fill`'s `GROUP`): the largest
/// integer a rule's bytecode holds (16 bits signed), so a weighted sum compares against any
/// threshold a rule can write. With [`RULE_GROUP_MAX_MEMBERS`] it bounds a sum below 2^31 times
/// the copies of one item, far inside `i64` (a design value).
pub const RULE_WEIGHT_MAX: i64 = i16::MAX as i64;

/// Most members of one group sum, plain or weighted; compiler and loader refuse more (a design
/// value: the group index is 16 bits wide, and a group is not meant to be larger).
pub const RULE_GROUP_MAX_MEMBERS: usize = u16::MAX as usize;

/// Most terms a rule may take as a bit rule (`data_fill`'s fold: an OR of terms, each a set of
/// atom bits that must all be set). Turning AND over OR into that form multiplies terms, so a
/// rule that would need more, at any step of the way, stays bytecode (a design value: the real
/// test world's rules are counted in the PR that added this).
pub const RULE_BITS_MAX_TERMS: usize = 16;

/// Most tests (atom bits and time windows) one term of a bit rule may hold. A def's terms are
/// copied into every rule that refers to it and AND joins the lists, so without this a long
/// def referred to from many sites grows the bit rules by sites times its length; a rule with a
/// longer term, at any step of the way, stays bytecode (a design value).
pub const RULE_BITS_MAX_TERM_TESTS: usize = 32;

/// Most tests a loaded world's bit rules may hold in all (two bytes each), def terms copied
/// into sites counted: once reached, the rules not yet compiled stay bytecode. A site's rule
/// is at most [`RULE_BITS_MAX_TERMS`] terms of [`RULE_BITS_MAX_TERM_TESTS`] tests, so this
/// bounds a load whatever the manifest (a design value: the heavy synthetic world of 1,000
/// checks holds under a tenth of it).
pub const RULE_BITS_MAX_WORLD_TESTS: usize = 1 << 18;

/// Full fills a seed may start before the data-only fill gives up as stuck: a net for what the
/// exact fill's repairs in place cannot mend, not its way of coping (each retry takes the next
/// draws of the seed's random stream and goes deepest first; how often it is entered is in the
/// fill report, `attempts`). A design value. Counted on 9,926 rooms with 20 allowed (tight
/// synthetic worlds of 60 to 1,000 checks at 1, 3, 10 and 100 players, both fixture worlds,
/// 3,000 seeds each): none needed more than 3, one needed 3, and none that drawn order fills in
/// 20 attempts is stuck here in 3.
pub const FILL_MAX_ATTEMPTS: u32 = 3;

/// Version of a prepared world's layout beside its rules section (`data_fill::prepared`): which
/// sections the file holds and what is in them. Raise it with any change to that. A world is
/// also prepared again by every new server build, so this guards builds that share a version
/// text (local and test builds).
pub const PREPARED_WORLD_LAYOUT: u16 = 2;

/// Attempts one world's shuffles get per seed (`data_fill::shuffle`): a failed layout or
/// whole-world check starts every shuffle of that world again on the next attempt's random
/// streams, and at the cap the room's generation fails naming the world and the shuffle (a
/// design value, doc 17 section 4.7 of the design docs).
pub const SHUFFLE_MAX_ATTEMPTS: u32 = 4;

/// Most shuffles one world may declare (a design value).
pub const SHUFFLES_MAX: usize = 64;

/// Most slots over all shuffles of one world, and most things of one shuffle (a design value:
/// the largest entrance shuffle we know of is reported at 611 exits). Bounds the permission
/// tables, which hold a cell per slot and thing.
pub const SHUFFLE_MAX_SLOTS: usize = 1024;

/// Longest shuffle name in bytes: a name keys a random stream, and a stream label is short
/// (`spec_rng::derive`).
pub const SHUFFLE_NAME_MAX_BYTES: usize = 48;

/// Most steps the load of a world's layout index may take: per shuffled exit, the regions
/// that reach its region by exits that are not shuffled (what the layout solve tells a branch
/// from a dead end by, about 2 bytes each), and the exits walked to find them (a design
/// value).
pub const SHUFFLE_MAX_BEHIND: usize = 1 << 20;

/// Most permission rows one shuffle may carry: a row is applied to the whole table of its
/// shuffle when the world is compiled (a design value).
pub const SHUFFLE_MAX_ROWS: usize = 64;

/// Most things a shuffle may have when a rule reads its result as a rule: such a read is one
/// shared rule listing every candidate, and a rule lists at most this many parts (the
/// engine's limit, one byte).
pub const SHUFFLE_MAX_READ_THINGS: usize = 255;

/// Longest error text the shuffle checks give back, bytes: a message may quote names from the
/// manifest, and a name's length is not known to be small when it is quoted (a design value).
pub const SHUFFLE_ERROR_MAX_BYTES: usize = 4096;

/// Placed items an exact fill tries to move, one fresh solve of the room each, for one item
/// that has nowhere to go (`data_fill::room`, `displace`), before the attempt counts as stuck
/// (a design value).
pub const FILL_DISPLACE_TRIES: usize = 32;

/// Rule evaluations an exact fill may spend on repair, over all its attempts: the fresh solves
/// of moves, one solve's worth more per move for the pass over the rounds after it, and per
/// divided round the evaluations of the attempt's first solve with none of the rounds' items
/// (`data_fill::room`, `charge_division`), counted the same by the fill and its cross-check.
/// Within the budget means at most this much spent: once more is spent, checked before each
/// solve and at each division, the attempt is stuck (so a fill ends at most one solve past
/// it), and so is any later one that needs a move or a division. A budget of work, not of
/// time: a seed is the same on every machine. A design value with a margin over the
/// most repaired bench room, keys at 100 players, which charges 34.9 to 37.5 million
/// (counted, `bench-room` run 45948967615, seeds 1 to 3): 1.7 times that. By the growth of
/// that charge over room size (counted at 22 and 100 players) it lets key rooms up to about
/// 120 players and chain rooms up to about 200 fill without a restart (estimated). Time is
/// not bounded by it: on that run keys at 100 players took 4.3 to 4.4 s for 78 to 83 million
/// evaluations in all (measured), about 55 ns an evaluation; at that rate the budget is about
/// 3.5 s of repair, and at the heavy profile's 186 ns about 12 s (both estimated).
pub const FILL_REPAIR_EVALS: u64 = 64_000_000;

/// The least p value the product's fill may show against uniform beatable placements on the
/// one-player fixture world (`data-fill` `tests/fairness.rs`; a design value: drawn order gives
/// 0.39 there, each player's items deepest first gave 0.00008, counted over 300 seeds).
pub const FILL_FAIRNESS_MIN_P: f64 = 0.01;

/// Random orders of a world's progression the early capacity ratio is the mean over
/// (`data_fill::capacity`), the setup-time warning that a world leaves little room early on
/// (as many orders as the measurement that found the ratio used; a design value).
pub const FILL_CAPACITY_ORDERS: u32 = 30;

/// Longest game or version name a manifest may carry, bytes (a design value; it is kept in
/// listing columns and an index). Also the longest name in a shuffle's vocabulary (a slot, a
/// thing, a group, an exit key, a door).
pub const MANIFEST_NAME_MAX_BYTES: usize = 128;

/// Deepest a rule string may nest (parentheses, `not`, function arguments), as written: the
/// provisional author cap of design doc 16 (question 2, a design value), well inside the
/// engine's [`RULES_MAX_DEPTH`].
pub const WORLD_RULE_MAX_DEPTH: usize = 64;

/// Most errors (and, apart, warnings) one world build, one settings parse or one settings check
/// reports before it stops collecting; past it one more line says how many were dropped (a
/// design value: enough to fix a file in one pass, bounded so a broken file cannot print
/// without end).
pub const MAX_ERRORS: usize = 200;

/// Most parts one rule node of the engine takes: the parts of an `all` or `any`, the items of
/// `has_any` and `has_all`, the operands of a sum, `min` or `max`, and `n` in `has` (the
/// engine's limit, one byte).
pub const RULE_MAX_PARTS: usize = 255;

/// Largest item or location id on the wire and in a world's ledger: 2^53 - 1, the range world
/// ids keep to so every client holds them exactly (a format value, not a limit).
pub const ID_MAX: i64 = (1 << 53) - 1;

/// Largest seed number `rando generate` takes or draws: 2^53 - 1, so the number in a JSON
/// spoiler reads back exactly in any JSON reader (a format value, not a limit).
pub const SEED_NUMBER_MAX: u64 = (1 << 53) - 1;

/// Deepest that blocks (`for`, `region`, `extend`) may nest in a `.rando` file: the parser and
/// the expansion recurse once per block (a design value; a world nests three or four deep).
pub const WORLD_BLOCK_MAX_DEPTH: usize = 32;

/// Most items, checks or regions one world may declare: the engine's 16-bit index width
/// (design doc 16 section 5.3).
pub const WORLD_TABLE_MAX: usize = 65_536;

/// Largest `.rando` file (or table file) the build tool reads, bytes: the input is untrusted,
/// and a world of a few thousand checks is well under a megabyte (a design value).
pub const WORLD_SOURCE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Most files (modules and table files) one world may read (a design value).
pub const WORLD_MODULES_MAX: usize = 256;

/// Most declarations the families of one world may expand to, so a product of a few long
/// lists cannot run away (a design value: the survey's largest family is a few thousand).
pub const WORLD_FAMILY_MAX: usize = 100_000;

/// Stack of the thread a world build runs on, bytes: lowering recurses once per rule node, up
/// to [`RULES_MAX_DEPTH`] nodes deep through defs, at several frames a node (a design value,
/// well over what that depth takes in a debug build; only the pages used are committed).
pub const WORLD_BUILD_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Most copies of one item a world may hold, and the largest fill weight and grant (design doc
/// 16 section 5.3, a design value that fits the engine's 16-bit numbers).
pub const WORLD_ITEM_COPIES_MAX: i64 = 32_767;

/// Most options a world may declare: the engine's limit, an option being one byte in a rule.
pub const WORLD_OPTIONS_MAX: usize = 256;

/// Most start item copies one seed's instance may grant in all, so a world's numbers never
/// decide how much memory the per-seed step takes (a design value: start items are a handful).
pub const WORLD_START_COPIES_MAX: usize = 1024;

/// Most option combinations the build tool's pool test tries (design doc 16 section 3.4): past
/// it the test is skipped with a warning, and generation still checks the actual settings (a
/// design value).
pub const WORLD_POOL_TEST_MAX_COMBINATIONS: usize = 4096;

/// The generator version a data-only seed records (what clients of the compatible protocol
/// expect; a format value, not a limit).
pub const SEED_GENERATOR_VERSION: (u32, u32, u32) = (0, 6, 8);

/// Hint cost, percent of a slot's locations, that a data-only seed starts with (design value).
pub const SEED_HINT_COST_PCT: u8 = 10;

/// Hint points a data-only seed gives per checked location (design value).
pub const SEED_CHECK_POINTS: u32 = 1;

/// Most players one room's fill takes (a design value; player numbers are 16 bits).
pub const ROOM_MAX_PLAYERS: usize = 1024;
