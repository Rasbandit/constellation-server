//! The pure room state machine (doc 05 7.1, doc 02 D3). No clock, no OS rng, no IO.
//!
//! A room is created from a fixed placement table (location to receiving slot and item). `apply`
//! turns a command into events and one reply; `fold` rebuilds state from a snapshot and a log
//! tail. Both go through `RoomState::record`, so live state and rehydrated state cannot drift.
//!
//! Applied cursors (`Ack`) are not log events (D-02): `fold` does not restore them, the caller
//! reads them from the store and calls [`RoomState::restore_applied`].
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::{collections::BTreeMap, collections::BTreeSet, vec, vec::Vec};
use config::Limits;
use core::fmt;
use proto::{Cid, Command, Event, EventKind, ItemSeq, Seq, SlotNo, Timestamp};
use serde::{Deserialize, Serialize};

/// Location id to (receiving slot, item id). Fixed for the life of the room.
pub type Placement = BTreeMap<u32, (SlotNo, u32)>;

/// Values the runtime supplies; the core never reads a clock (doc 05 7.1).
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    /// Time of this command.
    pub now: Timestamp,
    /// The one source of limits (the `config` crate).
    pub limits: &'a Limits,
}

/// Per-slot delivery cursors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotCursor {
    /// Last `item_seq` assigned to this slot (0 = none).
    pub item_head: ItemSeq,
    /// Highest `item_seq` the client confirmed. Never above `item_head`, never decreasing.
    pub applied: ItemSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Dedupe {
    seq: Seq,
    at: Timestamp,
}

/// Everything the room remembers. Serializable as a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomState {
    placement: Placement,
    head: Seq,
    checked: BTreeSet<u32>,
    slots: BTreeMap<SlotNo, SlotCursor>,
    /// `(slot, cid)` index, expiring after `dedupe_window_secs` (D-13).
    dedupe: BTreeMap<SlotNo, BTreeMap<Cid, Dedupe>>,
}

/// The reply to the caller (a `result` frame), sent only after the events commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// The check was applied (or is a repeat of the cid that was); `seq` is its `Checked` event.
    Checked {
        /// Seq of the `Checked` event.
        seq: Seq,
    },
    /// The location was already checked; nothing was emitted.
    AlreadyChecked,
    /// The ack was accepted or was a harmless repeat.
    Acked {
        /// The slot's applied cursor after the ack.
        applied: ItemSeq,
    },
}

/// Output of one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// To append to the log, in order. Empty for repeats, acks and already-checked locations.
    pub events: Vec<Event>,
    /// For the caller, after the append commits.
    pub reply: Reply,
}

/// Why a command or a fold was refused. State is unchanged on every error from `apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreError {
    /// The slot is not in this room.
    UnknownSlot(SlotNo),
    /// The location is not in the placement table.
    UnknownLocation(u32),
    /// An ack above the slot's `item_head`.
    AckAhead {
        /// The slot's current `item_head`.
        head: ItemSeq,
    },
    /// `seq` or `item_seq` would pass `u64::MAX`.
    SeqOverflow,
    /// A folded event is not the next `seq`.
    LogGap {
        /// The seq the state needed next.
        expected: Seq,
        /// The seq the event carried.
        got: Seq,
    },
    /// A folded item is not the next `item_seq` for its slot.
    ItemGap {
        /// The `item_seq` the slot needed next.
        expected: ItemSeq,
        /// The `item_seq` the event carried.
        got: ItemSeq,
    },
    /// A folded event has no slot.
    BadEvent,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl core::error::Error for CoreError {}

fn next_seq(s: Seq) -> Result<Seq, CoreError> {
    s.0.checked_add(1).map(Seq).ok_or(CoreError::SeqOverflow)
}

fn next_item_seq(s: ItemSeq) -> Result<ItemSeq, CoreError> {
    s.0.checked_add(1).map(ItemSeq).ok_or(CoreError::SeqOverflow)
}

/// A dedupe entry written at `at` is live at `now` while `now < at + window`. Clock skew
/// (`now < at`) counts as live.
const fn live(at: Timestamp, now: Timestamp, window: u64) -> bool {
    now.0 < at.0.saturating_add(window)
}

impl RoomState {
    /// A fresh room. Every placement target must be one of `slots`.
    ///
    /// # Errors
    /// `UnknownSlot` when the placement names a slot that is not in `slots`.
    pub fn new(
        slots: impl IntoIterator<Item = SlotNo>,
        placement: Placement,
    ) -> Result<Self, CoreError> {
        let slots: BTreeMap<_, _> = slots
            .into_iter()
            .map(|s| (s, SlotCursor { item_head: ItemSeq(0), applied: ItemSeq(0) }))
            .collect();
        if let Some((slot, _)) = placement.values().find(|(s, _)| !slots.contains_key(s)) {
            return Err(CoreError::UnknownSlot(*slot));
        }
        Ok(Self {
            placement,
            head: Seq(0),
            checked: BTreeSet::new(),
            slots,
            dedupe: BTreeMap::new(),
        })
    }

    /// The last assigned room `seq` (0 for a new room).
    #[must_use]
    pub const fn head(&self) -> Seq {
        self.head
    }

    /// A slot's cursors, or `None` for an unknown slot.
    #[must_use]
    pub fn cursor(&self, slot: SlotNo) -> Option<SlotCursor> {
        self.slots.get(&slot).copied()
    }

    /// Every slot in the room, in order.
    pub fn slot_ids(&self) -> impl Iterator<Item = SlotNo> + '_ {
        self.slots.keys().copied()
    }

    /// Raise a slot's applied cursor; a lower value is a harmless no-op. Returns the cursor after.
    /// Used by `Ack` and by rehydration from stored cursors.
    ///
    /// # Errors
    /// `UnknownSlot`, or `AckAhead` when `applied` is above the slot's `item_head`.
    pub fn restore_applied(
        &mut self,
        slot: SlotNo,
        applied: ItemSeq,
    ) -> Result<ItemSeq, CoreError> {
        let c = self.slots.get_mut(&slot).ok_or(CoreError::UnknownSlot(slot))?;
        if applied > c.item_head {
            return Err(CoreError::AckAhead { head: c.item_head });
        }
        c.applied = c.applied.max(applied);
        Ok(c.applied)
    }

    /// The only place an event changes state; shared by `apply` and `fold`.
    fn record(&mut self, ev: &Event, limits: &Limits) -> Result<(), CoreError> {
        let expected = next_seq(self.head)?;
        if ev.seq != expected {
            return Err(CoreError::LogGap { expected, got: ev.seq });
        }
        let slot = ev.slot.ok_or(CoreError::BadEvent)?;
        let cursor = self.slots.get_mut(&slot).ok_or(CoreError::UnknownSlot(slot))?;
        match ev.kind {
            EventKind::Item { item_seq, .. } => {
                let expected = next_item_seq(cursor.item_head)?;
                if item_seq != expected {
                    return Err(CoreError::ItemGap { expected, got: item_seq });
                }
                cursor.item_head = item_seq;
            }
            EventKind::Checked { cid, location, at } => {
                self.checked.insert(location);
                let index = self.dedupe.entry(slot).or_default();
                // shortcut: prunes this slot's whole index on each new check, O(entries);
                // upgrade to a time-ordered index when a room's 24 h window gets large.
                index.retain(|_, d| live(d.at, at, limits.dedupe_window_secs));
                index.insert(cid, Dedupe { seq: ev.seq, at });
            }
        }
        self.head = ev.seq;
        Ok(())
    }
}

/// Apply one command. On `Err` the state is unchanged.
///
/// # Errors
/// See [`CoreError`]: unknown slot or location, ack ahead of head, counter overflow.
pub fn apply(state: &mut RoomState, ctx: &Ctx<'_>, cmd: Command) -> Result<Applied, CoreError> {
    match cmd {
        Command::Ack { slot, applied } => {
            let applied = state.restore_applied(slot, applied)?;
            Ok(Applied { events: Vec::new(), reply: Reply::Acked { applied } })
        }
        Command::Check { slot, cid, location } => {
            if !state.slots.contains_key(&slot) {
                return Err(CoreError::UnknownSlot(slot));
            }
            let window = ctx.limits.dedupe_window_secs;
            if let Some(d) = state.dedupe.get(&slot).and_then(|m| m.get(&cid)) {
                if live(d.at, ctx.now, window) {
                    return Ok(Applied {
                        events: Vec::new(),
                        reply: Reply::Checked { seq: d.seq },
                    });
                }
            }
            let &(receiver, item) =
                state.placement.get(&location).ok_or(CoreError::UnknownLocation(location))?;
            if state.checked.contains(&location) {
                return Ok(Applied { events: Vec::new(), reply: Reply::AlreadyChecked });
            }
            // Validate every counter first so `record` cannot fail half way.
            let checked_seq = next_seq(state.head)?;
            let item_event_seq = next_seq(checked_seq)?;
            let item_seq = next_item_seq(
                state.slots.get(&receiver).ok_or(CoreError::UnknownSlot(receiver))?.item_head,
            )?;
            let events = vec![
                Event {
                    seq: checked_seq,
                    slot: Some(slot),
                    kind: EventKind::Checked { cid, location, at: ctx.now },
                },
                Event {
                    seq: item_event_seq,
                    slot: Some(receiver),
                    kind: EventKind::Item { location, item, item_seq },
                },
            ];
            for e in &events {
                state.record(e, ctx.limits)?;
            }
            Ok(Applied { events, reply: Reply::Checked { seq: checked_seq } })
        }
    }
}

/// Rebuild state from a snapshot and the log events after it.
///
/// # Errors
/// `LogGap`, `ItemGap`, `BadEvent` or `SeqOverflow` when the tail is not a valid continuation.
pub fn fold(
    mut snapshot: RoomState,
    limits: &Limits,
    tail: impl IntoIterator<Item = Event>,
) -> Result<RoomState, CoreError> {
    for e in tail {
        snapshot.record(&e, limits)?;
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests;
