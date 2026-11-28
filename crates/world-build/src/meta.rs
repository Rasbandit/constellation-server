//! `rando.toml` (design doc 19 section 2): the one fixed file of a world folder. It names the
//! entry file and holds what is not game logic (name, version, authors, license, links,
//! assets). The engine never reads it; `rando pack` writes it, normalised, as `meta.json`.

use config::Limits;
use toml::{Table, Value};

/// The file that marks a world folder.
pub const FILE: &str = "rando.toml";

/// The keys `[world]` takes; any other key or table is an error, so a typo is never dropped.
const WORLD_KEYS: [&str; 9] = [
    "name",
    "version",
    "entry",
    "authors",
    "license",
    "description",
    "icon",
    "homepage",
    "repository",
];

/// A checked `rando.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// The world's name: its game key.
    pub name: String,
    /// Semantic Versioning, no build metadata.
    pub version: String,
    /// The entry file, relative to the folder (default `world.rando`).
    pub entry: String,
    /// Credit, shown publicly. Empty when not given (required to publish).
    pub authors: Vec<String>,
    /// An SPDX license expression (required to publish).
    pub license: Option<String>,
    /// Plain text for the world page (required to publish).
    pub description: Option<String>,
    /// A PNG or WebP file in the folder.
    pub icon: Option<String>,
    /// An `https://` link.
    pub homepage: Option<String>,
    /// An `https://` link.
    pub repository: Option<String>,
    /// `[assets] files`: PNG, WebP or plain-text files for the world page.
    pub assets: Vec<String>,
}

impl Meta {
    /// The fields `rando pack` requires that this file leaves out, as `world.KEY`.
    #[must_use]
    pub fn missing_to_publish(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.authors.is_empty() {
            out.push("world.authors");
        }
        if self.license.is_none() {
            out.push("world.license");
        }
        if self.description.is_none() {
            out.push("world.description");
        }
        out
    }
}

/// Collects every problem of one file.
struct P(Vec<String>);

impl P {
    fn string(&mut self, t: &Table, key: &str) -> Option<String> {
        match t.get(key)? {
            Value::String(s) => Some(s.clone()),
            _ => {
                self.0.push(format!("`world.{key}` must be a string"));
                None
            }
        }
    }

    fn strings(&mut self, t: &Table, table: &str, key: &str) -> Vec<String> {
        let Some(v) = t.get(key) else { return Vec::new() };
        let list = v.as_array().map(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
        if let Some(Some(list)) = list {
            return list.into_iter().map(str::to_owned).collect();
        }
        self.0.push(format!("`{table}.{key}` must be a list of strings"));
        Vec::new()
    }

    fn check(&mut self, ok: bool, msg: impl FnOnce() -> String) {
        if !ok {
            self.0.push(msg());
        }
    }
}

/// Parse and check `rando.toml`.
///
/// # Errors
/// Every problem found, one message each.
pub fn parse(text: &str, l: &Limits) -> Result<Meta, Vec<String>> {
    if text.len() > l.world_toml_max_bytes {
        return Err(vec![format!("is over {} bytes", l.world_toml_max_bytes)]);
    }
    let t: Table = text.parse().map_err(|e: toml::de::Error| vec![e.to_string()])?;
    let mut p = P(Vec::new());
    for k in t.keys().filter(|k| !["world", "assets"].contains(&k.as_str())) {
        p.0.push(format!("unknown table or key `{k}`"));
    }
    let empty = Table::new();
    let w = match t.get("world") {
        Some(Value::Table(w)) => w,
        Some(_) => {
            p.0.push("`world` must be a table, `[world]`".into());
            &empty
        }
        None => {
            p.0.push("`[world]` is missing".into());
            &empty
        }
    };
    for k in w.keys().filter(|k| !WORLD_KEYS.contains(&k.as_str())) {
        p.0.push(format!("unknown key `world.{k}`"));
    }
    let name = p.string(w, "name");
    let most = l.world_name_max_bytes;
    p.check(
        name.as_ref().is_some_and(|n| {
            !n.is_empty() && n.len() <= most && crate::world::name_shape(n)
        }),
        || format!("`world.name` is required: a `.rando` name (lowercase letters, digits and `_`, not starting with a digit, with at least one letter) of at most {most} bytes"),
    );
    let version = p.string(w, "version");
    if let Some(v) = &version {
        p.check(!v.contains('+'), || {
            format!("`world.version` {v:?} has build metadata (`+...`): two contents would share one version; leave it out")
        });
        p.check(v.len() <= l.world_version_max_bytes, || {
            format!("`world.version` is over {} bytes", l.world_version_max_bytes)
        });
        p.check(v.contains('+') || semver(v), || {
            format!("`world.version` {v:?} is not Semantic Versioning: MAJOR.MINOR.PATCH, with an optional pre-release (`1.2.0-beta.1`)")
        });
    } else {
        p.check(false, || "`world.version` is required, as Semantic Versioning (`1.0.0`)".into());
    }
    let entry = p.string(w, "entry").unwrap_or_else(|| crate::front::ENTRY.to_owned());
    p.check(crate::front::safe(&entry) && entry.ends_with(".rando"), || {
        format!(
            "`world.entry` {entry:?} must be a relative path inside the folder ending in `.rando`"
        )
    });
    let authors = p.strings(w, "world", "authors");
    if w.contains_key("authors") {
        let most = l.world_authors_max;
        p.check((1..=most).contains(&authors.len()), || {
            format!("`world.authors` takes 1 to {most} names")
        });
    }
    // shortcut: no warning yet that an email written here is public (doc 19 section 2.3); a
    // warning would refuse `pack`, so it needs its own channel first. Add it with the portal.
    for a in &authors {
        let most = l.world_author_max_chars;
        p.check(text_ok(a, most, false), || {
            format!("author {a:?} must be 1 to {most} characters of plain text")
        });
    }
    let lim = l;
    let license = p.string(w, "license");
    if let Some(l) = &license {
        p.check(license_ok(l, lim.world_license_max_bytes), || {
            format!("`world.license` {l:?} is not an SPDX license expression (`MIT`, `MIT OR Apache-2.0`)")
        });
    }
    let description = p.string(w, "description");
    if let Some(d) = &description {
        let most = lim.world_description_max_chars;
        p.check(text_ok(d, most, true), || {
            format!("`world.description` must be 1 to {most} characters of plain text")
        });
    }
    let icon = p.string(w, "icon");
    // shortcut: the icon's pixel size (square, 64 to 512) is not read from its header yet;
    // the server checks it (doc 19 section 5.1, step 4). Add it before the portal opens.
    if let Some(i) = &icon {
        p.check(crate::front::safe(i), || {
            format!("`world.icon` {i:?} must be a relative path inside the folder")
        });
    }
    let (homepage, repository) = (p.string(w, "homepage"), p.string(w, "repository"));
    for (k, u) in [("homepage", &homepage), ("repository", &repository)] {
        if let Some(u) = u {
            let most = lim.world_url_max_bytes;
            p.check(url_ok(u, most), || {
                format!("`world.{k}` must be an `https://` URL of at most {most} bytes")
            });
        }
    }
    let mut assets = Vec::new();
    match t.get("assets") {
        None => {}
        Some(Value::Table(a)) => {
            for k in a.keys().filter(|k| *k != "files") {
                p.0.push(format!("unknown key `assets.{k}`"));
            }
            assets = p.strings(a, "assets", "files");
            let most = lim.world_assets_max;
            p.check(assets.len() <= most, || {
                format!("`assets.files` lists more than {most} files")
            });
            let mut seen = std::collections::HashSet::new();
            for f in assets.iter().filter(|f| !seen.insert(*f)) {
                p.0.push(format!("asset {f:?} is listed twice"));
            }
            for f in &assets {
                p.check(crate::front::safe(f), || {
                    format!("asset {f:?} must be a relative path inside the folder (no globs)")
                });
            }
        }
        Some(_) => p.0.push("`assets` must be a table, `[assets]`".into()),
    }
    if !p.0.is_empty() {
        return Err(p.0);
    }
    Ok(Meta {
        name: name.unwrap_or_default(),
        version: version.unwrap_or_default(),
        entry,
        authors,
        license,
        description,
        icon,
        homepage,
        repository,
        assets,
    })
}

/// Semantic Versioning 2.0.0 without build metadata, numbers in 64 bits.
fn semver(v: &str) -> bool {
    let num = |n: &str| {
        !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && (n == "0" || !n.starts_with('0'))
            && n.parse::<u64>().is_ok()
    };
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|n| num(n))
        && pre.is_none_or(|p| {
            p.split('.').all(|id| {
                !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && (!id.bytes().all(|b| b.is_ascii_digit()) || num(id))
            })
        })
}

/// Plain text: 1 to `max` characters, no control characters (line breaks and tabs only where
/// `lines`).
fn text_ok(s: &str, max: usize, lines: bool) -> bool {
    (1..=max).contains(&s.chars().count())
        && s.chars().all(|c| !c.is_control() || (lines && matches!(c, '\n' | '\t')))
}

/// The shape of an SPDX expression, at most `max` bytes. shortcut: the identifiers are not
/// checked against the SPDX license list; add the list when the portal opens (doc 19 2.3).
fn license_ok(l: &str, max: usize) -> bool {
    (1..=max).contains(&l.len())
        && l.bytes().all(|b| b.is_ascii_alphanumeric() || b" .-+():".contains(&b))
        && l.bytes().any(|b| b.is_ascii_alphanumeric())
}

/// An `https://` URL of at most `max` bytes with no spaces or control characters.
fn url_ok(u: &str, max: usize) -> bool {
    u.len() <= max
        && u.len() > "https://".len()
        && u.starts_with("https://")
        && u.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_follow_semantic_versioning_without_build_metadata() {
        for ok in ["0.1.0", "1.2.3", "1.0.0-beta.1", "1.0.0-0.3.7", "1.0.0-x-y"] {
            assert!(semver(ok), "{ok}");
        }
        for bad in ["1.0", "1.0.0.0", "01.0.0", "1.0.0-", "1.0.0-01", "1.0.0-a..b", "v1.0.0", ""] {
            assert!(!semver(bad), "{bad}");
        }
        assert!(!semver("18446744073709551616.0.0"), "past 64 bits");
    }
}
