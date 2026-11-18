//! Validated strings on the wire: command ids, type and extension names, state paths, game ids,
//! checksums and secrets (doc 01 D1.10, protocol constants and rules 2, 7 and 8).
//!
//! Each is checked when it is decoded, so a value that parses is well formed. Every grammar is
//! ASCII-only, so byte and character lengths agree and the JSON Schema patterns accept exactly
//! what the Rust checks accept.
use alloc::string::String;
#[cfg(test)]
use alloc::string::ToString as _; // the schema derive's `regex` attribute calls `to_string`
use core::{borrow::Borrow, fmt, str::FromStr};
use serde::{Deserialize, Deserializer, Serialize, de};

/// Longest command id (doc 01 D1.10 protocol constants).
pub const CMD_ID_MAX: usize = 64;
/// Longest type name, extension id, stream name or game id, in bytes.
pub const NAME_MAX: usize = 128;
/// Longest state path, in bytes.
pub const STATE_PATH_MAX: usize = 256;
/// Longest secret (token, ticket, session id), in bytes.
pub const SECRET_MAX: usize = 512;

/// Why a name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// Empty, or longer than the limit for its kind.
    Length,
    /// A character or segment outside the grammar.
    Syntax,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Length => "empty or too long",
            Self::Syntax => "not well formed",
        })
    }
}

impl core::error::Error for NameError {}

/// `[a-z][a-z0-9_]*`: one segment of a type, extension or stream name.
fn is_word(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// Dot-separated words, at least `min` of them.
fn is_dotted(s: &str, min: usize) -> bool {
    let mut n = 0_usize;
    for part in s.split('.') {
        if !is_word(part) {
            return false;
        }
        n = n.saturating_add(1);
    }
    n >= min
}

macro_rules! validated {
    ($(#[$doc:meta])* $name:ident, $check:path, $pattern:literal, $max:expr) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        #[cfg_attr(test, derive(schemars::JsonSchema))]
        pub struct $name(
            #[cfg_attr(test, schemars(length(min = 1, max = $max), regex(pattern = $pattern)))]
            String,
        );

        impl $name {
            /// The grammar as a regular expression, the same one the JSON Schema carries.
            pub const PATTERN: &'static str = $pattern;
            /// The length limit in bytes (equal to characters: every grammar is ASCII).
            pub const MAX: usize = $max;

            /// Checks `s` against the grammar.
            ///
            /// # Errors
            /// [`NameError`] when `s` is empty, too long, or not well formed.
            pub fn new(s: impl Into<String>) -> Result<Self, NameError> {
                let s = s.into();
                if s.is_empty() || s.len() > $max {
                    return Err(NameError::Length);
                }
                if !$check(&s) {
                    return Err(NameError::Syntax);
                }
                Ok(Self(s))
            }

            /// The value as sent on the wire.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::new(s)
                    .map_err(|e| de::Error::custom(format_args!("{}: {e}", stringify!($name))))
            }
        }

        impl FromStr for $name {
            type Err = NameError;
            fn from_str(s: &str) -> Result<Self, NameError> {
                Self::new(s)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = NameError;
            fn try_from(s: &str) -> Result<Self, NameError> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = NameError;
            fn try_from(s: String) -> Result<Self, NameError> {
                Self::new(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
    };
}

/// A name whose value is safe to print.
macro_rules! printable {
    ($name:ident) => {
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.0).finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

/// Visible ASCII, `!` to `~`: bytes equal characters, and the value fits an HTTP header.
fn is_visible_ascii(s: &str) -> bool {
    s.bytes().all(|c| c.is_ascii_graphic())
}

fn is_ext_id(s: &str) -> bool {
    is_dotted(s, 2)
}

fn is_stream_name(s: &str) -> bool {
    is_dotted(s, 1)
}

fn is_type_name(s: &str) -> bool {
    match s.split_once('/') {
        None => is_dotted(s, 1),
        Some((ext, local)) => is_ext_id(ext) && is_dotted(local, 1),
    }
}

/// URL-unreserved characters, which never need percent-encoding.
const fn is_unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'~' | b'-')
}

const fn upper_hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c.wrapping_sub(b'0')),
        b'A'..=b'F' => Some(c.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// One segment: unreserved characters or `%XX` with upper-case hex for any other byte; never
/// `.` or `..`, and never an escape for a character that needs none (doc 01 D1.10 rule 7).
fn is_segment(seg: &str) -> bool {
    if seg.is_empty() || seg == "." || seg == ".." {
        return false;
    }
    let b = seg.as_bytes();
    let mut i = 0_usize;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let (Some(hi), Some(lo)) = (
                b.get(i.saturating_add(1)).copied().and_then(upper_hex),
                b.get(i.saturating_add(2)).copied().and_then(upper_hex),
            ) else {
                return false;
            };
            if is_unreserved(hi.wrapping_mul(16).wrapping_add(lo)) {
                return false;
            }
            i = i.saturating_add(3);
        } else if is_unreserved(c) {
            i = i.saturating_add(1);
        } else {
            return false;
        }
    }
    true
}

fn is_state_path(s: &str) -> bool {
    s.split('/').all(is_segment)
}

/// A DNS-style label: `[a-z0-9][a-z0-9-]*`.
fn is_label(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn is_game_id(s: &str) -> bool {
    s.split('.').count() >= 2 && s.split('.').all(is_label)
}

fn is_checksum(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// Crockford base32 digits (no I, L, O, U).
const fn is_crockford(c: u8) -> bool {
    matches!(c, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')
}

fn is_item_uid(s: &str) -> bool {
    s.strip_prefix("iu_")
        .is_some_and(|b| (8..=64).contains(&b.len()) && b.bytes().all(is_crockford))
}

validated!(
    /// Client-chosen command id (doc 01 D1.10 rule 3): 1 to 64 visible ASCII characters.
    CmdId,
    is_visible_ascii,
    "^[!-~]+$",
    CMD_ID_MAX
);
printable!(CmdId);

validated!(
    /// Extension id: at least two dotted words, `org.cwp.deathlink`, `com.vendor.name`, `x.name`.
    ExtId,
    is_ext_id,
    "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)+$",
    NAME_MAX
);
printable!(ExtId);

validated!(
    /// Type of a command or event: a core name (`check`, `item.grant`) or `<extension id>/<name>`
    /// (`org.cwp.deathlink/death`). Doc 01 D1.10 rule 2.
    TypeName,
    is_type_name,
    "^([a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)+/)?[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*$",
    NAME_MAX
);
printable!(TypeName);

validated!(
    /// An optional event stream an enabled extension declares (`lobby`, `lobby.host`, `tracker`).
    StreamName,
    is_stream_name,
    "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*$",
    NAME_MAX
);
printable!(StreamName);

validated!(
    /// A path in the room's state tree (`slot/3/status`, `kv/legacy/Energy%20Link`). Doc 01 D1.10
    /// rule 7. Only the syntax is checked here; [`super::CorePath::parse`] knows which paths exist.
    StatePath,
    is_state_path,
    r"^(?:(?:[A-Za-z0-9_~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))(?:[A-Za-z0-9._~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))*|\.(?:[A-Za-z0-9_~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))(?:[A-Za-z0-9._~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))*|\.\.(?:[A-Za-z0-9._~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))+)(?:/(?:(?:[A-Za-z0-9_~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))(?:[A-Za-z0-9._~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))*|\.(?:[A-Za-z0-9_~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))(?:[A-Za-z0-9._~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))*|\.\.(?:[A-Za-z0-9._~-]|%(?:[01][0-9A-F]|2[0-9A-CF]|3[A-F]|40|5[B-E]|60|7[B-DF]|[89A-F][0-9A-F]))+))*$",
    STATE_PATH_MAX
);
printable!(StatePath);

validated!(
    /// A game id: reverse-domain, lower-case, path-safe (`dev.example.mosscave`). Doc 01 D1.10 rule 8.
    GameId,
    is_game_id,
    "^[a-z0-9][a-z0-9-]*(\\.[a-z0-9][a-z0-9-]*)+$",
    NAME_MAX
);
printable!(GameId);

validated!(
    /// A content hash: `sha256:` and 64 lower-case hex digits.
    Checksum,
    is_checksum,
    "^sha256:[0-9a-f]{64}$",
    NAME_MAX
);
printable!(Checksum);

validated!(
    /// Stable item identity (doc 01 D1.10 rule 11): `iu_` and Crockford base32.
    ItemUid,
    is_item_uid,
    "^iu_[0-9A-HJKMNP-TV-Z]{8,64}$",
    NAME_MAX
);
printable!(ItemUid);

validated!(
    /// A credential or session id: slot token, integration credential, connect ticket, session.
    /// Its `Debug` and `Display` never print the value, so it cannot leak into logs.
    Secret,
    is_visible_ascii,
    "^[!-~]+$",
    SECRET_MAX
);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl TypeName {
    /// The extension this type belongs to, or `None` for a core type.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        self.0.split_once('/').map(|(ext, _)| ext)
    }
}

impl StatePath {
    /// The raw (still percent-encoded) segments.
    #[must_use]
    pub fn segments(&self) -> core::str::Split<'_, char> {
        self.0.split('/')
    }
}
