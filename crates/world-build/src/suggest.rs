//! "Did you mean": the declared name nearest an unknown one, by edit distance.

/// The candidate nearest `name` within a small edit distance (1 for names of up to 4
/// characters, 2 up to 8, 3 beyond), ties broken by the first in sorted order; `None` when
/// none is that close.
pub(crate) fn nearest<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let most = match name.chars().count() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name {
            continue;
        }
        let d = distance(name, c);
        if d <= most && best.is_none_or(|(bd, bc)| (d, c) < (bd, bc)) {
            best = Some((d, c));
        }
    }
    best.map(|b| b.1)
}

/// `help` led by "did you mean `x`?" when a candidate is near `name`.
pub(crate) fn help<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    help: &str,
) -> String {
    match nearest(name, candidates) {
        Some(n) if help.is_empty() => format!("did you mean `{n}`?"),
        Some(n) => format!("did you mean `{n}`? Otherwise {help}"),
        None => help.to_owned(),
    }
}

/// Levenshtein distance over characters.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_close_name_is_suggested() {
        assert_eq!(nearest("can_be_gorn", ["can_be_goron", "can_burn"]), Some("can_be_goron"));
        assert_eq!(nearest("hal", ["hall", "hill"]), Some("hall"));
        assert_eq!(nearest("xyz", ["hall"]), None);
        assert_eq!(nearest("", ["a"]), Some("a"));
        assert_eq!(nearest("a", ["a"]), None);
        assert_eq!(distance("kitten", "sitting"), 3);
    }
}
