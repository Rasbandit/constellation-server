//! Edits by byte span (design doc 20 section 8.3): an edit replaces only the bytes of what it
//! changes, and every other byte stays as the player wrote it. A value from a client must parse
//! as exactly one value; it is printed in canonical form before it is spliced in, so raw client
//! bytes never enter the file. Edits act only on top-level statements.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use config::Limits;
use rando_syntax::Diagnostic;
use rando_syntax::lex::{Kind, Tok, lex};

use crate::ast::{Count, Entry, File, Node, Op, Span, Stmt, Value, parse, parse_value};
use crate::diag;
use crate::fmt::value_text;
use crate::schema::Schema;

fn bad(msg: &str) -> Diagnostic {
    diag("edit", "settings.bad_value", None, msg.to_owned(), None)
}

/// `src` parsed, refused when it has a syntax error, and the top-level line for `key`, after
/// checking `key` is one name that no block of the file changes.
fn open(src: &str, key: &str, l: &Limits) -> Result<(File, Option<usize>), Diagnostic> {
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
        return Err(e);
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
        return Err(diag("edit", "settings.in_block", None, msg, Some(help)));
    }
    let at = f.body.iter().position(
        |line| matches!(&line.stmt, Stmt::Setting { key: k, op: Op::Set, .. } if k.v == key),
    );
    Ok((f, at))
}

fn splice(src: &str, edits: &mut Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| core::cmp::Reverse(e.0));
    let mut out = src.to_owned();
    for (a, b, t) in edits.iter() {
        out.replace_range(*a..*b, t);
    }
    out
}

/// The file's line ending.
fn newline(src: &str) -> &'static str {
    if src.contains("\r\n") { "\r\n" } else { "\n" }
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
) -> Result<String, Diagnostic> {
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
/// goes (the whole value is replaced then).
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
    let mut edits = Vec::new();
    let bytes = src.as_bytes();
    for (i, e) in old.iter().enumerate().take(last_kept) {
        if kept[i] {
            continue;
        }
        // The entry, its comma and the spaces after it on the same line.
        let mut end = e.span.end;
        while end < src.len() && bytes[end] != b',' {
            end += 1;
        }
        end += 1;
        while end < src.len() && matches!(bytes[end], b' ' | b'\t') {
            end += 1;
        }
        edits.push((e.span.start, end, String::new()));
    }
    let tail = old.last().map_or(old[last_kept].span, |e| e.span);
    let anchor: Span = old[last_kept].span;
    // The separator the list already uses: a line break and the last entry's indent when its
    // entries stand on lines of their own.
    let multi = old.len() > 1 && old[old.len() - 2].pos.line != old[old.len() - 1].pos.line;
    let sep = if multi {
        let start = src[..anchor.start].rfind('\n').map_or(0, |k| k + 1);
        format!(",{}{}", newline(src), &src[start..anchor.start])
    } else {
        ", ".to_owned()
    };
    let insert: String = added.iter().map(|t| format!("{sep}{t}")).collect();
    // Entries after the last kept one go from its end to the list's last entry.
    edits.push((anchor.end, tail.end.max(anchor.end), insert));
    Some(edits)
}

/// `src` without its top-level line for `key` (and that line's trailing comment); own-line
/// comments above it stay. A file that does not set `key` comes back unchanged.
///
/// # Errors
/// As [`set`].
pub fn reset(src: &str, key: &str, l: &Limits) -> Result<String, Diagnostic> {
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
        assert_eq!(
            out,
            HAND.replace("  rope,\r\n]", "  rope,\r\n  boots,\r\n]")
                .replace("  rope,\r\n  boots,", "  rope,\r\n  boots,")
        );
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
