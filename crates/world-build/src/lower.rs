//! Rule strings to the engine's rule trees (design doc 16 sections 5.0 to 5.3, doc 17 section
//! 5.4), with every name resolved against the world's one namespace and the format rules
//! checked where the author can see them: monotone only (D-132), this world only (D-133), and
//! options only where a value is settled before the fill (D-134).

use std::collections::{BTreeMap, HashMap};

use data_fill::section::monotone_cmp;
use serde_json::{Value, json};

use crate::model::{Pos, Rule, RuleAt};
use crate::rule::{Arith, Cmp, E, Expr, parse};
use crate::world::Errs;

/// What a name in the world's one namespace is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sort {
    Item,
    Check,
    Def,
    Event,
    Region,
    ItemGroup,
    Placement,
    Shuffle,
}

impl Sort {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Sort::Item => "an item",
            Sort::Check => "a check",
            Sort::Def => "a def",
            Sort::Event => "an event",
            Sort::Region => "a region",
            Sort::ItemGroup => "an item group",
            Sort::Placement => "a placement set",
            Sort::Shuffle => "a shuffle",
        }
    }
}

/// An option's type, as the build folds it to integers (doc 16 section 3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptTy {
    Toggle,
    Choice(Vec<String>),
    Range(i64, i64),
}

/// A shuffle as a rule can read it.
#[derive(Debug, Clone)]
pub(crate) enum Readable {
    /// Moves exits: nothing may read it yet (the engine's first slice).
    Layout,
    /// Free-name slots: its slots, and per thing whether it carries a rule.
    Names { slots: Vec<String>, things: Vec<(String, bool)> },
}

/// Everything a rule can name.
#[derive(Debug, Default)]
pub(crate) struct Names {
    pub sorts: HashMap<String, Sort>,
    pub options: HashMap<String, OptTy>,
    pub item_groups: HashMap<String, Vec<String>>,
    pub shuffles: HashMap<String, Readable>,
    pub defs: HashMap<String, RuleAt>,
}

/// Where a rule appears, which decides what it may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// An ordinary rule: exits, checks, events, defs, a shuffle thing's rule.
    Rule,
    /// The goal, the one place `reach` is allowed (doc 16 section 5.4).
    Goal,
    /// Settled from options before the fill: the field it is, and why.
    Static(&'static str),
}

/// Where the characters of the rule being lowered sit in the file.
#[derive(Debug, Clone, Copy)]
struct Place {
    base: Pos,
    one_line: bool,
}

/// The lowering state: the namespace, the errors, and the defs lowered so far.
pub(crate) struct Lower<'a> {
    names: &'a Names,
    errs: &'a mut Errs,
    /// Lowered defs by name, for the manifest.
    pub defs: BTreeMap<String, Value>,
    /// Per def: being lowered (a cycle guard), failed, or done and whether it reads state.
    state: HashMap<String, DefState>,
    /// Per lowered def: how deep its body nests, defs it reaches included.
    below: HashMap<String, usize>,
    place: Place,
    /// Rule nodes open in the recursion, defs reached through a name included.
    depth: usize,
    /// The deepest the recursion went since it was last reset (per def body).
    peak: usize,
}

/// Where a def's lowering stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefState {
    Busy,
    Failed,
    Done(bool),
}

type L = Option<(Value, bool)>;

/// What to do about a dotted name that is neither an option nor a shuffle read.
const ISOLATION: &str = "a rule reads only its own world (format rule b, D-133); options are `opt.<key>` and shuffle results `at.<shuffle>.<slot>`";

fn t() -> Value {
    json!({"true": {}})
}

impl<'a> Lower<'a> {
    pub(crate) fn new(names: &'a Names, errs: &'a mut Errs) -> Self {
        let place = Place { base: Pos::default(), one_line: true };
        Lower {
            names,
            errs,
            defs: BTreeMap::new(),
            state: HashMap::new(),
            below: HashMap::new(),
            place,
            depth: 0,
            peak: 0,
        }
    }

    pub(crate) fn errs(&mut self) -> &mut Errs {
        self.errs
    }

    fn pos(&self, at: usize) -> Pos {
        let p = self.place;
        // shortcut: a rule written over several lines reports where it starts, not the
        // character; upgrade if a front end allows multi-line rules.
        if p.one_line { p.base.right(at) } else { p.base }
    }

    fn err(&mut self, at: usize, msg: impl Into<String>, help: &str) -> L {
        let p = self.pos(at);
        self.errs.err(p, msg, help);
        None
    }

    /// A rule as written in the file: `true`, `false`, or a rule string. Returns the tree and
    /// whether it reads state (items, events, counters, reached regions).
    pub(crate) fn rule(&mut self, n: &RuleAt, mode: Mode) -> L {
        self.rule_as(n, mode, true)
    }

    /// [`Lower::rule`], warning about mixed `and` and `or` only when `warn`.
    fn rule_as(&mut self, n: &RuleAt, mode: Mode, warn: bool) -> L {
        let text = match &n.v {
            Rule::Const(b) => return Some((json!({b.to_string(): {}}), false)),
            Rule::Text(text) => text,
        };
        let saved = self.place;
        self.place = Place { base: n.pos, one_line: !text.contains('\n') };
        let out = match parse(text) {
            Err((at, msg)) => self.err(at, msg, ""),
            Ok(parsed) => {
                for at in parsed.mixed.into_iter().filter(|_| warn) {
                    let p = self.pos(at);
                    self.errs.warn(
                        p,
                        "`and` inside `or` without parentheses",
                        "it reads as `a or (b and c)`; add parentheses to say so",
                    );
                }
                self.b(&parsed.expr, mode)
            }
        };
        self.place = saved;
        out
    }

    /// Whether a def reads state, lowering it on first use. The recursion it adds counts
    /// against the one depth cap whether the def is lowered now or was earlier.
    fn def(&mut self, name: &str, at: usize) -> Option<bool> {
        match self.state.get(name).copied() {
            Some(DefState::Done(state)) => {
                let below = self.below.get(name).copied().unwrap_or(0);
                self.deep(self.depth + below, at)?;
                return Some(state);
            }
            Some(DefState::Busy) => {
                self.err(
                    at,
                    format!("def `{name}` refers to itself through other defs"),
                    "a def may use defs, without a cycle",
                );
                return None;
            }
            // Its own error is reported where it is.
            Some(DefState::Failed) => return None,
            None => {}
        }
        let node = self.names.defs.get(name)?.clone();
        self.state.insert(name.to_owned(), DefState::Busy);
        let (base, outer) = (self.depth, self.peak);
        self.peak = base;
        let out = self.rule(&node, Mode::Rule);
        self.below.insert(name.to_owned(), self.peak - base);
        self.peak = self.peak.max(outer);
        let Some((v, state)) = out else {
            self.state.insert(name.to_owned(), DefState::Failed);
            return None;
        };
        self.defs.insert(name.to_owned(), v);
        self.state.insert(name.to_owned(), DefState::Done(state));
        Some(state)
    }

    /// The recursion reaches `deep` nodes: within the engine's cap, or an error. Lowering
    /// recurses once per node, so this is what bounds its stack, as the engine's own cap
    /// bounds the loader's.
    fn deep(&mut self, deep: usize, at: usize) -> Option<()> {
        self.peak = self.peak.max(deep);
        let most = config::RULES_MAX_DEPTH as usize;
        if deep > most {
            self.err(
                at,
                format!("rules nest more than {most} deep, counting the defs they use"),
                "flatten a def chain, or split the rule",
            );
            return None;
        }
        Some(())
    }

    /// One level deeper for the duration of `f`.
    fn nested(&mut self, at: usize, f: impl FnOnce(&mut Self) -> L) -> L {
        self.depth += 1;
        let out = match self.deep(self.depth, at) {
            Some(()) => f(self),
            None => None,
        };
        self.depth -= 1;
        out
    }

    /// Lower every def, used or not, so each is checked and in the manifest.
    pub(crate) fn all_defs(&mut self) {
        let mut names: Vec<&String> = self.names.defs.keys().collect();
        names.sort();
        for n in names {
            self.def(n, 0);
        }
    }

    fn sort_of(&self, n: &str) -> Option<Sort> {
        self.names.sorts.get(n).copied()
    }

    fn item(&mut self, e: &Expr) -> Option<String> {
        match &e.e {
            E::Name(n) if self.sort_of(n) == Some(Sort::Item) => Some(n.clone()),
            E::Name(n) if n.contains('.') => {
                self.err(e.at, format!("`{n}` is not an item of this world"), ISOLATION);
                None
            }
            E::Name(n) => {
                match self.sort_of(n) {
                    Some(s) => self.err(e.at, format!("`{n}` is {}, not an item", s.word()), ""),
                    None => self.err(
                        e.at,
                        format!("unknown item `{n}`"),
                        "items are declared under `items:`",
                    ),
                };
                None
            }
            _ => {
                self.err(e.at, "expected an item name", "");
                None
            }
        }
    }

    fn state_ok(&mut self, mode: Mode, at: usize, state: bool) -> bool {
        if let (Mode::Static(field), true) = (mode, state) {
            let p = self.pos(at);
            self.errs.err(
                p,
                format!("`{field}` may read options only, and this reads what the player holds or reaches"),
                "check filters, placement permissions and start items are settled from options before the fill (format rule c, D-134); move the requirement into the check's rule",
            );
            return false;
        }
        true
    }

    /// `opt.k`, `at.s.slot` or a bare name, split.
    fn dotted<'n>(&mut self, n: &'n str, at: usize) -> Option<Dotted<'n>> {
        if let Some(k) = n.strip_prefix("opt.") {
            return match self.names.options.get(k) {
                Some(ty) => Some(Dotted::Opt(k, ty.clone())),
                None => {
                    self.err(
                        at,
                        format!("unknown option `{k}`"),
                        "options are declared under `options:`",
                    );
                    None
                }
            };
        }
        if let Some(rest) = n.strip_prefix("at.") {
            let Some((s, slot)) = rest.split_once('.').filter(|(_, x)| !x.contains('.')) else {
                self.err(at, format!("`{n}` is not a shuffle read"), "write `at.<shuffle>.<slot>`");
                return None;
            };
            return match self.names.shuffles.get(s) {
                Some(Readable::Layout) => self
                    .err(
                        at,
                        format!("`{s}` moves exits; a rule cannot read a layout result yet"),
                        "",
                    )
                    .map(|_| Dotted::Bare(n)),
                Some(Readable::Names { slots, .. }) if !slots.iter().any(|x| x == slot) => self
                    .err(
                        at,
                        format!("shuffle `{s}` has no slot `{slot}`"),
                        &format!("its slots: {}", slots.join(", ")),
                    )
                    .map(|_| Dotted::Bare(n)),
                Some(Readable::Names { .. }) => Some(Dotted::At(s, slot)),
                None => self.err(at, format!("unknown shuffle `{s}`"), "").map(|_| Dotted::Bare(n)),
            };
        }
        if n.contains('.') {
            self.err(at, format!("`{n}` is not a name in this world"), ISOLATION);
            return None;
        }
        Some(Dotted::Bare(n))
    }

    /// A rule node.
    fn b(&mut self, e: &Expr, mode: Mode) -> L {
        let out = self.nested(e.at, |s| s.b_inner(e, mode))?;
        self.state_ok(mode, e.at, out.1).then_some(out)
    }

    fn b_inner(&mut self, e: &Expr, mode: Mode) -> L {
        match &e.e {
            E::Bool(b) => Some((json!({b.to_string(): {}}), false)),
            E::Int(_) | E::Sum(_) | E::Arith(..) => {
                self.err(e.at, "a number is not a rule", "compare it, as in `count(x) >= 2`")
            }
            E::Name(n) => match self.dotted(n, e.at)? {
                Dotted::Opt(k, OptTy::Toggle) => Some((json!({"ne": [{"opt": k}, 0]}), false)),
                Dotted::Opt(k, _) => self.err(
                    e.at,
                    format!("option `{k}` is not a toggle, so it is not a rule by itself"),
                    &format!("compare it, as in `opt.{k} == <value>`"),
                ),
                Dotted::At(s, slot) => self.at_rule(s, slot, e.at),
                Dotted::Bare(n) => match self.sort_of(n) {
                    Some(Sort::Def) => {
                        let state = self.def(n, e.at)?;
                        if let Mode::Static(_) = mode {
                            // A settled field is folded from options by the per-seed step, which
                            // reads no defs: the body goes inline, checked as that field.
                            let node = self.names.defs.get(n)?.clone();
                            return self.rule_as(&node, mode, false);
                        }
                        Some((json!({"ref": n}), state))
                    }
                    Some(Sort::Event) => Some((json!({"ge": [{"counter": n}, 1]}), true)),
                    Some(Sort::Item) => self.err(
                        e.at,
                        format!("`{n}` is an item; a bare name in a rule is a def or an event"),
                        &format!("did you mean `has({n})`?"),
                    ),
                    Some(s) => self.err(
                        e.at,
                        format!(
                            "`{n}` is {}; a bare name in a rule is a def or an event",
                            s.word()
                        ),
                        "",
                    ),
                    None => self.err(
                        e.at,
                        format!("unknown name `{n}`"),
                        "declare it as an item, def or event, or check the spelling",
                    ),
                },
            },
            E::Not(x) => {
                let (v, state) = self.b(x, mode)?;
                if state {
                    return self.err(
                        e.at,
                        "`not` over what the player holds or reaches can turn a rule false as they collect",
                        "rules are monotone (format rule a, D-132): `not` may read options and shuffle name tests only; rewrite with `or` and the positive forms",
                    );
                }
                Some((json!({"not": v}), false))
            }
            E::And(xs) | E::Or(xs) => {
                if xs.len() > usize::from(u8::MAX) {
                    return self.err(
                        e.at,
                        format!(
                            "{} parts; the engine takes at most 255 in one `and` or `or`",
                            xs.len()
                        ),
                        "split it with a def",
                    );
                }
                let mut parts = Vec::with_capacity(xs.len());
                let mut state = false;
                let mut ok = true;
                for x in xs {
                    match self.b(x, mode) {
                        Some((v, s)) => {
                            parts.push(v);
                            state |= s;
                        }
                        None => ok = false,
                    }
                }
                let key = if matches!(e.e, E::And(_)) { "all" } else { "any" };
                ok.then(|| (json!({key: parts}), state))
            }
            E::Call(f, args) => self.call(f, args, e.at, mode),
            E::Cmp(op, a, b) => self.cmp(*op, a, b, e.at, mode),
            E::In(a, xs) => self.is_in(a, xs, e.at, mode),
        }
    }

    fn at_rule(&mut self, s: &str, slot: &str, at: usize) -> L {
        let Some(Readable::Names { things, .. }) = self.names.shuffles.get(s) else { return None };
        if let Some((t, _)) = things.iter().find(|(_, rule)| !rule) {
            let t = t.clone();
            return self.err(
                at,
                format!("`at.{s}.{slot}` is read as a rule, and thing `{t}` of `{s}` has no rule"),
                "give every thing a `rule`, or test which thing is there with `==`",
            );
        }
        Some((json!({"at": [s, slot]}), true))
    }

    fn call(&mut self, f: &str, args: &[Expr], at: usize, mode: Mode) -> L {
        match f {
            "has" => {
                let (x, n) = match args {
                    [x] => (x, None),
                    [x, n] => (x, Some(n)),
                    _ => return self.err(at, "`has` takes an item and an optional count", "`has(x)` or `has(x, 3)`"),
                };
                let item = self.item(x)?;
                match n.map(|n| &n.e) {
                    None | Some(E::Int(1)) => Some((json!({"has": item}), true)),
                    Some(E::Int(k)) if (0..=255).contains(k) => Some((json!({"has": item, "n": k}), true)),
                    Some(E::Int(k)) => self.err(
                        n.map_or(at, |n| n.at),
                        format!("`has` counts 0 to 255 copies, not {k} (the engine's limit)"),
                        "for more, write `count(x) >= n`",
                    ),
                    Some(_) => self.err(
                        n.map_or(at, |n| n.at),
                        "the count in `has` is a number as written",
                        "for a count set by an option, write `count(x) >= opt.k`",
                    ),
                }
            }
            "has_all" | "has_any" => {
                if args.is_empty() || args.len() > usize::from(u8::MAX) {
                    return self.err(at, format!("`{f}` takes 1 to 255 items"), "");
                }
                let mut items = Vec::new();
                for a in args {
                    items.push(self.item(a)?);
                }
                items.sort();
                items.dedup();
                Some((json!({f: items}), true))
            }
            "reach" => {
                if mode != Mode::Goal {
                    return self.err(
                        at,
                        "`reach` is allowed in the goal only",
                        "put an event in the target region and read the event by its name (doc 16 section 5.4)",
                    );
                }
                let [E::Name(r)] = args.iter().map(|a| &a.e).collect::<Vec<_>>()[..] else {
                    return self.err(at, "`reach` takes one region", "");
                };
                if self.sort_of(r) != Some(Sort::Region) {
                    return self.err(at, format!("`{r}` is not a region"), "");
                }
                Some((json!({"reach": format!("region:{r}")}), true))
            }
            "during" => self.err(
                at,
                "`during` needs a time context, which the build tool does not support yet",
                "contexts are proposed (D-129); a world for this version has no time of day",
            ),
            "roll" => self.err(
                at,
                "`roll` is not supported yet",
                "a roll's seed draw waits on the random stream decision (D-47), so a build cannot write it yet",
            ),
            "count" | "level" | "counter" | "group_count" | "min" | "max" => self.err(
                at,
                format!("`{f}(..)` is a number, not a rule"),
                &format!("compare it, as in `{f}(..) >= 1`"),
            ),
            _ => self.err(at, format!("unknown function `{f}`"), "rules use has, has_all, has_any and reach; numbers use count, level, counter, group_count, min and max"),
        }
    }

    /// A comparison: a choice value or shuffle name test, or two integer expressions.
    fn cmp(&mut self, op: Cmp, a: &Expr, b: &Expr, at: usize, mode: Mode) -> L {
        if matches!(op, Cmp::Eq | Cmp::Ne) {
            let key = if op == Cmp::Eq { "eq" } else { "ne" };
            for (x, y) in [(a, b), (b, a)] {
                let (Some(xn), Some(yn)) = (name(x), name(y)) else { continue };
                if let Some(k) = xn.strip_prefix("opt.")
                    && let Some(OptTy::Choice(values)) = self.names.options.get(k)
                    && !yn.contains('.')
                {
                    let ix = self.value(k, values, yn, y.at)?;
                    return Some((json!({key: [{"opt": k}, ix]}), false));
                }
                if xn.starts_with("at.") && !yn.contains('.') {
                    if let Mode::Static(field) = mode {
                        return self.err(
                            x.at,
                            format!("`{field}` cannot read a shuffle result yet"),
                            "it is settled from options before the shuffles run",
                        );
                    }
                    let Dotted::At(s, slot) = self.dotted(xn, x.at)? else { return None };
                    self.thing(s, yn, y.at)?;
                    return Some((json!({key: [{"at": [s, slot]}, yn]}), false));
                }
            }
        }
        for e in [a, b] {
            if name(e).is_some_and(|n| n.starts_with("at.")) {
                return self.err(
                    e.at,
                    "a shuffle read is tested with `==`, `!=` or `in` against a thing's name",
                    "",
                );
            }
        }
        let (x, xs) = self.i(a)?;
        let (y, ys) = self.i(b)?;
        if !monotone_cmp(op.index(), xs, ys) {
            let help = match op {
                Cmp::Eq | Cmp::Ne => {
                    "\"exactly n\" stops being true at n + 1; write `>=` or `>` with the items on the left"
                }
                _ => {
                    "write `>=` or `>` with the items, events or counters on the left and options or numbers on the right"
                }
            };
            return self.err(at, "this comparison can turn false as the player collects (rules are monotone, format rule a, D-132)", help);
        }
        let key = data_fill::section::CMP_OPS[op.index()];
        Some((json!({key: [x, y]}), xs || ys))
    }

    fn value(&mut self, k: &str, values: &[String], v: &str, at: usize) -> Option<usize> {
        let ix = values.iter().position(|x| x == v);
        if ix.is_none() {
            self.err(
                at,
                format!("`{v}` is not a value of option `{k}`"),
                &format!("its values: {}", values.join(", ")),
            );
        }
        ix
    }

    fn thing(&mut self, s: &str, t: &str, at: usize) -> Option<()> {
        let Some(Readable::Names { things, .. }) = self.names.shuffles.get(s) else { return None };
        if things.iter().any(|(n, _)| n == t) {
            return Some(());
        }
        let all: Vec<&str> = things.iter().map(|(n, _)| n.as_str()).collect();
        self.err(
            at,
            format!("shuffle `{s}` has no thing `{t}`"),
            &format!("its things: {}", all.join(", ")),
        );
        None
    }

    /// `x in [a, b]`: an `any` of `eq` (doc 16 section 5.1).
    fn is_in(&mut self, a: &Expr, xs: &[Expr], at: usize, mode: Mode) -> L {
        let E::Name(n) = &a.e else {
            return self.err(
                a.at,
                "`in` tests an option or a shuffle read",
                "as in `opt.k in [a, b]`",
            );
        };
        let mut parts = Vec::new();
        for x in xs {
            let eq = Expr {
                e: E::Cmp(Cmp::Eq, Box::new(a.clone()), Box::new(x.clone())),
                at: x.at,
                paren: false,
            };
            parts.push(self.b(&eq, mode)?.0);
        }
        if !(n.starts_with("opt.") || n.starts_with("at.")) {
            return self.err(
                at,
                "`in` tests an option or a shuffle read",
                "as in `opt.k in [a, b]`",
            );
        }
        Some((json!({"any": parts}), false))
    }

    /// An integer node, and whether it reads state.
    fn i(&mut self, e: &Expr) -> L {
        self.nested(e.at, |s| s.i_inner(e))
    }

    fn i_inner(&mut self, e: &Expr) -> L {
        match &e.e {
            E::Int(n) => {
                if i16::try_from(*n).is_err() {
                    return self.err(
                        e.at,
                        format!("{n} is outside -32768 to 32767, the engine's 16-bit number"),
                        "",
                    );
                }
                Some((json!(n), false))
            }
            E::Name(n) => {
                match self.dotted(n, e.at)? {
                    Dotted::Opt(k, _) => Some((json!({"opt": k}), false)),
                    Dotted::At(..) => self.err(
                        e.at,
                        "a shuffle result has no number yet",
                        "test which thing is there with `==`",
                    ),
                    Dotted::Bare(n) => {
                        let help = match self.sort_of(n) {
                        Some(Sort::Item) => format!("an item is counted with `count({n})`"),
                        Some(Sort::Event) => format!("an event's total is `counter({n})`"),
                        Some(Sort::ItemGroup) => format!("a group is counted with `group_count({n})`"),
                        _ => "a choice value is compared directly with its option: `opt.k == value`".into(),
                    };
                        self.err(e.at, format!("`{n}` is not a number"), &help)
                    }
                }
            }
            E::Call(f, args) => self.int_call(f, args, e.at),
            E::Sum(xs) => self.sum(xs, e.at),
            E::Arith(op, x, y) => self.arith(*op, x, y, e.at),
            _ => self.err(e.at, "expected a number here, found a rule", ""),
        }
    }

    fn int_call(&mut self, f: &str, args: &[Expr], at: usize) -> L {
        let one = |args: &[Expr]| match args {
            [x] => Some(x.clone()),
            _ => None,
        };
        match f {
            "count" | "level" => {
                let Some(x) = one(args) else {
                    return self.err(at, format!("`{f}` takes one item"), "");
                };
                Some((json!({"count": self.item(&x)?}), true))
            }
            "counter" => match one(args).map(|x| x.e) {
                Some(E::Name(n)) if self.sort_of(&n) == Some(Sort::Event) => {
                    Some((json!({"counter": n}), true))
                }
                _ => self.err(at, "`counter` takes one event", ""),
            },
            "group_count" => match one(args).map(|x| x.e) {
                Some(E::Name(g)) if self.sort_of(&g) == Some(Sort::ItemGroup) => {
                    let mut members = self.names.item_groups.get(&g).cloned().unwrap_or_default();
                    members.sort();
                    let counts: Vec<Value> = members.iter().map(|m| json!({"count": m})).collect();
                    let mut sum = vec![json!("+")];
                    sum.extend(counts);
                    Some((json!({"int": sum}), true))
                }
                _ => self.err(
                    at,
                    "`group_count` takes one item group",
                    "groups are declared under `item_groups:`",
                ),
            },
            "min" | "max" => {
                if args.is_empty() || args.len() > usize::from(u8::MAX) {
                    return self.err(at, format!("`{f}` takes 1 to 255 numbers"), "");
                }
                let mut out = vec![json!(f)];
                let mut state = false;
                for a in args {
                    let (v, s) = self.i(a)?;
                    out.push(v);
                    state |= s;
                }
                Some((json!({"int": out}), state))
            }
            "roll" => self.call(f, args, at, Mode::Rule),
            _ => self.err(
                at,
                format!("`{f}(..)` is not a number"),
                "numbers use count, level, counter, group_count, min and max",
            ),
        }
    }

    /// A sum: one `+` node over every term, parenthesized sums included (a sum of counts is
    /// then one counter read for the engine).
    fn sum(&mut self, xs: &[Expr], at: usize) -> L {
        let mut terms = Vec::new();
        let mut todo: Vec<&Expr> = xs.iter().rev().collect();
        while let Some(t) = todo.pop() {
            match &t.e {
                E::Sum(inner) => todo.extend(inner.iter().rev()),
                _ => terms.push(t),
            }
        }
        if terms.len() > usize::from(u8::MAX) {
            return self.err(
                at,
                format!("{} terms in one sum; the engine takes at most 255", terms.len()),
                "",
            );
        }
        let mut out = vec![json!("+")];
        let mut state = false;
        for t in terms {
            let (v, s) = self.i(t)?;
            out.push(v);
            state |= s;
        }
        Some((json!({"int": out}), state))
    }

    fn arith(&mut self, op: Arith, x: &Expr, y: &Expr, at: usize) -> L {
        match op {
            Arith::Sub | Arith::Mod => {
                let s = if op == Arith::Sub { "-" } else { "mod" };
                self.err(
                    at,
                    format!("`{s}` is not supported yet: the engine cannot evaluate it"),
                    "doc 16 allows it over options only, folded before the fill, which the engine does not do yet",
                )
            }
            Arith::Mul | Arith::Div => {
                let (a, sa) = self.i(x)?;
                let (b, sb) = self.i(y)?;
                let lit = |e: &Expr| match e.e {
                    E::Int(n) => Some(n),
                    _ => None,
                };
                if op == Arith::Div && !lit(y).is_some_and(|d| d > 0) {
                    return self.err(
                        y.at,
                        "the divisor of `div` is a positive number as written",
                        "",
                    );
                }
                if op == Arith::Mul
                    && ((sa && !lit(y).is_some_and(|c| c >= 0))
                        || (sb && !lit(x).is_some_and(|c| c >= 0)))
                {
                    return self.err(
                        at,
                        "a product that reads items needs a factor written as a number of at least 0",
                        "for an option factor, write one comparison per option value (doc 16 section 5.2)",
                    );
                }
                let s = if op == Arith::Mul { "*" } else { "div" };
                Some((json!({"int": [s, a, b]}), sa || sb))
            }
        }
    }
}

fn name(e: &Expr) -> Option<&str> {
    match &e.e {
        E::Name(n) => Some(n.as_str()),
        _ => None,
    }
}

enum Dotted<'n> {
    Opt(&'n str, OptTy),
    At(&'n str, &'n str),
    Bare(&'n str),
}

/// The always-true rule.
pub(crate) fn truth() -> Value {
    t()
}
