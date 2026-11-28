//! The settings parser (design doc 20 section 2): a tree over the shared lexer's tokens, each
//! node with its place and its byte span in the source, so edits can splice by span (8.3) and
//! the formatter can write the tokens again (2.9). The source is kept as written, so the tree
//! gives back the exact input.
//!
//! A line is read by its first tokens only (2.1): a name and `=`, `+=` or `-=` is a setting,
//! whatever the name; otherwise the first word must be a statement word allowed there. Nothing
//! else looks ahead, and nothing backtracks. An error skips to the next line, so every error of
//! a file is reported in one pass.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use config::Limits;
use rando_syntax::lex::{Comment, Kind, Tok, Token, lex};
use rando_syntax::{Diagnostic, Pos};

use crate::diag;

/// Bytes `start..end` of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// The first byte.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
}

/// A part of the tree, where it starts and the bytes it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node<T> {
    /// What it is.
    pub v: T,
    /// Where it starts.
    pub pos: Pos,
    /// Its bytes.
    pub span: Span,
}

/// How many copies a list entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// `x 3`.
    N(i64),
    /// `x all`, every copy the shuffle holds (only in `place`, 2.11).
    All,
}

/// A list entry: `rope` or `arrow x 10`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The item, place, word or group.
    pub name: Node<String>,
    /// Its count, when written.
    pub count: Option<Node<Count>>,
}

/// The size of a `some_of` (2.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// `N`.
    N(i64),
    /// `A to B`.
    Range(i64, i64),
    /// `any`.
    Any,
}

/// A value (2.2's `value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A name: a choice value, `on`, `off`, a named number, a set word.
    Name(String),
    /// A whole number.
    Int(i64),
    /// A string.
    Str(String),
    /// `[a, b x 3]`.
    List(Vec<Node<Entry>>),
    /// `{ k = v, ... }`, one level.
    Record(Vec<(Node<String>, Node<Value>)>),
    /// `any`.
    Any,
    /// `one_of(a, b)`.
    OneOf(Vec<Node<Value>>),
    /// `chance(a 50%, b 50%)`.
    Chance(Vec<(Node<Value>, Node<i64>)>),
    /// `between(A, B)` or `between(A, B, lean)`.
    Between(Node<i64>, Node<i64>, Option<Node<String>>),
    /// `some_of(size, names...)`.
    SomeOf(Node<Size>, Vec<Node<String>>),
}

impl Value {
    /// Whether it is a surprise form (2.10).
    #[must_use]
    pub fn is_surprise(&self) -> bool {
        matches!(
            self,
            Value::Any
                | Value::OneOf(_)
                | Value::Chance(_)
                | Value::Between(..)
                | Value::SomeOf(..)
        )
    }
}

/// `=`, `+=` or `-=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `=`
    Set,
    /// `+=`, only in a block (2.12).
    Add,
    /// `-=`, only in a block.
    Remove,
}

/// What a `place` places (2.2's `items_v`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Items {
    /// One item or group, with an optional count.
    One(Entry),
    /// A list of them.
    List(Vec<Node<Entry>>),
    /// `one_of`, `chance` or `some_of` over items.
    Pick(Value),
}

/// A modifier after a `place` (2.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mod {
    /// `copy`
    Copy,
    /// `if_possible`
    IfPossible,
    /// `N%`
    Chance(i64),
}

/// The direction of a `connect` (2.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Both ways (no word).
    Both,
    /// `one_way`
    OneWay,
    /// `way_out`
    WayOut,
}

/// A statement of the body, or a line inside a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// `name = value`, or inside a block `name += value` and `name -= value`.
    Setting {
        /// The option.
        key: Node<String>,
        /// The operator.
        op: Op,
        /// The value.
        value: Node<Value>,
    },
    /// `place ITEMS at PLACES [copy] [if_possible] [N%]`.
    Place {
        /// What.
        items: Node<Items>,
        /// Where: a place, a group, or a list of them.
        places: Node<Value>,
        /// The modifiers, in written order.
        mods: Vec<Node<Mod>>,
    },
    /// `connect A to B [one_way | way_out] [N%]`.
    Connect {
        /// The door.
        from: Node<String>,
        /// The target door, or `one_of(...)` of them.
        to: Node<Value>,
        /// Which way.
        dir: Dir,
        /// The chance, when written.
        chance: Option<Node<i64>>,
    },
    /// `pin THING at SLOT in SHUFFLE`.
    Pin {
        /// The thing.
        thing: Node<String>,
        /// The slot.
        slot: Node<String>,
        /// The shuffle.
        shuffle: Node<String>,
    },
    /// `when a = x and b = y [N%] { ... }` or `with N% { ... }`.
    Block {
        /// The conditions; empty for `with`.
        when: Vec<(Node<String>, Node<Value>)>,
        /// The chance, when written (always for `with`).
        chance: Option<Node<i64>>,
        /// The changes, one per line.
        body: Vec<Line>,
        /// The token index of the closing `}`.
        close: usize,
    },
    /// `legacy { name = value ... }`, passed through for sidecar worlds.
    Legacy {
        /// Its lines.
        body: Vec<Line>,
        /// The token index of the closing `}`.
        close: usize,
    },
}

/// One statement and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The statement.
    pub stmt: Stmt,
    /// Where it starts.
    pub pos: Pos,
    /// Its bytes, first token to last (a block's `}` included).
    pub span: Span,
    /// Its tokens; for a block, the head line up to and including `{`.
    pub toks: Range<usize>,
}

/// A preset named on a `preset` line (2.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetRef {
    /// A world preset, by name.
    World(String),
    /// A community preset, pinned by the token hash of its statements.
    Community {
        /// Its name.
        name: String,
        /// The SHA-256, hex.
        sha256: String,
    },
}

/// Which kind of settings file the header makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Header {
    /// `settings for WORLD`.
    Player,
    /// `preset NAME for WORLD`.
    Preset(String),
    /// `room settings`.
    Room,
}

/// The world a header names (2.2's `world_ref`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorldRef {
    /// The world's registry name.
    pub name: Option<Node<String>>,
    /// The `version "..."` requirement, Cargo's syntax.
    pub version: Option<Node<String>>,
    /// The `from "..."` path, for local tools only.
    pub from: Option<Node<String>>,
}

/// A parsed settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The source, as written.
    pub src: String,
    /// Its tokens.
    pub toks: Vec<Token>,
    /// Its comments.
    pub comments: Vec<Comment>,
    /// The `format N` line.
    pub format: Option<Node<i64>>,
    /// The header, when the file has one.
    pub header: Option<Node<Header>>,
    /// The world it names.
    pub world: WorldRef,
    /// The `player "..."` line.
    pub player: Option<Node<String>>,
    /// The `preset` lines, in written order.
    pub presets: Vec<Node<PresetRef>>,
    /// The token ranges of the header block's lines (`format`, header, `player`, `preset`,
    /// `name`), in order.
    pub head: Vec<Range<usize>>,
    /// The body.
    pub body: Vec<Line>,
}

impl File {
    /// The text of `s`.
    #[must_use]
    pub fn text(&self, s: Span) -> &str {
        &self.src[s.start..s.end]
    }
}

/// The words a body line may start with (2.2), for the error that names them.
const BODY_WORDS: &str = "`NAME = value`, `place`, `connect`, `pin`, `when`, `with` or `legacy`";

/// `src` parsed as the settings file `file`, with every error found. A file over
/// `settings_max_bytes`, or one the lexer refuses, gives an empty tree and that one error.
#[must_use]
pub fn parse(src: &str, file: &str, l: &Limits) -> (File, Vec<Diagnostic>) {
    let mut out = File {
        src: src.to_owned(),
        toks: Vec::new(),
        comments: Vec::new(),
        format: None,
        header: None,
        world: WorldRef::default(),
        player: None,
        presets: Vec::new(),
        head: Vec::new(),
        body: Vec::new(),
    };
    if src.len() > l.settings_max_bytes {
        let msg = format!(
            "This file is {} bytes. A settings file may hold at most {} (settings_max_bytes).",
            src.len(),
            l.settings_max_bytes
        );
        return (out, Vec::from([diag(file, "settings.limit", None, msg, None)]));
    }
    let (toks, comments) = match lex(src, Pos::new(1, 1), Kind::Settings) {
        Ok(t) => t,
        Err((pos, msg)) => {
            return (out, Vec::from([diag(file, "settings.syntax", Some(pos), msg, None)]));
        }
    };
    let mut p = P { toks: &toks, i: 0, file, l, errs: Vec::new(), stmts: 0, depth: 0 };
    if comments.len() > l.settings_max_comments {
        let msg = format!(
            "This file has {} comments. A settings file may hold at most {} \
             (settings_max_comments).",
            comments.len(),
            l.settings_max_comments
        );
        p.errs.push(diag(
            file,
            "settings.limit",
            Some(comments[l.settings_max_comments].pos),
            msg,
            None,
        ));
    }
    p.check_tokens();
    p.head(&mut out);
    let mut blocks = 0usize;
    let mut placements = 0usize;
    while p.i < toks.len() {
        let start = p.i;
        match p.statement(false) {
            Ok(line) => {
                match &line.stmt {
                    Stmt::Block { .. } => blocks += 1,
                    Stmt::Place { .. } | Stmt::Connect { .. } | Stmt::Pin { .. } => {
                        placements += 1;
                    }
                    _ => {}
                }
                if blocks == l.settings_max_blocks + 1 && matches!(line.stmt, Stmt::Block { .. }) {
                    let msg = format!(
                        "This file has more than {} `when` and `with` blocks \
                         (settings_max_blocks).",
                        l.settings_max_blocks
                    );
                    p.errs.push(diag(file, "settings.limit", Some(line.pos), msg, None));
                }
                if placements == l.settings_max_placements + 1
                    && matches!(
                        line.stmt,
                        Stmt::Place { .. } | Stmt::Connect { .. } | Stmt::Pin { .. }
                    )
                {
                    let msg = format!(
                        "This file has more than {} `place`, `connect` and `pin` lines \
                         (settings_max_placements).",
                        l.settings_max_placements
                    );
                    p.errs.push(diag(file, "settings.limit", Some(line.pos), msg, None));
                }
                out.body.push(line);
            }
            Err(e) => {
                p.errs.push(*e);
                p.skip_line(start);
            }
        }
    }
    if p.stmts > l.settings_max_statements {
        let msg = format!(
            "This file has {} statements. A settings file may hold at most {} \
             (settings_max_statements).",
            p.stmts, l.settings_max_statements
        );
        p.errs.push(diag(file, "settings.limit", None, msg, None));
    }
    let errs = p.errs;
    out.toks = toks;
    out.comments = comments;
    (out, errs)
}

/// One value from untrusted text (an edit, 8.3): exactly one `value` of 2.2, with nothing
/// before or after it, no newline and no comment.
///
/// # Errors
/// Anything else, as a `settings.bad_value` or `settings.limit` diagnostic.
pub fn parse_value(text: &str, l: &Limits) -> Result<(Value, Vec<Token>), Box<Diagnostic>> {
    let bad = |msg: &str| Box::new(diag("value", "settings.bad_value", None, msg.to_owned(), None));
    if text.len() > l.settings_max_value_bytes {
        let msg = format!(
            "A value may be at most {} bytes (settings_max_value_bytes).",
            l.settings_max_value_bytes
        );
        return Err(diag("value", "settings.limit", None, msg, None).into());
    }
    if text.contains(['\n', '\r']) {
        return Err(bad("A value is one line: it holds no line break."));
    }
    let (toks, comments) = lex(text, Pos::new(1, 1), Kind::Settings)
        .map_err(|(_, m)| diag("value", "settings.bad_value", None, m, None))?;
    if !comments.is_empty() {
        return Err(bad("A value holds no comment."));
    }
    if toks.is_empty() {
        return Err(bad("A value is missing."));
    }
    let mut p = P { toks: &toks, i: 0, file: "value", l, errs: Vec::new(), stmts: 0, depth: 0 };
    p.check_tokens();
    if let Some(e) = p.errs.pop() {
        return Err(e.into());
    }
    let v = p.value()?;
    if let Some(e) = p.errs.pop() {
        return Err(e.into());
    }
    if p.i < toks.len() {
        return Err(bad("A value is one value, with nothing after it."));
    }
    Ok((v.v, toks))
}

struct P<'a> {
    toks: &'a [Token],
    i: usize,
    file: &'a str,
    l: &'a Limits,
    errs: Vec<Diagnostic>,
    stmts: usize,
    /// How many values the current one is inside, counting itself.
    depth: usize,
}

type R<T> = Result<T, Box<Diagnostic>>;

impl P<'_> {
    fn err(&self, pos: Option<Pos>, msg: String) -> Box<Diagnostic> {
        Box::new(diag(self.file, "settings.syntax", pos, msg, None))
    }

    /// The caps that are about one token: name and string length, integer range.
    fn check_tokens(&mut self) {
        let l = self.l;
        for t in self.toks {
            let (what, over) = match &t.t {
                Tok::Name(n) if n.len() > l.settings_max_name_bytes => (
                    "name",
                    format!(
                        "at most {} bytes (settings_max_name_bytes)",
                        l.settings_max_name_bytes
                    ),
                ),
                Tok::Str(s) if s.len() > l.settings_max_string_bytes => (
                    "string",
                    format!(
                        "at most {} bytes (settings_max_string_bytes)",
                        l.settings_max_string_bytes
                    ),
                ),
                Tok::Int(n) if *n < l.settings_int_min || *n > l.settings_int_max => (
                    "number",
                    format!(
                        "from {} to {} (settings_int_range)",
                        l.settings_int_min, l.settings_int_max
                    ),
                ),
                _ => continue,
            };
            let msg = format!("This {what} is too long or too large: it must be {over}.");
            self.errs.push(diag(self.file, "settings.limit", Some(t.pos), msg, None));
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|t| &t.t)
    }

    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.i + k).map(|t| &t.t)
    }

    fn word(&self) -> Option<&str> {
        match self.peek() {
            Some(Tok::Name(n)) => Some(n),
            _ => None,
        }
    }

    /// The place of the current token, or of the end of the last one.
    fn here(&self) -> Pos {
        match self.toks.get(self.i) {
            Some(t) => t.pos,
            None => self.toks.last().map_or(Pos::new(1, 1), |t| t.pos.right(t.len)),
        }
    }

    fn eol(&self) -> bool {
        self.toks.get(self.i).is_none_or(|t| t.nl)
    }

    fn span_from(&self, start: usize) -> Span {
        let a = self.toks[start].off;
        let b = self.toks[self.i - 1].off + self.toks[self.i - 1].size;
        Span { start: a, end: b }
    }

    fn node<T>(&self, start: usize, v: T) -> Node<T> {
        Node { v, pos: self.toks[start].pos, span: self.span_from(start) }
    }

    fn show_here(&self) -> String {
        self.peek().map_or("the end of the file".into(), rando_syntax::lex::show)
    }

    fn expect(&mut self, t: &Tok, what: &str) -> R<()> {
        if self.peek() == Some(t) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(Some(self.here()), format!("expected {what}, found {}", self.show_here())))
        }
    }

    /// A word that counts only in this position (`for`, `at`, `to`, ...).
    fn ctx(&mut self, w: &str) -> bool {
        if self.word() == Some(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect_word(&mut self, w: &str) -> R<()> {
        if self.ctx(w) {
            Ok(())
        } else {
            Err(self.err(Some(self.here()), format!("expected `{w}`, found {}", self.show_here())))
        }
    }

    fn name(&mut self, what: &str) -> R<Node<String>> {
        let start = self.i;
        match self.peek() {
            Some(Tok::Name(n)) => {
                let n = n.clone();
                self.i += 1;
                Ok(self.node(start, n))
            }
            _ => {
                Err(self
                    .err(Some(self.here()), format!("expected {what}, found {}", self.show_here())))
            }
        }
    }

    fn string(&mut self, what: &str) -> R<Node<String>> {
        let start = self.i;
        match self.peek() {
            Some(Tok::Str(s)) => {
                let s = s.clone();
                self.i += 1;
                Ok(self.node(start, s))
            }
            _ => Err(self.err(
                Some(self.here()),
                format!("expected {what} in quotes, found {}", self.show_here()),
            )),
        }
    }

    fn int(&mut self, what: &str) -> R<Node<i64>> {
        let start = self.i;
        match self.peek() {
            Some(Tok::Int(n)) => {
                let n = *n;
                self.i += 1;
                Ok(self.node(start, n))
            }
            _ => {
                Err(self
                    .err(Some(self.here()), format!("expected {what}, found {}", self.show_here())))
            }
        }
    }

    /// `N%`, the `%` required (open question 2).
    fn percent(&mut self) -> R<Node<i64>> {
        let start = self.i;
        let n = self.int("a chance")?;
        if self.peek() != Some(&Tok::Percent) {
            let mut e = self
                .err(Some(self.here()), format!("a chance is written with `%`, like `{}%`", n.v));
            e.help = Some(format!("write `{}%`", n.v));
            return Err(e);
        }
        self.i += 1;
        Ok(self.node(start, n.v))
    }

    fn end_line(&self) -> R<()> {
        if self.eol() {
            Ok(())
        } else {
            Err(self.err(
                Some(self.here()),
                format!("expected the end of the line, found {}", self.show_here()),
            ))
        }
    }

    /// Skip to the next line start outside any bracket, at least one token past `start`.
    fn skip_line(&mut self, start: usize) {
        // shortcut: no bracket matching, so an error inside a multi-line list may give one
        // more error on its next line; a bracket that never closes cannot swallow the file.
        self.i = self.i.max(start + 1);
        while self.toks.get(self.i).is_some_and(|t| !t.nl) {
            self.i += 1;
        }
    }

    /// The header block: `format`, the header, then `player`, `preset` and `name` lines.
    fn head(&mut self, out: &mut File) {
        if self.word() == Some("format") && matches!(self.peek_at(1), Some(Tok::Int(_))) {
            let start = self.i;
            self.i += 1;
            let n = self.int("a format number").ok();
            let r = self.end_line();
            if let (Some(n), Ok(())) = (n, r) {
                out.format =
                    Some(Node { v: n.v, pos: self.toks[start].pos, span: self.span_from(start) });
            } else if let Err(e) = self.end_line() {
                self.errs.push(*e);
                self.skip_line(start);
            }
            out.head.push(start..self.i);
        }
        let start = self.i;
        let header = match (self.word(), self.peek_at(1)) {
            (Some("settings"), Some(Tok::Name(f))) if f == "for" => {
                self.i += 2;
                Some(Header::Player)
            }
            (Some("preset"), Some(Tok::Name(_))) if matches!(self.peek_at(2), Some(Tok::Name(f)) if f == "for") =>
            {
                let Some(Tok::Name(n)) = self.peek_at(1).cloned() else { unreachable!() };
                self.i += 3;
                Some(Header::Preset(n))
            }
            (Some("room"), Some(Tok::Name(s))) if s == "settings" => {
                self.i += 2;
                Some(Header::Room)
            }
            _ => None,
        };
        let Some(header) = header else {
            let e = diag(
                self.file,
                "settings.header",
                Some(self.here()),
                "A settings file starts with `settings for WORLD`, `preset NAME for WORLD` or \
                 `room settings`."
                    .into(),
                Some("add a first line such as `settings for lanternfall`".into()),
            );
            self.errs.push(e);
            return;
        };
        if header != Header::Room {
            match self.world_ref() {
                Ok(w) => out.world = w,
                Err(e) => {
                    self.errs.push(*e);
                    self.skip_line(start);
                }
            }
        } else if let Err(e) = self.end_line() {
            self.errs.push(*e);
            self.skip_line(start);
        }
        out.header = Some(self.node(start, header));
        if let Err(e) = self.one_line(start..self.i) {
            self.errs.push(*e);
        }
        out.head.push(start..self.i);
        // Header lines, until the first body line.
        loop {
            let start = self.i;
            let r = match (self.word(), self.peek_at(1)) {
                (Some("player"), Some(Tok::Str(_))) => {
                    self.i += 1;
                    self.string("a player name").and_then(|s| {
                        self.end_line()?;
                        if out.player.is_some() {
                            return Err(self.err(Some(s.pos), "a file names one player".into()));
                        }
                        out.player = Some(s);
                        Ok(())
                    })
                }
                (Some("preset"), Some(Tok::Name(_) | Tok::Str(_))) => {
                    self.i += 1;
                    self.preset_ref(start).map(|p| out.presets.push(p))
                }
                (Some("name"), Some(Tok::Str(_))) => {
                    self.i += 1;
                    self.string("a room name").and_then(|_| self.end_line())
                }
                _ => break,
            };
            if let Err(e) = r.and_then(|()| self.one_line(start..self.i)) {
                self.errs.push(*e);
                self.skip_line(start);
            }
            out.head.push(start..self.i);
        }
    }

    fn world_ref(&mut self) -> R<WorldRef> {
        let mut w = WorldRef { name: Some(self.name("the world's name")?), ..WorldRef::default() };
        if !self.eol() && self.ctx("version") {
            w.version = Some(self.string("a version requirement")?);
        }
        if !self.eol() && self.ctx("from") {
            w.from = Some(self.string("a path")?);
        }
        self.end_line()?;
        Ok(w)
    }

    fn preset_ref(&mut self, start: usize) -> R<Node<PresetRef>> {
        let p = match self.peek() {
            Some(Tok::Str(_)) => {
                let name = self.string("a preset name")?;
                self.expect_word("sha256")?;
                let sha = self.string("the preset's SHA-256")?;
                PresetRef::Community { name: name.v, sha256: sha.v }
            }
            _ => PresetRef::World(self.name("a preset name")?.v),
        };
        self.end_line()?;
        Ok(self.node(start, p))
    }

    /// The first token of `r` past the first that starts a new line outside any bracket: a
    /// statement is one line (2.1), and only `( )`, `[ ]` and a record's `{ }` may span lines.
    fn split(&self, r: Range<usize>) -> Option<Pos> {
        let mut depth = 0i64;
        for (k, t) in self.toks[r.clone()].iter().enumerate() {
            if k > 0 && depth == 0 && t.nl {
                return Some(t.pos);
            }
            depth += match t.t {
                Tok::Open | Tok::LBrack | Tok::LBrace => 1,
                Tok::Close | Tok::RBrack | Tok::RBrace => -1,
                _ => 0,
            };
        }
        None
    }

    fn one_line(&mut self, r: Range<usize>) -> R<()> {
        match self.split(r) {
            Some(at) => {
                Err(self
                    .err(Some(at), "a statement is one line; this one goes on to the next".into()))
            }
            None => Ok(()),
        }
    }

    /// One statement from the start of a line. `in_block`: a change line inside a block.
    fn statement(&mut self, in_block: bool) -> R<Line> {
        let start = self.i;
        self.stmts += 1;
        let pos = self.here();
        let stmt = self.stmt_kind(in_block)?;
        let toks_end = self.i;
        let close = match &stmt {
            Stmt::Block { close, .. } | Stmt::Legacy { close, .. } => Some(*close),
            _ => None,
        };
        let head_end = match close {
            Some(_) => self.toks[start..]
                .iter()
                .position(|t| t.t == Tok::LBrace)
                .map_or(toks_end, |k| start + k + 1),
            None => toks_end,
        };
        self.one_line(start..head_end)?;
        Ok(Line { stmt, pos, span: self.span_from(start), toks: start..head_end })
    }

    fn op(&mut self) -> Option<Op> {
        let glued = |k: usize| self.toks.get(self.i + k).is_some_and(|t| t.glued);
        match (self.peek_at(1), self.peek_at(2)) {
            (Some(Tok::Eq), _) => Some(Op::Set),
            (Some(Tok::Plus), Some(Tok::Eq)) if glued(2) => Some(Op::Add),
            (Some(Tok::Minus), Some(Tok::Eq)) if glued(2) => Some(Op::Remove),
            _ => None,
        }
    }

    fn stmt_kind(&mut self, in_block: bool) -> R<Stmt> {
        let start = self.i;
        let Some(word) = self.word().map(ToOwned::to_owned) else {
            return Err(self.err(
                Some(self.here()),
                format!("a line starts with {BODY_WORDS}, not {}", self.show_here()),
            ));
        };
        if let Some(op) = self.op() {
            let key = self.name("an option")?;
            self.i += if op == Op::Set { 1 } else { 2 };
            if op != Op::Set && !in_block {
                return Err(self.err(
                    Some(key.pos),
                    "`+=` and `-=` change a list only inside a `when` or `with` block; \
                     write `=` with the whole list"
                        .into(),
                ));
            }
            if self.eol() {
                let eq = &self.toks[self.i - 1];
                let at = eq.pos.right(eq.len);
                return Err(self.err(Some(at), format!("`{}` has no value after it", key.v)));
            }
            let value = self.value()?;
            self.end_line()?;
            return Ok(Stmt::Setting { key, op, value });
        }
        if in_block {
            let msg = match word.as_str() {
                "place" | "connect" | "pin" | "when" | "with" | "legacy" => {
                    format!("a block holds only changes (`NAME = value`, `+=`, `-=`), not `{word}`")
                }
                _ => format!(
                    "a line in a block is `NAME = value`, `NAME += value` or `NAME -= value`, not `{word}`"
                ),
            };
            let key = if matches!(
                word.as_str(),
                "place" | "connect" | "pin" | "when" | "with" | "legacy"
            ) {
                "settings.block"
            } else {
                "settings.syntax"
            };
            return Err(diag(self.file, key, Some(self.here()), msg, None).into());
        }
        self.i += 1;
        let stmt = match word.as_str() {
            "place" => {
                let items = self.items()?;
                self.expect_word("at")?;
                let places = self.places()?;
                let mut mods: Vec<Node<Mod>> = Vec::new();
                while !self.eol() {
                    let at = self.i;
                    let m = if self.ctx("copy") {
                        Mod::Copy
                    } else if self.ctx("if_possible") {
                        Mod::IfPossible
                    } else if matches!(self.peek(), Some(Tok::Int(_))) {
                        Mod::Chance(self.percent()?.v)
                    } else {
                        return Err(self.err(
                            Some(self.here()),
                            format!(
                                "after a place come `copy`, `if_possible` or a chance, not {}",
                                self.show_here()
                            ),
                        ));
                    };
                    let same = |x: &Node<Mod>| {
                        core::mem::discriminant(&x.v) == core::mem::discriminant(&m)
                    };
                    if mods.iter().any(same) {
                        return Err(self.err(
                            Some(self.toks[at].pos),
                            "each modifier is written at most once".into(),
                        ));
                    }
                    mods.push(self.node(at, m));
                }
                Stmt::Place { items, places, mods }
            }
            "connect" => {
                let from = self.name("a door")?;
                self.expect_word("to")?;
                let at = self.i;
                let to = if self.word() == Some("one_of") && self.peek_at(1) == Some(&Tok::Open) {
                    self.value()?
                } else {
                    let n = self.name("a door")?;
                    self.node(at, Value::Name(n.v))
                };
                let dir = if !self.eol() && self.ctx("one_way") {
                    Dir::OneWay
                } else if !self.eol() && self.ctx("way_out") {
                    Dir::WayOut
                } else {
                    Dir::Both
                };
                let chance = if self.eol() { None } else { Some(self.percent()?) };
                self.end_line()?;
                Stmt::Connect { from, to, dir, chance }
            }
            "pin" => {
                let thing = self.name("a thing to pin")?;
                self.expect_word("at")?;
                let slot = self.name("a slot")?;
                self.expect_word("in")?;
                let shuffle = self.name("a shuffle")?;
                self.end_line()?;
                Stmt::Pin { thing, slot, shuffle }
            }
            "when" | "with" => self.block(word == "with")?,
            "legacy" => {
                self.expect(&Tok::LBrace, "`{`")?;
                let (body, close) = self.block_body(start)?;
                for l in &body {
                    if let Stmt::Setting { op: Op::Add | Op::Remove, key, .. } = &l.stmt {
                        return Err(
                            self.err(Some(key.pos), "a `legacy` line is `NAME = value`".into())
                        );
                    }
                }
                Stmt::Legacy { body, close }
            }
            "player" | "preset" | "name" | "format" => {
                let mut e = self.err(
                    Some(self.toks[start].pos),
                    format!("a `{word}` line belongs in the header, before the first setting"),
                );
                e.help = Some("move this line up, under the `settings for` line".into());
                return Err(e);
            }
            "lock" | "game" | "share" => {
                return Err(self.err(
                    Some(self.toks[start].pos),
                    format!("`{word}` is for room files (`room settings`), not this file"),
                ));
            }
            _ => {
                self.i = start;
                return Err(self.err(
                    Some(self.here()),
                    format!("a line starts with {BODY_WORDS}; `{word}` is none of these"),
                ));
            }
        };
        if !matches!(stmt, Stmt::Block { .. } | Stmt::Legacy { .. }) {
            self.end_line()?;
        }
        Ok(stmt)
    }

    fn block(&mut self, with: bool) -> R<Stmt> {
        let start = self.i - 1;
        let mut when = Vec::new();
        let mut chance = None;
        if with {
            chance = Some(self.percent()?);
        } else {
            loop {
                let key = self.name("an option to test")?;
                self.expect(&Tok::Eq, "`=`")?;
                let at = self.i;
                let v = match self.peek().cloned() {
                    Some(Tok::Name(n)) => Value::Name(n),
                    Some(Tok::Int(n)) => Value::Int(n),
                    Some(Tok::Str(s)) => Value::Str(s),
                    _ => {
                        return Err(self.err(
                            Some(self.here()),
                            format!("a condition tests one value, not {}", self.show_here()),
                        ));
                    }
                };
                self.i += 1;
                when.push((key, self.node(at, v)));
                if when.len() == self.l.settings_max_conditions + 1 {
                    let msg = format!(
                        "A `when` tests at most {} conditions (settings_max_conditions).",
                        self.l.settings_max_conditions
                    );
                    self.errs.push(diag(
                        self.file,
                        "settings.limit",
                        Some(when[when.len() - 1].0.pos),
                        msg,
                        None,
                    ));
                }
                if !self.ctx("and") {
                    break;
                }
            }
            if matches!(self.peek(), Some(Tok::Int(_))) {
                chance = Some(self.percent()?);
            }
        }
        if self.peek() != Some(&Tok::LBrace) || self.eol() {
            return Err(self.err(Some(self.here()), "a block's `{` ends its first line".into()));
        }
        self.i += 1;
        let (body, close) = self.block_body(start)?;
        Ok(Stmt::Block { when, chance, body, close })
    }

    /// The lines of a block after its `{`, through the `}` alone on its line.
    fn block_body(&mut self, start: usize) -> R<(Vec<Line>, usize)> {
        self.end_line()?;
        let mut body = Vec::new();
        loop {
            let Some(t) = self.toks.get(self.i) else {
                return Err(
                    self.err(Some(self.toks[start].pos), "this block has no closing `}`".into())
                );
            };
            if t.t == Tok::RBrace {
                let close = self.i;
                self.i += 1;
                if !self.eol() {
                    return Err(self.err(
                        Some(self.toks[close].pos),
                        "a block's `}` stands on its own line".into(),
                    ));
                }
                return Ok((body, close));
            }
            let at = self.i;
            match self.statement(true) {
                Ok(l) => body.push(l),
                Err(e) => {
                    self.errs.push(*e);
                    self.skip_line(at);
                }
            }
        }
    }

    fn items(&mut self) -> R<Node<Items>> {
        let start = self.i;
        let pick = matches!(self.word(), Some("one_of" | "chance" | "some_of"))
            && self.peek_at(1) == Some(&Tok::Open);
        let v = if pick {
            Items::Pick(self.value()?.v)
        } else if self.peek() == Some(&Tok::LBrack) {
            Items::List(self.list()?)
        } else {
            Items::One(self.entry()?.v)
        };
        Ok(self.node(start, v))
    }

    fn places(&mut self) -> R<Node<Value>> {
        let start = self.i;
        if self.peek() == Some(&Tok::LBrack) {
            let l = self.list()?;
            return Ok(self.node(start, Value::List(l)));
        }
        let n = self.name("a place or a place group")?;
        Ok(self.node(start, Value::Name(n.v)))
    }

    fn entry(&mut self) -> R<Node<Entry>> {
        let start = self.i;
        let name = self.name("a name")?;
        let mut count = None;
        if self.word() == Some("x") && matches!(self.peek_at(1), Some(Tok::Int(_) | Tok::Name(_))) {
            self.i += 1;
            let at = self.i;
            let c = if self.ctx("all") {
                Count::All
            } else {
                let n = self.int("a count")?;
                if n.v < 1 || n.v > self.l.settings_max_copies {
                    let msg = format!(
                        "A count is 1 to {} (settings_int_range).",
                        self.l.settings_max_copies
                    );
                    self.errs.push(diag(self.file, "settings.limit", Some(n.pos), msg, None));
                }
                Count::N(n.v)
            };
            count = Some(self.node(at, c));
        }
        Ok(self.node(start, Entry { name, count }))
    }

    fn cap(&mut self, n: usize, at: Pos) {
        if n == self.l.settings_max_list_entries + 1 {
            let msg = format!(
                "A list holds at most {} entries (settings_max_list_entries).",
                self.l.settings_max_list_entries
            );
            self.errs.push(diag(self.file, "settings.limit", Some(at), msg, None));
        }
    }

    fn list(&mut self) -> R<Vec<Node<Entry>>> {
        self.expect(&Tok::LBrack, "`[`")?;
        let mut out: Vec<Node<Entry>> = Vec::new();
        while self.peek() != Some(&Tok::RBrack) {
            let e = self.entry()?;
            self.cap(out.len() + 1, e.pos);
            out.push(e);
            if self.peek() == Some(&Tok::Comma) {
                self.i += 1;
            } else if self.peek() != Some(&Tok::RBrack) {
                return Err(self.err(
                    Some(self.here()),
                    format!("expected `,` or `]`, found {}", self.show_here()),
                ));
            }
        }
        self.i += 1;
        Ok(out)
    }

    /// Comma-separated items up to `)`, after a `(`.
    fn args<T>(&mut self, mut one: impl FnMut(&mut Self) -> R<T>) -> R<Vec<T>> {
        let mut out = Vec::new();
        loop {
            let at = self.here();
            out.push(one(self)?);
            self.cap(out.len(), at);
            if self.peek() == Some(&Tok::Comma) {
                self.i += 1;
                if self.peek() == Some(&Tok::Close) {
                    self.i += 1;
                    return Ok(out);
                }
            } else {
                self.expect(&Tok::Close, "`,` or `)`")?;
                return Ok(out);
            }
        }
    }

    /// 2.2's `value`, refused past `settings_max_depth` before it recurses.
    fn value(&mut self) -> R<Node<Value>> {
        if self.depth >= self.l.settings_max_depth {
            let msg = format!(
                "Values nest at most {} deep: a surprise holds `any` or `between`, and a record a list (settings_max_depth).",
                self.l.settings_max_depth
            );
            return Err(diag(self.file, "settings.limit", Some(self.here()), msg, None).into());
        }
        self.depth += 1;
        let v = self.value_inner();
        self.depth -= 1;
        v
    }

    fn value_inner(&mut self) -> R<Node<Value>> {
        let start = self.i;
        let call = self.peek_at(1) == Some(&Tok::Open);
        let v = match self.peek().cloned() {
            Some(Tok::Name(n)) if n == "any" => {
                self.i += 1;
                Value::Any
            }
            Some(Tok::Name(n)) if call && n == "one_of" => {
                self.i += 2;
                Value::OneOf(self.args(Self::value)?)
            }
            Some(Tok::Name(n)) if call && n == "chance" => {
                self.i += 2;
                Value::Chance(self.args(|p| {
                    let v = p.value()?;
                    let c = p.percent()?;
                    Ok((v, c))
                })?)
            }
            Some(Tok::Name(n)) if call && n == "between" => {
                self.i += 2;
                let a = self.int("the lowest number")?;
                self.expect(&Tok::Comma, "`,`")?;
                let b = self.int("the highest number")?;
                let lean = if self.peek() == Some(&Tok::Comma) {
                    self.i += 1;
                    Some(self.name("`low`, `middle` or `high`")?)
                } else {
                    None
                };
                self.expect(&Tok::Close, "`)`")?;
                Value::Between(a, b, lean)
            }
            Some(Tok::Name(n)) if call && n == "some_of" => {
                self.i += 2;
                let at = self.i;
                let size = if self.ctx("any") {
                    Size::Any
                } else {
                    let a = self.int("a size: `N`, `A to B` or `any`")?;
                    if self.ctx("to") {
                        Size::Range(a.v, self.int("the largest size")?.v)
                    } else {
                        Size::N(a.v)
                    }
                };
                let size = self.node(at, size);
                self.expect(&Tok::Comma, "`,` and the names to pick from")?;
                Value::SomeOf(size, self.args(|p| p.name("a name"))?)
            }
            Some(Tok::Name(n)) => {
                self.i += 1;
                Value::Name(n)
            }
            Some(Tok::Int(n)) => {
                self.i += 1;
                Value::Int(n)
            }
            Some(Tok::Str(s)) => {
                self.i += 1;
                Value::Str(s)
            }
            Some(Tok::LBrack) => Value::List(self.list()?),
            Some(Tok::LBrace) => {
                self.i += 1;
                let mut fields = Vec::new();
                while self.peek() != Some(&Tok::RBrace) {
                    let k = self.name("a field")?;
                    self.expect(&Tok::Eq, "`=`")?;
                    let v = self.value()?;
                    if matches!(v.v, Value::Record(_)) || v.v.is_surprise() {
                        return Err(self.err(
                            Some(v.pos),
                            "a record's field holds a plain value: no record and no surprise"
                                .into(),
                        ));
                    }
                    fields.push((k, v));
                    if self.peek() == Some(&Tok::Comma) {
                        self.i += 1;
                    } else if self.peek() != Some(&Tok::RBrace) {
                        return Err(self.err(
                            Some(self.here()),
                            format!("expected `,` or `}}`, found {}", self.show_here()),
                        ));
                    }
                }
                self.i += 1;
                Value::Record(fields)
            }
            _ => {
                return Err(self.err(
                    Some(self.here()),
                    format!("expected a value, found {}", self.show_here()),
                ));
            }
        };
        Ok(self.node(start, v))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;
    use rando_syntax::lex::{RESERVED, RULE_WORDS};

    const L: Limits = Limits::DESIGN;

    pub(crate) const BEGINNER: &str = "settings for lanternfall\nplayer \"Mira\"\npreset relaxed\n\ngoal = find_the_crown\nstart_with = { items = [lantern] }\nshared_deaths = off\ntraps = one_of(none, a_few)\n";

    pub(crate) const ADVANCED: &str = r#"settings for lanternfall version "0.1"
player "Mira"
preset standard
preset "fast_start" sha256 "3f1c9e"   # a community preset, pinned

# Playing with others (common vocabulary, doc 15 section 4)
keep_moving = strong
must_reach = everything
keep_home = [lantern, rope]
send_away = [crown_piece]
start_with = { items = [boots], from_shuffle = [arrow x 10] }
reveal_items = [pickaxe]
reveal_places = [vault_altar]
no_important_at = [well_bucket]
important_at = [mine_cart]
shared_deaths = on                         # my opt-in; the host must allow it

# How hard
logic_level = normal
tricks = { ledge_clip = beginner, dark_room = on }
key_shuffle = own_area
door_shuffle = off
item_pool = standard

# This game's own options
shuffle_mine = on
lamp_color = teal                          # looks only: never changes the game
bombs = plenty                             # a named number (doc 18 section 2.14)
shops = [bakery, tailor]                   # a set of the world's own words
trap_mix = { slow = 30, ice = 70 }         # counts per name

# Surprise me
goal = one_of(find_the_crown, light_every_lamp)
traps = chance(none 50%, a_few 30%, lots 20%)
hint_count = between(3, 8, low)            # leans toward 3
music = any                                # looks only, drawn on the player's device
extra_tools = some_of(1 to 2, rope, boots, lamp_oil)

when goal = light_every_lamp {
  hint_count = 10
  keep_home += [lamp_oil]
}

# By hand
place pickaxe at mine_cart
place rope at [vault_altar, cave_chest]
place some_of(2, bomb_bag, lamp_oil, potion) at early_places if_possible
connect cave_mouth to mill_door one_way
"#;

    fn ok(src: &str) -> File {
        let (f, e) = parse(src, "f.randoset", &L);
        assert!(e.is_empty(), "{e:#?}");
        f
    }

    fn errs(src: &str) -> Vec<String> {
        parse(src, "f.randoset", &L).1.iter().map(ToString::to_string).collect()
    }

    fn keys(src: &str) -> Vec<String> {
        parse(src, "f.randoset", &L).1.into_iter().filter_map(|d| d.key).collect()
    }

    #[test]
    fn the_beginner_file_parses() {
        let f = ok(BEGINNER);
        assert_eq!(f.header.as_ref().unwrap().v, Header::Player);
        assert_eq!(f.world.name.as_ref().unwrap().v, "lanternfall");
        assert_eq!(f.player.as_ref().unwrap().v, "Mira");
        assert_eq!(f.presets[0].v, PresetRef::World("relaxed".into()));
        assert_eq!(f.body.len(), 4);
        let Stmt::Setting { key, value, .. } = &f.body[3].stmt else { panic!() };
        assert_eq!(key.v, "traps");
        assert_eq!(f.text(value.span), "one_of(none, a_few)");
        assert!(matches!(&value.v, Value::OneOf(v) if v.len() == 2));
        assert_eq!(f.text(f.body[1].span), "start_with = { items = [lantern] }");
    }

    #[test]
    fn the_advanced_file_parses() {
        let f = ok(ADVANCED);
        assert_eq!(f.world.version.as_ref().unwrap().v, "0.1");
        assert_eq!(
            f.presets[1].v,
            PresetRef::Community { name: "fast_start".into(), sha256: "3f1c9e".into() }
        );
        let Some(Line { stmt: Stmt::Block { when, body, .. }, .. }) =
            f.body.iter().find(|l| matches!(l.stmt, Stmt::Block { .. }))
        else {
            panic!()
        };
        assert_eq!(when[0].0.v, "goal");
        assert!(matches!(body[1].stmt, Stmt::Setting { op: Op::Add, .. }));
        let places: Vec<&Line> =
            f.body.iter().filter(|l| matches!(l.stmt, Stmt::Place { .. })).collect();
        assert_eq!(places.len(), 3);
        let Stmt::Place { items, mods, .. } = &places[2].stmt else { panic!() };
        assert!(matches!(items.v, Items::Pick(Value::SomeOf(..))));
        assert_eq!(mods[0].v, Mod::IfPossible);
        assert!(matches!(f.body.last().unwrap().stmt, Stmt::Connect { dir: Dir::OneWay, .. }));
    }

    #[test]
    fn the_section_2_12_and_2_11_examples_parse() {
        ok(
            "settings for w\nwhen traps = lots and logic_level = expert 50% {\n  trap_mix = { slow = 20, ice = 80 }\n}\nwith 30% {\n  start_with = { items = [boots] }\n}\n",
        );
        ok(
            "settings for w\nplace [potion x 3, rope] at [p1, p2, p3, p4]\nplace chance(rope 25%, boots 75%) at s copy 50%\nplace arrow x all at cave_places\nconnect a to one_of(b, c) 50%\nconnect a to b way_out\npin stone_golem at mill_arena in bosses\ncount = chance(5 60%, between(1, 3, low) 40%)\nlegacy {\n  old_thing = 3\n}\n",
        );
    }

    /// Every word reserved in a world file, and every settings word, works as a key, a value,
    /// an item and a place (doc 20 section 8.5).
    #[test]
    fn no_word_is_reserved() {
        let settings_words = [
            "settings",
            "room",
            "for",
            "player",
            "preset",
            "name",
            "format",
            "version",
            "from",
            "sha256",
            "place",
            "at",
            "in",
            "others",
            "anyone",
            "connect",
            "to",
            "one_way",
            "way_out",
            "pin",
            "copy",
            "if_possible",
            "when",
            "with",
            "and",
            "lock",
            "game",
            "share",
            "legacy",
            "one_of",
            "chance",
            "between",
            "some_of",
            "low",
            "middle",
            "high",
            "x",
            "all",
            "on",
            "off",
            "true",
            "false",
        ];
        for w in RESERVED.iter().chain(&RULE_WORDS).chain(&settings_words) {
            let src = format!("settings for w\n{w} = {w}\nk = [{w}, {w} x 2]\nplace {w} at {w}\n");
            let f = ok(&src);
            let Stmt::Setting { key, value, .. } = &f.body[0].stmt else { panic!("{w}") };
            assert_eq!((key.v.as_str(), &value.v), (*w, &Value::Name((*w).into())));
        }
        // `any` is always the surprise form in a value; a world may not name a value `any`.
        let f = ok("settings for w\nany = any\n");
        assert!(
            matches!(&f.body[0].stmt, Stmt::Setting { key, value, .. } if key.v == "any" && value.v == Value::Any)
        );
    }

    #[test]
    fn a_header_line_after_the_body_says_to_move_it_up() {
        let e = errs("settings for w\ngoal = a\nplayer \"Mira\"\n");
        assert_eq!(e.len(), 1);
        assert!(e[0].starts_with("f.randoset:3:1: a `player` line belongs in the header"), "{e:?}");
        assert!(e[0].contains("help: move this line up"));
    }

    #[test]
    fn errors_recover_at_the_next_line() {
        let src =
            "settings for w\na = \nb = [x,\n  y\nc = chance(a 50, b 50%)\nd = ok\n+= 3\nk += [a]\n";
        let (f, e) = parse(src, "f", &L);
        let e: Vec<String> = e.iter().map(ToString::to_string).collect();
        assert!(e.iter().any(|m| m.starts_with("f:2:")), "{e:#?}");
        assert!(e.iter().any(|m| m.contains("`50%`")), "{e:#?}");
        assert!(
            e.iter().any(|m| m.starts_with("f:8:1:") && m.contains("inside a `when`")),
            "{e:#?}"
        );
        assert!(
            f.body.iter().any(|l| matches!(&l.stmt, Stmt::Setting { key, .. } if key.v == "d"))
        );
    }

    #[test]
    fn a_missing_header_and_a_block_without_its_close_are_errors() {
        assert_eq!(keys("goal = a\n"), ["settings.header"]);
        assert_eq!(keys("settings for w\nwhen a = b {\n  c = d\n"), ["settings.syntax"]);
        assert_eq!(keys("settings for w\nwhen a = b {\n  place x at y\n}\n"), ["settings.block"]);
        assert_eq!(keys("settings for w\nwhen a = b { c = d }\n"), ["settings.syntax"]);
        assert!(ok("format 1\nsettings for w\n").format.is_some());
        assert_eq!(ok("room settings\nname \"Friday\"\n").header.unwrap().v, Header::Room);
        assert_eq!(
            ok("preset fast for w\npreset base\n").header.unwrap().v,
            Header::Preset("fast".into())
        );
    }

    #[test]
    fn a_line_starting_with_another_word_names_the_allowed_words() {
        let e = errs("settings for w\ngoal find\n");
        assert!(e[0].contains("`place`, `connect`, `pin`, `when`, `with` or `legacy`"), "{e:?}");
        assert!(errs("settings for w\nplace a at b copy copy\n")[0].contains("at most once"));
    }

    #[test]
    fn options_named_like_statement_words_are_settings() {
        let f = ok("settings for w\nversion = 3\nplace = on\nconnect a to b\none_way = on\n");
        assert_eq!(f.body.len(), 4);
        assert!(matches!(f.body[2].stmt, Stmt::Connect { dir: Dir::Both, .. }));
    }

    #[test]
    fn spans_give_back_each_value_and_crlf_files_parse() {
        let src = "settings for w\r\nk = [a, # first\r\n  b x 2,\r\n]  # end\r\nluck = chance\r\n";
        let f = ok(src);
        let Stmt::Setting { value, .. } = &f.body[0].stmt else { panic!() };
        assert_eq!(f.text(value.span), "[a, # first\r\n  b x 2,\r\n]");
        let Value::List(l) = &value.v else { panic!() };
        assert_eq!(f.text(l[1].span), "b x 2");
        assert!(
            matches!(&f.body[1].stmt, Stmt::Setting { value, .. } if value.v == Value::Name("chance".into()))
        );
    }

    #[test]
    fn untrusted_values_are_exactly_one_value() {
        assert!(parse_value("[a, b x 2]", &L).is_ok());
        assert!(parse_value("one_of(a, b)", &L).is_ok());
        for bad in ["a\nb = c", "a # c", "a b", "a\r", "", "[a", "a = b"] {
            assert!(parse_value(bad, &L).is_err(), "{bad:?}");
        }
        let big = "a".repeat(L.settings_max_value_bytes + 1);
        assert_eq!(parse_value(&big, &L).unwrap_err().key.as_deref(), Some("settings.limit"));
    }

    /// Review of PR 233, high 1: nesting is capped before the parser recurses, so a deep file
    /// is refused, never a stack overflow.
    #[test]
    fn deep_nesting_is_refused_without_recursing() {
        let deep = |open: &str, close: &str| {
            format!("settings for w\ngoal = {}a{}\n", open.repeat(1000), close.repeat(1000))
        };
        assert_eq!(keys(&deep("one_of(", ", b)")), ["settings.limit"]);
        assert_eq!(keys(&deep("{a = ", "}")), ["settings.limit"]);
        assert_eq!(keys(&deep("chance(", " 50%, b 50%)")), ["settings.limit"]);
        // One level of surprise nesting, and a list in a record, are inside the cap.
        assert_eq!(
            keys(
                "settings for w\nk = chance(5 60%, between(1, 3, low) 40%)\nr = { items = [a] }\n"
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            keys("settings for w\nk = one_of(a, one_of(b, chance(c 100%)))\n"),
            ["settings.limit"]
        );
        assert!(
            parse_value(&format!("{}a{}", "one_of(".repeat(1000), ", b)".repeat(1000)), &L)
                .is_err()
        );
    }

    /// Review of PR 233, high 3: a limit hit inside a value reaches the caller.
    #[test]
    fn a_value_past_a_cap_is_refused() {
        for v in ["[rope x 70000]", "[rope x 0]", "one_of(a, one_of(b, one_of(c, d)))"] {
            assert_eq!(
                parse_value(v, &L).unwrap_err().key.as_deref(),
                Some("settings.limit"),
                "{v}"
            );
        }
    }

    /// Review of PR 233, low 9: a statement is one line outside its brackets (2.1).
    #[test]
    fn a_statement_does_not_run_onto_the_next_line() {
        for src in [
            "settings for w\nplace pickaxe\nat mine_cart\n",
            "settings for w\ngoal\n= find_the_crown\n",
            "settings for w\nwhen a = b and\nc = d {\n}\n",
            "settings for w\nconnect a\nto b\n",
            "settings\nfor w\n",
        ] {
            let k = keys(src);
            assert!(
                !k.is_empty() && k.iter().all(|k| k == "settings.syntax" || k == "settings.header"),
                "{src:?}: {k:?}"
            );
        }
        assert_eq!(
            keys("settings for w\nk = [a,\n  b]\nr = {\n  a = 1\n}\nc = one_of(a,\n  b)\n"),
            Vec::<String>::new()
        );
    }

    /// Review of PR 233, low 11: names in a settings file are lowercase, with no dots.
    #[test]
    fn names_are_lowercase_without_dots() {
        assert_eq!(keys("settings for w\nGoal = a\n"), ["settings.syntax"]);
        assert_eq!(keys("settings for w\ngoal.x = a\n"), ["settings.syntax"]);
        assert!(parse_value("one_of(one_of(a, b), c)", &L).is_err());
    }

    /// Mutation survivor M1: `+` and `=` with a space between are not `+=`.
    #[test]
    fn plus_equals_must_be_glued() {
        assert_eq!(
            keys("settings for w\nwhen a = b {\n  k + = [x]\n  k - = [x]\n}\n"),
            ["settings.syntax", "settings.syntax"]
        );
        assert_eq!(
            keys("settings for w\nwhen a = b {\n  k += [x]\n  k -= [x]\n}\n"),
            Vec::<String>::new()
        );
    }

    fn limit_keys(src: &str, l: &Limits) -> Vec<String> {
        parse(src, "f", l).1.into_iter().filter_map(|d| d.key).collect()
    }

    /// Every cap the parser holds, at the cap and one past it (doc 20 section 8.5).
    #[test]
    fn caps_at_the_limit_pass_and_past_it_fail() {
        let small = Limits {
            settings_max_bytes: 40,
            settings_max_statements: 2,
            settings_max_list_entries: 2,
            settings_max_comments: 1,
            settings_max_name_bytes: 8,
            settings_max_string_bytes: 2,
            settings_max_blocks: 1,
            settings_max_conditions: 1,
            settings_max_placements: 1,
            settings_max_copies: 4,
            settings_int_min: -5,
            settings_int_max: 5,
            ..L
        };
        let both = |at: &str, past: &str| {
            assert_eq!(limit_keys(at, &small), Vec::<String>::new(), "{at}");
            assert_eq!(limit_keys(past, &small), ["settings.limit"], "{past}");
        };
        let at40 = "settings for w\nabc = [a, b]\n# twelve ...";
        assert_eq!(at40.len(), 40);
        both(at40, &format!("{at40}."));
        both("settings for w\na = 1\nb = 2\n", "settings for w\na = 1\nb = 2\nc = 3\n");
        both("settings for w\na = [x, y]\n", "settings for w\na = [x, y, z]\n");
        both("settings for w\na = one_of(x, y)\n", "settings for w\na = one_of(x, y, z)\n");
        both("settings for w # c\n", "settings for w # c\n# d\n");
        both("settings for w\nabcdefgh = 1\n", "settings for w\nabcdefghi = 1\n");
        both("settings for w\na = \"ab\"\n", "settings for w\na = \"abc\"\n");
        both("settings for w\na = 5\n", "settings for w\na = 6\n");
        both("settings for w\na = -5\n", "settings for w\na = -6\n");
        both("settings for w\na = [x x 4]\n", "settings for w\na = [x x 5]\n");
        both(
            "settings for w\nwhen a = b {\n}\n",
            "settings for w\nwhen a = b {\n}\nwith 5% {\n}\n",
        );
        both("settings for w\nwhen a = b {\n}\n", "settings for w\nwhen a = b and c = d {\n}\n");
        both(
            "settings for w\npin a at b in c\n",
            "settings for w\npin a at b in c\nconnect a to b\n",
        );
        // A block's lines count as statements.
        assert_eq!(
            limit_keys("settings for w\nwhen a = b {\n  c = 1\n  d = 2\n}\n", &small),
            ["settings.limit"]
        );
        let _ = vec![0];
    }
}
