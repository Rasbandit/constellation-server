//! Commands in, events out (doc 05 7.1). Minimal: `Check` and `Ack` only; `Ext` and `Host` come later.
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
        /// Location id.
        location: u32,
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
/// Out of scope for now: ULID event ids (doc 05 7.1). The room-wide `seq` is the id until then.
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
        /// Location id.
        location: u32,
        /// Time the command was applied.
        at: Timestamp,
    },
    /// An item was delivered to the event's slot.
    Item {
        /// Location that produced it.
        location: u32,
        /// Item id.
        item: u32,
        /// Per-slot, contiguous from 1.
        item_seq: ItemSeq,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_survives_a_json_round_trip() {
        let c = Command::Check { slot: SlotNo(1), cid: Cid(7), location: 9 };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&s).unwrap(), c);
    }

    #[test]
    fn an_event_survives_a_json_round_trip() {
        let e = Event {
            seq: Seq(u64::MAX),
            slot: None,
            kind: EventKind::Item { location: 0, item: u32::MAX, item_seq: ItemSeq(0) },
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&s).unwrap(), e);
    }

    #[test]
    fn a_truncated_command_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Command>("{\"Check\":{\"slot\":1}").is_err());
    }
}
