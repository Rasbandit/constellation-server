//! Commands in, events out (doc 05 7.1). Minimal: `Check`, `Ack`, `Goal`, `KvSet` and the two
//! hint commands; `Ext` and `Host` come later.
//!
//! Location and item ids are `i64`: worlds made by other generators use ids up to 2^53 - 1 and
//! reserve negative ids, and a location id is only unique within the world of the slot that
//! checks it (two slots of the same game share ids).
use crate::{Cid, ItemSeq, Seq, SlotNo, Timestamp};
use alloc::{boxed::Box, string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One operation on a stored value. Our own names for the operations a client compatible with
/// Archipelago may send (protocol doc, "`DataStorageOperation`"); `room_core` defines what each
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KvOp {
    /// The operand becomes the value.
    Replace,
    /// Keep the value (the start value already applied the default).
    Default,
    /// Sum, or concatenation of strings and of lists.
    Add,
    /// Product, or repetition of a string or a list.
    Mul,
    /// Power.
    Pow,
    /// Floor modulo (the result takes the divisor's sign).
    Mod,
    /// Round down to an integer.
    Floor,
    /// Round up to an integer.
    Ceil,
    /// The larger; a tie keeps the current value.
    Max,
    /// The smaller; a tie keeps the current value.
    Min,
    /// Bitwise and.
    And,
    /// Bitwise or, or a merge of two objects.
    Or,
    /// Bitwise exclusive or.
    Xor,
    /// Shift left.
    LeftShift,
    /// Arithmetic shift right.
    RightShift,
    /// Remove the first equal element of a list.
    Remove,
    /// Remove a list index or an object key.
    Pop,
    /// Add missing elements to a list, or merge into an object.
    Update,
}

/// A write to the room's key-value store: the start value, then each operation in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvWrite {
    /// The writing slot.
    pub slot: SlotNo,
    /// The key.
    pub key: String,
    /// The start value when the key has none; `None` means the integer 0.
    pub default: Option<Value>,
    /// Operations with their operands, applied in order.
    pub ops: Vec<(KvOp, Value)>,
}

/// One location a hint request names, and the status a new hint for it starts with (an
/// already checked location starts as found whatever is asked; `room_core::hint` has the values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintAsk {
    /// The slot whose world holds the location.
    pub finding: SlotNo,
    /// Location id in that world.
    pub location: i64,
    /// Status of a new hint.
    pub status: u8,
}

/// Ask for hints: scouts that create hints (free, found ones
/// kept), and the `!hint` chat commands (new unfound hints cost points, found ones are shown
/// but not kept).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintRequest {
    /// The asking slot; it pays when `pay` is set.
    pub slot: SlotNo,
    /// The locations, most wanted first: with points for only one new hint, the first new
    /// unfound one is taken.
    pub asks: Vec<HintAsk>,
    /// New unfound hints cost points (one per request while the room's hint cost is not 0).
    pub pay: bool,
    /// Keep a new hint whose location is already checked (scouts do, `!hint` does not).
    pub keep_found: bool,
}

/// A client command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Write one key of the room's key-value store (boxed: much larger than the others).
    KvSet(Box<KvWrite>),
    /// Create hints (boxed: a list).
    Hint(Box<HintRequest>),
    /// Change the status of a known hint; only its receiver may.
    HintStatus {
        /// The asking slot.
        slot: SlotNo,
        /// The slot whose world holds the hinted location.
        finding: SlotNo,
        /// The location.
        location: i64,
        /// The new status.
        status: u8,
    },
}

/// One log entry.
///
/// Deviations from doc 05 7.1, both out of scope for now: no ULID event id (the room-wide `seq`
/// is the id), and `Item` carries no `item_uid` (`H(room, location_id)`, doc 02 D3); the
/// `location` field identifies the item until the hash exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Room-wide, gapless, starts at 1.
    pub seq: Seq,
    /// The slot the event is about: the checker for `Checked`, the receiver for `Item`.
    pub slot: Option<SlotNo>,
    /// What happened.
    pub kind: EventKind,
}

/// Event payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The event's slot wrote a key. Carries the resulting value, so a fold never evaluates
    /// operations again.
    KvSet {
        /// The key.
        key: String,
        /// Its value after the write.
        value: Box<Value>,
    },
    /// A hint was created at the request of the event's slot. Item and receiver follow from
    /// the placement; whether it is found, from the checks.
    HintAdded {
        /// The slot whose world holds the location.
        finding: SlotNo,
        /// The location.
        location: i64,
        /// Its status.
        status: u8,
        /// The event's slot spent points on it.
        paid: bool,
    },
    /// The event's slot changed a known hint's status.
    HintStatus {
        /// The slot whose world holds the location.
        finding: SlotNo,
        /// The location.
        location: i64,
        /// The new status.
        status: u8,
    },
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
    fn a_key_write_and_its_event_survive_a_json_round_trip() {
        let c = Command::KvSet(Box::new(KvWrite {
            slot: SlotNo(3),
            key: "k".into(),
            default: Some(serde_json::json!({"a": [1.5, null]})),
            ops: alloc::vec![(KvOp::Add, serde_json::json!(u64::MAX)), (KvOp::Floor, Value::Null)],
        }));
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&s).unwrap(), c);
        let e = Event {
            seq: Seq(1),
            slot: Some(SlotNo(3)),
            kind: EventKind::KvSet { key: "k".into(), value: Box::new(serde_json::json!(-0.0)) },
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&s).unwrap(), e);
    }

    #[test]
    fn hint_commands_and_events_survive_a_json_round_trip() {
        let ask = HintAsk { finding: SlotNo(2), location: -7, status: 30 };
        let c = Command::Hint(Box::new(HintRequest {
            slot: SlotNo(1),
            asks: alloc::vec![ask],
            pay: true,
            keep_found: false,
        }));
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&s).unwrap(), c);
        let c =
            Command::HintStatus { slot: SlotNo(1), finding: SlotNo(2), location: 3, status: 20 };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&s).unwrap(), c);
        for kind in [
            EventKind::HintAdded { finding: SlotNo(2), location: 3, status: 0, paid: true },
            EventKind::HintStatus { finding: SlotNo(2), location: 3, status: 10 },
        ] {
            let e = Event { seq: Seq(1), slot: Some(SlotNo(1)), kind };
            let s = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&s).unwrap(), e);
        }
    }

    #[test]
    fn a_truncated_command_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Command>("{\"Check\":{\"slot\":1}").is_err());
    }
}
