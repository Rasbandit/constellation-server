//! The formatter (design doc 20 section 2.9): the canonical layout, run only when asked
//! (`rando fmt`, format on save), never by an edit (8.3). It writes the file's own tokens
//! again, one statement per line, one space around `=`, values in their canonical spelling,
//! comments kept and blank lines kept at most one in a row. It never reorders statements.
//! Formatting a formatted file gives it back unchanged.
//!
//! The same layout without comments and blank lines is what a preset's token hash covers
//! (2.6).

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use config::Limits;
use rando_syntax::lex::{Tok, Token};
use rando_syntax::{Diagnostic, hex};
use sha2::{Digest, Sha256};

use crate::ast::{File, Line, Mod, Node, Stmt, Value, parse};

/// `src` in the canonical layout.
///
/// # Errors
/// Every syntax error: a file that does not parse is not formatted.
pub fn format_text(src: &str, file: &str, l: &Limits) -> Result<String, Vec<Diagnostic>> {
    let (f, errs) = parse(src, file, l);
    let syntax: Vec<Diagnostic> = errs
        .into_iter()
        .filter(|d| {
            matches!(
                d.key.as_deref(),
                Some("settings.syntax" | "settings.block" | "settings.limit")
            )
        })
        .collect();
    if syntax.is_empty() { Ok(format(&f)) } else { Err(syntax) }
}

/// `f` in the canonical layout.
#[must_use]
pub fn format(f: &File) -> String {
    layout(f, true)
}

/// The token hash of `f` (2.6): SHA-256, in hex, of its canonical layout with comments and
/// blank lines left out.
#[must_use]
pub fn token_hash(f: &File) -> String {
    hex(&Sha256::digest(layout(f, false).as_bytes()))
}

/// The file's line ending: CRLF when it has one, else LF.
pub(crate) fn newline(src: &str) -> &'static str {
    if src.contains("\r\n") { "\r\n" } else { "\n" }
}

/// The tokens of one value, `src` their source, printed in canonical form (8.3).
#[must_use]
pub fn value_text(toks: &[Token], src: &str) -> String {
    let order: Vec<usize> = (0..toks.len()).collect();
    join(toks, src, &order, None)
}

/// One output line: its tokens in the order to write them, its indent, the token range it
/// covers in the source, and the token whose text `true` or `false` becomes `on` or `off`.
struct Out {
    order: Vec<usize>,
    indent: usize,
    range: Range<usize>,
    flip: Option<usize>,
    body: bool,
}

fn lines(f: &File) -> Vec<Out> {
    let mut out: Vec<Out> = f
        .head
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| Out {
            order: r.clone().collect(),
            indent: 0,
            range: r.clone(),
            flip: None,
            body: false,
        })
        .collect();
    let one = |l: &Line, indent: usize| {
        let order = match &l.stmt {
            Stmt::Place { mods, .. } => place_order(f, l, mods),
            _ => l.toks.clone().collect(),
        };
        // `true` and `false` as a whole value are written `on` and `off`.
        // shortcut: done without the schema, so a choice value named `true` would be renamed;
        // pass the schema in if a world ever declares one.
        let flip = match &l.stmt {
            Stmt::Setting { value: Node { v: Value::Name(n), span, .. }, .. }
                if n == "true" || n == "false" =>
            {
                f.toks.iter().position(|t| t.off == span.start)
            }
            _ => None,
        };
        Out { order, indent, range: l.toks.clone(), flip, body: true }
    };
    for l in &f.body {
        out.push(one(l, 0));
        if let Stmt::Block { body, close, .. } | Stmt::Legacy { body, close } = &l.stmt {
            out.extend(body.iter().map(|x| one(x, 1)));
            let range = *close..*close + 1;
            out.push(Out {
                order: range.clone().collect(),
                indent: 0,
                range,
                flip: None,
                body: true,
            });
        }
    }
    out
}

/// A `place` line's tokens with its modifiers in the canonical order: `copy`, `if_possible`,
/// then the chance.
fn place_order(f: &File, l: &Line, mods: &[Node<Mod>]) -> Vec<usize> {
    let at = |off: usize| f.toks.iter().position(|t| t.off == off).unwrap_or(l.toks.end);
    let first = mods.iter().map(|m| at(m.span.start)).min().unwrap_or(l.toks.end);
    let mut order: Vec<usize> = (l.toks.start..first).collect();
    let rank = |m: &Mod| match m {
        Mod::Copy => 0,
        Mod::IfPossible => 1,
        Mod::Chance(_) => 2,
    };
    let mut ms: Vec<&Node<Mod>> = mods.iter().collect();
    ms.sort_by_key(|m| rank(&m.v));
    for m in ms {
        let a = at(m.span.start);
        let b = f.toks.iter().position(|t| t.off + t.size == m.span.end).map_or(a + 1, |k| k + 1);
        order.extend(a..b);
    }
    order
}

/// The tokens `order` of `toks` on one line, with canonical spacing; trailing commas dropped.
fn join(toks: &[Token], src: &str, order: &[usize], flip: Option<usize>) -> String {
    let mut s = String::new();
    let mut prev: Option<&Tok> = None;
    for (n, &k) in order.iter().enumerate() {
        let t = &toks[k];
        let next = order.get(n + 1).map(|&j| &toks[j].t);
        if t.t == Tok::Comma && matches!(next, Some(Tok::RBrack | Tok::RBrace | Tok::Close)) {
            continue;
        }
        let space = !matches!(
            (prev, &t.t),
            (None | Some(Tok::Open | Tok::LBrack), _)
                | (_, Tok::Comma | Tok::Close | Tok::RBrack | Tok::Percent)
                | (Some(Tok::Plus | Tok::Minus), Tok::Eq)
                | (Some(Tok::LBrace), Tok::RBrace)
                | (Some(Tok::Name(_)), Tok::Open)
        );
        if space {
            s.push(' ');
        }
        let text = &src[t.off..t.off + t.size];
        s.push_str(match (flip == Some(k), text) {
            (true, "true") => "on",
            (true, "false") => "off",
            _ => text,
        });
        prev = Some(&t.t);
    }
    s
}

fn layout(f: &File, with_comments: bool) -> String {
    // The hash never depends on line endings (a CRLF checkout keeps its pin).
    let nl = if with_comments { newline(&f.src) } else { "\n" };
    let out = lines(f);
    let ntok = f.toks.len();
    // Per output line: the comments above it and its trailing comment.
    let mut above: Vec<Vec<usize>> = (0..=out.len()).map(|_| Vec::new()).collect();
    let mut trailing: Vec<Option<usize>> = (0..out.len()).map(|_| None).collect();
    if with_comments {
        for (ci, c) in f.comments.iter().enumerate() {
            let k = c.before;
            if !c.own_line
                && k > 0
                && let Some(li) = out.iter().position(|o| o.range.start < k && k <= o.range.end)
            {
                if out[li].range.end == k && trailing[li].is_none() {
                    trailing[li] = Some(ci);
                } else {
                    above[li].push(ci);
                }
                continue;
            }
            let li = if k >= ntok {
                out.len()
            } else {
                out.iter()
                    .position(|o| o.range.start <= k && k < o.range.end.max(o.range.start + 1))
                    .or_else(|| out.iter().position(|o| o.range.start >= k))
                    .unwrap_or(out.len())
            };
            above[li].push(ci);
        }
    }
    let line_of = |k: usize| f.toks[k].pos.line;
    let mut s = String::new();
    let mut last_line = 0usize;
    let mut started_body = false;
    for (li, o) in out.iter().enumerate() {
        let first_src = above[li]
            .iter()
            .map(|&c| f.comments[c].pos.line)
            .chain([line_of(o.range.start)])
            .min()
            .unwrap_or(0);
        if o.body && !started_body {
            started_body = true;
            if !s.is_empty() && with_comments {
                s.push_str(nl);
            }
        } else if o.body && with_comments && first_src > last_line + 1 && last_line > 0 {
            s.push_str(nl);
        }
        let pad = "  ".repeat(o.indent);
        for &c in &above[li] {
            s.push_str(&pad);
            s.push_str(&f.comments[c].text);
            s.push_str(nl);
        }
        s.push_str(&pad);
        s.push_str(&join(&f.toks, &f.src, &o.order, o.flip));
        last_line = line_of(o.range.end.saturating_sub(1).max(o.range.start));
        if let Some(c) = trailing[li] {
            s.push(' ');
            s.push_str(&f.comments[c].text);
            last_line = f.comments[c].pos.line;
        }
        s.push_str(nl);
    }
    let end = &above[out.len()];
    if !end.is_empty() {
        let first = f.comments[end[0]].pos.line;
        if !out.is_empty() && first > last_line + 1 {
            s.push_str(nl);
        }
        for &c in end {
            s.push_str(&f.comments[c].text);
            s.push_str(nl);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::parse;
    use crate::ast::tests::{ADVANCED, BEGINNER};
    use alloc::vec;

    const L: Limits = Limits::DESIGN;

    fn fmt(src: &str) -> String {
        format_text(src, "f", &L).unwrap_or_else(|e| panic!("{e:#?}"))
    }

    const MESSY: &str = "settings   for lanternfall\nplayer \"Mira\"\n\n\npreset relaxed\ngoal=find_the_crown   #  why\nkeep_home = [lantern,\n  # inside\n  rope,]\n\n\n\nshared_deaths = true\nwhen goal = x {\nhint_count=10\n\n    keep_home  +=  [a]\n}\nplace rope at p1 50% if_possible copy\ntraps = chance( none 50 % , a_few 50%)\nk = {a=1,b=[x x 2]}\n# the end\n";

    const MESSY_FMT: &str = "settings for lanternfall\nplayer \"Mira\"\npreset relaxed\n\ngoal = find_the_crown #  why\n# inside\nkeep_home = [lantern, rope]\n\nshared_deaths = on\nwhen goal = x {\n  hint_count = 10\n\n  keep_home += [a]\n}\nplace rope at p1 copy if_possible 50%\ntraps = chance(none 50%, a_few 50%)\nk = { a = 1, b = [x x 2] }\n# the end\n";

    #[test]
    fn a_formatted_file_comes_back_unchanged() {
        assert_eq!(fmt(BEGINNER), BEGINNER);
    }

    #[test]
    fn a_messy_file_takes_the_canonical_layout() {
        assert_eq!(fmt(MESSY), MESSY_FMT);
    }

    #[test]
    fn formatting_is_idempotent_and_keeps_statements_and_comments() {
        for src in [BEGINNER, ADVANCED, MESSY] {
            let once = fmt(src);
            assert_eq!(fmt(&once), once, "{src}");
            let (a, _) = parse(src, "f", &L);
            let (b, _) = parse(&once, "f", &L);
            let texts = |f: &File| {
                let mut v: Vec<String> = f.comments.iter().map(|c| c.text.clone()).collect();
                v.sort();
                v
            };
            assert_eq!(texts(&a), texts(&b));
            assert_eq!(a.body.len(), b.body.len());
            let presets = |f: &File| f.presets.iter().map(|p| p.v.clone()).collect::<Vec<_>>();
            assert_eq!(presets(&a), presets(&b));
            for (x, y) in a.body.iter().zip(&b.body) {
                assert_eq!(core::mem::discriminant(&x.stmt), core::mem::discriminant(&y.stmt));
            }
        }
    }

    #[test]
    fn crlf_files_keep_their_line_ending() {
        assert_eq!(fmt("settings for w\r\na=1 # c\r\n"), "settings for w\r\n\r\na = 1 # c\r\n");
    }

    #[test]
    fn the_token_hash_ignores_comments_and_layout() {
        let h = |s: &str| token_hash(&parse(s, "f", &L).0);
        let a = h("preset fast for w\n\ngoal = a # why\n\n\nk = [x,y]\n");
        assert_eq!(a, h("# top\npreset   fast for w\ngoal=a\nk = [x, y,]\n"));
        assert_ne!(a, h("preset fast for w\ngoal = b\nk = [x, y]\n"));
        assert_eq!(a.len(), 64);
        let _ = vec![0];
    }

    /// Review of PR 233, medium 4: a preset's pin does not depend on its line endings.
    #[test]
    fn the_token_hash_ignores_line_endings() {
        let h = |s: &str| token_hash(&parse(s, "f", &L).0);
        assert_eq!(h("preset fast for w\ngoal = a\n"), h("preset fast for w\r\ngoal = a\r\n"));
    }

    #[test]
    fn a_value_prints_canonically() {
        let (_, toks) = crate::ast::parse_value("[a,b x 2,]", &L).unwrap();
        assert_eq!(value_text(&toks, "[a,b x 2,]"), "[a, b x 2]");
        let src = "chance( a 50 %,b 50%)";
        let (_, toks) = crate::ast::parse_value(src, &L).unwrap();
        assert_eq!(value_text(&toks, src), "chance(a 50%, b 50%)");
        let src = "{x=1}";
        let (_, toks) = crate::ast::parse_value(src, &L).unwrap();
        assert_eq!(value_text(&toks, src), "{ x = 1 }");
    }
}
