//! The world as an author wrote it, independent of the file syntax (design doc 16 sections 3
//! and 4, doc 17 section 5): what a front end ([`crate::front`]) produces from the source text
//! and the checks ([`crate::build_model`]) read. Every name and value keeps where it was
//! written, so an error can name the line and column. A front end only reads the syntax: it
//! checks nothing a model can say wrongly (names, rules, the format rules), so every front end
//! gets the same checks.

/// A place in the source: the file (an index into [`World::files`]), line and column, both
/// from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    /// The file, by index.
    pub file: u32,
    /// Line, from 1.
    pub line: usize,
    /// Column in characters, from 1.
    pub col: usize,
}

impl Pos {
    /// A place.
    #[must_use]
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { file: 0, line, col }
    }

    /// A place in file `file`.
    #[must_use]
    pub fn in_file(file: u32, line: usize, col: usize) -> Pos {
        Pos { file, line, col }
    }

    /// `n` characters further along the same line.
    #[must_use]
    pub fn right(self, n: usize) -> Pos {
        Pos { file: self.file, line: self.line, col: self.col + n }
    }
}

/// A value and where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct At<T> {
    /// The value.
    pub v: T,
    /// Where it starts.
    pub pos: Pos,
}

impl<T> At<T> {
    /// `v` written at `pos`.
    pub fn new(v: T, pos: Pos) -> Self {
        At { v, pos }
    }
}

/// A rule as written: a constant, or rule-language text (doc 16 section 5) whose first
/// character is at the [`At`]'s place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `true` or `false`.
    Const(bool),
    /// A rule string, parsed when the world is built.
    Text(String),
    /// A rule a front end has parsed already, with the place of every node.
    Parsed(crate::rule::Expr),
}

/// A rule and where its text starts.
pub type RuleAt = At<Rule>;

/// A plain value: an option's default, for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    /// `true` or `false`.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A name.
    Name(String),
}

/// A whole world file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct World {
    /// The files the world was read from, by [`Pos::file`]; empty for a model built in code.
    pub files: Vec<String>,
    /// The format line, `cgm-data/1`.
    pub format: Option<At<String>>,
    /// The game's key.
    pub game: Option<At<String>>,
    /// Raised by the author on each release (default 1).
    pub version: Option<At<i64>>,
    /// Display name (the manifest has no place for it yet).
    pub name: Option<At<String>>,
    /// Extensions the world needs; v1 defines none.
    pub requires: Vec<At<String>>,
    /// The start region.
    pub start: Option<At<String>>,
    /// Options, in file order.
    pub options: Vec<Opt>,
    /// Items, in file order (item ids follow it).
    pub items: Vec<Item>,
    /// `pool.fill_with`: item and weight.
    pub fill_with: Vec<(At<String>, At<i64>)>,
    /// Items held from the start.
    pub start_items: Vec<Grant>,
    /// Things logic treats as held that are never placed.
    pub assumed: Vec<Grant>,
    /// Named rules.
    pub defs: Vec<Def>,
    /// Named item lists that `group_count` reads.
    pub item_groups: Vec<(At<String>, Vec<At<String>>)>,
    /// Placement sets: name and the items confined to its checks.
    pub placement: Vec<(At<String>, Vec<At<String>>)>,
    /// Regions, in file order (check ids follow it).
    pub regions: Vec<Region>,
    /// The goal.
    pub goal: Option<RuleAt>,
    /// Shuffles.
    pub shuffles: Vec<Shuffle>,
    /// Fields the file uses that this model does not carry (`rolls`, `contexts`, ...): each is
    /// reported as not supported yet.
    pub later: Vec<At<String>>,
}

/// A named rule, with parameters when it is called with arguments (doc 18 section 2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    /// Its name.
    pub name: At<String>,
    /// Its parameters, in order; none for a plain def.
    pub params: Vec<At<String>>,
    /// Its rule.
    pub rule: RuleAt,
}

impl Def {
    /// A def without parameters.
    #[must_use]
    pub fn plain(name: At<String>, rule: RuleAt) -> Self {
        Def { name, params: Vec::new(), rule }
    }
}

/// An option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opt {
    /// Its key.
    pub key: At<String>,
    /// `toggle`, `choice` or `range`.
    pub ty: At<String>,
    /// A choice's values, in order.
    pub values: Vec<At<String>>,
    /// A range's lowest value.
    pub min: Option<At<i64>>,
    /// A range's highest value.
    pub max: Option<At<i64>>,
    /// Its default.
    pub default: Option<At<Lit>>,
}

/// An item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Its key: the name rules use.
    pub key: At<String>,
    /// Its permanent number from the ledger, when it has one; separate from the name, so a
    /// rename keeps it (doc 16 section 3.4).
    pub id: Option<At<i64>>,
    /// `progression`, `useful`, `filler` or `trap`.
    pub class: Option<At<String>>,
    /// Copies in the pool (default 1, or 0 for an item in `fill_with`).
    pub count: Option<At<i64>>,
    /// Copies as an options-only number expression, settled per seed (instead of `count`).
    pub count_rule: Option<RuleAt>,
    /// Fields not carried (a class table, an option-read count).
    pub later: Vec<At<String>>,
}

/// A start item or an assumed one: `n` copies of `item` when the options-only rule holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The item.
    pub item: At<String>,
    /// Copies (default 1).
    pub n: Option<At<i64>>,
    /// Options-only condition (default true).
    pub when: Option<RuleAt>,
}

/// A region and everything about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Its key.
    pub key: At<String>,
    /// Its checks, in file order.
    pub checks: Vec<Check>,
    /// Its exits.
    pub exits: Vec<Exit>,
    /// Events: name and rule.
    pub events: Vec<(At<String>, RuleAt)>,
    /// Options-only switch for everything in it.
    pub when: Option<RuleAt>,
    /// The placement set every check here joins.
    pub placement: Option<At<String>>,
    /// The player can wait here (meaningful with a time context).
    pub can_stay: bool,
    /// Fields not carried (`stays`).
    pub later: Vec<At<String>>,
}

/// A check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Its key: the name rules use. A check a front end generates from a family or a table
    /// carries the place of the declaration it came from.
    pub key: At<String>,
    /// Its permanent number from the ledger, when it has one; separate from the name.
    pub id: Option<At<i64>>,
    /// What reaching it takes.
    pub rule: RuleAt,
    /// Options-only: the check exists this seed.
    pub when: Option<RuleAt>,
    /// The item here in the unrandomized game.
    pub vanilla: Option<At<String>>,
    /// An item always placed here.
    pub holds: Option<At<String>>,
    /// Options-only: may not hold progression.
    pub no_progression: Option<RuleAt>,
    /// Its placement set.
    pub placement: Option<At<String>>,
    /// Fields not carried.
    pub later: Vec<At<String>>,
}

/// A one-way exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// The region it leads to.
    pub to: At<String>,
    /// What using it takes.
    pub rule: RuleAt,
    /// The door it is one side of (doc 17 section 5.5).
    pub door: Option<At<String>>,
    /// Its group, for shuffles.
    pub group: Option<At<String>>,
}

/// What a shuffle's slots or things are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Members {
    /// `{exits: [...]}`: exits by group, or by key (`region:to`).
    Exits(Vec<At<String>>),
    /// Free names the shuffle lists itself.
    Names(Vec<Thing>),
}

/// One thing (or free-name slot) of a shuffle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thing {
    /// Its name.
    pub name: At<String>,
    /// The rule a slot that gets it reads.
    pub rule: Option<RuleAt>,
    /// Its group.
    pub group: Option<At<String>>,
    /// Fields not carried (`count`, `min`, `to`, ...).
    pub later: Vec<At<String>>,
}

/// A permission row's form (doc 17 section 5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `{slots: S, only: T}`
    Only,
    /// `{slots: S, deny: T}`
    Deny,
    /// `{things: T, only_in: S}`
    OnlyIn,
    /// `{things: T, deny_in: S}`
    DenyIn,
}

/// A permission row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Its form, where the row starts.
    pub verb: At<Verb>,
    /// Slots for `only` and `deny`, things for `only_in` and `deny_in`.
    pub subject: Vec<At<String>>,
    /// The other side.
    pub object: Vec<At<String>>,
    /// Fields not carried (`again`, `when`).
    pub later: Vec<At<String>>,
}

/// A shuffle (doc 17 section 5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shuffle {
    /// Its name.
    pub name: At<String>,
    /// Its slots.
    pub slots: At<Members>,
    /// Its things (an exit shuffle's are its slots).
    pub things: Option<At<Members>>,
    /// Two-sided exits assigned both ways.
    pub coupled: Option<RuleAt>,
    /// Permission rows.
    pub allow: Vec<Row>,
    /// `none` or `layout` (derived when left out).
    pub check: Option<At<String>>,
    /// Fields not carried (`when`, `pin`, `vanilla`, `pool`, `after`, `kind`, `scope`).
    pub later: Vec<At<String>>,
}
