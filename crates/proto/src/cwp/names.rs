//! Validated strings on the wire: command ids, type names, extension ids and state paths.
//!
//! Each is checked when it is decoded, so a frame that parses holds only well-formed names.
use alloc::string::String;
#[cfg(test)]
use alloc::string::ToString as _; // the schema derive's `regex` attribute calls `to_string`
use core::fmt;
use serde::{Deserialize, Deserializer, Serialize, de};

/// Longest command id in bytes (doc 01 D1.4).
pub const CMD_ID_MAX: usize = 64;
/// Longest type name in bytes.
pub const TYPE_NAME_MAX: usize = 128;
/// Longest state path in bytes.
pub const STATE_PATH_MAX: usize = 256;

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

/// One dotted segment of a core type name or a type's local name: `[a-z][a-z0-9_]*`.
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
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        #[cfg_attr(test, derive(schemars::JsonSchema))]
        pub struct $name(
            #[cfg_attr(test, schemars(length(min = 1, max = $max), regex(pattern = $pattern)))]
            String,
        );

        impl $name {
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

            /// The name as sent on the wire.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::new(s).map_err(|e| de::Error::custom(format_args!("{}: {e}", stringify!($name))))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

/// Any non-empty string: the length limit is the only rule for a command id.
const fn any(_: &str) -> bool {
    true
}

fn is_ext_id(s: &str) -> bool {
    is_dotted(s, 2)
}

fn is_type_name(s: &str) -> bool {
    match s.split_once('/') {
        None => is_dotted(s, 1),
        Some((ext, local)) => is_ext_id(ext) && is_dotted(local, 1),
    }
}

/// Path segments: URL-unreserved characters plus `%`, so storage keys with other characters
/// travel percent-encoded.
fn is_state_path(s: &str) -> bool {
    s.split('/').all(|seg| {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'~' | b'-' | b'%'))
    })
}

validated!(
    /// Client-chosen command id, the idempotency key, unique per slot (doc 01 D1.4): 1 to 64 bytes.
    CmdId,
    any,
    "^.+$",
    CMD_ID_MAX
);

validated!(
    /// Extension id: at least two dotted words, `org.cwp.deathlink`, `com.vendor.name`, `x.name`.
    ExtId,
    is_ext_id,
    "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)+$",
    TYPE_NAME_MAX
);

validated!(
    /// Type of a command or event: a core name (`check`, `item.grant`) or `<extension id>/<name>`
    /// (`org.cwp.deathlink/death`). Doc 01 D1.10 rule 1.
    TypeName,
    is_type_name,
    "^([a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)+/)?[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*$",
    TYPE_NAME_MAX
);

validated!(
    /// A path in the room's state tree (`slot/3/status`, `kv/legacy/EnergyLink0`). Doc 01 D1.10
    /// rule 4. Only the syntax is checked here; which paths exist is the server's registry.
    StatePath,
    is_state_path,
    "^[A-Za-z0-9._~%-]+(/[A-Za-z0-9._~%-]+)*$",
    STATE_PATH_MAX
);

impl TypeName {
    /// The extension this type belongs to, or `None` for a core type.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        self.0.split_once('/').map(|(ext, _)| ext)
    }
}

impl StatePath {
    /// The first segment (`room`, `slot`, `game`, `kv`, `ext`).
    #[must_use]
    pub fn root(&self) -> &str {
        self.0.split('/').next().unwrap_or_default()
    }
}
