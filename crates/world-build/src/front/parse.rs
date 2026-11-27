//! One `.rando` file to a syntax tree (design doc 18 section 2.2, with the corrections listed
//! in `docs/world-build.md`, "Grammar decisions"). Every declaration starts with a keyword, so
//! after an error the parser skips to the next one and goes on: one pass reports every error.
//! It also records where the canonical layout breaks lines, for the formatter.

use crate::lex::{CHECK_MODIFIERS, Comment, Tok, Token, lex, show};
use crate::model::{At, Lit, Pos, Verb};
use crate::rule::{E, Expr, Piece, parse_in};

/// A name that may hold `{var}` pieces (expanded by its family).
pub type Tn = At<Vec<Piece>>;

/// A table column's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    /// A name.
    Name,
    /// A whole number.
    Int,
    /// `true` or `false`.
    Bool,
    /// A rule.
    Rule,
}

/// One table cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    /// A name.
    Name(String),
    /// A number.
    Int(i64),
    /// A constant.
    Bool(bool),
    /// A rule.
    Rule(Expr),
}

/// A table: columns, and rows inline or from a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub name: At<String>,
    pub cols: Vec<(String, Col)>,
    pub rows: Vec<At<Vec<Cell>>>,
    pub from: Option<At<String>>,
}

/// What a family variable runs over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Over {
    /// Every row of a table.
    Table(At<String>),
    /// Whole numbers from and to, both included (number expressions, folded when the family
    /// expands).
    Range(Expr, Expr),
    /// Names.
    List(Vec<At<String>>),
}

/// `NAME in OVER [where rule]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binder {
    pub var: At<String>,
    pub over: Over,
    pub cond: Option<Expr>,
}

/// A check, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckD {
    pub name: Tn,
    pub rule: Option<Expr>,
    pub when: Option<Expr>,
    pub vanilla: Option<Tn>,
    pub holds: Option<Tn>,
    pub no_progression: Option<(Pos, Option<Expr>)>,
    pub placement: Option<At<String>>,
    pub key: Option<At<String>>,
    pub was: Option<At<String>>,
}

/// An exit, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitD {
    pub to: Tn,
    pub rule: Option<Expr>,
    pub door: Option<Tn>,
    pub group: Option<At<String>>,
}

/// Something inside a region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    Check(Box<CheckD>),
    Exit(ExitD),
    Event(Tn, Expr),
    For(Vec<Binder>, Vec<Member>),
}

/// A region, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionD {
    pub name: Tn,
    pub when: Option<Expr>,
    pub placement: Option<At<String>>,
    pub can_stay: bool,
    pub members: Vec<Member>,
}

/// An item, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemD {
    pub name: Tn,
    pub class: At<String>,
    pub count: Option<Expr>,
    pub when: Option<Expr>,
    pub key: Option<At<String>>,
    pub was: Option<At<String>>,
}

/// One line of a shuffle block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShufItem {
    /// `slots exits(a, b)` or `things exits(a, b)`: `true` for slots.
    Exits(bool, Pos, Vec<At<String>>),
    /// `slots a, b`: free names.
    Names(Pos, Vec<At<String>>),
    /// `thing NAME [: rule] [group NAME]`.
    Thing(At<String>, Option<Expr>, Option<At<String>>),
    /// A permission row.
    Row(At<Verb>, Vec<At<String>>, Vec<At<String>>, Vec<At<String>>),
    /// `coupled [rule]`.
    Coupled(Pos, Option<Expr>),
    /// `check none|layout`.
    Check(At<String>),
    /// A field the engine's first slice does not have (`when`, `kind`, ...).
    Later(At<String>),
}

/// What an overlay targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Region(At<String>),
    Check(At<String>, At<String>),
    Exit(At<String>, At<String>),
    Def(At<String>),
    Item(At<String>),
}

/// An overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    Extend(Pos, Target, Vec<Member>),
    Replace(Pos, Target, Expr),
    Remove(Pos, Target),
}

/// One top-level declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    World(At<String>, Option<At<i64>>),
    Start(At<String>),
    Option {
        name: At<String>,
        ty: At<String>,
        values: Vec<At<String>>,
        range: Option<(At<i64>, At<i64>)>,
        default: Option<At<Lit>>,
    },
    Item(ItemD),
    Pool(Vec<(At<String>, Expr)>),
    Grant {
        assumed: bool,
        item: Tn,
        n: Option<Expr>,
        when: Option<Expr>,
    },
    Def {
        name: At<String>,
        params: Vec<At<String>>,
        rule: Expr,
    },
    Group(Tn, Vec<Tn>),
    /// A placement set, and the declarations of its block when it has one: every region in
    /// the block joins the set.
    Placement(Tn, Vec<Tn>, Vec<Decl>),
    Region(RegionD),
    Table(Table),
    For(Vec<Binder>, Vec<Decl>),
    Shuffle(At<String>, Vec<ShufItem>),
    Goal(Expr),
    Overlay(Overlay),
    /// A declaration this version does not build (`score`, `steps`, `trick`, `require`).
    Later(At<String>),
}

/// The `generated` block of a file a tool wrote (doc 18 section 2.9).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Generated {
    pub pos: Pos,
    pub tool: Option<String>,
    pub sources: Vec<(At<String>, String)>,
    pub content: Option<At<String>>,
    pub expect: Vec<(At<String>, i64)>,
    /// Index of the first token after the block.
    pub end: usize,
}

/// Where the canonical layout of a file puts line breaks and parentheses.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Per token: a line break before it, at this indent.
    pub brk: Vec<Option<usize>>,
    /// Per token: written right after the token before, no space (name pieces).
    pub glue: Vec<bool>,
    /// Token ranges `fmt` wraps in parentheses (`and` inside `or`).
    pub parens: Vec<(usize, usize)>,
    /// `and` and `or` nodes `fmt` may break over lines: first token, one past the last, the
    /// tokens to break before when the node is too long for its line (its operators), and how
    /// many such nodes hold it.
    pub wraps: Vec<(usize, usize, Vec<usize>, usize)>,
}

/// A parsed file.
#[derive(Debug, Clone, Default)]
pub struct FileAst {
    pub toks: Vec<Token>,
    pub comments: Vec<Comment>,
    pub generated: Option<Generated>,
    pub uses: Vec<At<String>>,
    pub decls: Vec<Decl>,
    pub layout: Layout,
    pub errors: Vec<(Pos, String)>,
}

const DECLS: [&str; 22] = [
    "world",
    "start",
    "option",
    "item",
    "pool",
    "start_item",
    "assumed",
    "def",
    "group",
    "score",
    "steps",
    "table",
    "for",
    "placement",
    "region",
    "shuffle",
    "require",
    "trick",
    "goal",
    "extend",
    "replace",
    "remove",
];

const MEMBERS: [&str; 5] = ["check", "exit", "event", "stay", "for"];

type R<T> = Result<T, (Pos, String)>;

/// Parse `src`, file number `file` of its world, every error collected.
#[must_use]
pub fn parse(src: &str, file: u32) -> FileAst {
    let (toks, comments) = match lex(src, Pos::in_file(file, 1, 1)) {
        Ok(x) => x,
        Err(e) => return FileAst { errors: vec![e], ..FileAst::default() },
    };
    let n = toks.len();
    let mut p = P {
        toks: &toks,
        k: 0,
        errors: Vec::new(),
        layout: Layout {
            brk: vec![None; n],
            glue: vec![false; n],
            parens: Vec::new(),
            wraps: Vec::new(),
        },
    };
    let generated = p.generated();
    let mut uses = Vec::new();
    let mut decls = Vec::new();
    while p.k < n {
        let start = p.k;
        p.brk(start, 0);
        let out = if p.word("use") {
            p.use_line().map(|u| uses.push(u))
        } else {
            p.decl(0).map(|d| decls.push(d))
        };
        if let Err(e) = out {
            p.errors.push(e);
            p.skip_to_decl(start);
        }
    }
    let (layout, errors) = (p.layout, p.errors);
    FileAst { toks, comments, generated, uses, decls, layout, errors }
}

struct P<'t> {
    toks: &'t [Token],
    k: usize,
    errors: Vec<(Pos, String)>,
    layout: Layout,
}

fn is_decl(t: &Tok) -> bool {
    matches!(t, Tok::Kw(k) if DECLS.contains(k) || *k == "use")
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.k).map(|t| &t.t)
    }

    fn here(&self) -> Pos {
        match self.toks.get(self.k) {
            Some(t) => t.pos,
            None => self.toks.last().map_or(Pos::default(), |t| t.pos.right(t.len)),
        }
    }

    fn word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Kw(k)) if *k == w)
    }

    /// A contextual word: a name that means something only here (`toggle`, `from`, `x`).
    fn ctx(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Name(n)) if n == w)
    }

    fn brk(&mut self, at: usize, depth: usize) {
        if let Some(b) = self.layout.brk.get_mut(at) {
            *b = Some(depth);
        }
    }

    fn got(&self) -> String {
        self.peek().map_or_else(|| "the end of the file".to_owned(), show)
    }

    fn expect(&mut self, t: &Tok, what: &str) -> R<Pos> {
        if self.peek() == Some(t) {
            self.k += 1;
            return Ok(self.toks[self.k - 1].pos);
        }
        Err((self.here(), format!("expected {what}, found {}", self.got())))
    }

    fn expect_word(&mut self, w: &str) -> R<Pos> {
        if self.ctx(w) || self.word(w) {
            self.k += 1;
            return Ok(self.toks[self.k - 1].pos);
        }
        Err((self.here(), format!("expected `{w}`, found {}", self.got())))
    }

    fn name(&mut self, what: &str) -> R<At<String>> {
        match self.peek().cloned() {
            Some(Tok::Name(n)) => {
                let pos = self.here();
                self.k += 1;
                Ok(At::new(n, pos))
            }
            Some(Tok::Kw(k)) => Err((
                self.here(),
                format!("`{k}` is a reserved word; write `` `{k}` `` to use it as {what}"),
            )),
            _ => Err((self.here(), format!("expected {what}, found {}", self.got()))),
        }
    }

    /// A whole number, with a `-` before it read as its sign: where a number is expected, a
    /// `-` cannot be subtraction (the lexer reads `-` after a name or number as `-`).
    fn int(&mut self, what: &str) -> R<At<i64>> {
        let pos = self.here();
        match (self.peek(), self.toks.get(self.k + 1).map(|t| &t.t)) {
            (Some(Tok::Int(n)), _) => {
                let n = *n;
                self.k += 1;
                Ok(At::new(n, pos))
            }
            // The sign is glued to a number written without one (`-3`, not `- 3` or `- -3`).
            (Some(Tok::Minus), Some(Tok::Int(n)))
                if *n >= 0
                    && self
                        .toks
                        .get(self.k + 1)
                        .is_some_and(|t| t.pos.line == pos.line && t.pos.col == pos.col + 1) =>
            {
                let n = -*n;
                // Written glued to its sign, as the lexer's own negative numbers are.
                if let Some(g) = self.layout.glue.get_mut(self.k + 1) {
                    *g = true;
                }
                self.k += 2;
                Ok(At::new(n, pos))
            }
            _ => Err((self.here(), format!("expected {what}, found {}", self.got()))),
        }
    }

    /// A number: a whole number with its sign, or a number expression (`d.keys - 1`,
    /// `opt.k + 1`, `2 * n`), folded where it is used. A rule here is an error.
    fn num(&mut self, depth: usize, what: &str) -> R<Expr> {
        let from = self.k;
        if matches!(self.peek(), Some(Tok::Minus)) {
            let n = self.int(what)?;
            return Ok(Expr { e: E::Int(n.v), at: n.pos, paren: false, span: (from, self.k) });
        }
        let pos = self.here();
        if !matches!(self.peek(), Some(Tok::Int(_) | Tok::Name(_) | Tok::Open)) {
            return Err((pos, format!("expected {what}, found {}", self.got())));
        }
        let e = self.rule(depth)?;
        if matches!(
            e.e,
            E::Bool(_) | E::Not(_) | E::And(_) | E::Or(_) | E::Cmp(..) | E::In(..) | E::Match(..)
        ) {
            return Err((pos, format!("expected {what}, found a rule")));
        }
        Ok(e)
    }

    fn string(&mut self, what: &str) -> R<At<String>> {
        match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                let pos = self.here();
                self.k += 1;
                Ok(At::new(s, pos))
            }
            _ => Err((self.here(), format!("expected {what} in quotes, found {}", self.got()))),
        }
    }

    /// Names separated by commas.
    fn names(&mut self, what: &str) -> R<Vec<At<String>>> {
        let mut out = vec![self.name(what)?];
        while self.peek() == Some(&Tok::Comma) {
            self.k += 1;
            out.push(self.name(what)?);
        }
        Ok(out)
    }

    /// A rule here: parsed, its `and` inside `or` noted for `fmt`, its `match` arms laid out.
    fn rule(&mut self, depth: usize) -> R<Expr> {
        let (e, end) = parse_in(self.toks, self.k, self.toks.len())?;
        self.k = end;
        self.lay_rule(&e, depth);
        self.lay_wrap(&e);
        Ok(e)
    }

    /// Where `fmt` may break `e` when it is too long for one line: before the `or`s of each
    /// `or`, and the `and`s of each `and`, outer ones first, each with how many `and`/`or`
    /// nodes hold it. A rule with a `match` (laid out over lines already) is left alone.
    fn lay_wrap(&mut self, e: &Expr) {
        let mut todo = vec![e];
        while let Some(x) = todo.pop() {
            if matches!(x.e, E::Match(..)) {
                return;
            }
            todo.extend(x.children());
        }
        let mut todo = vec![(e, 0usize)];
        while let Some((x, level)) = todo.pop() {
            let inner = match &x.e {
                E::Or(parts) | E::And(parts) => {
                    // A part in parentheses starts at its `(`; the operator is just before.
                    let at: Vec<usize> = parts.iter().skip(1).map(|p| p.span.0 - 1).collect();
                    let first = if x.paren { x.span.0 + 1 } else { x.span.0 };
                    self.layout.wraps.push((first, x.span.1, at, level));
                    level + 1
                }
                _ => level,
            };
            todo.extend(x.children().into_iter().map(|c| (c, inner)));
        }
    }

    /// A rule that must end on this line (a table cell).
    fn rule_on_line(&mut self, depth: usize) -> R<Expr> {
        let line_end =
            (self.k + 1..self.toks.len()).find(|i| self.toks[*i].nl).unwrap_or(self.toks.len());
        let (e, end) = parse_in(self.toks, self.k, line_end)?;
        self.k = end;
        self.lay_rule(&e, depth);
        Ok(e)
    }

    fn lay_rule(&mut self, e: &Expr, depth: usize) {
        let mut todo = vec![(e, depth)];
        while let Some((x, d)) = todo.pop() {
            match &x.e {
                E::Or(parts) => {
                    for p in parts.iter().filter(|p| matches!(p.e, E::And(_)) && !p.paren) {
                        self.layout.parens.push(p.span);
                    }
                }
                E::Interp(_) => {
                    for i in x.span.0 + 1..x.span.1 {
                        if let Some(g) = self.layout.glue.get_mut(i) {
                            *g = true;
                        }
                    }
                }
                E::Match(_, arms) => {
                    arms.iter().for_each(|a| self.brk(a.start, d + 1));
                    self.brk(x.span.1 - 1, d);
                }
                _ => {}
            }
            let inner = if matches!(x.e, E::Match(..)) { d + 1 } else { d };
            todo.extend(x.children().into_iter().map(|c| (c, inner)));
        }
    }

    /// The tokens after a failed declaration, up to the next one.
    fn skip_to_decl(&mut self, start: usize) {
        // Braces are counted from where the error stopped, not from the declaration's start:
        // a block left open by the error must not hide the declarations after it.
        self.k = self.k.max(start + 1);
        let mut depth = 0i32;
        while let Some(t) = self.peek() {
            match t {
                Tok::LBrace => depth += 1,
                Tok::RBrace => depth -= 1,
                t if depth <= 0 && is_decl(t) => return,
                _ => {}
            }
            self.k += 1;
        }
    }

    /// The tokens after a failed member, up to the next member or the block's end.
    fn skip_to_member(&mut self, start: usize) {
        self.k = self.k.max(start + 1);
        let mut depth = 0i32;
        while let Some(t) = self.peek() {
            match t {
                Tok::LBrace => depth += 1,
                Tok::RBrace if depth == 0 => return,
                Tok::RBrace => depth -= 1,
                Tok::Kw(k) if depth == 0 && (MEMBERS.contains(k) || DECLS.contains(k)) => return,
                _ => {}
            }
            self.k += 1;
        }
    }

    fn generated(&mut self) -> Option<Generated> {
        if !self.word("generated") {
            return None;
        }
        let pos = self.here();
        self.brk(0, 0);
        self.k += 1;
        let mut g = Generated { pos, ..Generated::default() };
        let mut body = || -> R<()> {
            self.expect(&Tok::LBrace, "`{`")?;
            while self.peek() != Some(&Tok::RBrace) {
                self.brk(self.k, 1);
                if self.ctx("tool") {
                    self.k += 1;
                    g.tool = Some(self.string("the tool's name")?.v);
                } else if self.ctx("source") {
                    self.k += 1;
                    let path = self.string("the source's path")?;
                    self.expect_word("sha256")?;
                    g.sources.push((path, self.string("its SHA-256")?.v));
                } else if self.ctx("content") {
                    self.k += 1;
                    self.expect_word("sha256")?;
                    g.content = Some(self.string("the content's SHA-256")?);
                } else if self.ctx("expect") {
                    self.k += 1;
                    let n = self.name("a table")?;
                    self.expect(&Tok::Cmp(crate::rule::Cmp::Ge), "`>=`")?;
                    g.expect.push((n, self.int("a row count")?.v));
                } else {
                    return Err((
                        self.here(),
                        format!(
                            "expected `tool`, `source`, `content` or `expect`, found {}",
                            self.got()
                        ),
                    ));
                }
            }
            self.brk(self.k, 0);
            self.k += 1;
            Ok(())
        };
        if let Err(e) = body() {
            self.errors.push(e);
            self.skip_to_decl(self.k);
        }
        g.end = self.k;
        Some(g)
    }

    /// `use a/b`.
    fn use_line(&mut self) -> R<At<String>> {
        self.k += 1;
        let first = self.name("a module path")?;
        let mut path = first.v;
        while self.peek() == Some(&Tok::Slash) {
            self.layout.glue[self.k] = true;
            self.k += 1;
            self.layout.glue[self.k] = true;
            path = format!("{path}/{}", self.name("a module path")?.v);
        }
        Ok(At::new(path, first.pos))
    }

    /// A name that may hold `{var}` pieces.
    fn tname(&mut self, what: &str) -> R<Tn> {
        let (start, pos) = (self.k, self.here());
        let (pieces, end) = crate::rule::pieces_at(self.toks, self.k, self.toks.len());
        if pieces.is_empty() {
            self.name(what)?;
            return Err((pos, format!("expected {what}")));
        }
        self.k = end;
        for i in start + 1..end {
            self.layout.glue[i] = true;
        }
        Ok(At::new(pieces, pos))
    }

    fn deep(&self, depth: usize) -> R<()> {
        if depth > config::WORLD_BLOCK_MAX_DEPTH {
            return Err((
                self.here(),
                format!("blocks nest more than {} deep", config::WORLD_BLOCK_MAX_DEPTH),
            ));
        }
        Ok(())
    }

    fn decl(&mut self, depth: usize) -> R<Decl> {
        let pos = self.here();
        self.deep(depth)?;
        let Some(Tok::Kw(kw)) = self.peek().cloned() else {
            return Err((
                pos,
                format!(
                    "expected a declaration (a keyword such as `item` or `region`), found {}",
                    self.got()
                ),
            ));
        };
        self.k += 1;
        Ok(match kw {
            "world" => {
                let name = self.name("the game's key")?;
                if matches!(self.peek(), Some(Tok::Str(_))) {
                    self.k += 1;
                }
                let version = if self.ctx("version") {
                    self.k += 1;
                    Some(self.int("the version")?)
                } else {
                    None
                };
                Decl::World(name, version)
            }
            "start" => Decl::Start(self.name("the start region")?),
            "option" => self.option()?,
            "item" => Decl::Item(self.item(depth)?),
            "pool" => {
                self.expect_word("fill")?;
                self.expect(&Tok::LBrace, "`{`")?;
                let mut fill = Vec::new();
                loop {
                    let n = self.name("an item")?;
                    fill.push((n, self.num(depth, "its weight")?));
                    if self.peek() != Some(&Tok::Comma) {
                        break;
                    }
                    self.k += 1;
                }
                self.expect(&Tok::RBrace, "`}`")?;
                Decl::Pool(fill)
            }
            "start_item" | "assumed" => {
                let item = self.tname("an item")?;
                let n = if self.ctx("x") {
                    self.k += 1;
                    Some(self.num(depth, "a number of copies")?)
                } else {
                    None
                };
                let when = if self.word("when") {
                    self.k += 1;
                    Some(self.rule(depth)?)
                } else {
                    None
                };
                Decl::Grant { assumed: kw == "assumed", item, n, when }
            }
            "def" => {
                let name = self.name("the def's name")?;
                let mut params = Vec::new();
                if self.peek() == Some(&Tok::Open) {
                    self.k += 1;
                    params = self.names("a parameter")?;
                    self.expect(&Tok::Close, "`)`")?;
                }
                self.expect(&Tok::Eq, "`=`")?;
                Decl::Def { name, params, rule: self.rule(depth)? }
            }
            "group" => {
                let name = self.tname("the group's name")?;
                self.expect(&Tok::Eq, "`=`")?;
                Decl::Group(name, self.tname_set("an item")?)
            }
            "placement" => {
                let name = self.tname("the placement set's name")?;
                let items = self.tname_set("an item")?;
                let body = if self.peek() == Some(&Tok::LBrace) {
                    self.k += 1;
                    self.block(depth)?
                } else {
                    Vec::new()
                };
                Decl::Placement(name, items, body)
            }
            "region" => Decl::Region(self.region(depth)?),
            "table" => Decl::Table(self.table(depth)?),
            "for" => {
                let binders = self.binders(depth)?;
                self.expect(&Tok::LBrace, "`{`")?;
                Decl::For(binders, self.block(depth)?)
            }
            "shuffle" => self.shuffle(depth)?,
            "goal" => Decl::Goal(self.rule(depth)?),
            "extend" | "replace" | "remove" => Decl::Overlay(self.overlay(kw, pos, depth)?),
            other @ ("score" | "steps" | "trick" | "require") => {
                // A declaration of doc 18 this version does not build: skipped whole.
                let start = self.k - 1;
                self.skip_to_decl(start);
                Decl::Later(At::new(other.to_owned(), pos))
            }
            "check" | "exit" | "event" | "stay" => {
                return Err((
                    pos,
                    format!("`{kw}` belongs inside a region: `region NAME {{ {kw} ... }}`"),
                ));
            }
            other => return Err((pos, format!("`{other}` cannot start a declaration here"))),
        })
    }

    /// The declarations of a family's or a placement set's block, after its `{`, up to and
    /// including its `}`: items, regions, start and assumed items, groups, placement sets and
    /// families.
    fn block(&mut self, depth: usize) -> R<Vec<Decl>> {
        let mut body = Vec::new();
        while !matches!(self.peek(), Some(Tok::RBrace) | None) {
            let start = self.k;
            self.brk(start, depth + 1);
            let inside = matches!(
                self.peek(),
                Some(Tok::Kw(
                    "item" | "region" | "start_item" | "assumed" | "for" | "group" | "placement"
                ))
            );
            if !inside {
                self.errors.push((self.here(), format!("{} cannot be inside a family or a placement block; they hold items, regions, start items, groups, placement sets and families", self.got())));
                self.skip_to_member(start);
                continue;
            }
            match self.decl(depth + 1) {
                Ok(d) => body.push(d),
                Err(e) => {
                    self.errors.push(e);
                    self.skip_to_member(start);
                }
            }
        }
        self.brk(self.k, depth);
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(body)
    }

    /// `{ a, b }` of names that may hold `{var}` pieces.
    fn tname_set(&mut self, what: &str) -> R<Vec<Tn>> {
        self.expect(&Tok::LBrace, "`{`")?;
        let mut out = vec![self.tname(what)?];
        while self.peek() == Some(&Tok::Comma) {
            self.k += 1;
            out.push(self.tname(what)?);
        }
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(out)
    }

    fn option(&mut self) -> R<Decl> {
        let name = self.name("the option's name")?;
        self.expect(&Tok::Colon, "`:` and the option's type")?;
        let ty_pos = self.here();
        let (ty, mut values, mut range) = (self.name("toggle, range or choice")?, Vec::new(), None);
        match ty.v.as_str() {
            "range" => {
                let lo = self.int("the lowest value")?;
                self.expect(&Tok::DotDot, "`..`")?;
                range = Some((lo, self.int("the highest value")?));
            }
            "choice" => {
                self.expect(&Tok::Open, "`(`")?;
                values = self.names("a value")?;
                self.expect(&Tok::Close, "`)`")?;
            }
            _ => {}
        }
        let default = if self.peek() == Some(&Tok::Eq) {
            self.k += 1;
            let pos = self.here();
            let lit = match self.peek().cloned() {
                Some(Tok::Int(n)) => Lit::Int(n),
                Some(Tok::Kw("true")) => Lit::Bool(true),
                Some(Tok::Kw("false")) => Lit::Bool(false),
                Some(Tok::Name(n)) if ty.v == "toggle" && (n == "on" || n == "off") => {
                    Lit::Bool(n == "on")
                }
                Some(Tok::Name(n)) => Lit::Name(n),
                _ => {
                    return Err((pos, format!("expected the default value, found {}", self.got())));
                }
            };
            self.k += 1;
            Some(At::new(lit, pos))
        } else {
            None
        };
        // Player-facing text: read and kept for later (the manifest has no place for it).
        if matches!(self.peek(), Some(Tok::Str(_))) {
            self.k += 1;
        }
        Ok(Decl::Option { name, ty: At::new(ty.v, ty_pos), values, range, default })
    }

    fn ident(&mut self, key: &mut Option<At<String>>, was: &mut Option<At<String>>) -> R<bool> {
        if self.ctx("key") {
            self.k += 1;
            *key = Some(self.string("the ledger key (`key` always starts one here)")?);
        } else if self.ctx("was") {
            self.k += 1;
            *was = Some(self.name("the old name")?);
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn item(&mut self, depth: usize) -> R<ItemD> {
        let name = self.tname("the item's name")?;
        let class = if self.word("match") {
            let pos = self.here();
            self.rule(depth)?;
            At::new("match".to_owned(), pos)
        } else if matches!(self.peek(), Some(Tok::Name(_))) {
            self.name("its class")?
        } else {
            return Err((
                self.here(),
                "an item needs a class: progression, useful, filler or trap".into(),
            ));
        };
        let mut d = ItemD { name, class, count: None, when: None, key: None, was: None };
        loop {
            if self.ctx("x") {
                self.k += 1;
                d.count = Some(self.num(depth, "a number of copies")?);
            } else if self.word("when") {
                self.k += 1;
                d.when = Some(self.rule(depth)?);
            } else if !self.ident(&mut d.key, &mut d.was)? {
                return Ok(d);
            }
        }
    }

    /// Whether a rule starts here (and not a keyword of the line it is on).
    fn rule_starts(&self) -> bool {
        match self.peek() {
            Some(Tok::Kw(k)) => matches!(*k, "not" | "true" | "false" | "match"),
            Some(Tok::RBrace) | None => false,
            Some(_) => true,
        }
    }

    fn region(&mut self, depth: usize) -> R<RegionD> {
        let name = self.tname("the region's name")?;
        let mut r =
            RegionD { name, when: None, placement: None, can_stay: false, members: Vec::new() };
        loop {
            if self.word("when") {
                self.k += 1;
                r.when = Some(self.rule(depth)?);
            } else if self.word("placement") {
                self.k += 1;
                r.placement = Some(self.name("a placement set")?);
            } else if self.ctx("can_stay") {
                self.k += 1;
                r.can_stay = true;
            } else {
                break;
            }
        }
        let open = self.expect(&Tok::LBrace, "`{` and the region's checks and exits")?;
        r.members = self.members(depth + 1, &format!("region `{}`", show_tn(&r.name)), open)?;
        Ok(r)
    }

    /// Members up to the closing `}`, which this consumes. `what` names the block, opened at
    /// `open`, for the error when it is left open.
    fn members(&mut self, depth: usize, what: &str, open: Pos) -> R<Vec<Member>> {
        let mut out = Vec::new();
        while !matches!(self.peek(), Some(Tok::RBrace) | None) {
            if matches!(self.peek(), Some(Tok::Kw(k)) if DECLS.contains(k) && *k != "for") {
                // A declaration where a member was expected: the block was left open. The
                // members read so far are kept, and reading goes on at this declaration.
                self.errors.push((
                    self.here(),
                    format!(
                        "{what}, opened at line {}, is not closed before this declaration",
                        open.line
                    ),
                ));
                return Ok(out);
            }
            let start = self.k;
            self.brk(start, depth);
            match self.member(depth) {
                Ok(m) => out.push(m),
                Err(e) => {
                    self.errors.push(e);
                    self.skip_to_member(start);
                }
            }
        }
        self.brk(self.k, depth - 1);
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(out)
    }

    fn member(&mut self, depth: usize) -> R<Member> {
        let pos = self.here();
        self.deep(depth)?;
        let Some(Tok::Kw(kw)) = self.peek().cloned() else {
            return Err((
                pos,
                format!("expected `check`, `exit`, `event` or `for`, found {}", self.got()),
            ));
        };
        self.k += 1;
        match kw {
            "check" => {
                let name = self.tname("the check's name")?;
                let rule = if self.peek() == Some(&Tok::Colon) {
                    self.k += 1;
                    Some(self.rule(depth)?)
                } else {
                    None
                };
                let mut c = CheckD {
                    name,
                    rule,
                    when: None,
                    vanilla: None,
                    holds: None,
                    no_progression: None,
                    placement: None,
                    key: None,
                    was: None,
                };
                // Each modifier once: a second would silently replace the first.
                let mut seen: Vec<&str> = Vec::new();
                loop {
                    let word = ["when", "placement"]
                        .into_iter()
                        .find(|w| self.word(w))
                        .or_else(|| CHECK_MODIFIERS.into_iter().find(|w| self.ctx(w)));
                    let Some(w) = word else { return Ok(Member::Check(Box::new(c))) };
                    if seen.contains(&w) {
                        return Err((self.here(), format!("`{w}` is given twice")));
                    }
                    seen.push(w);
                    let at = self.here();
                    if matches!(w, "key" | "was") {
                        self.ident(&mut c.key, &mut c.was)?;
                        continue;
                    }
                    self.k += 1;
                    match w {
                        "when" => c.when = Some(self.rule(depth)?),
                        "vanilla" => c.vanilla = Some(self.tname("an item")?),
                        "holds" => c.holds = Some(self.tname("an item")?),
                        "placement" => c.placement = Some(self.name("a placement set")?),
                        "no_progression" => {
                            // A modifier word after it starts the next modifier, not a rule: no
                            // def may take one of these names, and an event is state, which
                            // `no_progression` never reads.
                            let next = CHECK_MODIFIERS.iter().any(|w| self.ctx(w));
                            let r = if self.rule_starts() && !next {
                                Some(self.rule(depth)?)
                            } else {
                                None
                            };
                            c.no_progression = Some((at, r));
                        }
                        // A word added to the lists without a case here is a parse error, not
                        // a silent `no_progression`.
                        other => return Err((at, format!("`{other}` is not read here yet"))),
                    }
                }
            }
            "exit" => {
                let to = self.tname("the region it leads to")?;
                let rule = if self.peek() == Some(&Tok::Colon) {
                    self.k += 1;
                    Some(self.rule(depth)?)
                } else {
                    None
                };
                let mut x = ExitD { to, rule, door: None, group: None };
                loop {
                    if self.ctx("door") {
                        self.k += 1;
                        x.door = Some(self.tname("a door")?);
                    } else if self.word("group") {
                        self.k += 1;
                        x.group = Some(self.name("a group")?);
                    } else {
                        return Ok(Member::Exit(x));
                    }
                }
            }
            "event" => {
                let name = self.tname("the event's name")?;
                self.expect(&Tok::Colon, "`:` and the event's rule")?;
                Ok(Member::Event(name, self.rule(depth)?))
            }
            "for" => {
                let binders = self.binders(depth)?;
                let open = self.expect(&Tok::LBrace, "`{`")?;
                Ok(Member::For(binders, self.members(depth + 1, "a family's block", open)?))
            }
            "stay" => {
                Err((pos, "`stay` needs a time context, which this version does not build".into()))
            }
            k => Err((
                pos,
                format!(
                    "`{k}` is not something a region holds; expected `check`, `exit`, `event` or `for`"
                ),
            )),
        }
    }

    fn binders(&mut self, depth: usize) -> R<Vec<Binder>> {
        let mut out = Vec::new();
        loop {
            let var = self.name("a family variable")?;
            if !self.word("in") {
                return Err((self.here(), format!("expected `in`, found {}", self.got())));
            }
            self.k += 1;
            // A table is one name alone; anything else before `..` is a range's first number.
            let table = matches!(self.peek(), Some(Tok::Name(_)))
                && !matches!(
                    self.toks.get(self.k + 1).map(|t| &t.t),
                    Some(Tok::DotDot | Tok::Plus | Tok::Minus | Tok::Star | Tok::Kw("div" | "mod"))
                );
            let over = match self.peek().cloned() {
                Some(_) if !table && !matches!(self.peek(), Some(Tok::LBrack)) => {
                    let lo = self.num(depth, "the first number")?;
                    self.expect(&Tok::DotDot, "`..`")?;
                    Over::Range(lo, self.num(depth, "the last number")?)
                }
                Some(Tok::LBrack) => {
                    self.k += 1;
                    let list = self.names("a name")?;
                    self.expect(&Tok::RBrack, "`]`")?;
                    Over::List(list)
                }
                _ => {
                    Over::Table(self.name("a table, a range like `1..8`, or a list like `[a, b]`")?)
                }
            };
            let cond = if self.ctx("where") {
                self.k += 1;
                Some(self.rule(depth)?)
            } else {
                None
            };
            out.push(Binder { var, over, cond });
            if self.peek() != Some(&Tok::Comma) {
                return Ok(out);
            }
            self.k += 1;
        }
    }

    fn table(&mut self, depth: usize) -> R<Table> {
        let name = self.name("the table's name")?;
        self.expect(&Tok::Open, "`(` and the columns")?;
        let mut cols = Vec::new();
        loop {
            let c = self.name("a column")?;
            self.expect(&Tok::Colon, "`:` and the column's type")?;
            let ty = self.name("name, int, bool or rule")?;
            let ty = match ty.v.as_str() {
                "name" => Col::Name,
                "int" => Col::Int,
                "bool" => Col::Bool,
                "rule" => Col::Rule,
                t => {
                    return Err((
                        ty.pos,
                        format!("`{t}` is not a column type (name, int, bool, rule)"),
                    ));
                }
            };
            cols.push((c.v, ty));
            if self.peek() != Some(&Tok::Comma) {
                break;
            }
            self.k += 1;
        }
        self.expect(&Tok::Close, "`)`")?;
        let mut t = Table { name, cols, rows: Vec::new(), from: None };
        if self.ctx("from") {
            self.k += 1;
            t.from = Some(self.string("the table's file")?);
            return Ok(t);
        }
        self.expect(&Tok::LBrace, "`{` and the rows, one per line")?;
        while !matches!(self.peek(), Some(Tok::RBrace) | None) {
            let start = self.k;
            self.brk(start, depth + 1);
            match self.row(&t.cols, depth + 1) {
                Ok(r) => t.rows.push(r),
                Err(e) => {
                    self.errors.push(e);
                    self.k = (start + 1..self.toks.len())
                        .find(|i| self.toks[*i].nl)
                        .unwrap_or(self.toks.len());
                }
            }
        }
        self.brk(self.k, depth);
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(t)
    }

    /// One table row: one cell per column, all on one line.
    fn row(&mut self, cols: &[(String, Col)], depth: usize) -> R<At<Vec<Cell>>> {
        let pos = self.here();
        let line = pos.line;
        let mut cells = Vec::new();
        for (name, ty) in cols {
            if self.toks.get(self.k).is_none_or(|t| t.pos.line != line || t.t == Tok::RBrace) {
                let end = self.toks.get(self.k - 1).map_or(pos, |t| t.pos.right(t.len));
                return Err((
                    end,
                    format!(
                        "the row ends before column `{name}`; a row is one line with a value per column"
                    ),
                ));
            }
            let cell_pos = self.here();
            cells.push(match (ty, self.peek().cloned()) {
                (Col::Name, Some(Tok::Name(n))) => {
                    self.k += 1;
                    Cell::Name(n)
                }
                (Col::Int, Some(Tok::Int(_) | Tok::Minus)) => {
                    Cell::Int(self.int("a whole number")?.v)
                }
                (Col::Bool, Some(Tok::Kw(b @ ("true" | "false")))) => {
                    self.k += 1;
                    Cell::Bool(b == "true")
                }
                (Col::Rule, _) => Cell::Rule(self.rule_on_line(depth)?),
                _ => {
                    return Err((
                        cell_pos,
                        format!("column `{name}` takes {}, found {}", col_word(*ty), self.got()),
                    ));
                }
            });
        }
        if self.toks.get(self.k).is_some_and(|t| t.pos.line == line && t.t != Tok::RBrace) {
            return Err((
                self.here(),
                format!("the row has more values than the table's {} columns", cols.len()),
            ));
        }
        Ok(At::new(cells, pos))
    }

    fn shuffle(&mut self, depth: usize) -> R<Decl> {
        let name = self.name("the shuffle's name")?;
        let mut items = Vec::new();
        if self.ctx("kind") {
            let pos = self.here();
            self.k += 1;
            self.name("a preset")?;
            items.push(ShufItem::Later(At::new("kind".into(), pos)));
        }
        self.expect(&Tok::LBrace, "`{`")?;
        while !matches!(self.peek(), Some(Tok::RBrace) | None) {
            let start = self.k;
            self.brk(start, depth + 1);
            match self.shuf_item(depth + 1) {
                Ok(i) => items.push(i),
                Err(e) => {
                    self.errors.push(e);
                    self.k = (start + 1..self.toks.len())
                        .find(|i| self.toks[*i].nl)
                        .unwrap_or(self.toks.len());
                }
            }
        }
        self.brk(self.k, depth);
        self.expect(&Tok::RBrace, "`}`")?;
        Ok(Decl::Shuffle(name, items))
    }

    fn shuf_item(&mut self, depth: usize) -> R<ShufItem> {
        let pos = self.here();
        if self.word("when") {
            self.k += 1;
            self.rule(depth)?;
            return Ok(ShufItem::Later(At::new("when".into(), pos)));
        }
        if self.word("check") {
            self.k += 1;
            return Ok(ShufItem::Check(self.name("none or layout")?));
        }
        let Some(Tok::Name(w)) = self.peek().cloned() else {
            return Err((
                pos,
                format!(
                    "expected `slots`, `things`, `thing`, `coupled`, `check` or `when`, found {}",
                    self.got()
                ),
            ));
        };
        self.k += 1;
        match w.as_str() {
            "slots" | "things" => {
                let slots = w == "slots";
                if self.ctx("exits") {
                    self.k += 1;
                    self.expect(&Tok::Open, "`(`")?;
                    let list = self.names("an exit group or exit")?;
                    self.expect(&Tok::Close, "`)`")?;
                    return Ok(ShufItem::Exits(slots, pos, list));
                }
                let subject = self.names(if slots { "a slot" } else { "a thing" })?;
                let verb_pos = self.here();
                let verb = match self.peek() {
                    Some(Tok::Name(v)) => match (slots, v.as_str()) {
                        (true, "only") => Some(Verb::Only),
                        (true, "deny") => Some(Verb::Deny),
                        (false, "only_in") => Some(Verb::OnlyIn),
                        (false, "deny_in") => Some(Verb::DenyIn),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(verb) = verb else {
                    if slots {
                        return Ok(ShufItem::Names(pos, subject));
                    }
                    return Err((verb_pos, "after `things` names comes `only_in` or `deny_in`; list things with `thing NAME`".into()));
                };
                self.k += 1;
                let object = self.names("a name or group")?;
                let mut later = Vec::new();
                loop {
                    let at = self.here();
                    if self.word("when") {
                        self.k += 1;
                        self.rule(depth)?;
                        later.push(At::new("when".into(), at));
                    } else if self.ctx("again") {
                        self.k += 1;
                        later.push(At::new("again".into(), at));
                    } else {
                        break;
                    }
                }
                Ok(ShufItem::Row(At::new(verb, pos), subject, object, later))
            }
            "thing" => {
                let n = self.name("the thing's name")?;
                let rule = if self.peek() == Some(&Tok::Colon) {
                    self.k += 1;
                    Some(self.rule(depth)?)
                } else {
                    None
                };
                let group = if self.word("group") {
                    self.k += 1;
                    Some(self.name("a group")?)
                } else {
                    None
                };
                Ok(ShufItem::Thing(n, rule, group))
            }
            "coupled" => {
                let r = if self.rule_starts() && !self.toks[self.k].nl {
                    Some(self.rule(depth)?)
                } else {
                    None
                };
                Ok(ShufItem::Coupled(pos, r))
            }
            "pin" | "vanilla" | "pool" | "after" | "scope" => {
                self.k =
                    (self.k..self.toks.len()).find(|i| self.toks[*i].nl).unwrap_or(self.toks.len());
                Ok(ShufItem::Later(At::new(w, pos)))
            }
            other => Err((pos, format!("`{other}` is not part of a shuffle"))),
        }
    }

    fn overlay(&mut self, kw: &str, pos: Pos, depth: usize) -> R<Overlay> {
        let tpos = self.here();
        let kind =
            match self.peek().cloned() {
                Some(Tok::Kw(k)) => k,
                _ => return Err((
                    tpos,
                    "an overlay names what it changes: `region`, `check`, `exit`, `def` or `item`"
                        .into(),
                )),
            };
        self.k += 1;
        let dotted = |p: &mut Self, what: &str| -> R<(At<String>, At<String>)> {
            let n = p.name(what)?;
            match n.v.split_once('.') {
                Some((a, b)) if !b.contains('.') => {
                    Ok((At::new(a.to_owned(), n.pos), At::new(b.to_owned(), n.pos)))
                }
                _ => Err((n.pos, format!("a {what} is named `region.name`"))),
            }
        };
        let target = match kind {
            "region" => Target::Region(self.name("a region")?),
            "check" => {
                let (r, c) = dotted(self, "check")?;
                Target::Check(r, c)
            }
            "exit" => {
                let (r, c) = dotted(self, "exit")?;
                Target::Exit(r, c)
            }
            "def" => Target::Def(self.name("a def")?),
            "item" => Target::Item(self.name("an item")?),
            k => return Err((tpos, format!("an overlay cannot change a `{k}`"))),
        };
        Ok(match kw {
            "extend" => {
                if !matches!(target, Target::Region(_)) {
                    return Err((
                        tpos,
                        "`extend` adds to a region: `extend region NAME { ... }`".into(),
                    ));
                }
                let open = self.expect(&Tok::LBrace, "`{`")?;
                Overlay::Extend(pos, target, self.members(depth + 1, "this `extend`", open)?)
            }
            "replace" => {
                if !matches!(target, Target::Check(..) | Target::Exit(..) | Target::Def(_)) {
                    return Err((
                        tpos,
                        "`replace` swaps the rule of a check, an exit or a def".into(),
                    ));
                }
                self.expect(&Tok::Colon, "`:` and the new rule")?;
                Overlay::Replace(pos, target, self.rule(depth)?)
            }
            _ => Overlay::Remove(pos, target),
        })
    }
}

/// A name as written, its `{var}` pieces included.
fn show_tn(n: &Tn) -> String {
    pieces_text(&n.v)
}

/// Name pieces as written: `{s.pack}_pak`.
pub(crate) fn pieces_text(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t) => t.clone(),
            Piece::Var(v) => format!("{{{v}}}"),
        })
        .collect()
}

fn col_word(c: Col) -> &'static str {
    match c {
        Col::Name => "a name",
        Col::Int => "a whole number",
        Col::Bool => "true or false",
        Col::Rule => "a rule",
    }
}
