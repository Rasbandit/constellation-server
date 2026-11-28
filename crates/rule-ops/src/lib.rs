//! The comparison operators of the world rule language and the one monotone rule for them:
//! shared by the fill engine (`data-fill`) and the world build tool (`world-build`), so the
//! build tool runs without the engine and both read comparisons the same way.

#![no_std]

/// Comparison operators, in their bytecode operand order (the manifest spells them the same).
pub const CMP_OPS: [&str; 6] = ["eq", "ne", "lt", "le", "gt", "ge"];

/// Compare two integers by operand (an index into [`CMP_OPS`]).
#[must_use]
pub fn compare(op: u8, a: i64, b: i64) -> bool {
    match op {
        0 => a == b,
        1 => a != b,
        2 => a < b,
        3 => a <= b,
        4 => a > b,
        _ => a >= b,
    }
}

/// Whether operand `op` over sides that do (or do not) read state can only turn from false to
/// true as state grows: state may sit only on the side that must be larger.
#[must_use]
pub fn monotone_cmp(op: usize, left_state: bool, right_state: bool) -> bool {
    match op {
        // lt, le: the right side must be the larger one.
        2 | 3 => !left_state,
        // gt, ge.
        4 | 5 => !right_state,
        _ => !left_state && !right_state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_at_least_comparisons_take_state_and_on_the_growing_side() {
        assert!(monotone_cmp(5, true, false) && monotone_cmp(3, false, true));
        assert!(!monotone_cmp(5, false, true) && !monotone_cmp(2, true, false));
        assert!(!monotone_cmp(0, true, false) && monotone_cmp(0, false, false));
        assert!(compare(5, 3, 3) && !compare(4, 3, 3) && compare(1, 2, 3));
    }
}
