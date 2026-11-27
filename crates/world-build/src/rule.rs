//! The rule parser (design doc 16 section 5.0, doc 18 sections 2.2 and 2.4): recursive descent
//! by precedence, tightest first: function forms, parentheses and `match`; `*`, `div`, `mod`;
//! `+`, `-`; comparisons and `in` (no chaining); `not`; `and`; `or`. It reads the language's
//! tokens ([`crate::lex`]) and stops at the first token a rule cannot continue with, so a rule
//! written bare in a file ends where the next keyword or row begins. Nesting is capped at
//! [`config::WORLD_RULE_MAX_DEPTH`], arithmetic chains included, so no input can exhaust the
//! stack. Names are not resolved here: that is the lowering's job (`crate::lower`).

use crate::lex::{Tok, Token, lex, show};
use crate::model::Pos;

/// A comparison, in the engine's operand order (`rule_ops::CMP_OPS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl Cmp {
    /// The engine's operand index.
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// An arithmetic operator other than `+`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arith {
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `div`
    Div,
    /// `mod`
    Mod,
}

/// A piece of an interpolated name: `{d.key}_ante{ante}` is a variable, text, a variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Written as is.
    Text(String),
    /// A family variable, or a field of one (`d.key`).
    Var(String),
}

/// One arm of a `match`: its values (none for `_`) and its rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    /// The values it covers, each with its place; empty for `_`.
    pub values: Vec<(String, Pos)>,
    /// Its rule.
    pub rule: Expr,
    /// Index of its first token (a value or `_`).
    pub start: usize,
}

/// One parsed node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    /// What it is.
    pub e: E,
    /// Where its first character is.
    pub at: Pos,
    /// Written inside parentheses.
    pub paren: bool,
    /// Its tokens, first and one past the last, in the token list it was parsed from.
    pub span: (usize, usize),
}

/// The node kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum E {
    /// `true`, `false`.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A name, possibly dotted (`opt.k`, `at.s.slot`).
    Name(String),
    /// A name with `{...}` pieces, replaced when its family expands.
    Interp(Vec<Piece>),
    /// `f(args)`.
    Call(String, Vec<Expr>),
    /// `not a`.
    Not(Box<Expr>),
    /// `a and b and ...`.
    And(Vec<Expr>),
    /// `a or b or ...`.
    Or(Vec<Expr>),
    /// `a >= b` and the rest.
    Cmp(Cmp, Box<Expr>, Box<Expr>),
    /// `a in [b, c]`.
    In(Box<Expr>, Vec<Expr>),
    /// `a + b + ...`: one node for a chain, so a long sum does not nest.
    Sum(Vec<Expr>),
    /// `a - b`, `a * b`, `a div b`, `a mod b`.
    Arith(Arith, Box<Expr>, Box<Expr>),
    /// `match opt.k { a, b => rule  _ => rule }`: the option's key and the arms.
    Match(String, Vec<Arm>),
}

/// A parse error: where and why.
pub type Fail = (Pos, String);

/// Parse `text` as one whole rule; its first character is at `base`.
///
/// # Errors
/// The first syntax error.
pub fn parse(text: &str, base: Pos) -> Result<Expr, Fail> {
    let (toks, _) = lex(text, base)?;
    if toks.is_empty() {
        return Err((base, "the rule is empty; write `true` for no requirement".into()));
    }
    let (e, end) = parse_in(&toks, 0, toks.len())?;
    if let Some(t) = toks.get(end) {
        return Err((t.pos, format!("unexpected {} after a complete rule", show(&t.t))));
    }
    Ok(e)
}

/// Parse one rule from `toks[from..to]`, stopping at the first token it cannot continue with.
/// Returns the rule and the index of that token.
///
/// # Errors
/// The first syntax error.
pub fn parse_in(toks: &[Token], from: usize, to: usize) -> Result<(Expr, usize), Fail> {
    let mut p = P { toks, k: from, end: to.min(toks.len()), depth: 0 };
    let e = p.or()?;
    Ok((e, p.k))
}

/// `{` NAME `}` glued at `k`: a variable piece of a name.
fn var_piece(toks: &[Token], k: usize, end: usize) -> Option<String> {
    let t = |i: usize| (i < end).then(|| toks.get(i)).flatten();
    match (t(k)?, t(k + 1)?, t(k + 2)?) {
        (
            Token { t: Tok::LBrace, .. },
            b @ Token { t: Tok::Name(n), .. },
            c @ Token { t: Tok::RBrace, .. },
        ) if b.glued && c.glued => Some(n.clone()),
        _ => None,
    }
}

/// The pieces of a name starting at `k` (plain text and glued `{var}` pieces), and the index
/// after them. A plain name is one text piece.
#[must_use]
pub fn pieces_at(toks: &[Token], mut k: usize, end: usize) -> (Vec<Piece>, usize) {
    let mut out: Vec<Piece> = Vec::new();
    while k < end.min(toks.len()) {
        if !out.is_empty() && !toks[k].glued {
            break;
        }
        if let Some(v) = var_piece(toks, k, end) {
            out.push(Piece::Var(v));
            k += 3;
        } else if let Tok::Name(n) = &toks[k].t {
            if matches!(out.last(), Some(Piece::Text(_))) {
                break;
            }
            out.push(Piece::Text(n.clone()));
            k += 1;
        } else {
            break;
        }
    }
    (out, k)
}

struct P<'t> {
    toks: &'t [Token],
    k: usize,
    end: usize,
    depth: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        (self.k < self.end).then(|| &self.toks[self.k].t)
    }

    fn here(&self) -> Pos {
        match self.toks.get(self.k) {
            Some(t) if self.k < self.end => t.pos,
            _ => self
                .toks
                .get(self.k.saturating_sub(1))
                .map_or(Pos::default(), |t| t.pos.right(t.len)),
        }
    }

    fn word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Kw(k)) if *k == w)
    }

    fn expect(&mut self, t: &Tok, what: &str) -> Result<(), Fail> {
        if self.peek() == Some(t) {
            self.k += 1;
            return Ok(());
        }
        let got = self.peek().map_or_else(|| "the end of the rule".to_owned(), show);
        Err((self.here(), format!("expected {what}, found {got}")))
    }

    fn enter(&mut self) -> Result<(), Fail> {
        self.depth += 1;
        if self.depth > config::WORLD_RULE_MAX_DEPTH {
            let most = config::WORLD_RULE_MAX_DEPTH;
            return Err((self.here(), format!("the rule nests more than {most} deep")));
        }
        Ok(())
    }

    fn ex(&self, e: E, at: Pos, from: usize) -> Expr {
        Expr { e, at, paren: false, span: (from, self.k) }
    }

    fn or(&mut self) -> Result<Expr, Fail> {
        self.enter()?;
        let (at, from) = (self.here(), self.k);
        let mut parts = vec![self.and()?];
        while self.word("or") {
            self.k += 1;
            parts.push(self.and()?);
        }
        self.depth -= 1;
        Ok(if parts.len() == 1 { parts.remove(0) } else { self.ex(E::Or(parts), at, from) })
    }

    fn and(&mut self) -> Result<Expr, Fail> {
        let (at, from) = (self.here(), self.k);
        let mut parts = vec![self.not()?];
        while self.word("and") {
            self.k += 1;
            parts.push(self.not()?);
        }
        Ok(if parts.len() == 1 { parts.remove(0) } else { self.ex(E::And(parts), at, from) })
    }

    fn not(&mut self) -> Result<Expr, Fail> {
        if self.word("not") {
            let (at, from) = (self.here(), self.k);
            self.k += 1;
            self.enter()?;
            let inner = self.not()?;
            self.depth -= 1;
            return Ok(self.ex(E::Not(Box::new(inner)), at, from));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, Fail> {
        let (at, from) = (self.here(), self.k);
        let left = self.sum()?;
        let e = if let Some(Tok::Cmp(op)) = self.peek().cloned() {
            self.k += 1;
            E::Cmp(op, Box::new(left), Box::new(self.sum()?))
        } else if self.word("in") {
            self.k += 1;
            self.expect(&Tok::LBrack, "`[` after `in`")?;
            E::In(Box::new(left), self.list(&Tok::RBrack, "`]`")?)
        } else {
            return Ok(left);
        };
        if matches!(self.peek(), Some(Tok::Cmp(_))) || self.word("in") {
            return Err((self.here(), "comparisons do not chain; join them with `and`".into()));
        }
        Ok(self.ex(e, at, from))
    }

    /// A chain of `+` is one [`E::Sum`]; each `-` nests, so it counts against the depth cap.
    fn sum(&mut self) -> Result<Expr, Fail> {
        let from = self.k;
        let first = self.product()?;
        let at = first.at;
        let mut terms = vec![first];
        let mut nested = 0;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.k += 1;
                    terms.push(self.product()?);
                }
                Some(Tok::Minus) => {
                    self.k += 1;
                    self.enter()?;
                    nested += 1;
                    let left = self.chain(std::mem::take(&mut terms), at, from);
                    let right = self.product()?;
                    terms.push(self.ex(
                        E::Arith(Arith::Sub, Box::new(left), Box::new(right)),
                        at,
                        from,
                    ));
                }
                _ => break,
            }
        }
        self.depth -= nested;
        Ok(self.chain(terms, at, from))
    }

    fn chain(&self, mut terms: Vec<Expr>, at: Pos, from: usize) -> Expr {
        if terms.len() == 1 { terms.remove(0) } else { self.ex(E::Sum(terms), at, from) }
    }

    /// Each `*`, `div` and `mod` nests, so it counts against the depth cap.
    fn product(&mut self) -> Result<Expr, Fail> {
        let from = self.k;
        let mut left = self.atom()?;
        let mut nested = 0;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => Arith::Mul,
                Some(Tok::Kw("div")) => Arith::Div,
                Some(Tok::Kw("mod")) => Arith::Mod,
                _ => break,
            };
            self.k += 1;
            self.enter()?;
            nested += 1;
            let right = self.atom()?;
            let at = left.at;
            left = self.ex(E::Arith(op, Box::new(left), Box::new(right)), at, from);
        }
        self.depth -= nested;
        Ok(left)
    }

    /// Comma-separated rules up to `close`.
    fn list(&mut self, close: &Tok, what: &str) -> Result<Vec<Expr>, Fail> {
        let mut out = Vec::new();
        if self.peek() == Some(close) {
            self.k += 1;
            return Ok(out);
        }
        loop {
            out.push(self.or()?);
            if self.peek() == Some(&Tok::Comma) {
                self.k += 1;
                continue;
            }
            self.expect(close, &format!("`,` or {what}"))?;
            return Ok(out);
        }
    }

    fn pieces(&mut self) -> E {
        let (out, end) = pieces_at(self.toks, self.k, self.end);
        self.k = end;
        match out.as_slice() {
            [Piece::Text(n)] => E::Name(n.clone()),
            _ => E::Interp(out),
        }
    }

    fn atom(&mut self) -> Result<Expr, Fail> {
        let (at, from) = (self.here(), self.k);
        if var_piece(self.toks, self.k, self.end).is_some() {
            let e = self.pieces();
            return self.call_or(e, at, from);
        }
        let Some(t) = self.peek().cloned() else {
            return Err((at, "the rule ends where a value was expected".into()));
        };
        self.k += 1;
        match t {
            Tok::Int(n) => Ok(self.ex(E::Int(n), at, from)),
            Tok::Kw("true") => Ok(self.ex(E::Bool(true), at, from)),
            Tok::Kw("false") => Ok(self.ex(E::Bool(false), at, from)),
            Tok::Open => {
                let mut inner = self.or()?;
                self.expect(&Tok::Close, "`)`")?;
                inner.paren = true;
                inner.span = (from, self.k);
                Ok(inner)
            }
            Tok::Kw("match") => self.matching(at, from),
            Tok::Name(_) => {
                self.k -= 1;
                let e = self.pieces();
                self.call_or(e, at, from)
            }
            Tok::Kw(k) => Err((
                at,
                format!("`{k}` is a reserved word; write `` `{k}` `` to use it as a name"),
            )),
            t => Err((at, format!("expected a value, found {}", show(&t)))),
        }
    }

    /// `name(args)` when an open parenthesis follows a plain name.
    fn call_or(&mut self, e: E, at: Pos, from: usize) -> Result<Expr, Fail> {
        if let (E::Name(f), Some(Tok::Open)) = (&e, self.peek()) {
            let f = f.clone();
            self.k += 1;
            self.enter()?;
            let args = self.list(&Tok::Close, "`)`")?;
            self.depth -= 1;
            return Ok(self.ex(E::Call(f, args), at, from));
        }
        Ok(self.ex(e, at, from))
    }

    /// `match opt.k { a, b => rule ... _ => rule }`.
    fn matching(&mut self, at: Pos, from: usize) -> Result<Expr, Fail> {
        let key = match self.peek().cloned() {
            Some(Tok::Name(n)) if n.starts_with("opt.") => n["opt.".len()..].to_owned(),
            _ => {
                return Err((
                    self.here(),
                    "`match` reads an option: `match opt.<key> { ... }`".into(),
                ));
            }
        };
        self.k += 1;
        self.expect(&Tok::LBrace, "`{` after the option")?;
        self.enter()?;
        let mut arms = Vec::new();
        while self.peek() != Some(&Tok::RBrace) {
            let start = self.k;
            let mut values = Vec::new();
            if self.peek() == Some(&Tok::Under) {
                self.k += 1;
            } else {
                loop {
                    match self.peek().cloned() {
                        Some(Tok::Name(v)) => {
                            values.push((v, self.here()));
                            self.k += 1;
                        }
                        _ => {
                            return Err((
                                self.here(),
                                "an arm starts with option values or `_`".into(),
                            ));
                        }
                    }
                    if self.peek() != Some(&Tok::Comma) {
                        break;
                    }
                    self.k += 1;
                }
            }
            self.expect(&Tok::Arrow, "`=>`")?;
            let rule = self.or()?;
            arms.push(Arm { values, rule, start });
        }
        self.k += 1;
        self.depth -= 1;
        Ok(self.ex(E::Match(key, arms), at, from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compact rendering of a tree: `(and a b)`, `(>= (count x) 3)`.
    fn sx(e: &Expr) -> String {
        let list = |head: &str, xs: &[Expr]| {
            let inner: Vec<String> = xs.iter().map(sx).collect();
            format!("({head} {})", inner.join(" "))
        };
        match &e.e {
            E::Bool(b) => b.to_string(),
            E::Int(n) => n.to_string(),
            E::Name(n) => n.clone(),
            E::Interp(ps) => ps
                .iter()
                .map(|p| match p {
                    Piece::Text(t) => t.clone(),
                    Piece::Var(v) => format!("{{{v}}}"),
                })
                .collect(),
            E::Call(f, a) => list(f, a),
            E::Not(x) => format!("(not {})", sx(x)),
            E::And(x) => list("and", x),
            E::Or(x) => list("or", x),
            E::Cmp(op, a, b) => {
                let s = ["==", "!=", "<", "<=", ">", ">="][op.index()];
                format!("({s} {} {})", sx(a), sx(b))
            }
            E::In(a, xs) => format!("(in {} {})", sx(a), list("list", xs)),
            E::Sum(xs) => list("+", xs),
            E::Arith(op, a, b) => {
                let s = match op {
                    Arith::Sub => "-",
                    Arith::Mul => "*",
                    Arith::Div => "div",
                    Arith::Mod => "mod",
                };
                format!("({s} {} {})", sx(a), sx(b))
            }
            E::Match(k, arms) => {
                let arms: Vec<String> = arms
                    .iter()
                    .map(|a| {
                        let v: Vec<&str> = a.values.iter().map(|v| v.0.as_str()).collect();
                        format!(
                            "[{}] {}",
                            if v.is_empty() { "_".into() } else { v.join(",") },
                            sx(&a.rule)
                        )
                    })
                    .collect();
                format!("(match {k} {})", arms.join(" "))
            }
        }
    }

    fn p(s: &str) -> String {
        sx(&parse(s, Pos::new(1, 1)).unwrap_or_else(|e| panic!("{s}: {e:?}")))
    }

    /// The column (from 1) of a parse error in `s`.
    fn fail(s: &str) -> (usize, String) {
        let (pos, msg) = parse(s, Pos::new(1, 1)).unwrap_err();
        (pos.col, msg)
    }

    #[test]
    fn every_operator_parses_to_its_node() {
        assert_eq!(p("true"), "true");
        assert_eq!(p("false"), "false");
        assert_eq!(p("has(rope)"), "(has rope)");
        assert_eq!(p("has(shard, 3)"), "(has shard 3)");
        assert_eq!(p("has_all(a, b)"), "(has_all a b)");
        assert_eq!(p("has_any(a, b)"), "(has_any a b)");
        assert_eq!(p("can_climb"), "can_climb");
        assert_eq!(p("opt.hard"), "opt.hard");
        assert_eq!(p("at.guards.hall"), "at.guards.hall");
        assert_eq!(p("not opt.hard"), "(not opt.hard)");
        assert_eq!(p("a and b"), "(and a b)");
        assert_eq!(p("a or b"), "(or a b)");
        for (s, op) in
            [(">=", ">="), (">", ">"), ("<=", "<="), ("<", "<"), ("==", "=="), ("!=", "!=")]
        {
            assert_eq!(p(&format!("count(x) {s} 2")), format!("({op} (count x) 2)"));
        }
        assert_eq!(p("opt.k in [a, b]"), "(in opt.k (list a b))");
        assert_eq!(p("opt.k in []"), "(in opt.k (list ))");
        assert_eq!(p("reach(cliff)"), "(reach cliff)");
        assert_eq!(p("count(a, b, c) >= 2"), "(>= (count a b c) 2)");
        assert_eq!(p("min(a, b) + max(c, 1) >= 2"), "(>= (+ (min a b) (max c 1)) 2)");
        assert_eq!(p("a * 2 div 3 mod 4 >= 1"), "(>= (mod (div (* a 2) 3) 4) 1)");
        assert_eq!(p("0 - opt.k >= -3"), "(>= (- 0 opt.k) -3)");
        assert_eq!(p("has({s.key}_track)"), "(has {s.key}_track)");
        assert_eq!(
            p("d.unlock and count(joker) >= max(0, ante - 3)"),
            "(and d.unlock (>= (count joker) (max 0 (- ante 3))))"
        );
        assert_eq!(
            p("match opt.d { easy => has(a) and has(b)  normal => has(a)  hard, glitch => true }"),
            "(match d [easy] (and (has a) (has b)) [normal] (has a) [hard,glitch] true)"
        );
        assert_eq!(
            p("match opt.d { easy => has(a) _ => true }"),
            "(match d [easy] (has a) [_] true)"
        );
        assert_eq!(p("`region` and has(`group`)"), "(and region (has group))");
    }

    #[test]
    fn precedence_follows_doc_16_section_5_0() {
        assert_eq!(p("a or b and c"), "(or a (and b c))");
        assert_eq!(p("a and b or c"), "(or (and a b) c)");
        assert_eq!(p("not a and b"), "(and (not a) b)");
        assert_eq!(p("not count(x) >= 3"), "(not (>= (count x) 3))");
        assert_eq!(p("count(x) + 1 * 2 >= 3"), "(>= (+ (count x) (* 1 2)) 3)");
        assert_eq!(p("(a or b) and c"), "(and (or a b) c)");
        assert_eq!(p("opt.a -3 >= 0"), "(>= (- opt.a 3) 0)");
        assert_eq!(p("min(-3, 2) >= -1"), "(>= (min -3 2) -1)");
        assert_eq!(p("a or -3 > 1"), "(or a (> -3 1))");
        assert_eq!(p("not -1 < opt.k and -2 < 0"), "(and (not (< -1 opt.k)) (< -2 0))");
        assert_eq!(p("a + b - c + d"), "(+ (- (+ a b) c) d)");
    }

    #[test]
    fn a_rule_in_a_file_stops_at_the_first_token_it_cannot_use() {
        let (toks, _) = lex("has(a) and b\n  check c when opt.x", Pos::new(1, 1)).unwrap();
        let (e, end) = parse_in(&toks, 0, toks.len()).unwrap();
        assert_eq!(sx(&e), "(and (has a) b)");
        assert_eq!(toks[end].t, Tok::Kw("check"));
        let (toks, _) = lex("a b", Pos::new(1, 1)).unwrap();
        assert_eq!(parse_in(&toks, 0, toks.len()).unwrap().1, 1);
    }

    #[test]
    fn errors_name_the_line_and_column_where_they_start() {
        assert_eq!(fail("a < b < c").0, 7);
        assert!(fail("a < b < c").1.contains("do not chain"));
        assert_eq!(fail("has(x").0, 6);
        assert_eq!(fail("a and").0, 6);
        assert_eq!(fail("a b").0, 3);
        assert_eq!(fail("a = b").0, 3);
        assert_eq!(fail("a ! b").0, 3);
        assert_eq!(fail("3.5 > 1").0, 1);
        assert_eq!(fail("").0, 1);
        assert_eq!(fail("opt.").0, 1);
        assert_eq!(fail("and b").0, 1);
        assert!(fail("check").1.contains("reserved word"));
        assert_eq!(fail("[a]").0, 1);
        assert_eq!(fail("é").0, 1);
        assert_eq!(fail("has(x) é").0, 8);
        assert!(fail("match d { }").1.contains("reads an option"));
        assert!(fail("match opt.d { easy has(a) }").1.contains("`=>`"));
        let (pos, _) = parse("has(a) and\n   (b or", Pos::new(10, 5)).unwrap_err();
        assert_eq!(pos, Pos::new(11, 9));
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_stack_overflow() {
        let n = config::WORLD_RULE_MAX_DEPTH;
        let ok = "(".repeat(n - 1) + "a" + &")".repeat(n - 1);
        assert!(parse(&ok, Pos::new(1, 1)).is_ok());
        assert!(parse(&("a".to_owned() + &" + a".repeat(10_000)), Pos::new(1, 1)).is_ok());
        for deep in [
            "(".repeat(100_000) + "a" + &")".repeat(100_000),
            "not ".repeat(100_000) + "a",
            "has(".repeat(100_000) + "a" + &")".repeat(100_000),
            "a".to_owned() + &" * a".repeat(100_000),
            "a".to_owned() + &" div 2".repeat(100_000),
            "a".to_owned() + &" - 1".repeat(100_000),
            "match opt.k { a => ".repeat(10_000) + "true",
        ] {
            assert!(fail(&deep).1.contains("deep"), "{}", &deep[..20]);
        }
    }
}
