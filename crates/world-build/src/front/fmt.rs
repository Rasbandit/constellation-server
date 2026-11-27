//! The formatter (design doc 18 section 5.1, `fmt`): one canonical layout, comments kept.
//! It writes the file's own tokens again, breaking lines where the parser marked a
//! declaration, a member, a row or an arm, two spaces per level, one space between tokens
//! except around punctuation, and parentheses around `and` inside `or`. A rule too long for a
//! line of [`WIDTH`] characters is broken before each top-level `or` (or `and`), indented one
//! level under its line; a generated file's hash does not count these breaks. Formatting a
//! formatted file gives it back unchanged.

use super::parse::FileAst;
use crate::lex::{RESERVED, RULE_WORDS, Tok, Token};

/// The longest line `fmt` writes a rule on before breaking it (a line with one long name can
/// still be longer).
pub const WIDTH: usize = 100;

/// The file in its canonical layout.
#[must_use]
pub fn format(ast: &FileAst) -> String {
    format_from(ast, 0)
}

/// The file from token `from` on (the content after a `generated` block), canonical.
#[must_use]
pub fn format_from(ast: &FileAst, from: usize) -> String {
    layout(ast, from, true)
}

/// [`format_from`] without the comments: what a generated file's hash covers.
#[must_use]
pub fn format_tokens(ast: &FileAst, from: usize) -> String {
    layout(ast, from, false)
}

fn layout(ast: &FileAst, from: usize, with_comments: bool) -> String {
    let toks = &ast.toks;
    let mut out = String::new();
    let mut depth = 0usize;
    let mut last_line = toks.get(from).map_or(0, |t| t.pos.line);
    let mut need_break = false;
    let comments: Vec<_> =
        ast.comments.iter().filter(|c| with_comments && c.before >= from).collect();
    let mut ci = 0;
    // Parentheses to add before and after each token, counted once: a scan of every span per
    // token was quadratic in a rule's size.
    let (mut open, mut close) = (vec![0usize; toks.len()], vec![0usize; toks.len()]);
    for &(a, b) in &ast.layout.parens {
        if let Some(n) = open.get_mut(a) {
            *n += 1;
        }
        if let Some(n) = b.checked_sub(1).and_then(|c| close.get_mut(c)) {
            *n += 1;
        }
    }
    // Rules too long for one line, by first token; and the tokens to break before, with
    // their indent, once one is found too long.
    let wraps: std::collections::HashMap<usize, (usize, &Vec<usize>)> = if with_comments {
        ast.layout.wraps.iter().map(|(a, b, at)| (*a, (*b, at))).collect()
    } else {
        std::collections::HashMap::new()
    };
    let mut wrap_at: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for i in from..=toks.len() {
        if let Some((end, at)) = wraps.get(&i)
            && !at.is_empty()
        {
            let line = out.rsplit('\n').next().map_or(0, |l| l.chars().count());
            let mut len = 0;
            for k in i..(*end).min(toks.len()) {
                let glue = ast.layout.glue.get(k).copied().unwrap_or(false);
                len += text(&toks[k].t).chars().count() + open[k] + close[k];
                len += usize::from(k > i && space(&toks[k - 1], &toks[k], glue));
            }
            if line + 1 + len > WIDTH {
                at.iter().for_each(|k| {
                    wrap_at.insert(*k, depth + 1);
                });
            }
        }
        // Comments that come before this token.
        while let Some(c) = comments.get(ci).filter(|c| c.before == i) {
            if c.own_line {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if !out.is_empty() && c.pos.line > last_line + 1 {
                    out.push('\n');
                }
                // At the depth of what it sits in: a comment before a closing `}` stays with
                // the members, one inside a rule that goes on stays with the rule.
                let d = match ast.layout.brk.get(i).copied().flatten() {
                    Some(b) => b.max(depth),
                    None => depth + 1,
                };
                out.push_str(&"  ".repeat(d));
            } else {
                out.push(' ');
            }
            out.push_str(c.text.trim_end());
            out.push('\n');
            last_line = c.pos.line;
            need_break = true;
            ci += 1;
        }
        let Some(t) = toks.get(i) else { break };
        let brk = ast.layout.brk.get(i).copied().flatten();
        if let Some(d) = brk {
            depth = d;
        }
        if let Some(d) = wrap_at.get(&i).copied().filter(|_| brk.is_none() && !need_break) {
            out.push('\n');
            out.push_str(&"  ".repeat(d));
        } else if brk.is_some() || need_break {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            // Without comments (the hash), blank lines are left out: a comment's line is not one.
            if with_comments && !out.is_empty() && t.pos.line > last_line + 1 && brk.is_some() {
                out.push('\n');
            }
            let d = brk.unwrap_or(depth + 1);
            out.push_str(&"  ".repeat(d));
        } else if i > from
            && space(&toks[i - 1], t, ast.layout.glue.get(i).copied().unwrap_or(false))
        {
            out.push(' ');
        }
        need_break = false;
        out.extend(std::iter::repeat_n('(', open[i]));
        out.push_str(&text(&t.t));
        out.extend(std::iter::repeat_n(')', close[i]));
        last_line = t.pos.line;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Whether one space goes between `a` and `b`.
fn space(a: &Token, b: &Token, glue: bool) -> bool {
    !glue
        && !matches!(
            (&a.t, &b.t),
            (_, Tok::Comma | Tok::Close | Tok::RBrack | Tok::Colon | Tok::DotDot)
                | (Tok::Open | Tok::LBrack | Tok::DotDot, _)
                | (Tok::Name(_), Tok::Open)
        )
}

/// A token as source text.
#[must_use]
pub fn text(t: &Tok) -> String {
    match t {
        Tok::Name(n) => {
            let reserved = RESERVED.contains(&n.as_str()) || RULE_WORDS.contains(&n.as_str());
            if reserved { format!("`{n}`") } else { n.clone() }
        }
        Tok::Kw(k) => (*k).to_owned(),
        Tok::Int(n) => n.to_string(),
        Tok::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Tok::Open => "(".into(),
        Tok::Close => ")".into(),
        Tok::LBrack => "[".into(),
        Tok::RBrack => "]".into(),
        Tok::LBrace => "{".into(),
        Tok::RBrace => "}".into(),
        Tok::Comma => ",".into(),
        Tok::Colon => ":".into(),
        Tok::Eq => "=".into(),
        Tok::Arrow => "=>".into(),
        Tok::DotDot => "..".into(),
        Tok::Slash => "/".into(),
        Tok::Under => "_".into(),
        Tok::Cmp(c) => ["==", "!=", "<", "<=", ">", ">="][c.index()].into(),
        Tok::Plus => "+".into(),
        Tok::Minus => "-".into(),
        Tok::Star => "*".into(),
    }
}
