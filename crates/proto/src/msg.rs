//! Commands in, events out (doc 05 7.1). Minimal: `Check`, `Ack` and `Goal`; `Ext` and `Host` come later.
//!
//! Location and item ids are `i64`: worlds made by other generators use ids up to 2^53 - 1 and
//! reserve negative ids, and a location id is only unique within the world of the slot that
//! checks it (two slots of the same game share ids).
use crate::{Cid, ItemSeq, Seq, SlotNo, Timestamp};
use serde::{Deserialize, Serialize};

/// A client command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Check a location. `(slot, cid)` is the idempotency key (doc 02 D3).
    Check {
        /// The checking slot.
        slot: SlotNo,
        /// Client message id.
        cid: Cid,
        /// Location id in the checking slot's world.
        location: i64,
    },
    /// The slot reached its goal. Once per slot; a repeat is answered from the first.
    Goal {
        /// The slot.
        slot: SlotNo,
    },
    /// Cumulative item ack: a cursor update, not a log event (D-02).
    Ack {
        /// The acking slot.
        slot: SlotNo,
        /// Highest `item_seq` the client has applied.
        applied: ItemSeq,
    },
}

/// One log entry.
///
/// Deviations from doc 05 7.1, both out of scope for now: no ULID event id (the room-wide `seq`
/// is the id), and `Item` carries no `item_uid` (`H(room, location_id)`, doc 02 D3); the
/// `location` field identifies the item until the hash exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Room-wide, gapless, starts at 1.
    pub seq: Seq,
    /// The slot the event is about: the checker for `Checked`, the receiver for `Item`.
    pub slot: Option<SlotNo>,
    /// What happened.
    pub kind: EventKind,
}

/// Event payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// A location was checked. Carries the cid and time so a fold can rebuild the dedupe index.
    Checked {
        /// The checker's message id.
        cid: Cid,
        /// Location id in the checking slot's world.
        location: i64,
        /// Time the command was applied.
        at: Timestamp,
    },
    /// An item was delivered to the event's slot.
    Item {
        /// The slot whose location produced it (the checker).
        from: SlotNo,
        /// Location in `from`'s world that produced it.
        location: i64,
        /// Item id in the receiving slot's world.
        item: i64,
        /// Per-slot, contiguous from 1.
        item_seq: ItemSeq,
    },
    /// The event's slot reached its goal.
    Goal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_survives_a_json_round_trip() {
        let c = Command::Check { slot: SlotNo(1), cid: Cid(7), location: -9 };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&s).unwrap(), c);
    }

    #[test]
    fn an_event_survives_a_json_round_trip() {
        let e = Event {
            seq: Seq(u64::MAX),
            slot: None,
            kind: EventKind::Item {
                from: SlotNo(2),
                location: i64::MIN,
                item: i64::MAX,
                item_seq: ItemSeq(0),
            },
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&s).unwrap(), e);
    }

    #[test]
    fn a_truncated_command_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Command>("{\"Check\":{\"slot\":1}").is_err());
    }
}
