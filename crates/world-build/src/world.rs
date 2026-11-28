//! The checks and the manifest: a [`World`] in, the engine's manifest and the per-seed model
//! out, with every error the world has (design doc 16 sections 3 to 6, doc 17 section 5 as the
//! engine's first shuffle slice reads it).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::lex::CHECK_MODIFIERS;
use crate::lower::{Lower, Mode, Names, OptTy, Readable, Sort, truth};
use crate::model::{At, Lit, Members, Pos, Rule, Verb, World};
use crate::{Built, Error, instance};

/// The format line this version reads.
pub(crate) const FORMAT: &str = "cgm-data/1";

/// The platform's filler item (doc 16 section 3.4).
pub(crate) const NOTHING: &str = "nothing";

/// Words a name may not be: the rule language's own, the filler item, and the prefixes.
const RESERVED: [&str; 11] =
    ["and", "or", "not", "in", "div", "mod", "true", "false", NOTHING, "opt", "at"];

/// Shuffle names that key the engine's own random streams (doc 17 section 4.3).
const RESERVED_SHUFFLES: [&str; 2] = ["layout", "own_fill"];

/// Errors and warnings collected over one build.
#[derive(Debug, Default)]
pub(crate) struct Errs {
    pub(crate) files: Vec<String>,
    pub errors: Vec<Error>,
    pub warnings: Vec<Error>,
    /// Errors past the cap, dropped.
    dropped: usize,
    /// Warnings past the cap, dropped.
    dropped_warnings: usize,
    /// Every message given, so one mistake a family repeats is reported once.
    seen: HashSet<(Option<Pos>, String)>,
}

impl Errs {
    pub(crate) fn new(files: Vec<String>) -> Self {
        Errs { files, ..Errs::default() }
    }

    fn make(&self, pos: Option<Pos>, msg: impl Into<String>, help: &str) -> Error {
        let help = (!help.is_empty()).then(|| help.to_owned());
        let file = pos.and_then(|p| self.files.get(p.file as usize)).or(self.files.first());
        Error { file: file.cloned().unwrap_or_default(), pos, msg: msg.into(), help }
    }

    /// An error at `pos`. Past [`config::WORLD_BUILD_MAX_ERRORS`] errors (or warnings) the rest
    /// are dropped and [`Errs::finish`] says how many.
    pub(crate) fn err(&mut self, pos: Pos, msg: impl Into<String>, help: &str) {
        self.push(Some(pos), msg, help);
    }

    /// An error about the whole file.
    pub(crate) fn whole(&mut self, msg: impl Into<String>, help: &str) {
        self.push(None, msg, help);
    }

    /// An error or warning about the whole of `file`, a file that is not a `.rando` module.
    pub(crate) fn in_file(&mut self, file: &str, msg: impl Into<String>, help: &str, warn: bool) {
        let help = (!help.is_empty()).then(|| help.to_owned());
        let e = Error { file: file.to_owned(), pos: None, msg: msg.into(), help };
        if warn { self.warnings.push(e) } else { self.errors.push(e) }
    }

    fn push(&mut self, pos: Option<Pos>, msg: impl Into<String>, help: &str) {
        let msg = msg.into();
        if !self.seen.insert((pos, msg.clone())) {
            return;
        }
        if self.errors.len() < config::WORLD_BUILD_MAX_ERRORS {
            let e = self.make(pos, msg, help);
            self.errors.push(e);
        } else {
            self.dropped += 1;
        }
    }

    pub(crate) fn warn(&mut self, pos: Pos, msg: impl Into<String>, help: &str) {
        self.warning(Some(pos), msg, help);
    }

    pub(crate) fn warn_whole(&mut self, msg: impl Into<String>, help: &str) {
        self.warning(None, msg, help);
    }

    fn warning(&mut self, pos: Option<Pos>, msg: impl Into<String>, help: &str) {
        let msg = msg.into();
        if !self.seen.insert((pos, msg.clone())) {
            return;
        }
        if self.warnings.len() < config::WORLD_BUILD_MAX_ERRORS {
            let e = self.make(pos, msg, help);
            self.warnings.push(e);
        } else {
            self.dropped_warnings += 1;
        }
    }

    /// The warnings, and one more line for what the cap dropped.
    pub(crate) fn warnings_list(mut self) -> Vec<Error> {
        if self.dropped_warnings > 0 {
            let e =
                self.make(None, format!("{} more warnings not shown", self.dropped_warnings), "");
            self.warnings.push(e);
        }
        self.warnings
    }

    /// The errors ([`Errs::finish`]) and the warnings ([`Errs::warnings_list`]).
    pub(crate) fn split(mut self) -> (Vec<Error>, Vec<Error>) {
        let w = Errs {
            files: self.files.clone(),
            warnings: std::mem::take(&mut self.warnings),
            dropped_warnings: self.dropped_warnings,
            ..Errs::default()
        };
        (self.finish(), w.warnings_list())
    }

    /// The errors in file order, and one more line for what the cap dropped.
    pub(crate) fn finish(mut self) -> Vec<Error> {
        self.errors.sort_by_key(|e| e.pos);
        if self.dropped > 0 {
            let e = self.make(
                None,
                format!("{} more errors not shown", self.dropped),
                "fix these and build again",
            );
            self.errors.push(e);
        }
        self.errors
    }
}

/// An option as the per-seed step reads it.
#[derive(Debug, Clone)]
pub(crate) struct OptM {
    pub key: String,
    pub ty: OptTy,
    pub default: i64,
}

/// A check as the per-seed step reads it: its rules already lowered (options only).
#[derive(Debug, Clone)]
pub(crate) struct CheckM {
    pub key: String,
    pub when: Value,
    pub no_progression: Value,
    pub holds: Option<String>,
    pub set: Option<String>,
}

/// A start or assumed grant, lowered.
#[derive(Debug, Clone)]
pub(crate) struct GrantM {
    pub item: String,
    pub n: i64,
    pub when: Value,
}

/// What the per-seed step needs: the world-side fields the manifest has no place for (doc 16
/// section 4), which the engine reads per seed as its instance.
#[derive(Debug, Clone, Default)]
pub(crate) struct Model {
    pub options: Vec<OptM>,
    /// Key, class, count; in key order.
    pub items: Vec<(String, String, i64)>,
    /// Copies an options-only number sets, by item, in place of the count in `items`.
    pub copies: BTreeMap<String, Value>,
    /// The fill and its weights, in key order.
    pub fill_with: Vec<(String, i64)>,
    pub checks: Vec<CheckM>,
    pub start_items: Vec<GrantM>,
    pub assumed: Vec<GrantM>,
    /// Item to the placement set it is confined to.
    pub confined: BTreeMap<String, String>,
    /// The lowered defs, which settled fields may name.
    pub defs: BTreeMap<String, Value>,
}

/// The shape of a name the rule language can read: `[a-z_][a-z0-9_]*` with at least one
/// letter, at most [`config::MANIFEST_NAME_MAX_BYTES`]. The one rule for `.rando` identifiers.
pub(crate) fn name_shape(s: &str) -> bool {
    s.len() <= config::MANIFEST_NAME_MAX_BYTES
        && s.bytes().any(|b| b.is_ascii_lowercase())
        && s.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A game key: lowercase letters, digits, `_` and `-`, with at least one letter, at most
/// [`config::MANIFEST_NAME_MAX_BYTES`]. The one rule for the `world` line and `rando.toml`'s
/// `name`; looser than [`name_shape`] on purpose, since a game key is never read in a rule.
pub(crate) fn game_key_ok(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_lowercase())
        && s.len() <= config::MANIFEST_NAME_MAX_BYTES
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A name the rule language can read: [`name_shape`], not reserved.
fn name_ok(errs: &mut Errs, n: &At<String>, what: &str) -> bool {
    let s = n.v.as_str();
    let most = config::MANIFEST_NAME_MAX_BYTES;
    if !name_shape(s) {
        errs.err(
            n.pos,
            format!("`{s}` is not a valid {what} name"),
            &format!("a name is lowercase letters, digits and `_`, starts with a letter or `_`, holds at least one letter, at most {most} bytes"),
        );
        return false;
    }
    if s.contains("__") {
        errs.err(
            n.pos,
            format!("`{s}` holds `__`, which the build keeps for the defs it writes"),
            "",
        );
        return false;
    }
    if RESERVED.contains(&s) {
        errs.err(n.pos, format!("`{s}` is a reserved word and cannot name {what}"), "");
        return false;
    }
    true
}

/// A field the model carries but this version does not build: why, by field.
fn later(errs: &mut Errs, fields: &[At<String>], whose: &str) {
    for f in fields {
        let help = match f.v.as_str() {
            "rolls" => "a roll's seed draw waits on the random stream decision (D-47)",
            "contexts" | "stays" => {
                "time contexts are proposed (D-129) and not built in the tool yet"
            }
            "require" => {
                "world invariants are not in the engine's first shuffle slice (doc 17 section 5.3)"
            }
            "from_vanilla" => "a pool from vanilla items is an open gap (doc 16 section 4.1)",
            _ => "",
        };
        errs.err(f.pos, format!("{whose} `{}` is not supported yet", f.v), help);
    }
}

/// The world's one namespace while it is built: each name, what it is and where it was first
/// declared. Events may repeat across regions; nothing else may.
#[derive(Default)]
struct Space {
    first: HashMap<String, (Sort, Pos)>,
}

impl Space {
    fn declare(&mut self, errs: &mut Errs, n: &At<String>, sort: Sort) -> bool {
        if !name_ok(errs, n, sort.word().trim_start_matches("an ").trim_start_matches("a ")) {
            return false;
        }
        // Events are state, which `no_progression` never reads, so only defs can clash.
        if sort == Sort::Def && CHECK_MODIFIERS.contains(&n.v.as_str()) {
            errs.err(
                n.pos,
                format!("`{}` starts a check modifier, so it cannot name {}", n.v, sort.word()),
                "a rule reads a def by its bare name, and after `no_progression` this word starts the next modifier; pick another name",
            );
            return false;
        }
        match self.first.get(&n.v) {
            Some((Sort::Event, _)) if sort == Sort::Event => true,
            Some((was, at)) => {
                let file = errs.files.get(at.file as usize).cloned().unwrap_or_default();
                errs.err(
                    n.pos,
                    format!("`{}` is already {} ({file}:{}:{})", n.v, was.word(), at.line, at.col),
                    "items, defs, events, regions, item groups, placement sets and shuffles share one namespace (checks have their own)",
                );
                false
            }
            None => {
                self.first.insert(n.v.clone(), (sort, n.pos));
                true
            }
        }
    }
}

/// `{"all": [a, b]}`, or just one of them when the other is true.
fn both(a: &Value, b: Value) -> Value {
    if *a == truth() {
        b
    } else if b == truth() {
        a.clone()
    } else {
        json!({"all": [a, b]})
    }
}

/// Build `w`, read from `file`.
pub(crate) fn build(w: &World, file: &str) -> Result<Built, Vec<Error>> {
    // The checks recurse once per rule node, to a bounded depth; a thread with a stack sized
    // for that bound keeps it off the caller's stack, whatever that stack is.
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(config::WORLD_BUILD_STACK_BYTES)
            .spawn_scoped(s, || build_here(w, file))
            .map_or_else(
                |e| {
                    Err(vec![Error {
                        file: file.to_owned(),
                        pos: None,
                        msg: format!("cannot start the build: {e}"),
                        help: None,
                    }])
                },
                |h| {
                    h.join().unwrap_or_else(|_| {
                        Err(vec![Error {
                            file: file.to_owned(),
                            pos: None,
                            msg: "the build stopped unexpectedly".into(),
                            help: Some("please report it with the world file".into()),
                        }])
                    })
                },
            )
    })
}

fn build_here(w: &World, file: &str) -> Result<Built, Vec<Error>> {
    let files = if w.files.is_empty() { vec![file.to_owned()] } else { w.files.clone() };
    let mut errs = Errs::new(files);
    let out = check(w, &mut errs);
    if errs.errors.is_empty()
        && let Some((manifest, model)) = out
    {
        if let Err(e) = engine_check(&manifest) {
            // The engine counts depth over its own nodes, which a short form (a toggle, an
            // event, `in`, a shuffle read) turns into one or two more than the build counts:
            // past the build's own cap the engine's check is the one that decides.
            if e.contains("nested more than") {
                errs.whole(
                    format!(
                        "{e} in the engine's form, counting the defs and shuffle reads it reaches"
                    ),
                    "flatten a def chain, or split the rule",
                );
            } else {
                errs.whole(
                    format!("the engine refused the built manifest: {e}"),
                    "the build's own checks missed this; please report it with the world file",
                );
            }
        } else {
            instance::pool_test(&model, &mut errs);
        }
        match crate::canon::to_string(&manifest) {
            Ok(text) if errs.errors.is_empty() => {
                return Ok(Built {
                    manifest,
                    warnings: errs.warnings_list(),
                    text,
                    model,
                    ledger: None,
                    meta: None,
                });
            }
            Ok(_) => {}
            Err(e) => errs.whole(format!("the manifest is not canonical: {e}"), ""),
        }
    }
    Err(errs.finish())
}

fn check(w: &World, errs: &mut Errs) -> Option<(Value, Model)> {
    later(errs, &w.later, "field");
    // Header.
    match &w.format {
        Some(f) if f.v == FORMAT => {}
        Some(f) => errs.err(
            f.pos,
            format!("format `{}` is not one this tool reads", f.v),
            &format!("write `{FORMAT}`"),
        ),
        None => errs.whole("the world has no `format`", &format!("write `format: {FORMAT}` first")),
    }
    let game = match &w.game {
        Some(g) => {
            if !game_key_ok(&g.v) {
                errs.err(
                    g.pos,
                    format!("`{}` is not a game key", g.v),
                    "a game key is lowercase letters, digits, `_` and `-`, with at least one letter",
                );
            }
            g.v.clone()
        }
        None => {
            errs.whole("the world has no `game`", "add `game: <key>`");
            String::new()
        }
    };
    let version = w.version.as_ref().map_or(1, |v| {
        if v.v < 1 {
            errs.err(v.pos, "the version is a whole number from 1", "");
        }
        v.v
    });
    for r in &w.requires {
        errs.err(
            r.pos,
            format!("extension `{}` is not defined", r.v),
            "version 1 of the format defines no extensions",
        );
    }

    // Options: their own namespace, read as `opt.<key>`.
    let mut names = Names::default();
    let mut options = Vec::new();
    let most = config::WORLD_OPTIONS_MAX;
    if w.options.len() > most {
        errs.err(
            w.options[most].key.pos,
            format!("{} options; the engine reads at most {most}", w.options.len()),
            "",
        );
    }
    for o in &w.options {
        if !name_ok(errs, &o.key, "option") {
            continue;
        }
        if names.options.contains_key(&o.key.v) {
            errs.err(o.key.pos, format!("option `{}` is declared twice", o.key.v), "");
            continue;
        }
        let bad_default = |errs: &mut Errs, want: &str| {
            if let Some(d) = &o.default {
                errs.err(d.pos, format!("the default of `{}` must be {want}", o.key.v), "");
            }
        };
        let (ty, default) = match o.ty.v.as_str() {
            "toggle" => match o.default.as_ref().map(|d| &d.v) {
                None | Some(Lit::Bool(false)) => (OptTy::Toggle, 0),
                Some(Lit::Bool(true)) => (OptTy::Toggle, 1),
                _ => {
                    bad_default(errs, "true or false");
                    (OptTy::Toggle, 0)
                }
            },
            "choice" => {
                let mut values: Vec<String> = Vec::new();
                for v in &o.values {
                    if values.contains(&v.v) {
                        errs.err(v.pos, format!("value `{}` is listed twice", v.v), "");
                    } else if name_ok(errs, v, "choice value") || v.v == NOTHING {
                        values.push(v.v.clone());
                    }
                }
                if o.values.is_empty() {
                    errs.err(
                        o.ty.pos,
                        format!("choice `{}` has no values", o.key.v),
                        "list them: `choice(a, b)`",
                    );
                }
                let d = match o.default.as_ref().map(|d| &d.v) {
                    None => 0,
                    Some(Lit::Name(n)) if values.contains(n) => {
                        values.iter().position(|x| x == n).unwrap_or(0)
                    }
                    _ => {
                        bad_default(errs, "one of its values");
                        0
                    }
                };
                (OptTy::Choice(values), i64::try_from(d).unwrap_or(0))
            }
            "range" => {
                let (Some(lo), Some(hi)) = (&o.min, &o.max) else {
                    errs.err(o.ty.pos, format!("range `{}` needs `min` and `max`", o.key.v), "");
                    continue;
                };
                let fits = |n: i64| i32::try_from(n).is_ok();
                if lo.v > hi.v || !fits(lo.v) || !fits(hi.v) {
                    errs.err(
                        lo.pos,
                        "a range's `min` is at most its `max`, both 32-bit numbers",
                        "",
                    );
                }
                let d = match o.default.as_ref().map(|d| &d.v) {
                    None => lo.v,
                    Some(Lit::Int(n)) if (lo.v..=hi.v).contains(n) => *n,
                    _ => {
                        bad_default(errs, &format!("a number from {} to {}", lo.v, hi.v));
                        lo.v
                    }
                };
                (OptTy::Range(lo.v, hi.v), d)
            }
            t => {
                errs.err(
                    o.ty.pos,
                    format!("option type `{t}` is not supported yet"),
                    "this version reads toggle, choice and range",
                );
                continue;
            }
        };
        names.options.insert(o.key.v.clone(), ty.clone());
        options.push(OptM { key: o.key.v.clone(), ty, default });
    }

    // Items.
    let mut space = Space::default();
    let fill: HashSet<&str> = w.fill_with.iter().map(|(k, _)| k.v.as_str()).collect();
    let mut items: Vec<(String, String, i64)> = Vec::new();
    for it in &w.items {
        later(errs, &it.later, "item field");
        if !space.declare(errs, &it.key, Sort::Item) {
            continue;
        }
        let class = match &it.class {
            Some(c) if ["progression", "useful", "filler", "trap"].contains(&c.v.as_str()) => {
                c.v.clone()
            }
            Some(c) => {
                errs.err(
                    c.pos,
                    format!("class `{}` is not one of progression, useful, filler, trap", c.v),
                    "",
                );
                continue;
            }
            None => {
                errs.err(
                    it.key.pos,
                    format!("item `{}` has no class", it.key.v),
                    "give it `class: progression`, `useful`, `filler` or `trap`",
                );
                continue;
            }
        };
        let count = match &it.count {
            Some(c) if (0..=config::WORLD_ITEM_COPIES_MAX).contains(&c.v) => c.v,
            Some(c) => {
                errs.err(
                    c.pos,
                    format!("count {} is outside 0 to {}", c.v, config::WORLD_ITEM_COPIES_MAX),
                    "doc 16 section 5.3 bounds copies of one item",
                );
                continue;
            }
            None if it.count_rule.is_some() => 0,
            None => i64::from(!fill.contains(it.key.v.as_str())),
        };
        items.push((it.key.v.clone(), class, count));
    }
    if w.items.is_empty() {
        errs.whole("the world has no items", "a world needs at least one item");
    }
    let mut fill_with = Vec::new();
    let mut seen = HashSet::new();
    for (k, wt) in &w.fill_with {
        if !items.iter().any(|i| i.0 == k.v) {
            errs.err(k.pos, format!("`{}` in `fill_with` is not an item", k.v), "");
        } else if !seen.insert(k.v.clone()) {
            errs.err(k.pos, format!("`{}` is in `fill_with` twice", k.v), "");
        } else if wt.v < 1 || wt.v > config::WORLD_ITEM_COPIES_MAX {
            errs.err(
                wt.pos,
                format!(
                    "a fill weight is a whole number from 1 to {}",
                    config::WORLD_ITEM_COPIES_MAX
                ),
                "",
            );
        } else {
            fill_with.push((k.v.clone(), wt.v));
        }
    }
    fill_with.sort();
    let item_set: HashSet<String> = items.iter().map(|i| i.0.clone()).collect();
    let is_item = |k: &str| item_set.contains(k);

    // Groups and placement sets.
    for (g, members) in &w.item_groups {
        if !space.declare(errs, g, Sort::ItemGroup) {
            continue;
        }
        if members.is_empty() {
            errs.err(g.pos, format!("item group `{}` is empty", g.v), "");
        }
        let mut seen = HashSet::new();
        for m in members {
            if !is_item(&m.v) {
                errs.err(m.pos, format!("`{}` in item group `{}` is not an item", m.v, g.v), "");
            } else if !seen.insert(m.v.as_str()) {
                errs.err(m.pos, format!("`{}` is in item group `{}` twice", m.v, g.v), "");
            }
        }
        if members.len() > config::RULE_MAX_PARTS {
            errs.err(
                g.pos,
                format!(
                    "item group `{}` has {} items; a sum the engine reads has at most {}",
                    g.v,
                    members.len(),
                    config::RULE_MAX_PARTS
                ),
                "",
            );
        }
        names.item_groups.insert(g.v.clone(), members.iter().map(|m| m.v.clone()).collect());
    }
    let mut confined = BTreeMap::new();
    let mut sets = HashSet::new();
    for (s, members) in &w.placement {
        if !space.declare(errs, s, Sort::Placement) {
            continue;
        }
        sets.insert(s.v.clone());
        for m in members {
            if !is_item(&m.v) {
                errs.err(m.pos, format!("`{}` in placement set `{}` is not an item", m.v, s.v), "");
            } else if let Some(other) = confined.insert(m.v.clone(), s.v.clone()) {
                errs.err(
                    m.pos,
                    format!("`{}` is already confined to placement set `{other}`", m.v),
                    "an item is confined to one set",
                );
            }
        }
    }

    // Regions, checks, events, defs, shuffles: declared before any rule is read.
    let mut checks_seen: HashSet<&str> = HashSet::new();
    for r in &w.regions {
        space.declare(errs, &r.key, Sort::Region);
        later(errs, &r.later, "region field");
        let mut here: HashMap<&str, Pos> = HashMap::new();
        for c in &r.checks {
            later(errs, &c.later, "check field");
            if let Some(at) = here.get(c.key.v.as_str()) {
                let file = errs.files.get(at.file as usize).cloned().unwrap_or_default();
                errs.err(
                    c.key.pos,
                    format!(
                        "`{}` is already a check in region `{}` ({file}:{}:{})",
                        c.key.v, r.key.v, at.line, at.col
                    ),
                    "a check is declared once in each region it can be reached from",
                );
            } else {
                here.insert(c.key.v.as_str(), c.key.pos);
                // Declared again in another region: the same check, reachable from both.
                // Checks have their own namespace: rules never read them.
                if checks_seen.insert(c.key.v.as_str()) {
                    name_ok(errs, &c.key, "check");
                }
            }
        }
        for (e, _) in &r.events {
            space.declare(errs, e, Sort::Event);
        }
    }
    if w.regions.len() > config::WORLD_TABLE_MAX {
        errs.whole(
            format!(
                "{} regions; the engine indexes at most {}",
                w.regions.len(),
                config::WORLD_TABLE_MAX
            ),
            "",
        );
    }
    if w.regions.is_empty() {
        errs.whole("the world has no regions", "");
    }
    for d in &w.defs {
        if space.declare(errs, &d.name, Sort::Def) {
            let mut params: Vec<String> = Vec::new();
            for p in &d.params {
                if !name_ok(errs, p, "parameter") {
                } else if params.contains(&p.v) {
                    errs.err(p.pos, format!("parameter `{}` is listed twice", p.v), "");
                } else {
                    params.push(p.v.clone());
                }
            }
            names.defs.insert(d.name.v.clone(), (params, d.rule.clone()));
        }
    }
    for sh in &w.shuffles {
        let n = &sh.name;
        let most = config::SHUFFLE_NAME_MAX_BYTES;
        if n.v.len() > most {
            errs.err(n.pos, format!("a shuffle name is at most {most} bytes"), "");
            continue;
        }
        if RESERVED_SHUFFLES.contains(&n.v.as_str()) {
            errs.err(n.pos, format!("`{}` is reserved for the engine's own draws", n.v), "");
            continue;
        }
        if !space.declare(errs, n, Sort::Shuffle) {
            continue;
        }
        let readable = match (&sh.slots.v, &sh.things) {
            (Members::Names(slots), Some(At { v: Members::Names(things), .. })) => {
                Readable::Names {
                    slots: slots.iter().map(|s| s.name.v.clone()).collect(),
                    things: things.iter().map(|t| (t.name.v.clone(), t.rule.is_some())).collect(),
                }
            }
            _ => Readable::Layout,
        };
        names.shuffles.insert(n.v.clone(), readable);
    }
    names.sorts = space.first.iter().map(|(k, (s, _))| (k.clone(), *s)).collect();
    names.checks = checks_seen.iter().map(|c| (*c).to_owned()).collect();
    let region_of: HashSet<&str> = w.regions.iter().map(|r| r.key.v.as_str()).collect();
    let shuffled = !w.shuffles.is_empty();

    let mut lw = Lower::new(&names, errs);
    lw.all_defs();
    // Copies that read options, settled per seed.
    let mut copies: BTreeMap<String, Value> = BTreeMap::new();
    for it in &w.items {
        if let Some(r) = &it.count_rule
            && let Some(v) = lw.number(r, "a number of copies")
        {
            copies.insert(it.key.v.clone(), v);
        }
    }

    // Regions to the manifest.
    let mut regions = Vec::new();
    let mut checks_m = Vec::new();
    let mut locations = Vec::new();
    let mut held_by: HashMap<String, Vec<Pos>> = HashMap::new();
    let mut exit_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut exit_keys: BTreeSet<String> = BTreeSet::new();
    let merged = merge_checks(lw.errs(), w);
    let firsts: Vec<&crate::model::Check> = w
        .regions
        .iter()
        .flat_map(|r| &r.checks)
        .filter(|c| merged.get(c.key.v.as_str()).is_some_and(|m| std::ptr::eq(m.first, *c)))
        .collect();
    let check_ids =
        ids(lw.errs(), &firsts.iter().map(|c| c.id.as_ref()).collect::<Vec<_>>(), "check");
    let check_id: HashMap<&str, i64> =
        firsts.iter().map(|c| c.key.v.as_str()).zip(check_ids).collect();
    // Each region's condition first: an exit into a region carries the region's condition
    // too, so a region that does not exist this seed has no way in.
    let mut present: HashMap<&str, Value> = HashMap::new();
    for r in &w.regions {
        let when = match &r.when {
            Some(x) => lw.rule(x, Mode::Static("when")).map_or_else(truth, |v| v.0),
            None => truth(),
        };
        if when != truth() && w.start.as_ref().is_some_and(|s| s.v == r.key.v) {
            lw.errs().err(
                r.key.pos,
                format!("the start region always exists; `{}` cannot have a `when`", r.key.v),
                "move the `when` onto its checks",
            );
        }
        present.entry(r.key.v.as_str()).or_insert(when);
    }
    let when_of = |k: &str| present.get(k).cloned().unwrap_or_else(truth);
    let mut sites: Vec<(Pos, Option<&str>, Value, Value)> = Vec::new();
    // Exit keys that lead into or out of a region with a `when`: no shuffle may move them.
    let mut optional_exits: BTreeSet<String> = BTreeSet::new();
    let mut fired: HashMap<&str, Vec<&str>> = HashMap::new();
    for r in &w.regions {
        let when = when_of(&r.key.v);
        let set_ok = |lw: &mut Lower<'_>, s: &Option<At<String>>| match s {
            Some(s) if !sets.contains(&s.v) => {
                lw.errs().err(
                    s.pos,
                    format!("`{}` is not a placement set", s.v),
                    "declare it with `placement NAME { items }`",
                );
                None
            }
            s => s.as_ref().map(|s| s.v.clone()),
        };
        let mut checks = Vec::new();
        for c in &r.checks {
            let rule = lw.rule(&c.rule, Mode::Rule).map_or_else(truth, |v| v.0);
            // The site carries its region's condition: a removed region is still reached
            // (by the exit from the start), and its sites must give nothing then.
            checks.push(json!({"loc": c.key.v, "rule": both(&when, rule.clone())}));
            let Some(m) = merged.get(c.key.v.as_str()) else { continue };
            let cwhen = match m.when {
                Some(x) => lw.rule(x, Mode::Static("when")).map_or_else(truth, |v| v.0),
                None => truth(),
            };
            sites.push((c.rule.pos, Some(r.key.v.as_str()), cwhen.clone(), rule));
            if !std::ptr::eq(m.first, c) {
                continue;
            }
            let no_progression = match m.no_progression {
                Some(x) => lw.rule(x, Mode::Static("no_progression")).map_or_else(truth, |v| v.0),
                None => json!({"false": {}}),
            };
            let mut loc =
                json!({"id": check_id.get(c.key.v.as_str()).copied().unwrap_or(0), "key": c.key.v});
            for (field, it) in [("vanilla", m.vanilla), ("holds", m.holds)] {
                let Some(it) = it else { continue };
                if !is_item(&it.v) {
                    lw.errs().err(it.pos, format!("`{field}: {}` is not an item", it.v), "");
                } else if field == "vanilla" {
                    loc["vanilla"] = json!(it.v);
                } else {
                    held_by.entry(it.v.clone()).or_default().push(it.pos);
                }
            }
            // The check exists where at least one of its regions does.
            let mut wheres: Vec<Value> = Vec::new();
            for q in &m.regions {
                let v = when_of(&q.key.v);
                if !wheres.contains(&v) {
                    wheres.push(v);
                }
            }
            let exists = if wheres.contains(&truth()) || wheres.is_empty() {
                truth()
            } else if wheres.len() == 1 {
                wheres.remove(0)
            } else {
                json!({"any": wheres})
            };
            let mut region_sets: Vec<Option<String>> =
                m.regions.iter().map(|q| set_ok(&mut lw, &q.placement)).collect();
            region_sets.dedup();
            if region_sets.len() > 1 && m.placement.is_none() {
                lw.errs().err(
                    c.key.pos,
                    format!("check `{}` is in regions of different placement sets", c.key.v),
                    "give the check its own `placement`",
                );
            }
            let set = set_ok(&mut lw, &m.placement.cloned()).or_else(|| region_sets.remove(0));
            locations.push(loc);
            checks_m.push(CheckM {
                key: c.key.v.clone(),
                when: both(&exists, cwhen),
                no_progression,
                holds: m.holds.map(|h| h.v.clone()),
                set,
            });
        }
        let mut events = Vec::new();
        for (e, rule) in &r.events {
            let rule = lw.rule(rule, Mode::Rule).map_or_else(truth, |v| v.0);
            sites.push((e.pos, Some(r.key.v.as_str()), truth(), rule.clone()));
            fired.entry(e.v.as_str()).or_default().push(r.key.v.as_str());
            events.push(json!({"event": e.v, "rule": both(&when, rule)}));
        }
        let mut exits = Vec::new();
        // Several exits to one region are several ways in. A shuffle keys each exit: each of
        // them is keyed by its door when it has one, by its place among them otherwise.
        let mut ways: HashMap<&str, usize> = HashMap::new();
        r.exits.iter().for_each(|x| *ways.entry(x.to.v.as_str()).or_default() += 1);
        let mut doors_seen: HashSet<(&str, &str)> = HashSet::new();
        let mut nth: HashMap<&str, usize> = HashMap::new();
        for x in &r.exits {
            let parallel = ways.get(x.to.v.as_str()).copied().unwrap_or(0) > 1;
            let place = {
                let k = nth.entry(x.to.v.as_str()).or_default();
                *k += 1;
                *k
            };
            if !region_of.contains(x.to.v.as_str()) {
                let help = crate::suggest::help(&x.to.v, region_of.iter().copied(), "");
                lw.errs().err(
                    x.to.pos,
                    format!("exit to `{}`, which is not a region", x.to.v),
                    &help,
                );
            } else if let Some(d) = x.door.as_ref().filter(|_| parallel)
                && !doors_seen.insert((x.to.v.as_str(), d.v.as_str()))
            {
                lw.errs().err(
                    d.pos,
                    format!(
                        "region `{}` has two exits to `{}` through door `{}`",
                        r.key.v, x.to.v, d.v
                    ),
                    "each exit to one region names its own door",
                );
            }
            let rule = lw.rule(&x.rule, Mode::Rule).map_or_else(truth, |v| v.0);
            let to_when = when_of(&x.to.v);
            // The exit exists where its target does too: gone with a removed target.
            sites.push((x.rule.pos, Some(r.key.v.as_str()), to_when.clone(), rule.clone()));
            let mut e = json!({"rule": both(&when, both(&to_when, rule)), "to": x.to.v});
            let key = match (&x.door, parallel) {
                (Some(d), true) => format!("{}:{}:{}", r.key.v, x.to.v, d.v),
                (None, true) => format!("{}:{}:{place}", r.key.v, x.to.v),
                _ => format!("{}:{}", r.key.v, x.to.v),
            };
            if when != truth() || to_when != truth() {
                optional_exits.insert(key.clone());
                // A door pairs exits for the engine whether a shuffle moves them or not.
                if let Some(d) = &x.door {
                    lw.errs().err(
                        d.pos,
                        "an exit into or out of a region with a `when` cannot have a `door` yet",
                        "a shuffle that changes with the options is not in the engine's first slice; keep the region, and put the `when` on its checks",
                    );
                }
            }
            for (field, v) in [("door", &x.door), ("group", &x.group)] {
                if let Some(v) = v {
                    name_ok(lw.errs(), v, field);
                }
                if shuffled {
                    e[field] = v.as_ref().map_or(Value::Null, |v| json!(v.v));
                } else if let Some(v) = v {
                    lw.errs().err(v.pos, format!("a `{field}` means something only to a shuffle, and the world declares none"), "");
                }
            }
            if let Some(g) = &x.group {
                exit_groups.entry(g.v.clone()).or_default().push(key.clone());
            }
            if shuffled {
                e["key"] = json!(key);
            }
            exit_keys.insert(key);
            exits.push(e);
        }
        let by = |f: &'static str| move |a: &Value, b: &Value| a[f].as_str().cmp(&b[f].as_str());
        checks.sort_by(by("loc"));
        events.sort_by(by("event"));
        exits.sort_by(by("to"));
        regions.push(json!({
            "can_stay": r.can_stay, "checks": checks, "events": events, "exits": exits,
            "key": r.key.v, "stays": [],
        }));
    }
    // The engine reaches every region of the manifest in a world with a shuffle (it has no
    // way to say a region does not exist this seed): a region that is gone is reached from the
    // start by an exit that opens only then. Nothing in it exists, and its own exits are shut.
    if let Some(s) = &w.start
        && let Some(start) = regions.iter_mut().find(|x| x["key"] == s.v.as_str())
        && let Some(list) = start["exits"].as_array_mut()
    {
        for r in w.regions.iter().filter(|r| r.key.v != s.v) {
            let when = when_of(&r.key.v);
            if when != truth() {
                list.push(json!({"rule": {"not": when}, "to": r.key.v}));
            }
        }
        list.sort_by(|a, b| a["to"].as_str().cmp(&b["to"].as_str()));
    }
    let counts: HashMap<&str, i64> = items.iter().map(|i| (i.0.as_str(), i.2)).collect();
    for (item, places) in held_by.iter().filter(|(i, _)| !copies.contains_key(*i)) {
        let count = counts.get(item.as_str()).copied().unwrap_or(0);
        if let Some(p) = places.get(usize::try_from(count).unwrap_or(0)) {
            lw.errs().err(
                *p,
                format!("`holds: {item}` has no copy left: `{item}` has {count}"),
                "raise its `count`",
            );
        }
    }

    // Start and goal.
    let start = match &w.start {
        Some(s) if region_of.contains(s.v.as_str()) => Some(s.v.clone()),
        Some(s) => {
            let help = crate::suggest::help(&s.v, region_of.iter().copied(), "");
            lw.errs().err(s.pos, format!("start `{}` is not a region", s.v), &help);
            None
        }
        None => {
            lw.errs().whole("the world has no `start`", "name the region the player starts in");
            None
        }
    };
    let goal = match &w.goal {
        Some(g) => lw.rule(g, Mode::Goal).map(|v| v.0),
        None => {
            lw.errs().whole("the world has no `goal`", "");
            None
        }
    };
    let grants = |lw: &mut Lower<'_>, list: &[crate::model::Grant], field: &'static str| {
        let mut out = Vec::new();
        for g in list {
            let n = g.n.as_ref().map_or(1, |n| n.v);
            if !is_item(&g.item.v) {
                lw.errs().err(
                    g.item.pos,
                    format!("`{}` in `{field}` is not an item", g.item.v),
                    "",
                );
                continue;
            }
            if !(0..=config::WORLD_ITEM_COPIES_MAX).contains(&n) {
                lw.errs().err(
                    g.n.as_ref().map_or(g.item.pos, |n| n.pos),
                    format!("a grant is 0 to {} copies", config::WORLD_ITEM_COPIES_MAX),
                    "",
                );
                continue;
            }
            let when = match &g.when {
                Some(x) => lw.rule(x, Mode::Static(field)).map_or_else(truth, |v| v.0),
                None => truth(),
            };
            out.push(GrantM { item: g.item.v.clone(), n, when });
        }
        out.sort_by(|a, b| a.item.cmp(&b.item));
        out
    };
    let start_items = grants(&mut lw, &w.start_items, "start_items");
    let assumed = grants(&mut lw, &w.assumed, "assumed");

    // Shuffles.
    let mut shuffles = Map::new();
    for sh in &w.shuffles {
        if let Some(v) = shuffle(&mut lw, sh, &exit_groups, &exit_keys, &optional_exits) {
            shuffles.insert(sh.name.v.clone(), v);
        }
    }
    reads_cycle(&mut lw, w, &shuffles);
    let defs = std::mem::take(&mut lw.defs);
    drop(lw);
    if let (Some(start), Some(g)) = (&start, &goal) {
        sites.push((w.goal.as_ref().map_or(Pos::default(), |g| g.pos), None, truth(), g.clone()));
        // A shuffle thing's rule is read wherever its slot is: in no region in particular.
        for sh in &w.shuffles {
            for t in
                shuffles.get(&sh.name.v).and_then(|v| v["things"].as_object()).into_iter().flatten()
            {
                if let Some(r) = t.1.get("rule") {
                    sites.push((sh.name.pos, None, truth(), r.clone()));
                }
            }
        }
        let map = crate::presence::Map {
            start,
            regions: w
                .regions
                .iter()
                .map(|r| (r.key.v.as_str(), r.key.pos, when_of(&r.key.v)))
                .collect(),
            exits: w
                .regions
                .iter()
                .flat_map(|r| r.exits.iter().map(|x| (r.key.v.as_str(), x.to.v.as_str())))
                .collect(),
            fired,
            sites,
        };
        crate::presence::check(&map, &options, &defs, errs);
    }

    // Every region can be reached from the start by some exits (rules aside).
    if let Some(start) = &start {
        let exits_of: HashMap<&str, &Vec<crate::model::Exit>> =
            w.regions.iter().map(|r| (r.key.v.as_str(), &r.exits)).collect();
        let mut seen: HashSet<&str> = HashSet::from([start.as_str()]);
        let mut todo = vec![start.as_str()];
        while let Some(r) = todo.pop() {
            for x in exits_of.get(r).copied().into_iter().flatten() {
                if region_of.contains(x.to.v.as_str()) && seen.insert(&x.to.v) {
                    todo.push(&x.to.v);
                }
            }
        }
        for r in w.regions.iter().filter(|r| !seen.contains(r.key.v.as_str())) {
            errs.err(
                r.key.pos,
                format!(
                    "region `{}` cannot be reached: no exits lead to it from the start",
                    r.key.v
                ),
                "add an exit into it, or remove it",
            );
        }
    }
    if !errs.errors.is_empty() {
        return None;
    }

    // The manifest, in canonical order (doc 16 section 3.5 rule 3).
    let mut ordered: Vec<(String, String, i64)> = items.clone();
    if fill_with.is_empty() {
        ordered.push((NOTHING.to_owned(), "filler".to_owned(), 0));
        fill_with.push((NOTHING.to_owned(), 1));
    }
    let item_ids = ids(errs, &w.items.iter().map(|i| i.id.as_ref()).collect::<Vec<_>>(), "item");
    let mut id_of: HashMap<&str, i64> =
        w.items.iter().map(|i| i.key.v.as_str()).zip(item_ids.iter().copied()).collect();
    let next = item_ids.iter().max().map_or(1, |m| m + 1);
    id_of.entry(NOTHING).or_insert(next);
    if !errs.errors.is_empty() {
        return None;
    }
    let mut item_rows: Vec<Value> =
        ordered.iter().map(|i| json!({"id": id_of[i.0.as_str()], "key": i.0})).collect();
    item_rows.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    locations.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    regions.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    let mut opt_rows: Vec<Value> =
        options.iter().map(|o| json!({"default": o.default, "key": o.key})).collect();
    opt_rows.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    let mut m = json!({
        "defs": defs, "game": game, "goal": goal?, "items": item_rows, "locations": locations,
        "options": opt_rows, "regions": regions, "start": start?, "version": version,
        "x.time": {"slices": 1, "start_mask": 1},
    });
    if !shuffles.is_empty() {
        m["shuffles"] = Value::Object(shuffles);
    }
    ordered.sort();
    let model = Model {
        options,
        items: ordered,
        copies,
        fill_with,
        checks: checks_m,
        start_items,
        assumed,
        confined,
        defs: m["defs"].as_object().map(|d| d.clone().into_iter().collect()).unwrap_or_default(),
    };
    Some((m, model))
}

/// One check, from every region that declares it: the first declaration, the regions, and
/// each modifier from the one declaration that gives it.
struct Merged<'w> {
    first: &'w crate::model::Check,
    regions: Vec<&'w crate::model::Region>,
    when: Option<&'w crate::model::RuleAt>,
    vanilla: Option<&'w At<String>>,
    holds: Option<&'w At<String>>,
    no_progression: Option<&'w crate::model::RuleAt>,
    placement: Option<&'w At<String>>,
}

/// Every check by name, its declarations in several regions joined: a check reachable from
/// several regions is declared in each, with that region's rule, and every other modifier in
/// one declaration only (a second is an error naming the first's region).
fn merge_checks<'w>(errs: &mut Errs, w: &'w World) -> HashMap<&'w str, Merged<'w>> {
    let mut out: HashMap<&str, Merged<'w>> = HashMap::new();
    let mut given: HashMap<(&str, &str), &str> = HashMap::new();
    for r in &w.regions {
        for c in &r.checks {
            let m = out.entry(c.key.v.as_str()).or_insert(Merged {
                first: c,
                regions: Vec::new(),
                when: None,
                vanilla: None,
                holds: None,
                no_progression: None,
                placement: None,
            });
            if m.regions.iter().any(|q| q.key.v == r.key.v) {
                continue;
            }
            m.regions.push(r);
            let fields: [(&str, Option<Pos>); 5] = [
                ("when", c.when.as_ref().map(|x| x.pos)),
                ("vanilla", c.vanilla.as_ref().map(|x| x.pos)),
                ("holds", c.holds.as_ref().map(|x| x.pos)),
                ("no_progression", c.no_progression.as_ref().map(|x| x.pos)),
                ("placement", c.placement.as_ref().map(|x| x.pos)),
            ];
            for (field, pos) in fields {
                let Some(pos) = pos else { continue };
                if let Some(other) = given.get(&(c.key.v.as_str(), field)) {
                    errs.err(
                        pos,
                        format!(
                            "`{field}` of check `{}` is also given in region `{other}`",
                            c.key.v
                        ),
                        "a check in several regions gives each modifier in one declaration only",
                    );
                    continue;
                }
                given.insert((c.key.v.as_str(), field), r.key.v.as_str());
                match field {
                    "when" => m.when = c.when.as_ref(),
                    "vanilla" => m.vanilla = c.vanilla.as_ref(),
                    "holds" => m.holds = c.holds.as_ref(),
                    "no_progression" => m.no_progression = c.no_progression.as_ref(),
                    _ => m.placement = c.placement.as_ref(),
                }
            }
        }
    }
    out
}

/// One shuffle as the engine's first slice reads it (`data_fill::shuffle`): exit slots
/// (`layout`), or free-name slots whose things may carry rules (`none`).
fn shuffle(
    lw: &mut Lower<'_>,
    sh: &crate::model::Shuffle,
    exit_groups: &BTreeMap<String, Vec<String>>,
    exit_keys: &BTreeSet<String>,
    optional: &BTreeSet<String>,
) -> Option<Value> {
    let name = &sh.name.v;
    later(lw.errs(), &sh.later, "shuffle field");
    for r in &sh.allow {
        later(lw.errs(), &r.later, "permission row field");
    }
    let mut ok = sh.later.is_empty() && sh.allow.iter().all(|r| r.later.is_empty());
    let check = sh.check.as_ref().map(|c| (c.v.as_str(), c.pos));
    let coupled = match &sh.coupled {
        None => false,
        Some(At { v: Rule::Const(b), .. }) => *b,
        Some(c) => {
            lw.errs().err(
                c.pos,
                "a `coupled` that reads options is not supported yet",
                "write true or false",
            );
            return None;
        }
    };
    match &sh.slots.v {
        Members::Exits(list) => {
            // A name is an exit group, or one exit by its key `region:to`.
            let pick =
                |lw: &mut Lower<'_>, list: &[At<String>]| -> Option<(Vec<String>, Vec<String>)> {
                    let (mut groups, mut keys) = (Vec::new(), Vec::new());
                    for n in list {
                        if exit_groups.contains_key(&n.v) {
                            groups.push(n.v.clone());
                        } else if exit_keys.contains(&n.v) {
                            keys.push(n.v.clone());
                        } else {
                            lw.errs().err(
                                n.pos,
                                format!("`{}` is neither an exit group nor an exit", n.v),
                                "name an exit group, or one exit as `region:to`",
                            );
                            return None;
                        }
                    }
                    groups.sort();
                    groups.dedup();
                    keys.sort();
                    keys.dedup();
                    Some((groups, keys))
                };
            let expand = |(groups, keys): &(Vec<String>, Vec<String>)| -> BTreeSet<String> {
                groups
                    .iter()
                    .flat_map(|g| exit_groups[g].iter().cloned())
                    .chain(keys.iter().cloned())
                    .collect()
            };
            let slots = expand(&pick(lw, list)?);
            if let Some(k) = slots.iter().find(|k| optional.contains(*k)) {
                lw.errs().err(
                    sh.slots.pos,
                    format!("shuffle `{name}` moves exit `{k}`, which leads into or out of a region with a `when`"),
                    "a shuffle that changes with the options is not in the engine's first slice; keep the region, and put the `when` on its checks",
                );
                return None;
            }
            if slots.is_empty() {
                lw.errs().err(sh.slots.pos, "the shuffle has no slots", "");
                return None;
            }
            match &sh.things {
                None => {}
                Some(At { v: Members::Exits(t), pos }) => {
                    if expand(&pick(lw, t)?) != slots {
                        lw.errs().err(
                            *pos,
                            "the things of an exit shuffle are its slots",
                            "list the same exits as `slots`, or leave `things` out",
                        );
                        ok = false;
                    }
                }
                Some(t) => {
                    lw.errs().err(t.pos, "the things of an exit shuffle are exits", "");
                    ok = false;
                }
            }
            match check {
                None | Some(("layout", _)) => {}
                Some((c, p)) => {
                    lw.errs().err(
                        p,
                        format!("check `{c}` on a shuffle that moves exits"),
                        "a shuffle that moves exits is `layout`",
                    );
                    ok = false;
                }
            }
            let mut rows = Vec::new();
            for r in &sh.allow {
                let (s, o) = (pick(lw, &r.subject)?, pick(lw, &r.object)?);
                let sel = |(g, k): (Vec<String>, Vec<String>)| json!({"groups": g, "names": k});
                rows.push(row(r.verb.v, sel(s), sel(o)));
            }
            ok.then(|| {
                json!({"allow": rows, "check": "layout", "coupled": coupled,
                       "slots": {"names": slots, "type": "exit"}})
            })
        }
        Members::Names(slots) => {
            let mut slot_names = Vec::new();
            for s in slots {
                if !name_ok(lw.errs(), &s.name, "slot") {
                    ok = false;
                } else if slot_names.contains(&s.name.v) {
                    lw.errs().err(s.name.pos, format!("slot `{}` is listed twice", s.name.v), "");
                    ok = false;
                } else if s.rule.is_some() || s.group.is_some() || !s.later.is_empty() {
                    lw.errs().err(
                        s.name.pos,
                        "a free-name slot is a name only",
                        "rules and groups belong to things",
                    );
                    ok = false;
                }
                slot_names.push(s.name.v.clone());
            }
            if slots.is_empty() {
                lw.errs().err(sh.slots.pos, "the shuffle has no slots", "");
                return None;
            }
            let Some(At { v: Members::Names(things), .. }) = &sh.things else {
                let p = sh.things.as_ref().map_or(sh.name.pos, |t| t.pos);
                lw.errs().err(
                    p,
                    format!("shuffle `{name}` needs things to put in its slots"),
                    "list names, each with an optional rule and group",
                );
                return None;
            };
            let mut out = Map::new();
            let mut groups = BTreeSet::new();
            for t in things {
                later(lw.errs(), &t.later, "thing field");
                ok &= t.later.is_empty() && name_ok(lw.errs(), &t.name, "thing");
                if out.contains_key(&t.name.v) {
                    lw.errs().err(t.name.pos, format!("thing `{}` is listed twice", t.name.v), "");
                    ok = false;
                    continue;
                }
                let mut v = json!({"group": t.group.as_ref().map(|g| g.v.clone())});
                if let Some(g) = &t.group {
                    ok &= name_ok(lw.errs(), g, "group");
                    groups.insert(g.v.clone());
                }
                if let Some(rule) = &t.rule {
                    match lw.rule(rule, Mode::Rule) {
                        Some((r, _)) => v["rule"] = r,
                        None => ok = false,
                    }
                }
                out.insert(t.name.v.clone(), v);
            }
            if things.is_empty() {
                lw.errs().err(sh.slots.pos, format!("shuffle `{name}` has no things"), "");
                return None;
            }
            match check {
                None | Some(("none", _)) => {}
                Some((c, p)) => {
                    lw.errs().err(
                        p,
                        format!("check `{c}` on a shuffle of names"),
                        "a shuffle of names it lists itself is `none`",
                    );
                    ok = false;
                }
            }
            if coupled {
                let p = sh.coupled.as_ref().map_or(sh.name.pos, |c| c.pos);
                lw.errs().err(p, "only a shuffle that moves exits has doors to couple", "");
                ok = false;
            }
            let mut rows = Vec::new();
            for r in &sh.allow {
                let slots_side = matches!(r.verb.v, Verb::Only | Verb::Deny);
                let (sl, th) =
                    if slots_side { (&r.subject, &r.object) } else { (&r.object, &r.subject) };
                let mut slot_sel = Vec::new();
                for s in sl {
                    if slot_names.contains(&s.v) {
                        slot_sel.push(s.v.clone());
                    } else {
                        lw.errs().err(s.pos, format!("shuffle `{name}` has no slot `{}`", s.v), "");
                        ok = false;
                    }
                }
                let (mut tg, mut tn) = (Vec::new(), Vec::new());
                for t in th {
                    if groups.contains(&t.v) {
                        tg.push(t.v.clone());
                    } else if out.contains_key(&t.v) {
                        tn.push(t.v.clone());
                    } else {
                        lw.errs().err(
                            t.pos,
                            format!("shuffle `{name}` has no thing or group `{}`", t.v),
                            "",
                        );
                        ok = false;
                    }
                }
                for l in [&mut slot_sel, &mut tg, &mut tn] {
                    l.sort();
                    l.dedup();
                }
                let s = json!({"groups": [], "names": slot_sel});
                let t = json!({"groups": tg, "names": tn});
                rows.push(if slots_side { row(r.verb.v, s, t) } else { row(r.verb.v, t, s) });
            }
            slot_names.sort();
            ok.then(|| {
                json!({"allow": rows, "check": "none", "slots": {"names": slot_names, "type": "name"},
                       "things": out})
            })
        }
    }
}

/// A permission row in the engine's form: the subject's field and the verb's.
fn row(verb: Verb, subject: Value, object: Value) -> Value {
    let (s, v) = match verb {
        Verb::Only => ("slots", "only"),
        Verb::Deny => ("slots", "deny"),
        Verb::OnlyIn => ("things", "only_in"),
        Verb::DenyIn => ("things", "deny_in"),
    };
    json!({s: subject, v: object})
}

/// Every shuffle a lowered rule reads with `at`, by name.
fn reads(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            o.iter().for_each(|(k, x)| match (k.as_str(), x.as_array().map(Vec::as_slice)) {
                ("at", Some([Value::String(s), _])) => {
                    out.insert(s.clone());
                }
                _ => reads(x, out),
            })
        }
        Value::Array(a) => a.iter().for_each(|x| reads(x, out)),
        _ => {}
    }
}

/// A shuffle runs after every shuffle its things' rules read (doc 17 section 4.3), so the reads
/// may not go round in a cycle, a shuffle reading itself included. Defs a rule names are
/// followed, since a read through a def is still a read.
fn reads_cycle(lw: &mut Lower<'_>, w: &World, built: &Map<String, Value>) {
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, sh) in built {
        let mut out = BTreeSet::new();
        reads(&sh["things"], &mut out);
        let mut seen_defs = BTreeSet::new();
        let mut todo = refs(&sh["things"]);
        while let Some(d) = todo.pop() {
            if seen_defs.insert(d.clone())
                && let Some(body) = lw.defs.get(&d)
            {
                reads(body, &mut out);
                todo.extend(refs(body));
            }
        }
        edges.insert(name.clone(), out);
    }
    for sh in &w.shuffles {
        // A walk from this shuffle that comes back to it is a cycle.
        let start = &sh.name.v;
        let mut seen = BTreeSet::new();
        let mut todo: Vec<&String> = edges.get(start).into_iter().flatten().collect();
        while let Some(n) = todo.pop() {
            if n == start {
                lw.errs().err(
                    sh.name.pos,
                    format!(
                        "shuffle `{start}` reads its own result, directly or through other shuffles"
                    ),
                    "a shuffle runs after every shuffle its things' rules read; break the cycle",
                );
                break;
            }
            if seen.insert(n) {
                todo.extend(edges.get(n).into_iter().flatten());
            }
        }
    }
}

/// Every def a lowered rule names.
fn refs(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![v];
    while let Some(v) = todo.pop() {
        match v {
            Value::Object(o) => {
                if let Some(Value::String(d)) = o.get("ref") {
                    out.push(d.clone());
                }
                todo.extend(o.values());
            }
            Value::Array(a) => todo.extend(a),
            _ => {}
        }
    }
    out
}

/// The ids of one table (items or checks), in file order: a ledger id where the model gives
/// one (a whole number from 1 to 2^53, used once), and the rest numbered on from the largest
/// given, in file order. Without a ledger that is file order from 1.
fn ids(errs: &mut Errs, given: &[Option<&At<i64>>], what: &str) -> Vec<i64> {
    let most = 1i64 << 53;
    let mut used = HashSet::new();
    for id in given.iter().flatten() {
        if !(1..=most).contains(&id.v) {
            errs.err(id.pos, format!("{what} id {} is outside 1 to 2^53", id.v), "");
        } else if !used.insert(id.v) {
            errs.err(
                id.pos,
                format!("{what} id {} is used twice", id.v),
                &format!("an id names one {what} forever"),
            );
        }
    }
    if given.len() > config::WORLD_TABLE_MAX {
        let most = config::WORLD_TABLE_MAX;
        errs.whole(format!("{} {what}s; the engine indexes at most {most}", given.len()), "");
    }
    let mut next = used.iter().max().map_or(1, |m| m + 1);
    let mut out = Vec::with_capacity(given.len());
    for id in given {
        out.push(match id {
            Some(id) => id.v,
            None => {
                next += 1;
                next - 1
            }
        });
    }
    if next - 1 > most {
        errs.whole(
            format!("{what} ids would pass 2^53 numbering on from the largest given"),
            "give the new names ids from the ledger",
        );
    }
    out
}

/// The engine's own compiler over the built manifest, the last check (doc 18 section 5.2):
/// only when the build tool is built with the engine (feature `engine`, on by default).
#[cfg(feature = "engine")]
fn engine_check(m: &Value) -> Result<(), String> {
    data_fill::section::compile(m).map(|_| ())
}

#[cfg(not(feature = "engine"))]
fn engine_check(_: &Value) -> Result<(), String> {
    Ok(())
}
