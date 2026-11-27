//! The front end (design doc 18): a world's `.rando` files to the [`World`] model the checks
//! read. It loads the entry file and the modules it names, expands tables and families into
//! plain declarations, applies overlays, gives every item and check its ledger id, and checks
//! generated files' headers. Every value keeps its file, line and column. Name kinds, rules
//! and the format rules are the checks' job ([`crate::build_model`]), not this module's.

pub mod fmt;
mod ledger;
pub mod parse;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use parse::{
    Binder, Cell, CheckD, Col, Decl, ExitD, FileAst, Member, Over, Overlay, ShufItem, Target, Tn,
};
use sha2::{Digest, Sha256};

use crate::Error;
use crate::lower::fold;
use crate::model::{
    At, Check, Def, Exit, Grant, Item, Members, Opt, Pos, Region, Row, Rule, RuleAt, Shuffle,
    Thing, World,
};
use crate::rule::{Cmp, E, Expr, Piece};
use crate::world::{Errs, FORMAT};

/// The entry file of a world folder.
pub const ENTRY: &str = "world.rando";
/// The ledger file of a world folder (doc 18 section 2.11).
pub const LEDGER: &str = "ledger.json";
/// The extension of a module named by `use`.
pub const EXT: &str = ".rando";

/// Where a world's files come from: a folder, or memory in tests and tools.
pub trait Files {
    /// The text of `path` (relative to the world's folder): `None` when it does not exist.
    fn read(&self, path: &str) -> Option<Result<String, String>>;
}

/// A path the front end may open: relative, inside the folder, plain characters only.
fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
}

/// A world folder on disk.
#[derive(Debug, Clone)]
pub struct Dir(pub PathBuf);

impl Files for Dir {
    fn read(&self, path: &str) -> Option<Result<String, String>> {
        if !safe(path) {
            return Some(Err(format!(
                "{path:?} is not a plain relative path inside the world's folder"
            )));
        }
        let p = self.0.join(path);
        let meta = std::fs::metadata(&p).ok()?;
        if meta.len() > config::WORLD_SOURCE_MAX_BYTES as u64 {
            return Some(Err(format!("{path} is over {} bytes", config::WORLD_SOURCE_MAX_BYTES)));
        }
        Some(std::fs::read_to_string(&p).map_err(|e| format!("{path}: {e}")))
    }
}

/// A world held in memory: path to text.
#[derive(Debug, Clone, Default)]
pub struct Mem(pub BTreeMap<String, String>);

impl Files for Mem {
    fn read(&self, path: &str) -> Option<Result<String, String>> {
        let text = self.0.get(path)?;
        if text.len() > config::WORLD_SOURCE_MAX_BYTES {
            return Some(Err(format!("{path} is over {} bytes", config::WORLD_SOURCE_MAX_BYTES)));
        }
        Some(Ok(text.clone()))
    }
}

/// How strict a read is.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Accept a generated file whose content no longer matches its hash (local experiments).
    pub allow_edited: bool,
    /// Building to publish: edited generated files are refused whatever `allow_edited` says.
    pub publish: bool,
}

/// What a read gives: the world, the ledger to write back, warnings, and the parsed files.
#[derive(Debug, Clone)]
pub struct Read {
    /// The world model.
    pub world: World,
    /// The ledger after this build, canonical JSON.
    pub ledger: String,
    /// Things worth a look.
    pub warnings: Vec<Error>,
}

/// Read the world in `files` (the entry file is [`ENTRY`]).
///
/// # Errors
/// Every syntax, module, family, overlay, ledger and header error found.
pub fn read(files: &dyn Files, opts: &Options) -> Result<Read, Vec<Error>> {
    let mut x = X::default();
    x.load(files);
    x.tables(files);
    let order = x.order.clone();
    for f in &order {
        let decls = std::mem::take(&mut x.asts[*f as usize].decls);
        for d in &decls {
            x.decl(*f, d, &Env::new(), None);
        }
        x.asts[*f as usize].decls = decls;
    }
    x.apply_overlays();
    x.visibility();
    x.generated(files, opts);
    let ledger = ledger::assign(&mut x.world, &x.idents, files, &mut x.errs);
    x.world.files.clone_from(&x.paths);
    let (world, errs) = (x.world, x.errs);
    if !errs.errors.is_empty() {
        return Err(errs.finish());
    }
    let warnings = errs.warnings_list();
    Ok(Read { world, ledger, warnings })
}

/// Parse and format one file's text.
///
/// # Errors
/// Its syntax errors, positions in file `name`.
pub fn format_text(src: &str, name: &str) -> Result<String, Vec<Error>> {
    let ast = parse::parse(src, 0);
    if !ast.errors.is_empty() {
        let mut errs = Errs::new(vec![name.to_owned()]);
        ast.errors.iter().for_each(|(p, m)| errs.err(*p, m.clone(), ""));
        return Err(errs.finish());
    }
    Ok(fmt::format(&ast))
}

/// The SHA-256 of a generated file's content: its text after the `generated` block, formatted.
#[must_use]
pub fn content_hash(ast: &FileAst) -> String {
    let from = ast.generated.as_ref().map_or(0, |g| g.end);
    hex(&Sha256::digest(fmt::format_from(ast, from).as_bytes()))
}

/// A generated file's text with a correct header: what a generator writes (doc 18 section
/// 2.9). `body` is the file without its header; `sources` are path and text of each input.
///
/// # Errors
/// The body's syntax errors.
pub fn write_generated(
    tool: &str,
    sources: &[(&str, &str)],
    body: &str,
) -> Result<String, Vec<Error>> {
    let body = format_text(body, "generated body")?;
    let hash = hex(&Sha256::digest(body.as_bytes()));
    let mut out =
        format!("generated {{\n  tool {}\n", fmt::text(&crate::lex::Tok::Str(tool.to_owned())));
    for (path, text) in sources {
        let h = hex(&Sha256::digest(text.as_bytes()));
        out += &format!(
            "  source {} sha256 \"{h}\"\n",
            fmt::text(&crate::lex::Tok::Str((*path).to_owned()))
        );
    }
    out += &format!("  content sha256 \"{hash}\"\n}}\n");
    Ok(out + &body)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A family variable's value.
#[derive(Debug, Clone)]
enum Val {
    Int(i64),
    Name(String),
    /// A table row: the table's name and the row's index.
    Row(String, usize),
}

type Env = Vec<(String, Val)>;

/// A table, read.
#[derive(Debug, Clone)]
struct Tab {
    cols: Vec<(String, Col)>,
    rows: Vec<Vec<Cell>>,
    file: u32,
}

/// What the ledger needs of one item or check: its name, ledger key, old name and place.
#[derive(Debug, Clone)]
pub(crate) struct Ident {
    pub check: bool,
    pub name: String,
    pub key: Option<String>,
    pub was: Option<String>,
    pub pos: Pos,
}

#[derive(Default)]
struct X {
    paths: Vec<String>,
    asts: Vec<FileAst>,
    /// Files in the order their declarations apply: each after the files it uses.
    order: Vec<u32>,
    uses: Vec<Vec<u32>>,
    errs: Errs,
    tables: HashMap<String, Tab>,
    world: World,
    idents: Vec<Ident>,
    overlays: Vec<(u32, Overlay)>,
    /// Names each file reads, with where: checked against the files it uses.
    refs: Vec<(u32, String, Pos)>,
    made: usize,
}

impl X {
    fn file_index(&mut self, path: &str) -> u32 {
        match self.paths.iter().position(|p| p == path) {
            Some(i) => u32::try_from(i).unwrap_or(0),
            None => {
                self.paths.push(path.to_owned());
                self.asts.push(FileAst::default());
                self.uses.push(Vec::new());
                u32::try_from(self.paths.len() - 1).unwrap_or(0)
            }
        }
    }

    /// The entry file and every module it reaches, each parsed once.
    fn load(&mut self, files: &dyn Files) {
        self.errs = Errs::new(vec![ENTRY.to_owned()]);
        let entry = self.file_index(ENTRY);
        let mut seen = HashSet::new();
        // An explicit stack: (file, next use to follow); a file joins `order` after its uses.
        let mut stack = vec![(entry, None::<Pos>, 0usize)];
        while let Some((f, from, at)) = stack.pop() {
            if at == 0 {
                if !seen.insert(f) {
                    continue;
                }
                let path = self.paths[f as usize].clone();
                match files.read(&path) {
                    None => {
                        let msg = format!("`{path}` does not exist");
                        match from {
                            Some(p) => self.errs.err(p, msg, "check the path in this `use`"),
                            None => self.errs.whole(msg, "a world folder has a `world.rando`"),
                        }
                        continue;
                    }
                    Some(Err(e)) => {
                        self.errs.whole(e, "");
                        continue;
                    }
                    Some(Ok(text)) => {
                        let ast = parse::parse(&text, f);
                        ast.errors.iter().for_each(|(p, m)| self.errs.err(*p, m.clone(), ""));
                        self.asts[f as usize] = ast;
                    }
                }
                self.errs.files.clone_from(&self.paths);
            }
            let uses = self.asts[f as usize].uses.clone();
            if let Some(u) = uses.get(at) {
                stack.push((f, from, at + 1));
                let path = format!("{}{EXT}", u.v);
                if self.paths.len() >= config::WORLD_MODULES_MAX && !self.paths.contains(&path) {
                    self.errs.err(
                        u.pos,
                        format!("more than {} files", config::WORLD_MODULES_MAX),
                        "",
                    );
                    continue;
                }
                let g = self.file_index(&path);
                self.errs.files.clone_from(&self.paths);
                self.uses[f as usize].push(g);
                if !seen.contains(&g) {
                    stack.push((g, Some(u.pos), 0));
                }
            } else if !self.order.contains(&f) {
                self.order.push(f);
            }
        }
    }

    /// Files reachable from `f` by `use`, `f` included.
    fn closure(&self, f: u32) -> BTreeSet<u32> {
        let mut out = BTreeSet::from([f]);
        let mut todo = vec![f];
        while let Some(g) = todo.pop() {
            for h in self.uses.get(g as usize).into_iter().flatten() {
                if out.insert(*h) {
                    todo.push(*h);
                }
            }
        }
        out
    }

    /// Every table of every file, inline or from its file, read before any family runs.
    fn tables(&mut self, files: &dyn Files) {
        for f in self.order.clone() {
            for d in self.asts[f as usize].decls.clone() {
                let Decl::Table(t) = d else { continue };
                if self.tables.contains_key(&t.name.v) {
                    self.errs.err(
                        t.name.pos,
                        format!("table `{}` is declared twice", t.name.v),
                        "",
                    );
                    continue;
                }
                let rows = match &t.from {
                    None => t.rows.iter().map(|r| r.v.clone()).collect(),
                    Some(path) => self.csv(files, path, &t.cols),
                };
                let mut keys = HashSet::new();
                for (k, row) in rows.iter().enumerate() {
                    if let Some(Cell::Name(n)) = row.first()
                        && !keys.insert(n.clone())
                    {
                        let at = t.rows.get(k).map_or(t.name.pos, |r| r.pos);
                        self.errs.err(
                            at,
                            format!("table `{}` has two rows `{n}`", t.name.v),
                            "the first column names a row",
                        );
                    }
                }
                self.tables.insert(t.name.v.clone(), Tab { cols: t.cols.clone(), rows, file: f });
            }
        }
    }

    /// A table's rows from a CSV file: a header naming the columns, then one row per line,
    /// fields split at commas (no quoting), each read as its column's type.
    fn csv(
        &mut self,
        files: &dyn Files,
        path: &At<String>,
        cols: &[(String, Col)],
    ) -> Vec<Vec<Cell>> {
        let text = match files.read(&path.v) {
            Some(Ok(t)) => t,
            Some(Err(e)) => {
                self.errs.err(path.pos, e, "");
                return Vec::new();
            }
            None => {
                self.errs.err(path.pos, format!("`{}` does not exist", path.v), "");
                return Vec::new();
            }
        };
        let file = self.file_index(&path.v);
        self.errs.files.clone_from(&self.paths);
        let at = |line: usize, col: usize| Pos::in_file(file, line, col);
        let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
        let Some((_, head)) = lines.next() else {
            self.errs.err(at(1, 1), "the table file is empty", "its first line names the columns");
            return Vec::new();
        };
        let want: Vec<&str> = cols.iter().map(|c| c.0.as_str()).collect();
        let got: Vec<&str> = head.split(',').map(str::trim).collect();
        if got != want {
            self.errs.err(
                at(1, 1),
                format!(
                    "the columns are {}, and the table declares {}",
                    got.join(", "),
                    want.join(", ")
                ),
                "",
            );
            return Vec::new();
        }
        let mut rows = Vec::new();
        for (n, line) in lines {
            if line.contains('"') {
                self.errs.err(at(n + 1, 1), "a table file has no quoted fields", "");
                continue;
            }
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() != cols.len() {
                self.errs.err(
                    at(n + 1, 1),
                    format!("{} values; the table has {} columns", fields.len(), cols.len()),
                    "",
                );
                continue;
            }
            let mut row = Vec::new();
            let mut col = 1;
            for (field, (name, ty)) in fields.iter().zip(cols) {
                let lead = field.len() - field.trim_start().len();
                let v = field.trim();
                let p = at(n + 1, col + lead);
                col += field.chars().count() + 1;
                let cell = match ty {
                    Col::Name
                        if !v.is_empty()
                            && v.bytes().all(|b| {
                                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
                            }) =>
                    {
                        Some(Cell::Name(v.to_owned()))
                    }
                    Col::Int => v.parse().ok().map(Cell::Int),
                    Col::Bool => match v {
                        "true" => Some(Cell::Bool(true)),
                        "false" => Some(Cell::Bool(false)),
                        _ => None,
                    },
                    Col::Rule => match crate::rule::parse(v, p) {
                        Ok(e) => Some(Cell::Rule(e)),
                        Err((p, m)) => {
                            self.errs.err(p, m, "");
                            continue;
                        }
                    },
                    Col::Name => None,
                };
                match cell {
                    Some(c) => row.push(c),
                    None => self.errs.err(
                        p,
                        format!(
                            "`{v}` in column `{name}` is not {}",
                            match ty {
                                Col::Name => "a name",
                                Col::Int => "a whole number",
                                _ => "true or false",
                            }
                        ),
                        "",
                    ),
                }
            }
            if row.len() == cols.len() {
                rows.push(row);
            }
        }
        rows
    }

    /// Every combination of a family's variables, each with its `where` left over (what does
    /// not fold at build time becomes the declarations' `when`).
    fn combos(&mut self, binders: &[Binder], env: &Env) -> Vec<(Env, Option<Expr>)> {
        let mut out = vec![(env.clone(), None::<Expr>)];
        for b in binders {
            if matches!(b.var.v.as_str(), "opt" | "at") || env.iter().any(|(v, _)| *v == b.var.v) {
                self.errs.err(
                    b.var.pos,
                    format!("`{}` cannot name a family variable here", b.var.v),
                    "",
                );
                return Vec::new();
            }
            let values: Vec<Val> = match &b.over {
                Over::Range(lo, hi) => {
                    if hi < lo || hi - lo >= config::WORLD_FAMILY_MAX as i64 {
                        self.errs.err(
                            b.var.pos,
                            format!("range {lo}..{hi} is empty or too long"),
                            "",
                        );
                        return Vec::new();
                    }
                    (*lo..=*hi).map(Val::Int).collect()
                }
                Over::List(names) => names.iter().map(|n| Val::Name(n.v.clone())).collect(),
                Over::Table(t) => match self.tables.get(&t.v) {
                    Some(tab) => (0..tab.rows.len()).map(|r| Val::Row(t.v.clone(), r)).collect(),
                    None => {
                        self.errs.err(
                            t.pos,
                            format!("unknown table `{}`", t.v),
                            "declare it with `table`",
                        );
                        return Vec::new();
                    }
                },
            };
            let mut next = Vec::new();
            for (e, cond) in out {
                for v in &values {
                    let mut e2 = e.clone();
                    e2.push((b.var.v.clone(), v.clone()));
                    let mut c2 = cond.clone();
                    if let Some(w) = &b.cond {
                        match self.subst(w, &e2).map(simplify) {
                            Ok(Simp::Const(false)) => continue,
                            Ok(Simp::Const(true)) => {}
                            Ok(Simp::Rule(r)) => c2 = Some(and(c2, r)),
                            Err((p, m)) => {
                                self.errs.err(p, m, "");
                                return Vec::new();
                            }
                        }
                    }
                    self.made += 1;
                    if self.made > config::WORLD_FAMILY_MAX {
                        self.errs.err(
                            b.var.pos,
                            format!(
                                "families expand to more than {} declarations",
                                config::WORLD_FAMILY_MAX
                            ),
                            "",
                        );
                        return Vec::new();
                    }
                    next.push((e2, c2));
                }
            }
            out = next;
        }
        out
    }

    fn cell(&self, var: &str, field: &str, env: &Env, at: Pos) -> Result<Cell, (Pos, String)> {
        match env.iter().rev().find(|(v, _)| v == var).map(|x| &x.1) {
            Some(Val::Row(t, r)) => {
                let tab = &self.tables[t];
                let c = tab
                    .cols
                    .iter()
                    .position(|c| c.0 == field)
                    .ok_or_else(|| (at, format!("table `{t}` has no column `{field}`")))?;
                Ok(tab.rows[*r][c].clone())
            }
            Some(_) => Err((at, format!("`{var}` is not a table row, so it has no `.{field}`"))),
            None => Err((at, format!("`{var}` is not a family variable here"))),
        }
    }

    /// A name piece's text.
    fn piece(&self, v: &str, env: &Env, at: Pos) -> Result<String, (Pos, String)> {
        let cell = match v.split_once('.') {
            Some((var, field)) => self.cell(var, field, env, at)?,
            None => match env.iter().rev().find(|(n, _)| n == v).map(|x| &x.1) {
                Some(Val::Int(n)) => Cell::Int(*n),
                Some(Val::Name(n)) => Cell::Name(n.clone()),
                Some(Val::Row(..)) => {
                    return Err((
                        at,
                        format!("`{v}` is a table row; name a column, as in `{{{v}.key}}`"),
                    ));
                }
                None => return Err((at, format!("`{{{v}}}` is not a family variable here"))),
            },
        };
        match cell {
            Cell::Name(n) => Ok(n),
            Cell::Int(n) if n >= 0 => Ok(n.to_string()),
            _ => Err((
                at,
                format!("`{{{v}}}` is not a name or a number, so it cannot be part of a name"),
            )),
        }
    }

    fn tn(&mut self, n: &Tn, env: &Env) -> Option<At<String>> {
        let mut s = String::new();
        for p in &n.v {
            match p {
                Piece::Text(t) => s.push_str(t),
                Piece::Var(v) => match self.piece(v, env, n.pos) {
                    Ok(t) => s.push_str(&t),
                    Err((p, m)) => {
                        self.errs.err(p, m, "");
                        return None;
                    }
                },
            }
        }
        Some(At::new(s, n.pos))
    }

    /// `e` with the family's variables put in.
    fn subst(&self, e: &Expr, env: &Env) -> Result<Expr, (Pos, String)> {
        let mut out = e.clone();
        let mut todo = vec![&mut out];
        while let Some(x) = todo.pop() {
            let put = match &x.e {
                E::Name(n) if !n.starts_with("opt.") && !n.starts_with("at.") => {
                    let found = match n.split_once('.') {
                        Some((var, field)) if env.iter().any(|(v, _)| v == var) => {
                            Some(self.cell(var, field, env, x.at)?)
                        }
                        _ => match env.iter().rev().find(|(v, _)| v == n).map(|x| &x.1) {
                            Some(Val::Int(k)) => Some(Cell::Int(*k)),
                            Some(Val::Name(k)) => Some(Cell::Name(k.clone())),
                            Some(Val::Row(..)) => {
                                return Err((
                                    x.at,
                                    format!("`{n}` is a table row; name a column, as in `{n}.key`"),
                                ));
                            }
                            None => None,
                        },
                    };
                    found.map(|c| match c {
                        Cell::Int(k) => E::Int(k),
                        Cell::Name(k) => E::Name(k),
                        Cell::Bool(b) => E::Bool(b),
                        Cell::Rule(r) => E::Or(vec![r]),
                    })
                }
                E::Interp(ps) => {
                    let mut s = String::new();
                    for p in ps {
                        match p {
                            Piece::Text(t) => s.push_str(t),
                            Piece::Var(v) => s.push_str(&self.piece(v, env, x.at)?),
                        }
                    }
                    Some(E::Name(s))
                }
                _ => None,
            };
            if let Some(e) = put {
                // A rule cell goes in as one parenthesized node.
                x.e = match e {
                    E::Or(mut one) if one.len() == 1 => {
                        let r = one.remove(0);
                        x.paren = true;
                        r.e
                    }
                    e => e,
                };
                continue;
            }
            match &mut x.e {
                E::Call(_, xs) | E::And(xs) | E::Or(xs) | E::Sum(xs) => todo.extend(xs.iter_mut()),
                E::In(a, xs) => {
                    todo.push(a);
                    todo.extend(xs.iter_mut());
                }
                E::Not(a) => todo.push(a),
                E::Cmp(_, a, b) | E::Arith(_, a, b) => {
                    todo.push(a);
                    todo.push(b);
                }
                E::Match(_, arms) => todo.extend(arms.iter_mut().map(|a| &mut a.rule)),
                _ => {}
            }
        }
        Ok(out)
    }

    /// A rule with the family put in, as the model holds it; the names it reads noted.
    fn rule(&mut self, f: u32, e: &Expr, env: &Env) -> Option<RuleAt> {
        match self.subst(e, env) {
            Ok(r) => {
                self.note_refs(f, &r);
                Some(At::new(Rule::Parsed(r), e.at))
            }
            Err((p, m)) => {
                self.errs.err(p, m, "");
                None
            }
        }
    }

    fn opt_rule(
        &mut self,
        f: u32,
        e: Option<&Expr>,
        env: &Env,
        cond: Option<&Expr>,
    ) -> Option<RuleAt> {
        let joined = match (e, cond) {
            (None, None) => return None,
            (Some(e), None) => e.clone(),
            (None, Some(c)) => c.clone(),
            (Some(e), Some(c)) => and(Some(c.clone()), e.clone()),
        };
        self.rule(f, &joined, env)
    }

    fn note_refs(&mut self, f: u32, e: &Expr) {
        let mut todo = vec![e];
        while let Some(x) = todo.pop() {
            match &x.e {
                E::Name(n) => self.refs.push((f, n.clone(), x.at)),
                E::Call(c, xs) => {
                    self.refs.push((f, c.clone(), x.at));
                    todo.extend(xs);
                }
                E::And(xs) | E::Or(xs) | E::Sum(xs) => todo.extend(xs),
                E::In(a, xs) => {
                    todo.push(a);
                    todo.extend(xs);
                }
                E::Not(a) => todo.push(a),
                E::Cmp(_, a, b) | E::Arith(_, a, b) => {
                    todo.push(a);
                    todo.push(b);
                }
                E::Match(k, arms) => {
                    self.refs.push((f, format!("opt.{k}"), x.at));
                    todo.extend(arms.iter().map(|a| &a.rule));
                }
                _ => {}
            }
        }
    }

    fn ident(
        &mut self,
        check: bool,
        name: &At<String>,
        key: Option<&At<String>>,
        was: Option<&At<String>>,
        env: &Env,
    ) {
        // A family's key is its expanded name (doc 18 section 2.6): a written `key` is for one
        // declaration only.
        let key = key.map(|k| k.v.clone()).filter(|_| env.is_empty());
        self.idents.push(Ident {
            check,
            name: name.v.clone(),
            key,
            was: was.map(|w| w.v.clone()),
            pos: name.pos,
        });
    }

    fn decl(&mut self, f: u32, d: &Decl, env: &Env, cond: Option<&Expr>) {
        let no_cond = |x: &mut Self, what: &str, at: Pos| {
            if cond.is_some() {
                x.errs.err(
                    at,
                    format!("a family's `where` cannot switch off {what}"),
                    "move the `where` onto checks or regions, which have a `when`",
                );
            }
        };
        match d {
            Decl::World(n, v) => {
                if self.world.game.is_some() {
                    self.errs.err(n.pos, "a world has one `world` line", "");
                }
                self.world.format = Some(At::new(FORMAT.to_owned(), n.pos));
                self.world.game = Some(n.clone());
                self.world.version.clone_from(v);
            }
            Decl::Start(s) => {
                if self.world.start.is_some() {
                    self.errs.err(s.pos, "a world has one `start`", "");
                }
                self.world.start = Some(s.clone());
            }
            Decl::Option { name, ty, values, range, default } => {
                no_cond(self, "an option", name.pos);
                self.world.options.push(Opt {
                    key: name.clone(),
                    ty: ty.clone(),
                    values: values.clone(),
                    min: range.as_ref().map(|r| r.0.clone()),
                    max: range.as_ref().map(|r| r.1.clone()),
                    default: default.clone(),
                });
            }
            Decl::Item(i) => {
                let Some(name) = self.tn(&i.name, env) else { return };
                let mut later = Vec::new();
                if i.class.v == "match" {
                    later.push(At::new("class table".to_owned(), i.class.pos));
                }
                if i.when.is_some() || cond.is_some() {
                    later.push(At::new(
                        "when".to_owned(),
                        i.when.as_ref().map_or(name.pos, |w| w.at),
                    ));
                }
                self.ident(false, &name, i.key.as_ref(), i.was.as_ref(), env);
                let class = (i.class.v != "match").then(|| i.class.clone());
                self.world.items.push(Item {
                    key: name,
                    id: None,
                    class,
                    count: i.count.clone(),
                    later,
                });
            }
            Decl::Pool(fill) => self.world.fill_with.extend(fill.iter().cloned()),
            Decl::Grant { assumed, item, n, when } => {
                let Some(item) = self.tn(item, env) else { return };
                let when = self.opt_rule(f, when.as_ref(), env, cond);
                let g = Grant { item, n: n.clone(), when };
                if *assumed { self.world.assumed.push(g) } else { self.world.start_items.push(g) }
            }
            Decl::Def { name, params, rule } => {
                no_cond(self, "a def", name.pos);
                if let Some(r) = self.rule(f, rule, env) {
                    self.world.defs.push(Def {
                        name: name.clone(),
                        params: params.clone(),
                        rule: r,
                    });
                }
            }
            Decl::Group(n, m) => self.world.item_groups.push((n.clone(), m.clone())),
            Decl::Placement(n, m) => self.world.placement.push((n.clone(), m.clone())),
            Decl::Region(r) => {
                let Some(key) = self.tn(&r.name, env) else { return };
                let when = self.opt_rule(f, r.when.as_ref(), env, cond);
                let mut region = Region {
                    key,
                    checks: Vec::new(),
                    exits: Vec::new(),
                    events: Vec::new(),
                    when,
                    placement: r.placement.clone(),
                    can_stay: r.can_stay,
                    later: Vec::new(),
                };
                for m in &r.members {
                    self.member(f, m, env, None, &mut region);
                }
                self.world.regions.push(region);
            }
            Decl::Table(_) => {}
            Decl::For(binders, body) => {
                for (e2, c2) in self.combos(binders, env) {
                    let c = match (cond, c2) {
                        (Some(a), Some(b)) => Some(and(Some(a.clone()), b)),
                        (Some(a), None) => Some(a.clone()),
                        (None, b) => b,
                    };
                    for d in body {
                        self.decl(f, d, &e2, c.as_ref());
                    }
                }
            }
            Decl::Shuffle(name, items) => {
                no_cond(self, "a shuffle", name.pos);
                self.shuffle(f, name, items, env);
            }
            Decl::Goal(g) => {
                if self.world.goal.is_some() {
                    self.errs.err(g.at, "a world has one `goal`", "join the conditions with `and`");
                }
                self.world.goal = self.rule(f, g, env);
            }
            Decl::Overlay(o) => {
                if !env.is_empty() {
                    let at = match o {
                        Overlay::Extend(p, ..)
                        | Overlay::Replace(p, ..)
                        | Overlay::Remove(p, _) => *p,
                    };
                    self.errs.err(at, "an overlay cannot be inside a family", "");
                    return;
                }
                self.overlays.push((f, o.clone()));
            }
            Decl::Later(w) => self.world.later.push(w.clone()),
        }
    }

    fn member(&mut self, f: u32, m: &Member, env: &Env, cond: Option<&Expr>, region: &mut Region) {
        match m {
            Member::Check(c) => {
                if let Some(check) = self.check(f, c, env, cond) {
                    region.checks.push(check);
                }
            }
            Member::Exit(x) => {
                if let Some(exit) = self.exit(f, x, env, cond) {
                    region.exits.push(exit);
                }
            }
            Member::Event(n, r) => {
                let (Some(name), Some(rule)) =
                    (self.tn(n, env), self.opt_rule(f, Some(r), env, cond))
                else {
                    return;
                };
                region.events.push((name, rule));
            }
            Member::For(binders, body) => {
                for (e2, c2) in self.combos(binders, env) {
                    let c = match (cond, c2) {
                        (Some(a), Some(b)) => Some(and(Some(a.clone()), b)),
                        (Some(a), None) => Some(a.clone()),
                        (None, b) => b,
                    };
                    for m in body {
                        self.member(f, m, &e2, c.as_ref(), region);
                    }
                }
            }
        }
    }

    fn check(&mut self, f: u32, c: &CheckD, env: &Env, cond: Option<&Expr>) -> Option<Check> {
        let key = self.tn(&c.name, env)?;
        let rule = match &c.rule {
            Some(r) => self.rule(f, r, env)?,
            None => At::new(Rule::Const(true), key.pos),
        };
        let when = self.opt_rule(f, c.when.as_ref(), env, cond);
        let no_progression = match &c.no_progression {
            None => None,
            Some((p, None)) => Some(At::new(Rule::Const(true), *p)),
            Some((_, Some(r))) => Some(self.rule(f, r, env)?),
        };
        let vanilla = match &c.vanilla {
            Some(v) => Some(self.tn(v, env)?),
            None => None,
        };
        let holds = match &c.holds {
            Some(v) => Some(self.tn(v, env)?),
            None => None,
        };
        self.ident(true, &key, c.key.as_ref(), c.was.as_ref(), env);
        Some(Check {
            key,
            id: None,
            rule,
            when,
            vanilla,
            holds,
            no_progression,
            placement: c.placement.clone(),
            later: Vec::new(),
        })
    }

    fn exit(&mut self, f: u32, x: &ExitD, env: &Env, cond: Option<&Expr>) -> Option<Exit> {
        let to = self.tn(&x.to, env)?;
        let rule = self
            .opt_rule(f, x.rule.as_ref(), env, cond)
            .unwrap_or_else(|| At::new(Rule::Const(true), to.pos));
        let door = match &x.door {
            Some(d) => Some(self.tn(d, env)?),
            None => None,
        };
        Some(Exit { to, rule, door, group: x.group.clone() })
    }

    fn shuffle(&mut self, f: u32, name: &At<String>, items: &[ShufItem], env: &Env) {
        let mut sh = Shuffle {
            name: name.clone(),
            slots: At::new(Members::Names(Vec::new()), name.pos),
            things: None,
            coupled: None,
            allow: Vec::new(),
            check: None,
            later: Vec::new(),
        };
        let mut slots_seen = false;
        let mut things: Vec<Thing> = Vec::new();
        let mut things_pos = None;
        for it in items {
            match it {
                ShufItem::Exits(true, p, _) | ShufItem::Names(p, _) if slots_seen => {
                    self.errs.err(*p, "a shuffle lists its slots once", "");
                }
                ShufItem::Exits(true, p, list) => {
                    slots_seen = true;
                    sh.slots = At::new(Members::Exits(list.clone()), *p);
                }
                ShufItem::Names(p, list) => {
                    slots_seen = true;
                    let names = list
                        .iter()
                        .map(|n| Thing {
                            name: n.clone(),
                            rule: None,
                            group: None,
                            later: Vec::new(),
                        })
                        .collect();
                    sh.slots = At::new(Members::Names(names), *p);
                }
                ShufItem::Exits(false, p, list) => {
                    sh.things = Some(At::new(Members::Exits(list.clone()), *p))
                }
                ShufItem::Thing(n, r, g) => {
                    things_pos.get_or_insert(n.pos);
                    let rule = match r {
                        Some(r) => self.rule(f, r, env),
                        None => None,
                    };
                    things.push(Thing {
                        name: n.clone(),
                        rule,
                        group: g.clone(),
                        later: Vec::new(),
                    });
                }
                ShufItem::Row(verb, subject, object, later) => {
                    sh.allow.push(Row {
                        verb: verb.clone(),
                        subject: subject.clone(),
                        object: object.clone(),
                        later: later.clone(),
                    });
                }
                ShufItem::Coupled(p, r) => {
                    sh.coupled = Some(match r {
                        None => At::new(Rule::Const(true), *p),
                        Some(r) => At::new(Rule::Parsed(r.clone()), r.at),
                    });
                }
                ShufItem::Check(c) => sh.check = Some(c.clone()),
                ShufItem::Later(w) => sh.later.push(w.clone()),
            }
        }
        if let Some(p) = things_pos {
            sh.things = Some(At::new(Members::Names(things), p));
        }
        self.world.shuffles.push(sh);
    }

    /// Overlays, in the order their files apply (doc 18 section 2.8): every target named,
    /// a target that is gone an error, two replacements of one target an error unless one
    /// file uses the other.
    fn apply_overlays(&mut self) {
        let mut replaced: HashMap<String, (u32, Pos)> = HashMap::new();
        for (f, o) in std::mem::take(&mut self.overlays) {
            let (verb, pos, target) = match &o {
                Overlay::Extend(p, t, _) => ("extend", *p, t),
                Overlay::Replace(p, t, _) => ("replace", *p, t),
                Overlay::Remove(p, t) => ("remove", *p, t),
            };
            let label = match target {
                Target::Region(r) => format!("region {}", r.v),
                Target::Check(r, c) => format!("check {}.{}", r.v, c.v),
                Target::Exit(r, c) => format!("exit {}.{}", r.v, c.v),
                Target::Def(d) => format!("def {}", d.v),
                Target::Item(i) => format!("item {}", i.v),
            };
            if verb == "replace" {
                if let Some((g, at)) = replaced.get(&label).copied()
                    && !self.closure(f).contains(&g)
                    && !self.closure(g).contains(&f)
                {
                    self.errs.err(pos, format!("`replace {label}` is also replaced at line {}, and neither file uses the other", at.line), "add a `use` from the overlay that should win to the other");
                }
                replaced.insert(label.clone(), (f, pos));
            }
            let found = self.overlay(&o);
            if !found {
                self.errs.err(
                    pos,
                    format!(
                        "`{verb} {label}`: the base no longer has this {} (overlay.target_missing)",
                        label.split(' ').next().unwrap_or("target")
                    ),
                    "retarget or delete this overlay",
                );
            }
        }
    }

    /// Apply one overlay; `false` when its target does not exist.
    fn overlay(&mut self, o: &Overlay) -> bool {
        let region = |w: &mut World, r: &str| w.regions.iter().position(|x| x.key.v == r);
        match o {
            Overlay::Extend(_, Target::Region(r), members) => {
                let Some(i) = region(&mut self.world, &r.v) else { return false };
                let mut reg = self.world.regions[i].clone();
                let f = r.pos.file;
                for m in members {
                    self.member(f, m, &Env::new(), None, &mut reg);
                }
                self.world.regions[i] = reg;
                true
            }
            Overlay::Replace(_, t, rule) => {
                let f = rule.at.file;
                let Some(new) = self.rule(f, rule, &Env::new()) else { return true };
                match t {
                    Target::Check(r, c) => {
                        let Some(i) = region(&mut self.world, &r.v) else { return false };
                        let Some(ch) =
                            self.world.regions[i].checks.iter_mut().find(|x| x.key.v == c.v)
                        else {
                            return false;
                        };
                        ch.rule = new;
                    }
                    Target::Exit(r, c) => {
                        let Some(i) = region(&mut self.world, &r.v) else { return false };
                        let Some(x) =
                            self.world.regions[i].exits.iter_mut().find(|x| x.to.v == c.v)
                        else {
                            return false;
                        };
                        x.rule = new;
                    }
                    Target::Def(d) => {
                        let Some(def) = self.world.defs.iter_mut().find(|x| x.name.v == d.v) else {
                            return false;
                        };
                        def.rule = new;
                    }
                    _ => return false,
                }
                true
            }
            Overlay::Remove(_, t) => match t {
                Target::Region(r) => {
                    let Some(i) = region(&mut self.world, &r.v) else { return false };
                    self.world.regions.remove(i);
                    true
                }
                Target::Check(r, c) => {
                    let Some(i) = region(&mut self.world, &r.v) else { return false };
                    let before = self.world.regions[i].checks.len();
                    self.world.regions[i].checks.retain(|x| x.key.v != c.v);
                    before != self.world.regions[i].checks.len()
                }
                Target::Exit(r, c) => {
                    let Some(i) = region(&mut self.world, &r.v) else { return false };
                    let before = self.world.regions[i].exits.len();
                    self.world.regions[i].exits.retain(|x| x.to.v != c.v);
                    before != self.world.regions[i].exits.len()
                }
                Target::Def(d) => {
                    let before = self.world.defs.len();
                    self.world.defs.retain(|x| x.name.v != d.v);
                    before != self.world.defs.len()
                }
                Target::Item(n) => {
                    let before = self.world.items.len();
                    self.world.items.retain(|x| x.key.v != n.v);
                    before != self.world.items.len()
                }
            },
            Overlay::Extend(..) => false,
        }
    }

    /// A file reads a name declared in another file only if it uses that file, directly or
    /// through modules it uses, or the name is in the entry file (doc 18 section 2.5). An
    /// exit's target is not a read: regions in different files lead to each other, and a `use`
    /// each way would be a cycle for no gain.
    fn visibility(&mut self) {
        let w = &self.world;
        let mut declared: HashMap<String, Pos> = HashMap::new();
        let mut put = |k: String, p: Pos| {
            declared.entry(k).or_insert(p);
        };
        w.items.iter().for_each(|i| put(i.key.v.clone(), i.key.pos));
        w.options.iter().for_each(|o| put(format!("opt.{}", o.key.v), o.key.pos));
        w.defs.iter().for_each(|d| put(d.name.v.clone(), d.name.pos));
        w.item_groups.iter().for_each(|g| put(g.0.v.clone(), g.0.pos));
        w.placement.iter().for_each(|g| put(g.0.v.clone(), g.0.pos));
        w.shuffles.iter().for_each(|s| put(s.name.v.clone(), s.name.pos));
        for r in &w.regions {
            put(r.key.v.clone(), r.key.pos);
            r.checks.iter().for_each(|c| put(c.key.v.clone(), c.key.pos));
            r.events.iter().for_each(|e| put(e.0.v.clone(), e.0.pos));
        }
        let mut closures: HashMap<u32, BTreeSet<u32>> = HashMap::new();
        let mut seen = HashSet::new();
        for (f, name, at) in std::mem::take(&mut self.refs) {
            let key = match name.strip_prefix("at.") {
                Some(rest) => rest.split('.').next().unwrap_or("").to_owned(),
                None => name.clone(),
            };
            let Some(d) = declared.get(&key) else { continue };
            if d.file == f || d.file == 0 {
                continue;
            }
            let c = closures.entry(f).or_insert_with(|| self.closure(f));
            if !c.contains(&d.file) && seen.insert((f, d.file)) {
                let path = self.paths[d.file as usize].clone();
                let module = path.strip_suffix(EXT).unwrap_or(&path).to_owned();
                self.errs.err(
                    at,
                    format!("`{name}` is declared in `{path}`, which this file does not use"),
                    &format!("add `use {module}` to this file"),
                );
            }
        }
    }

    /// Each generated file's header (doc 18 section 2.9): its content hash, its sources' and
    /// its tables' promised sizes.
    fn generated(&mut self, files: &dyn Files, opts: &Options) {
        for f in self.order.clone() {
            let ast = &self.asts[f as usize];
            let Some(g) = ast.generated.clone() else { continue };
            let hash = content_hash(ast);
            match &g.content {
                None => self.errs.err(
                    g.pos,
                    "a generated file records its content's SHA-256",
                    "`content sha256 \"...\"`",
                ),
                Some(h) if h.v != hash => {
                    let msg = "this file was edited after it was generated";
                    let help = "put the change in an overlay, or regenerate the file";
                    if opts.allow_edited && !opts.publish {
                        self.errs.warn(h.pos, msg, help);
                    } else {
                        self.errs.err(h.pos, msg, help);
                    }
                }
                Some(_) => {}
            }
            for (path, want) in &g.sources {
                if let Some(Ok(text)) = files.read(&path.v)
                    && hex(&Sha256::digest(text.as_bytes())) != *want
                {
                    self.errs.warn(
                        path.pos,
                        format!("`{}` changed after this file was generated", path.v),
                        "regenerate it",
                    );
                }
            }
            for (t, n) in &g.expect {
                match self.tables.get(&t.v) {
                    Some(tab) if tab.file == f && tab.rows.len() as i64 >= *n => {}
                    Some(tab) if tab.file == f => {
                        let msg = format!(
                            "table `{}` has {} rows, and the generator promised at least {n}",
                            t.v,
                            tab.rows.len()
                        );
                        self.errs.err(
                            t.pos,
                            msg,
                            "the generator's input may have changed; check it before publishing",
                        );
                    }
                    _ => self.errs.err(t.pos, format!("this file declares no table `{}`", t.v), ""),
                }
            }
        }
    }
}

/// A rule simplified with what is known at build time.
enum Simp {
    Const(bool),
    Rule(Expr),
}

/// `e` with constants folded: comparisons of numbers or of plain names, `and`, `or`, `not`.
fn simplify(e: Expr) -> Simp {
    match e.e {
        E::Bool(b) => Simp::Const(b),
        E::Cmp(op, ref a, ref b) => {
            let names = match (&a.e, &b.e) {
                (E::Name(x), E::Name(y)) if !x.contains('.') && !y.contains('.') => Some(x.cmp(y)),
                _ => None,
            };
            let ord = names.or_else(|| Some(fold(a)?.cmp(&fold(b)?)));
            match ord {
                Some(o) => Simp::Const(match op {
                    Cmp::Eq => o.is_eq(),
                    Cmp::Ne => o.is_ne(),
                    Cmp::Lt => o.is_lt(),
                    Cmp::Le => o.is_le(),
                    Cmp::Gt => o.is_gt(),
                    Cmp::Ge => o.is_ge(),
                }),
                None => Simp::Rule(e),
            }
        }
        E::Not(ref a) => match simplify((**a).clone()) {
            Simp::Const(b) => Simp::Const(!b),
            Simp::Rule(_) => Simp::Rule(e),
        },
        E::And(ref xs) | E::Or(ref xs) => {
            let is_and = matches!(e.e, E::And(_));
            let mut kept = Vec::new();
            for x in xs.clone() {
                match simplify(x) {
                    Simp::Const(b) if b == is_and => {}
                    Simp::Const(b) => return Simp::Const(b),
                    Simp::Rule(r) => kept.push(r),
                }
            }
            match kept.len() {
                0 => Simp::Const(is_and),
                1 => Simp::Rule(kept.remove(0)),
                _ => Simp::Rule(Expr { e: if is_and { E::And(kept) } else { E::Or(kept) }, ..e }),
            }
        }
        _ => Simp::Rule(e),
    }
}

/// `a and b`, or `b` alone.
fn and(a: Option<Expr>, b: Expr) -> Expr {
    match a {
        None => b,
        Some(a) => {
            let (at, span) = (a.at, a.span);
            let wrap = |mut x: Expr| {
                x.paren = true;
                x
            };
            Expr { e: E::And(vec![wrap(a), wrap(b)]), at, paren: false, span }
        }
    }
}
