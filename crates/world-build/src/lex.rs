//! The lexer of the world language (design doc 18 section 2), shared by the front end and the
//! rule-string parser: tokens with their line and column, whether whitespace came before
//! (interpolated names are glued pieces) and whether a newline did (a table row ends at one).
//! Comments are kept beside the tokens, for the formatter.

use crate::model::Pos;
use crate::rule::Cmp;

/// Words that are always keywords: those that start a declaration or a member, `when`,
/// `match`, and the rule's own words. A reserved word is written as a name in backticks:
/// `` `group` ``. Words that follow a complete name or rule (`key`, `was`, `door`,
/// `vanilla`, `holds`, `no_progression`, `can_stay`) are not reserved: a rule ends before them.
#[rustfmt::skip]
pub const RESERVED: [&str; 33] = [
    "world", "start", "option", "item", "pool", "start_item", "assumed", "def", "group", "score",
    "steps", "table", "for", "placement", "region", "shuffle", "require", "trick", "goal",
    "extend", "replace", "remove", "generated", "use", "check", "exit", "event", "stay", "when",
    "match", "and", "or", "not",
];

/// The words that start a check's modifiers. After a bare `no_progression` they start the next
/// modifier, never a rule, so no def (a bare name `no_progression` could read) may be named one.
pub const CHECK_MODIFIERS: [&str; 5] = ["key", "was", "vanilla", "holds", "no_progression"];

/// More words a rule reads as operators or constants: reserved as well.
pub const RULE_WORDS: [&str; 5] = ["in", "div", "mod", "true", "false"];

/// One token kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// A name, dots included (`opt.k`, `at.s.slot`, `d.key`, `village.cliff`).
    Name(String),
    /// A reserved word.
    Kw(&'static str),
    /// A whole number, a leading `-` included when it is a literal.
    Int(i64),
    /// A quoted string.
    Str(String),
    /// `(` `)` `[` `]` `{` `}`
    Open,
    Close,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
    /// `,` `:` `=` `=>` `..` `/` `_`
    Comma,
    Colon,
    Eq,
    Arrow,
    DotDot,
    Slash,
    Under,
    /// A comparison.
    Cmp(Cmp),
    /// `+` `-` `*`
    Plus,
    Minus,
    Star,
}

/// A token and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What it is.
    pub t: Tok,
    /// Where it starts.
    pub pos: Pos,
    /// Nothing (no space, no comment) between it and the token before.
    pub glued: bool,
    /// A newline between it and the token before (or it is the first token).
    pub nl: bool,
    /// Its length in characters.
    pub len: usize,
}

/// A comment and where it is: `before` is the index of the token after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// The text, `#` included.
    pub text: String,
    /// Where it starts.
    pub pos: Pos,
    /// Index of the first token after it.
    pub before: usize,
    /// It is the only thing on its line.
    pub own_line: bool,
}

/// What a token is, in words.
#[must_use]
pub fn show(t: &Tok) -> String {
    match t {
        Tok::Name(n) => format!("`{n}`"),
        Tok::Kw(k) => format!("`{k}`"),
        Tok::Int(n) => format!("`{n}`"),
        Tok::Str(s) => format!("{s:?}"),
        Tok::Open => "`(`".into(),
        Tok::Close => "`)`".into(),
        Tok::LBrack => "`[`".into(),
        Tok::RBrack => "`]`".into(),
        Tok::LBrace => "`{`".into(),
        Tok::RBrace => "`}`".into(),
        Tok::Comma => "`,`".into(),
        Tok::Colon => "`:`".into(),
        Tok::Eq => "`=`".into(),
        Tok::Arrow => "`=>`".into(),
        Tok::DotDot => "`..`".into(),
        Tok::Slash => "`/`".into(),
        Tok::Under => "`_`".into(),
        Tok::Cmp(_) => "a comparison".into(),
        Tok::Plus => "`+`".into(),
        Tok::Minus => "`-`".into(),
        Tok::Star => "`*`".into(),
    }
}

fn keyword(w: &str) -> Option<&'static str> {
    RESERVED.iter().chain(&RULE_WORDS).find(|k| **k == w).copied()
}

/// A lex error: where and why.
pub type Fail = (Pos, String);

/// The tokens and comments of `src`, whose first character is at `base`.
///
/// # Errors
/// The first character that starts no token.
pub fn lex(src: &str, base: Pos) -> Result<(Vec<Token>, Vec<Comment>), Fail> {
    let chars: Vec<char> = src.chars().collect();
    let (mut toks, mut comments): (Vec<Token>, Vec<Comment>) = (Vec::new(), Vec::new());
    let (mut line, mut col) = (base.line, base.col);
    let (mut i, mut glued, mut nl) = (0, false, true);
    while i < chars.len() {
        let c = chars[i];
        let at = Pos::in_file(base.file, line, col);
        let step = |n: usize, col: &mut usize| *col += n;
        if c == '\n' {
            (line, col, i, glued, nl) = (line + 1, 1, i + 1, false, true);
            continue;
        }
        if c.is_whitespace() {
            (i, glued) = (i + 1, false);
            step(1, &mut col);
            continue;
        }
        if c == '#' {
            let end = chars[i..].iter().position(|x| *x == '\n').map_or(chars.len(), |p| i + p);
            let text: String = chars[i..end].iter().collect();
            comments.push(Comment { text, pos: at, before: toks.len(), own_line: nl });
            step(end - i, &mut col);
            (i, glued) = (end, false);
            continue;
        }
        let operand_before = match toks.last().map(|t| &t.t) {
            Some(Tok::Kw(k)) => matches!(*k, "true" | "false"),
            Some(Tok::Name(_) | Tok::Int(_) | Tok::Close | Tok::RBrack | Tok::RBrace) => true,
            _ => false,
        };
        let next = chars.get(i + 1).copied();
        let (t, len) = match c {
            '(' => (Tok::Open, 1),
            ')' => (Tok::Close, 1),
            '[' => (Tok::LBrack, 1),
            ']' => (Tok::RBrack, 1),
            '{' => (Tok::LBrace, 1),
            '}' => (Tok::RBrace, 1),
            ',' => (Tok::Comma, 1),
            ':' => (Tok::Colon, 1),
            '/' => (Tok::Slash, 1),
            '+' => (Tok::Plus, 1),
            '*' => (Tok::Star, 1),
            '.' if next == Some('.') => (Tok::DotDot, 2),
            '=' if next == Some('>') => (Tok::Arrow, 2),
            '=' if next == Some('=') => (Tok::Cmp(Cmp::Eq), 2),
            '=' => (Tok::Eq, 1),
            '!' if next == Some('=') => (Tok::Cmp(Cmp::Ne), 2),
            '!' => return Err((at, "`!` alone is not an operator; write `not` or `!=`".into())),
            '>' if next == Some('=') => (Tok::Cmp(Cmp::Ge), 2),
            '>' => (Tok::Cmp(Cmp::Gt), 1),
            '<' if next == Some('=') => (Tok::Cmp(Cmp::Le), 2),
            '<' => (Tok::Cmp(Cmp::Lt), 1),
            '-' if !operand_before && next.is_some_and(|d| d.is_ascii_digit()) => {
                let (n, len) = number(&chars, i + 1, at)?;
                (Tok::Int(-n), len + 1)
            }
            '-' => (Tok::Minus, 1),
            '"' => {
                let mut s = String::new();
                let mut k = i + 1;
                loop {
                    match chars.get(k) {
                        None | Some('\n') => {
                            return Err((at, "a string ends on its own line".into()));
                        }
                        Some('"') => break,
                        Some('\\') => match chars.get(k + 1) {
                            Some(e @ ('"' | '\\')) => {
                                s.push(*e);
                                k += 2;
                            }
                            _ => {
                                return Err((
                                    Pos::in_file(base.file, line, col + k - i),
                                    "a string escapes only `\\\"` and `\\\\`".into(),
                                ));
                            }
                        },
                        Some(x) => {
                            s.push(*x);
                            k += 1;
                        }
                    }
                }
                (Tok::Str(s), k + 1 - i)
            }
            '`' => {
                let end = chars[i + 1..].iter().position(|x| *x == '`').map(|p| i + 1 + p);
                let Some(end) = end.filter(|e| *e > i + 1) else {
                    return Err((at, "a quoted name is `name` in backticks".into()));
                };
                let name: String = chars[i + 1..end].iter().collect();
                if !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                {
                    return Err((at, format!("`{name}` is not a name")));
                }
                (Tok::Name(name), end + 1 - i)
            }
            d if d.is_ascii_digit() => {
                let (n, len) = number(&chars, i, at)?;
                (Tok::Int(n), len)
            }
            // `_` alone is a `match` wildcard; glued between name pieces (`c{i}_{j}`) it is text.
            '_' if !next.is_some_and(|x| x.is_ascii_alphanumeric() || x == '_' || x == '{')
                && !(glued && matches!(toks.last().map(|t| &t.t), Some(Tok::RBrace))) =>
            {
                (Tok::Under, 1)
            }
            a if a.is_ascii_alphabetic() || a == '_' => {
                let mut end = i;
                while end < chars.len()
                    && (chars[end].is_ascii_alphanumeric()
                        || chars[end] == '_'
                        || (chars[end] == '.'
                            && chars
                                .get(end + 1)
                                .is_some_and(|x| x.is_ascii_alphabetic() || *x == '_')))
                {
                    end += 1;
                }
                let word: String = chars[i..end].iter().collect();
                if chars.get(end) == Some(&'.') && chars.get(end + 1) != Some(&'.') {
                    return Err((at, format!("`{word}.` is not a name")));
                }
                (keyword(&word).map_or(Tok::Name(word), Tok::Kw), end - i)
            }
            c => return Err((at, format!("unexpected character `{c}`"))),
        };
        toks.push(Token { t, pos: at, glued, nl, len });
        step(len, &mut col);
        (i, glued, nl) = (i + len, true, false);
    }
    Ok((toks, comments))
}

fn number(chars: &[char], from: usize, at: Pos) -> Result<(i64, usize), Fail> {
    let mut end = from;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    let dotdot = chars.get(end) == Some(&'.') && chars.get(end + 1) == Some(&'.');
    if !dotdot && chars.get(end).is_some_and(|c| c.is_ascii_alphabetic() || *c == '_' || *c == '.')
    {
        return Err((at, "a number is whole and decimal, like `3` or `-3`".into()));
    }
    let digits: String = chars[from..end].iter().collect();
    let n = digits.parse().map_err(|_| (at, format!("{digits} is too large")))?;
    Ok((n, end - from))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src, Pos::new(1, 1)).unwrap().0.into_iter().map(|t| t.t).collect()
    }

    #[test]
    fn tokens_keep_their_place_glue_and_line_breaks() {
        let (t, c) = lex("check {d.key}_x: has(a) # note\n  exit b", Pos::new(1, 1)).unwrap();
        assert_eq!(t[0].t, Tok::Kw("check"));
        assert_eq!((t[1].t.clone(), t[1].glued), (Tok::LBrace, false));
        assert_eq!((t[2].t.clone(), t[2].glued), (Tok::Name("d.key".into()), true));
        assert_eq!(t[4].t, Tok::Name("_x".into()));
        assert!(t[4].glued);
        let exit = t.iter().find(|x| x.t == Tok::Kw("exit")).unwrap();
        assert_eq!((exit.pos, exit.nl), (Pos::new(2, 3), true));
        assert_eq!(
            (c[0].text.as_str(), c[0].own_line, c[0].pos),
            ("# note", false, Pos::new(1, 25))
        );
    }

    #[test]
    fn numbers_ranges_strings_and_quoted_names() {
        assert_eq!(kinds("1..8"), [Tok::Int(1), Tok::DotDot, Tok::Int(8)]);
        assert_eq!(
            kinds("a -3 min(-3)"),
            [
                Tok::Name("a".into()),
                Tok::Minus,
                Tok::Int(3),
                Tok::Name("min".into()),
                Tok::Open,
                Tok::Int(-3),
                Tok::Close
            ]
        );
        assert_eq!(kinds("or -3"), [Tok::Kw("or"), Tok::Int(-3)]);
        assert_eq!(kinds(r#""a \"b\"""#), [Tok::Str("a \"b\"".into())]);
        assert_eq!(
            kinds("`group` group key"),
            [Tok::Name("group".into()), Tok::Kw("group"), Tok::Name("key".into())]
        );
        assert_eq!(kinds("_ => _x"), [Tok::Under, Tok::Arrow, Tok::Name("_x".into())]);
        assert_eq!(
            kinds("a == b = c"),
            [
                Tok::Name("a".into()),
                Tok::Cmp(Cmp::Eq),
                Tok::Name("b".into()),
                Tok::Eq,
                Tok::Name("c".into())
            ]
        );
    }

    #[test]
    fn bad_characters_are_errors_with_their_place() {
        assert_eq!(lex("a\n  3.5", Pos::new(1, 1)).unwrap_err().0, Pos::new(2, 3));
        assert_eq!(lex("é", Pos::new(4, 9)).unwrap_err().0, Pos::new(4, 9));
        assert!(lex("\"open", Pos::new(1, 1)).is_err());
        assert!(lex("a.", Pos::new(1, 1)).is_err());
        assert!(lex("``", Pos::new(1, 1)).is_err());
    }
}
