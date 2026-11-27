//! The rule-string parser (design doc 16 section 5.0): a lexer, then recursive descent by
//! precedence, tightest first: function forms and parentheses; `*`, `div`, `mod`; `+`, `-`;
//! comparisons and `in` (no chaining); `not`; `and`; `or`. Nesting is capped at
//! [`config::WORLD_RULE_MAX_DEPTH`], so no input can exhaust the stack. Names are not resolved
//! here: that is the lowering's job (`crate::lower`).

/// A comparison, in the engine's operand order (`data_fill::section::CMP_OPS`).
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
    /// The engine's operand index and name.
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// An arithmetic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arith {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `div`
    Div,
    /// `mod`
    Mod,
}

/// One parsed node. `at` is the character offset of its first character in the rule string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    /// What it is.
    pub e: E,
    /// Character offset in the string.
    pub at: usize,
    /// Written inside parentheses.
    pub paren: bool,
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
    /// `a + b` and the rest.
    Arith(Arith, Box<Expr>, Box<Expr>),
}

/// A parse error: character offset and message.
pub type Fail = (usize, String);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Name(String),
    Int(i64),
    Open,
    Close,
    LBrack,
    RBrack,
    Comma,
    Cmp(Cmp),
    Plus,
    Minus,
    Star,
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, Fail> {
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<(Tok, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        let operand_before =
            matches!(out.last(), Some((Tok::Name(_) | Tok::Int(_) | Tok::Close | Tok::RBrack, _)));
        let tok = match c {
            c if c.is_whitespace() => {
                i += 1;
                continue;
            }
            '(' => Tok::Open,
            ')' => Tok::Close,
            '[' => Tok::LBrack,
            ']' => Tok::RBrack,
            ',' => Tok::Comma,
            '+' => Tok::Plus,
            '*' => Tok::Star,
            '-' if !operand_before && chars.get(i + 1).is_some_and(char::is_ascii_digit) => {
                i += 1;
                let (n, end) = number(&chars, i, start, true)?;
                out.push((Tok::Int(n), start));
                i = end;
                continue;
            }
            '-' => Tok::Minus,
            '>' | '<' | '=' | '!' => {
                let two = chars.get(i + 1) == Some(&'=');
                let op = match (c, two) {
                    ('>', true) => Cmp::Ge,
                    ('>', false) => Cmp::Gt,
                    ('<', true) => Cmp::Le,
                    ('<', false) => Cmp::Lt,
                    ('=', true) => Cmp::Eq,
                    ('!', true) => Cmp::Ne,
                    ('=', false) => {
                        return Err((i, "`=` alone is not an operator; write `==`".into()));
                    }
                    _ => {
                        return Err((
                            i,
                            "`!` alone is not an operator; write `not` or `!=`".into(),
                        ));
                    }
                };
                i += usize::from(two);
                Tok::Cmp(op)
            }
            c if c.is_ascii_digit() => {
                let (n, end) = number(&chars, i, start, false)?;
                out.push((Tok::Int(n), start));
                i = end;
                continue;
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut end = i;
                while end < chars.len()
                    && (chars[end].is_ascii_alphanumeric()
                        || chars[end] == '_'
                        || chars[end] == '.')
                {
                    end += 1;
                }
                let name: String = chars[i..end].iter().collect();
                if name.ends_with('.') || name.contains("..") {
                    return Err((i, format!("`{name}` is not a name")));
                }
                out.push((Tok::Name(name), start));
                i = end;
                continue;
            }
            c => return Err((i, format!("unexpected character `{c}`"))),
        };
        out.push((tok, start));
        i += 1;
    }
    Ok(out)
}

fn number(chars: &[char], from: usize, start: usize, neg: bool) -> Result<(i64, usize), Fail> {
    let mut end = from;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    if chars.get(end).is_some_and(|c| c.is_ascii_alphabetic() || *c == '_' || *c == '.') {
        return Err((start, "a number is whole and decimal, like `3` or `-3`".into()));
    }
    let digits: String = chars[from..end].iter().collect();
    let n: i64 = digits.parse().map_err(|_| (start, format!("{digits} is too large")))?;
    Ok((if neg { -n } else { n }, end))
}

struct P {
    toks: Vec<(Tok, usize)>,
    k: usize,
    depth: usize,
    end: usize,
    /// Places where `and` sits unparenthesized inside `or`.
    mixed: Vec<usize>,
}

/// A rule string parsed, and the places where `and` and `or` mix without parentheses (the
/// build warns there: design doc 16 section 5.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The tree.
    pub expr: Expr,
    /// Offsets of unparenthesized `and` groups inside an `or`.
    pub mixed: Vec<usize>,
}

/// Parse one rule string.
///
/// # Errors
/// The first syntax error, with its character offset.
pub fn parse(src: &str) -> Result<Parsed, Fail> {
    let toks = lex(src)?;
    let end = src.chars().count();
    let mut p = P { toks, k: 0, depth: 0, end, mixed: Vec::new() };
    if p.toks.is_empty() {
        return Err((0, "the rule is empty; write `true` for no requirement".into()));
    }
    let expr = p.or()?;
    if let Some((t, at)) = p.toks.get(p.k) {
        return Err((*at, format!("unexpected {} after a complete rule", show(t))));
    }
    Ok(Parsed { expr, mixed: p.mixed })
}

fn show(t: &Tok) -> String {
    match t {
        Tok::Name(n) => format!("`{n}`"),
        Tok::Int(n) => format!("`{n}`"),
        Tok::Open => "`(`".into(),
        Tok::Close => "`)`".into(),
        Tok::LBrack => "`[`".into(),
        Tok::RBrack => "`]`".into(),
        Tok::Comma => "`,`".into(),
        Tok::Cmp(_) => "a comparison".into(),
        Tok::Plus => "`+`".into(),
        Tok::Minus => "`-`".into(),
        Tok::Star => "`*`".into(),
    }
}

fn ex(e: E, at: usize) -> Expr {
    Expr { e, at, paren: false }
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.k).map(|x| &x.0)
    }

    fn here(&self) -> usize {
        self.toks.get(self.k).map_or(self.end, |x| x.1)
    }

    fn word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Name(n)) if n == w)
    }

    fn expect(&mut self, t: &Tok, what: &str) -> Result<(), Fail> {
        if self.peek() == Some(t) {
            self.k += 1;
            Ok(())
        } else {
            let got = self.peek().map_or_else(|| "the end of the rule".to_owned(), show);
            Err((self.here(), format!("expected {what}, found {got}")))
        }
    }

    fn enter(&mut self) -> Result<(), Fail> {
        self.depth += 1;
        if self.depth > config::WORLD_RULE_MAX_DEPTH {
            let most = config::WORLD_RULE_MAX_DEPTH;
            return Err((self.here(), format!("the rule nests more than {most} deep")));
        }
        Ok(())
    }

    fn or(&mut self) -> Result<Expr, Fail> {
        self.enter()?;
        let at = self.here();
        let mut parts = vec![self.and()?];
        while self.word("or") {
            self.k += 1;
            parts.push(self.and()?);
        }
        self.depth -= 1;
        if parts.len() == 1 {
            return Ok(parts.remove(0));
        }
        for p in &parts {
            if matches!(p.e, E::And(_)) && !p.paren {
                self.mixed.push(p.at);
            }
        }
        Ok(ex(E::Or(parts), at))
    }

    fn and(&mut self) -> Result<Expr, Fail> {
        let at = self.here();
        let mut parts = vec![self.not()?];
        while self.word("and") {
            self.k += 1;
            parts.push(self.not()?);
        }
        Ok(if parts.len() == 1 { parts.remove(0) } else { ex(E::And(parts), at) })
    }

    fn not(&mut self) -> Result<Expr, Fail> {
        if self.word("not") {
            let at = self.here();
            self.k += 1;
            self.enter()?;
            let inner = self.not()?;
            self.depth -= 1;
            return Ok(ex(E::Not(Box::new(inner)), at));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, Fail> {
        let at = self.here();
        let left = self.sum()?;
        let e = if let Some(Tok::Cmp(op)) = self.peek().cloned() {
            self.k += 1;
            let right = self.sum()?;
            E::Cmp(op, Box::new(left), Box::new(right))
        } else if self.word("in") {
            self.k += 1;
            self.expect(&Tok::LBrack, "`[` after `in`")?;
            let items = self.list(&Tok::RBrack, "`]`")?;
            E::In(Box::new(left), items)
        } else {
            return Ok(left);
        };
        if matches!(self.peek(), Some(Tok::Cmp(_))) || self.word("in") {
            return Err((self.here(), "comparisons do not chain; join them with `and`".into()));
        }
        Ok(ex(e, at))
    }

    fn sum(&mut self) -> Result<Expr, Fail> {
        let mut left = self.product()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => Arith::Add,
                Some(Tok::Minus) => Arith::Sub,
                _ => return Ok(left),
            };
            self.k += 1;
            let right = self.product()?;
            let at = left.at;
            left = ex(E::Arith(op, Box::new(left), Box::new(right)), at);
        }
    }

    fn product(&mut self) -> Result<Expr, Fail> {
        let mut left = self.atom()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => Arith::Mul,
                Some(Tok::Name(n)) if n == "div" => Arith::Div,
                Some(Tok::Name(n)) if n == "mod" => Arith::Mod,
                _ => return Ok(left),
            };
            self.k += 1;
            let right = self.atom()?;
            let at = left.at;
            left = ex(E::Arith(op, Box::new(left), Box::new(right)), at);
        }
    }

    /// Comma-separated expressions up to `close`.
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

    fn atom(&mut self) -> Result<Expr, Fail> {
        let at = self.here();
        let Some(t) = self.peek().cloned() else {
            return Err((at, "the rule ends where a value was expected".into()));
        };
        self.k += 1;
        match t {
            Tok::Int(n) => Ok(ex(E::Int(n), at)),
            Tok::Open => {
                let mut inner = self.or()?;
                self.expect(&Tok::Close, "`)`")?;
                inner.paren = true;
                Ok(inner)
            }
            Tok::Name(n) => match n.as_str() {
                "true" => Ok(ex(E::Bool(true), at)),
                "false" => Ok(ex(E::Bool(false), at)),
                "and" | "or" | "not" | "in" | "div" | "mod" => {
                    Err((at, format!("`{n}` needs a value before it")))
                }
                _ if self.peek() == Some(&Tok::Open) => {
                    self.k += 1;
                    self.enter()?;
                    let args = self.list(&Tok::Close, "`)`")?;
                    self.depth -= 1;
                    Ok(ex(E::Call(n, args), at))
                }
                _ => Ok(ex(E::Name(n), at)),
            },
            t => Err((at, format!("expected a value, found {}", show(&t)))),
        }
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
            E::Call(f, a) => list(f, a),
            E::Not(x) => format!("(not {})", sx(x)),
            E::And(x) => list("and", x),
            E::Or(x) => list("or", x),
            E::Cmp(op, a, b) => {
                let s = ["==", "!=", "<", "<=", ">", ">="][op.index()];
                format!("({s} {} {})", sx(a), sx(b))
            }
            E::In(a, xs) => format!("(in {} {})", sx(a), list("list", xs)),
            E::Arith(op, a, b) => {
                let s = match op {
                    Arith::Add => "+",
                    Arith::Sub => "-",
                    Arith::Mul => "*",
                    Arith::Div => "div",
                    Arith::Mod => "mod",
                };
                format!("({s} {} {})", sx(a), sx(b))
            }
        }
    }

    fn p(s: &str) -> String {
        sx(&parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}")).expr)
    }

    fn fail(s: &str) -> Fail {
        parse(s).unwrap_err()
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
        assert_eq!(p("level(sword) >= 2"), "(>= (level sword) 2)");
        assert_eq!(p("group_count(medals) >= 2"), "(>= (group_count medals) 2)");
        assert_eq!(p("counter(bell) >= 3"), "(>= (counter bell) 3)");
        assert_eq!(p("min(a, b) + max(c, 1) >= 2"), "(>= (+ (min a b) (max c 1)) 2)");
        assert_eq!(p("a * 2 div 3 mod 4 >= 1"), "(>= (mod (div (* a 2) 3) 4) 1)");
        assert_eq!(p("0 - opt.k >= -3"), "(>= (- 0 opt.k) -3)");
        assert_eq!(p("during(night)"), "(during night)");
        assert_eq!(p("roll(price) <= 5"), "(<= (roll price) 5)");
    }

    #[test]
    fn precedence_follows_doc_16_section_5_0() {
        assert_eq!(p("a or b and c"), "(or a (and b c))");
        assert_eq!(p("a and b or c"), "(or (and a b) c)");
        assert_eq!(p("not a and b"), "(and (not a) b)");
        assert_eq!(p("not count(x) >= 3"), "(not (>= (count x) 3))");
        assert_eq!(p("count(x) + 1 * 2 >= 3"), "(>= (+ (count x) (* 1 2)) 3)");
        assert_eq!(p("(a or b) and c"), "(and (or a b) c)");
        assert_eq!(
            p("count(shard) >= opt.shards_needed and reach(cliff)"),
            "(and (>= (count shard) opt.shards_needed) (reach cliff))"
        );
        // A minus right after an operand is subtraction; elsewhere it starts a literal.
        assert_eq!(p("opt.a -3 >= 0"), "(>= (- opt.a 3) 0)");
        assert_eq!(p("min(-3, 2) >= -1"), "(>= (min -3 2) -1)");
    }

    #[test]
    fn mixing_and_with_or_without_parentheses_is_noted_for_a_warning() {
        assert_eq!(parse("a or b and c").unwrap().mixed, [5]);
        assert!(parse("a or (b and c)").unwrap().mixed.is_empty());
        assert!(parse("a and b").unwrap().mixed.is_empty());
    }

    #[test]
    fn errors_name_the_character_where_they_start() {
        assert_eq!(fail("a < b < c").0, 6);
        assert!(fail("a < b < c").1.contains("do not chain"));
        assert_eq!(fail("has(x").0, 5);
        assert_eq!(fail("a and").0, 5);
        assert_eq!(fail("a b").0, 2);
        assert_eq!(fail("a = b").0, 2);
        assert_eq!(fail("a ! b").0, 2);
        assert_eq!(fail("x # y").0, 2);
        assert_eq!(fail("3.5 > 1").0, 0);
        assert_eq!(fail("").0, 0);
        assert_eq!(fail("  ").0, 0);
        assert_eq!(fail("opt.").0, 0);
        assert_eq!(fail("a..b").0, 0);
        assert_eq!(fail("and b").0, 0);
        assert_eq!(fail("99999999999999999999 > 1").0, 0);
        assert_eq!(fail("[a]").0, 0);
        assert_eq!(fail("é").0, 0);
        // Offsets count characters, not bytes.
        assert_eq!(fail("has(x) é").0, 7);
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_stack_overflow() {
        let n = config::WORLD_RULE_MAX_DEPTH;
        let ok = "(".repeat(n - 1) + "a" + &")".repeat(n - 1);
        assert!(parse(&ok).is_ok());
        for deep in [
            "(".repeat(100_000) + "a" + &")".repeat(100_000),
            "not ".repeat(100_000) + "a",
            "has(".repeat(100_000) + "a" + &")".repeat(100_000),
        ] {
            assert!(fail(&deep).1.contains("deep"));
        }
    }
}
