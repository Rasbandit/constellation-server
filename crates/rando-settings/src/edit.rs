//! Edits by byte span (design doc 20 section 8.3): an edit replaces only the bytes of what it
//! changes, and every other byte stays as the player wrote it. A value from a client must parse
//! as exactly one value; it is printed in canonical form before it is spliced in, so raw client
//! bytes never enter the file. Edits act only on top-level statements.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use config::Limits;
use rando_syntax::Diagnostic;
use rando_syntax::lex::{Kind, Tok, lex};

use crate::ast::{Count, Entry, File, Node, Op, Span, Stmt, Value, parse, parse_value};
use crate::diag;
use crate::fmt::{newline, value_text};
use crate::schema::Schema;

fn bad(msg: &str) -> Box<Diagnostic> {
    Box::new(diag("edit", "settings.bad_value", None, msg.to_owned(), None))
}

/// `src` parsed, refused when it has a syntax error, and the top-level line for `key`, after
/// checking `key` is one name that no block of the file changes.
fn open(src: &str, key: &str, l: &Limits) -> Result<(File, Option<usize>), Box<Diagnostic>> {
    match lex(key, rando_syntax::Pos::new(1, 1), Kind::Settings) {
        Ok((t, c))
            if c.is_empty()
                && t.len() == 1
                && matches!(t[0].t, Tok::Name(_))
                && t[0].size == key.len() => {}
        _ => return Err(bad("A key is one name.")),
    }
    let (f, errs) = parse(src, "edit", l);
    if let Some(e) = errs.into_iter().find(|d| d.key.as_deref() != Some("settings.header")) {
        return Err(e.into());
    }
    let in_block = f.body.iter().any(|line| match &line.stmt {
        Stmt::Block { body, .. } => {
            body.iter().any(|b| matches!(&b.stmt, Stmt::Setting { key: k, .. } if k.v == key))
        }
        _ => false,
    });
    if in_block {
        let msg = format!("A rule of this file changes '{key}', so it is edited as a rule.");
        let help = "edit the rule: on the Rules panel, with set_rule, or as text".to_owned();
        return Err(diag("edit", "settings.in_block", None, msg, Some(help)).into());
    }
    let at = f.body.iter().position(
        |line| matches!(&line.stmt, Stmt::Setting { key: k, op: Op::Set, .. } if k.v == key),
    );
    Ok((f, at))
}

fn splice(src: &str, edits: &mut [(usize, usize, String)]) -> String {
    edits.sort_by_key(|e| core::cmp::Reverse(e.0));
    let mut out = src.to_owned();
    for (a, b, t) in edits.iter() {
        out.replace_range(*a..*b, t);
    }
    out
}

/// The byte where the line holding byte `at` ends (before its line break), or the end.
fn line_end(src: &str, at: usize) -> usize {
    src[at..].find(['\r', '\n']).map_or(src.len(), |k| at + k)
}

/// An entry as text in canonical form.
fn entry_text(e: &Entry) -> String {
    match e.count.as_ref().map(|c| c.v) {
        None => e.name.v.clone(),
        Some(Count::N(n)) => format!("{} x {n}", e.name.v),
        Some(Count::All) => format!("{} x all", e.name.v),
    }
}

/// `src` with `key` set to `value`: the old value's bytes replaced, or a list spliced entry by
/// entry, or a new line added after the last top-level statement of the key's screen group
/// (at the end without a schema or a statement of that group).
///
/// # Errors
/// A file that does not parse, a key that is not one name, a value that is not exactly one
/// value (`settings.bad_value`, `settings.limit`), or a key a block of this file changes
/// (`settings.in_block`).
pub fn set(
    src: &str,
    key: &str,
    value: &str,
    schema: Option<&Schema>,
    l: &Limits,
) -> Result<String, Box<Diagnostic>> {
    let out = set_unchecked(src, key, value, schema, l)?;
    // A belt over the value's own checks: an edit never leaves a file the next edit refuses.
    let (_, errs) = parse(&out, "edit", l);
    match errs.into_iter().find(|d| d.key.as_deref() != Some("settings.header")) {
        Some(e) => Err(e.into()),
        None => Ok(out),
    }
}

fn set_unchecked(
    src: &str,
    key: &str,
    value: &str,
    schema: Option<&Schema>,
    l: &Limits,
) -> Result<String, Box<Diagnostic>> {
    let (new, toks) = parse_value(value.trim_matches([' ', '\t']), l)?;
    let printed = value_text(&toks, value.trim_matches([' ', '\t']));
    let (f, at) = open(src, key, l)?;
    if let Some(i) = at {
        let Stmt::Setting { value: old, .. } = &f.body[i].stmt else { unreachable!() };
        let mut edits = match (&old.v, &new) {
            (Value::List(o), Value::List(n)) if !o.is_empty() => list_edits(src, o, n),
            _ => None,
        }
        .unwrap_or_else(|| Vec::from([(old.span.start, old.span.end, printed)]));
        return Ok(splice(src, &mut edits));
    }
    let nl = newline(src);
    let line = format!("{key} = {printed}");
    let group = |k: &str| schema.and_then(|s| s.options.get(k)).and_then(|o| o.section.clone());
    let mine = group(key);
    let after = f
        .body
        .iter()
        .rev()
        .find(|x| matches!(&x.stmt, Stmt::Setting { key: k, .. } if schema.is_none() || group(&k.v) == mine))
        .filter(|_| schema.is_some())
        .map(|x| x.span);
    let Some(after) = after else {
        // At the end of the file.
        let mut out = src.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&line);
        out.push_str(nl);
        return Ok(out);
    };
    let end = line_end(src, after.end);
    let mut out = src.to_owned();
    if end == src.len() {
        out.push_str(nl);
        out.push_str(&line);
        out.push_str(nl);
    } else {
        // After the line break of the statement's last line, so the new line keeps the
        // file's own ending.
        let brk = if src[end..].starts_with("\r\n") { end + 2 } else { end + 1 };
        out.insert_str(brk, &format!("{line}{nl}"));
    }
    Ok(out)
}

/// The edits that turn list `old` into `new` entry by entry, or `None` when every old entry
/// goes (the whole value is replaced then). A removed entry standing alone on its line goes
/// with its whole line, its comment included; one sharing a line goes with its comma. Kept
/// entries keep their comments.
fn list_edits(
    src: &str,
    old: &[Node<Entry>],
    new: &[Node<Entry>],
) -> Option<Vec<(usize, usize, String)>> {
    let new_texts: Vec<String> = new.iter().map(|e| entry_text(&e.v)).collect();
    let old_texts: Vec<String> = old.iter().map(|e| entry_text(&e.v)).collect();
    let kept: Vec<bool> = old_texts.iter().map(|t| new_texts.contains(t)).collect();
    let last_kept = kept.iter().rposition(|k| *k)?;
    let added: Vec<&String> = new_texts.iter().filter(|t| !old_texts.contains(t)).collect();
    let bytes = src.as_bytes();
    let nl = newline(src);
    let skip_spaces = |mut k: usize| {
        while k < src.len() && matches!(bytes[k], b' ' | b'\t') {
            k += 1;
        }
        k
    };
    // One past the comma after byte `end` (spaces between allowed), when there is one.
    let comma_after = |end: usize| {
        let k = skip_spaces(end);
        (bytes.get(k) == Some(&b',')).then_some(k + 1)
    };
    let line_start = |at: usize| src[..at].rfind('\n').map_or(0, |k| k + 1);
    let alone_before = |at: usize| src[line_start(at)..at].trim_matches([' ', '\t']).is_empty();
    // Only spaces and a comment from `k` to the line's end.
    let rest_is_comment = |k: usize| {
        let k = skip_spaces(k);
        k >= src.len() || matches!(bytes[k], b'#' | b'\r' | b'\n')
    };
    let next_line = |k: usize| src[k..].find('\n').map_or(src.len(), |n| k + n + 1);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut drop_anchor_comma = false;
    for (i, e) in old.iter().enumerate() {
        if kept[i] {
            continue;
        }
        let comma = comma_after(e.span.end);
        let after = comma.unwrap_or(e.span.end);
        if alone_before(e.span.start) && rest_is_comment(after) {
            let start = line_start(e.span.start);
            edits.push((start, next_line(after), String::new()));
        } else if let Some(c) = comma {
            edits.push((e.span.start, skip_spaces(c), String::new()));
        } else {
            // The last entry, sharing its line: the comma before it goes too.
            edits.push((e.span.start, e.span.end, String::new()));
            drop_anchor_comma = i > last_kept;
        }
    }
    let anchor: Span = old[last_kept].span;
    let anchor_comma = comma_after(anchor.end);
    let multi = old.len() > 1 && old[old.len() - 2].pos.line != old[old.len() - 1].pos.line;
    let indent = &src[line_start(anchor.start)..anchor.start];
    let (at, text) = if multi
        && alone_before(anchor.start)
        && rest_is_comment(anchor_comma.unwrap_or(anchor.end))
    {
        // New entries on lines of their own, after the anchor's line and its comment.
        let lines: String = added.iter().map(|t| format!("{indent}{t},{nl}")).collect();
        if anchor_comma.is_none() && !added.is_empty() {
            edits.push((anchor.end, anchor.end, ",".into()));
        }
        (next_line(anchor_comma.unwrap_or(anchor.end)), lines)
    } else {
        (anchor.end, added.iter().map(|t| format!(", {t}")).collect())
    };
    if drop_anchor_comma && let Some(c) = anchor_comma {
        edits.push((anchor.end, skip_spaces(c), String::new()));
    }
    if !text.is_empty() {
        // An insertion where a deletion starts joins it, so the two never race.
        match edits.iter_mut().find(|e| e.0 == at) {
            Some(e) => e.2.insert_str(0, &text),
            None => edits.push((at, at, text)),
        }
    }
    Some(edits)
}

/// `src` without its top-level line for `key` (and that line's trailing comment); own-line
/// comments above it stay. A file that does not set `key` comes back unchanged.
///
/// # Errors
/// As [`set`].
pub fn reset(src: &str, key: &str, l: &Limits) -> Result<String, Box<Diagnostic>> {
    let (f, at) = open(src, key, l)?;
    let Some(i) = at else { return Ok(src.to_owned()) };
    let span = f.body[i].span;
    let start = src[..span.start].rfind('\n').map_or(0, |k| k + 1);
    let end = line_end(src, span.end);
    let end = if src[end..].starts_with("\r\n") { end + 2 } else { (end + 1).min(src.len()) };
    let mut out = src.to_owned();
    out.replace_range(start..end, "");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::tests::lanternfall;
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;

    const L: Limits = Limits::DESIGN;

    const HAND: &str = "settings for lanternfall\r\nplayer \"Mira\"\r\n\r\n# Playing with others\r\nshared_deaths = true      # my opt-in\r\nkeep_home = [lantern, # the light\r\n  rope,\r\n]\r\n\r\n# How hard\r\nlogic_level   = normal\r\nwhen goal = light_every_lamp {\r\n  hint_count = 10\r\n}\r\n";

    fn ok(src: &str, key: &str, value: &str) -> String {
        set(src, key, value, Some(&lanternfall()), &L).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The bytes that differ between `a` and `b`: `a`'s and `b`'s middle after the common
    /// prefix and suffix.
    fn diff<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
        let p = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
        let s = a[p..].bytes().rev().zip(b[p..].bytes().rev()).take_while(|(x, y)| x == y).count();
        (&a[p..a.len() - s], &b[p..b.len() - s])
    }

    #[test]
    fn setting_a_key_replaces_only_its_value() {
        let out = ok(HAND, "logic_level", "expert");
        assert_eq!(diff(HAND, &out), ("normal", "expert"));
        let out = ok(HAND, "shared_deaths", "off");
        assert_eq!(diff(HAND, &out), ("true", "off"));
        // A client's raw bytes never reach the file: the value is printed canonically.
        let out = ok(HAND, "logic_level", "  one_of( normal,expert )");
        assert_eq!(diff(HAND, &out).1, "one_of(normal, expert)");
    }

    #[test]
    fn a_list_is_spliced_entry_by_entry() {
        let out = ok(HAND, "keep_home", "[lantern, rope, boots]");
        assert_eq!(out, HAND.replace("  rope,\r\n]", "  rope,\r\n  boots,\r\n]"));
        let removed = ok(HAND, "keep_home", "[rope]");
        assert!(removed.contains("keep_home = [# the light\r\n  rope,\r\n]"), "{removed}");
        let one_line = "settings for w\nk = [a, b, c] # list\n";
        let s = |v: &str| set(one_line, "k", v, None, &L).unwrap();
        assert_eq!(s("[a, c]"), "settings for w\nk = [a, c] # list\n");
        assert_eq!(s("[a, b]"), "settings for w\nk = [a, b] # list\n");
        assert_eq!(s("[a]"), "settings for w\nk = [a] # list\n");
        assert_eq!(s("[b, c, d]"), "settings for w\nk = [b, c, d] # list\n");
        assert_eq!(s("[x]"), "settings for w\nk = [x] # list\n");
        assert_eq!(s("[]"), "settings for w\nk = [] # list\n");
        assert_eq!(
            set("settings for w\nk = []\n", "k", "[a]", None, &L).unwrap(),
            "settings for w\nk = [a]\n"
        );
    }

    #[test]
    fn a_new_key_is_one_added_line_in_its_group() {
        let out = ok(HAND, "keep_moving", "strong");
        assert_eq!(diff(HAND, &out), ("", "keep_moving = strong\r\n"));
        assert!(out.contains("]\r\nkeep_moving = strong\r\n\r\n# How hard"), "{out}");
        let out = ok(HAND, "bombs", "plenty");
        assert!(out.contains("logic_level   = normal\r\nbombs = plenty\r\nwhen"), "{out}");
        assert_eq!(set("settings for w", "a", "1", None, &L).unwrap(), "settings for w\na = 1\n");
    }

    #[test]
    fn reset_deletes_the_line_and_its_trailing_comment() {
        let out = reset(HAND, "shared_deaths", &L).unwrap();
        assert_eq!(diff(HAND, &out), ("shared_deaths = true      # my opt-in\r\n", ""));
        assert!(out.contains("# Playing with others\r\nkeep_home"));
        assert_eq!(reset(HAND, "bombs", &L).unwrap(), HAND);
    }

    /// Review of PR 233, medium 5: removing or adding entries keeps the comments of the
    /// entries that stay on their own lines.
    #[test]
    fn a_splice_keeps_the_comments_of_kept_entries() {
        let src = "settings for w\nk = [\n  lantern,   # keep\n  rope,   # remove me\n  boots,   # remove\n]\n";
        assert_eq!(
            set(src, "k", "[lantern]", None, &L).unwrap(),
            "settings for w\nk = [\n  lantern,   # keep\n]\n"
        );
        let src = "settings for w\nk = [\n  lantern,   # keep\n  rope,   # r\n]\n";
        assert_eq!(
            set(src, "k", "[lantern, rope, boots]", None, &L).unwrap(),
            "settings for w\nk = [\n  lantern,   # keep\n  rope,   # r\n  boots,\n]\n"
        );
        assert_eq!(
            set(src, "k", "[rope]", None, &L).unwrap(),
            "settings for w\nk = [\n  rope,   # r\n]\n"
        );
    }

    /// Review of PR 233, high 3: a value past a cap is refused at `set`, so the file stays
    /// editable.
    #[test]
    fn a_value_past_a_cap_is_refused_and_the_file_stays_editable() {
        let src = "settings for lanternfall\nkeep_home = [lantern]\n";
        for v in ["[rope x 70000]", "[rope x 0]"] {
            assert_eq!(
                set(src, "keep_home", v, None, &L).unwrap_err().key.as_deref(),
                Some("settings.limit"),
                "{v}"
            );
        }
        assert!(reset(src, "keep_home", &L).is_ok());
    }

    #[test]
    fn edits_are_refused_when_they_must_be() {
        let key = |src: &str, k: &str, v: &str| {
            set(src, k, v, None, &L).unwrap_err().key.unwrap_or_default()
        };
        assert_eq!(key(HAND, "hint_count", "3"), "settings.in_block");
        assert_eq!(
            reset(HAND, "hint_count", &L).unwrap_err().key.as_deref(),
            Some("settings.in_block")
        );
        for bad in ["a\nb = c", "a # c", "a b", "on\r\nplace x at y"] {
            assert_eq!(key(HAND, "logic_level", bad), "settings.bad_value", "{bad:?}");
        }
        for k in ["a b", "a = 1", "", "\"a\""] {
            assert_eq!(key(HAND, k, "1"), "settings.bad_value", "{k:?}");
        }
        assert_eq!(key("settings for w\nk = [a,\n", "k", "1"), "settings.syntax");
        let big = alloc::format!("\"{}\"", "a".repeat(L.settings_max_value_bytes));
        assert_eq!(key(HAND, "greeting", &big), "settings.limit");
        let e = set(HAND, "hint_count", "3", None, &L).unwrap_err().to_string();
        assert!(e.contains("Rules"), "{e}");
        let _: Vec<u8> = vec![];
    }
}
