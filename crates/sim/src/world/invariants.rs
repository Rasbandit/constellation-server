//! Checks over a whole log, written independently of `room-core` so they can catch its bugs.
use proto::{Event, EventKind, ItemSeq, Seq, SlotNo};
use room_core::Placement;
use std::collections::{BTreeMap, BTreeSet};

/// Return `Err(message)` unless the condition holds.
macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!($($arg)+));
        }
    };
}
pub(crate) use ensure;

/// doc 02 section 6.3: contiguous `seq`, gapless `item_seq` per slot, every location checked and
/// delivered at most once, every item matching the placement table and following its check.
pub(super) fn check_log(events: &[Event], placement: &Placement) -> Result<(), String> {
    let mut expected_seq = 1_u64;
    let mut item_seq: BTreeMap<SlotNo, u64> = BTreeMap::new();
    let (mut checked, mut delivered) = (BTreeSet::new(), BTreeSet::new());
    let mut last_checked: Option<(SlotNo, i64)> = None;
    for e in events {
        ensure!(
            e.seq == Seq(expected_seq),
            "seq not contiguous: expected {expected_seq}, got {}",
            e.seq.0
        );
        expected_seq = expected_seq.saturating_add(1);
        let slot = e.slot.ok_or_else(|| format!("event {} has no slot", e.seq.0))?;
        match e.kind {
            EventKind::Goal | EventKind::KvSet { .. } => {}
            EventKind::Checked { location, .. } => {
                ensure!(
                    checked.insert((slot, location)),
                    "location {location} checked twice (seq {})",
                    e.seq.0
                );
                last_checked = Some((slot, location));
            }
            EventKind::Item { from, location, item, item_seq: got } => {
                ensure!(
                    last_checked == Some((from, location)),
                    "item at seq {} does not follow its check",
                    e.seq.0
                );
                ensure!(delivered.insert((from, location)), "location {location} delivered twice");
                ensure!(
                    placement.get(&from).and_then(|m| m.get(&location)) == Some(&(slot, item)),
                    "item at seq {} disagrees with the placement table",
                    e.seq.0
                );
                let next = item_seq.entry(slot).or_default().saturating_add(1);
                ensure!(
                    got == ItemSeq(next),
                    "slot {} item_seq gap: expected {next}, got {}",
                    slot.0,
                    got.0
                );
                item_seq.insert(slot, next);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{Cid, Timestamp};

    fn placement() -> Placement {
        Placement::from([(SlotNo(2), [(1, (SlotNo(1), 50)), (2, (SlotNo(1), 51))].into())])
    }

    fn checked(seq: u64, location: i64) -> Event {
        Event {
            seq: Seq(seq),
            slot: Some(SlotNo(2)),
            kind: EventKind::Checked { cid: Cid(seq), location, at: Timestamp(0) },
        }
    }

    /// An item slot 2 found for slot 1.
    fn item(seq: u64, location: i64, item: i64, item_seq: u64) -> Event {
        Event {
            seq: Seq(seq),
            slot: Some(SlotNo(1)),
            kind: EventKind::Item { from: SlotNo(2), location, item, item_seq: ItemSeq(item_seq) },
        }
    }

    fn good() -> Vec<Event> {
        vec![checked(1, 1), item(2, 1, 50, 1), checked(3, 2), item(4, 2, 51, 2)]
    }

    #[test]
    fn a_valid_log_and_an_empty_log_pass() {
        assert_eq!(check_log(&good(), &placement()), Ok(()));
        assert_eq!(check_log(&[], &placement()), Ok(()));
    }

    #[test]
    fn each_broken_invariant_is_reported() {
        let p = placement();
        let mut gap = good();
        gap.remove(1);
        assert!(check_log(&gap, &p).unwrap_err().contains("seq not contiguous"));
        let mut skip = good();
        skip[3] = item(4, 2, 51, 3);
        assert!(check_log(&skip, &p).unwrap_err().contains("item_seq gap"));
        let mut dup = good();
        dup[2] = checked(3, 1);
        assert!(check_log(&dup, &p).unwrap_err().contains("checked twice"));
        let mut wrong = good();
        wrong[1] = item(2, 1, 99, 1);
        assert!(check_log(&wrong, &p).unwrap_err().contains("placement"));
        let mut orphan = good();
        orphan[1] = item(2, 2, 51, 1);
        assert!(check_log(&orphan, &p).unwrap_err().contains("does not follow"));
        let mut twice = good();
        twice.extend([item(5, 2, 51, 3)]);
        assert!(check_log(&twice, &p).is_err());
    }
}
