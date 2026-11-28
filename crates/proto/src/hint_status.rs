//! Hint status values (protocol doc, "`HintStatus`"): the codes a hint is stored and sent with.
//! Here, not in the room, because the seed format stores them too.

/// No status chosen.
pub const UNSPECIFIED: u8 = 0;
/// The receiver does not need it soon.
pub const NO_PRIORITY: u8 = 10;
/// The receiver would rather not get it (traps).
pub const AVOID: u8 = 20;
/// The receiver wants it.
pub const PRIORITY: u8 = 30;
/// The location is checked. Never set by a request; it follows from the checks.
pub const FOUND: u8 = 40;

/// Whether `s` is a status a hint may be stored with.
#[must_use]
pub const fn valid(s: u8) -> bool {
    matches!(s, UNSPECIFIED | NO_PRIORITY | AVOID | PRIORITY | FOUND)
}
