//! The pure room state machine (doc 05 7.1, doc 02 D3). No clock, no OS rng, no IO.
//!
//! A room is created from a fixed placement table (checking slot and location to receiving slot
//! and item; a location id is only unique within its slot's world). `apply`
//! turns a command into events and one reply; `fold` rebuilds state from a snapshot and a log
//! tail. Both go through `RoomState::record`, so live state and rehydrated state cannot drift.
//!
//! Deviations from doc 05 7.1: `fold` also takes `&Limits` (dedupe pruning needs the window) and
//! returns `Result` (a corrupt log is an error, not a panic); `Ctx::limits` is a borrow, not
//! `&'static`; `Applied` has one reply, `depends_on`, and no `effects`; `Item` events carry no
//! `item_uid` (see `proto::Event`).
//!
//! Applied cursors (`Ack`) are not log events (D-02): `fold` does not restore them, the caller
//! reads them from the store and calls [`RoomState::restore_applied`].
//!
//! The room also holds a key-value store written by `KvSet` (data storage, docs/protocol-plan.md
//! section 1). `apply` evaluates the operations (the `kv` module); the event carries the resulting value,
//! so `fold` never evaluates them again.
//!
//! Hints and hint points live in the `hint` module (docs/protocol-plan.md section 2).
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod hint;
mod kv;

use alloc::{boxed::Box, collections::BTreeMap, string::String, vec, vec::Vec};
use config::Limits;
use core::fmt;
pub use hint::{Hint, valid as valid_hint_status};
pub use hint::{HintRefusal, HintRules, HintShown, Hinted, status as hint_status};
pub use kv::KvRefusal;
use proto::{Cid, Command, Event, EventKind, ItemSeq, KvWrite, Seq, SlotNo, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Keys with this prefix are read only (protocol doc, "Set"; reference `MultiServer.py:2286`).
pub const RESERVED_KEY_PREFIX: &str = "_read_";

/// Checking slot, then location id in that slot's world, to (receiving slot, item id). Fixed for
/// the life of the room. Nested rather than keyed by a pair so a snapshot encodes as JSON.
pub type Placement = BTreeMap<SlotNo, BTreeMap<i64, (SlotNo, i64)>>;

/// Values the runtime supplies; the core never reads a clock (doc 05 7.1).
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    /// Time of this command.
    pub now: Timestamp,
    /// The one source of limits (the `config` crate).
    pub limits: &'a Limits,
    /// Float power for the `pow` operation: `core` has no float math library, so the host
    /// supplies it like the clock (the result is recorded in the event, so replay never needs
    /// it).
    pub powf: fn(f64, f64) -> f64,
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
    /// Highest `seq` known to be in the log. Live states lag `head` until `mark_durable`.
    durable: Seq,
    /// Checked locations per checking slot and the `seq` of their `Checked` event.
    checked: BTreeMap<SlotNo, BTreeMap<i64, Seq>>,
    /// A `Checked` event whose `Item` has not been folded yet (only between the two events).
    awaiting: Option<(SlotNo, i64)>,
    /// Slots that reached their goal and the `seq` of their `Goal` event.
    goals: BTreeMap<SlotNo, Seq>,
    slots: BTreeMap<SlotNo, SlotCursor>,
    /// `(slot, cid)` index, expiring after `dedupe_window_secs` (D-13).
    dedupe: BTreeMap<SlotNo, BTreeMap<Cid, Dedupe>>,
    /// The key-value store.
    #[serde(default)]
    kv: BTreeMap<String, Value>,
    /// Bytes the store holds: every key plus its value as compact JSON.
    #[serde(default)]
    kv_bytes: u64,
    /// `KvSet` events folded so far (one per write, so the head can be checked).
    #[serde(default)]
    kv_writes: u64,
    /// Bytes of every key write ever logged (key plus value as JSON), for
    /// `Limits::kv_max_room_log_bytes`: the log keeps each write in full.
    #[serde(default)]
    kv_log_bytes: u64,
    /// The hint economy, from the seed.
    #[serde(default)]
    hint_rules: HintRules,
    /// Hints by the slot whose world holds the location, then the location.
    #[serde(default)]
    hints: BTreeMap<SlotNo, BTreeMap<i64, hint::HintRec>>,
    /// Paid hints per asking slot.
    #[serde(default)]
    hints_used: BTreeMap<SlotNo, u64>,
    /// Hint events folded so far (one per event, so the head can be checked).
    #[serde(default)]
    hint_writes: u64,
}

/// The reply to the caller (a `result` frame), sent only after the events commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The check was applied (or is a repeat of the cid that was); `seq` is its `Checked` event.
    Checked {
        /// Seq of the `Checked` event.
        seq: Seq,
    },
    /// The location was already checked; nothing was emitted.
    AlreadyChecked,
    /// The goal is recorded (now or by an earlier command); `seq` is its `Goal` event.
    Goal {
        /// Seq of the `Goal` event.
        seq: Seq,
    },
    /// The ack was accepted or was a harmless repeat.
    Acked {
        /// The slot's applied cursor after the ack.
        applied: ItemSeq,
    },
    /// The key was written (boxed: much larger than the other replies).
    KvSet(Box<KvDone>),
    /// A hint request was answered (boxed: a list).
    Hinted(Box<Hinted>),
    /// A status change was applied, or changed nothing (an unknown or found hint, or the same
    /// status).
    HintStatus {
        /// A `HintStatus` event was written.
        changed: bool,
    },
}

/// A key write that was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvDone {
    /// Seq of the `KvSet` event.
    pub seq: Seq,
    /// The value before the write: the stored one, else the default, else 0.
    pub original: Value,
    /// The value after.
    pub value: Value,
    /// Bytes the write logs (the key plus the value as compact JSON), measured once here
    /// (`entry_bytes`); the edge charges them to the slot's byte budget.
    pub bytes: u64,
}

/// Output of one command.
///
/// **Hold rule.** A caller must not release `reply` until `depends_on` (when `Some`) is durable in
/// the log, and must then call [`RoomState::mark_durable`]. `depends_on` is set whenever the reply
/// rests on an event that is not yet known durable: the command's own new events, or the original
/// event behind a repeated cid or an already-checked location. Releasing earlier would answer
/// "done" for something a crash can still erase (doc 05 8.1 rule 2). Batches must hold whole
/// commands, so durable always covers a command's `Checked` and `Item` events together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// To append to the log, in order. Empty for repeats, acks and already-checked locations.
    pub events: Vec<Event>,
    /// For the caller, after the append commits.
    pub reply: Reply,
    /// Hold `reply` until this `seq` is durable; `None` means release at once.
    pub depends_on: Option<Seq>,
}

/// Why a command or a fold was refused. State is unchanged on every error from `apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreError {
    /// The slot is not in this room.
    UnknownSlot(SlotNo),
    /// The location is not in the checking slot's placement table.
    UnknownLocation(i64),
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
    /// A folded event disagrees with the placement table or breaks the check-then-item pairing.
    PlacementMismatch,
    /// A folded `Goal` for a slot that already has one.
    RepeatedGoal(SlotNo),
    /// A snapshot failed [`RoomState::validate`]; the text names the rule.
    BadSnapshot(&'static str),
    /// `mark_durable` above the head.
    DurableAhead {
        /// The current head.
        head: Seq,
    },
    /// A key write was refused; nothing was stored.
    Kv(KvRefusal),
    /// A hint command was refused; nothing changed.
    Hint(HintRefusal),
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
        if let Some(slot) = placement
            .iter()
            .flat_map(|(from, m)| core::iter::once(from).chain(m.values().map(|(to, _)| to)))
            .find(|s| !slots.contains_key(s))
        {
            return Err(CoreError::UnknownSlot(*slot));
        }
        Ok(Self {
            placement,
            head: Seq(0),
            durable: Seq(0),
            checked: BTreeMap::new(),
            awaiting: None,
            goals: BTreeMap::new(),
            slots,
            dedupe: BTreeMap::new(),
            kv: BTreeMap::new(),
            kv_bytes: 0,
            kv_writes: 0,
            kv_log_bytes: 0,
            hint_rules: HintRules::default(),
            hints: BTreeMap::new(),
            hints_used: BTreeMap::new(),
            hint_writes: 0,
        })
    }

    /// Check a state loaded from storage before trusting it: a snapshot is input, and a corrupt
    /// or hostile one must be an error, never a panic or a silently wrong room. `snapshot_seq` is
    /// the log position the store recorded for it.
    ///
    /// # Errors
    /// `BadSnapshot` naming the first rule that fails.
    pub fn validate(&self, snapshot_seq: Seq) -> Result<(), CoreError> {
        let bad = |why| Err(CoreError::BadSnapshot(why));
        if self.head != snapshot_seq {
            return bad("head differs from the snapshot seq");
        }
        if self.durable != self.head {
            return bad("snapshot taken before its events were durable");
        }
        if self.awaiting.is_some() {
            return bad("snapshot stops between a check and its item");
        }
        if self.placement.iter().any(|(from, m)| {
            !self.slots.contains_key(from) || m.values().any(|(s, _)| !self.slots.contains_key(s))
        }) {
            return bad("placement names a missing slot");
        }
        if self.slots.values().any(|c| c.applied > c.item_head) {
            return bad("applied is above item_head");
        }
        if self.dedupe.keys().any(|s| !self.slots.contains_key(s)) {
            return bad("dedupe entry for a missing slot");
        }
        // A dedupe entry and a check record the `Checked` event, which its `Item` event always
        // follows, so in a valid snapshot both sit below the head.
        if self.dedupe.values().flat_map(BTreeMap::values).any(|d| d.seq >= self.head) {
            return bad("dedupe entry at or beyond the head");
        }
        if self.checked_pairs().any(|(from, l, s)| self.place(from, l).is_none() || s >= self.head)
        {
            return bad("checked location outside the placement or at or beyond the head");
        }
        if self.goals.iter().any(|(slot, s)| !self.slots.contains_key(slot) || *s > self.head) {
            return bad("goal for a missing slot or beyond the head");
        }
        let checks = u64::try_from(self.checked_pairs().count()).unwrap_or(u64::MAX);
        let goals = u64::try_from(self.goals.len()).unwrap_or(u64::MAX);
        // One event per kind's unit: two per check, one per goal, key write and hint event.
        let events = checks
            .saturating_mul(2)
            .saturating_add(goals)
            .saturating_add(self.kv_writes)
            .saturating_add(self.hint_writes);
        if self.head.0 != events {
            return bad("head is not two events per check, one per goal, key write and hint event");
        }
        self.validate_hints().or_else(bad)?;
        if usize::try_from(self.kv_writes).is_ok_and(|w| w < self.kv.len()) {
            return bad("more keys than key writes");
        }
        if self.kv.keys().any(|k| k.starts_with(RESERVED_KEY_PREFIX)) {
            return bad("a reserved key is stored");
        }
        if self.kv.iter().map(|(k, v)| entry_bytes(k, v)).fold(0, u64::saturating_add)
            != self.kv_bytes
        {
            return bad("kv_bytes does not match the stored keys");
        }
        for (slot, c) in &self.slots {
            let items = self
                .checked_pairs()
                .filter(|&(from, l, _)| self.place(from, l).is_some_and(|(s, _)| s == *slot))
                .count();
            if c.item_head.0 != u64::try_from(items).unwrap_or(u64::MAX) {
                return bad("item_head does not match the checks placed at the slot");
            }
        }
        Ok(())
    }

    /// Every checked `(checking slot, location, seq)`.
    fn checked_pairs(&self) -> impl Iterator<Item = (SlotNo, i64, Seq)> + '_ {
        self.checked.iter().flat_map(|(&from, m)| m.iter().map(move |(&l, &s)| (from, l, s)))
    }

    /// Where `from`'s location `location` sends its item.
    fn place(&self, from: SlotNo, location: i64) -> Option<(SlotNo, i64)> {
        self.placement.get(&from).and_then(|m| m.get(&location)).copied()
    }

    fn checked_at(&self, from: SlotNo, location: i64) -> Option<Seq> {
        self.checked.get(&from).and_then(|m| m.get(&location)).copied()
    }

    /// The stored value of `key`, if any.
    #[must_use]
    pub fn kv_get(&self, key: &str) -> Option<&Value> {
        self.kv.get(key)
    }

    /// Whether `slot` has reached its goal.
    #[must_use]
    pub fn has_goal(&self, slot: SlotNo) -> bool {
        self.goals.contains_key(&slot)
    }

    /// Highest `seq` known durable. Equals `head` for a state rebuilt by `fold`.
    #[must_use]
    pub const fn durable(&self) -> Seq {
        self.durable
    }

    /// Number of `(slot, cid)` dedupe entries held, for tests and metrics.
    #[must_use]
    pub fn dedupe_len(&self) -> usize {
        self.dedupe.values().map(BTreeMap::len).sum()
    }

    /// Record that the log holds everything up to `seq` (the caller's append committed). Lower
    /// values are a no-op. See the hold rule on [`Applied`].
    ///
    /// # Errors
    /// `DurableAhead` when `seq` is above the head.
    pub fn mark_durable(&mut self, seq: Seq) -> Result<(), CoreError> {
        if seq > self.head {
            return Err(CoreError::DurableAhead { head: self.head });
        }
        self.durable = self.durable.max(seq);
        Ok(())
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

    /// `Some(seq)` when `seq` (the last event of an original command) is not yet durable.
    fn wait_for(&self, seq: u64) -> Option<Seq> {
        (seq > self.durable.0).then_some(Seq(seq))
    }

    /// The only place an event changes state; shared by `apply` and `fold`.
    fn record(&mut self, ev: &Event, limits: &Limits) -> Result<(), CoreError> {
        let expected = next_seq(self.head)?;
        if ev.seq != expected {
            return Err(CoreError::LogGap { expected, got: ev.seq });
        }
        let slot = ev.slot.ok_or(CoreError::BadEvent)?;
        if !self.slots.contains_key(&slot) {
            return Err(CoreError::UnknownSlot(slot));
        }
        // Every check is validated against the placement table before anything changes, so a
        // hostile or corrupt log is an error and never a half-applied state.
        match &ev.kind {
            EventKind::KvSet { key, value } => {
                if self.awaiting.is_some() {
                    return Err(CoreError::PlacementMismatch);
                }
                let writes = self.kv_writes.checked_add(1).ok_or(CoreError::SeqOverflow)?;
                let old = self.kv.get(key).map_or(0, |v| entry_bytes(key, v));
                let new = entry_bytes(key, value);
                self.kv_bytes = self.kv_bytes.saturating_sub(old).saturating_add(new);
                self.kv_log_bytes = self.kv_log_bytes.saturating_add(new);
                self.kv.insert(key.clone(), (**value).clone());
                self.kv_writes = writes;
            }
            EventKind::HintAdded { .. } | EventKind::HintStatus { .. } => {
                self.record_hint(slot, ev)?;
            }
            EventKind::Goal => {
                if self.awaiting.is_some() {
                    return Err(CoreError::PlacementMismatch);
                }
                if self.goals.contains_key(&slot) {
                    return Err(CoreError::RepeatedGoal(slot));
                }
                self.goals.insert(slot, ev.seq);
            }
            &EventKind::Item { from, location, item, item_seq } => {
                if self.awaiting != Some((from, location))
                    || self.place(from, location) != Some((slot, item))
                {
                    return Err(CoreError::PlacementMismatch);
                }
                let cursor = self.slots.get_mut(&slot).ok_or(CoreError::UnknownSlot(slot))?;
                let expected = next_item_seq(cursor.item_head)?;
                if item_seq != expected {
                    return Err(CoreError::ItemGap { expected, got: item_seq });
                }
                cursor.item_head = item_seq;
                self.awaiting = None;
            }
            &EventKind::Checked { cid, location, at } => {
                if self.place(slot, location).is_none() {
                    return Err(CoreError::UnknownLocation(location));
                }
                if self.awaiting.is_some() || self.checked_at(slot, location).is_some() {
                    return Err(CoreError::PlacementMismatch);
                }
                self.checked.entry(slot).or_default().insert(location, ev.seq);
                self.awaiting = Some((slot, location));
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

/// Bytes one store entry counts for: the key plus the value as compact JSON.
fn entry_bytes(key: &str, value: &Value) -> u64 {
    u64::try_from(key.len().saturating_add(kv::size(value))).unwrap_or(u64::MAX)
}

/// Evaluate a key write against the limits, without changing the state: the value before and
/// after.
/// The value before, the value after, and the bytes the write logs.
type KvWritten = (Value, Value, u64);

fn kv_write(state: &RoomState, ctx: &Ctx<'_>, w: &KvWrite) -> Result<KvWritten, KvRefusal> {
    let l = ctx.limits;
    if w.key.starts_with(RESERVED_KEY_PREFIX) {
        return Err(KvRefusal::ReservedKey);
    }
    if w.key.len() > l.kv_max_key_bytes {
        return Err(KvRefusal::KeyTooLong);
    }
    if w.ops.len() > l.kv_max_ops {
        return Err(KvRefusal::TooManyOps);
    }
    if w.default.iter().chain(w.ops.iter().map(|(_, v)| v)).any(|v| kv::depth(v) > l.kv_max_depth) {
        return Err(KvRefusal::TooDeep);
    }
    // The reference starts from 0 when the packet has no default (`MultiServer.py:2292`).
    let original =
        state.kv.get(&w.key).or(w.default.as_ref()).cloned().unwrap_or_else(|| Value::from(0));
    let value = kv::evaluate(original.clone(), &w.ops, l.kv_max_value_bytes, ctx.powf)?;
    if kv::size(&value) > l.kv_max_value_bytes {
        return Err(KvRefusal::ValueTooLarge);
    }
    let old = state.kv.get(&w.key).map_or(0, |v| entry_bytes(&w.key, v));
    let new = entry_bytes(&w.key, &value);
    if state.kv_bytes.saturating_sub(old).saturating_add(new) > l.kv_max_room_bytes {
        return Err(KvRefusal::RoomFull);
    }
    if state.kv_log_bytes.saturating_add(new) > l.kv_max_room_log_bytes {
        return Err(KvRefusal::LogFull);
    }
    Ok((original, value, new))
}

/// Apply one command. On `Err` the state is unchanged.
///
/// # Errors
/// See [`CoreError`]: unknown slot or location, ack ahead of head, counter overflow.
pub fn apply(state: &mut RoomState, ctx: &Ctx<'_>, cmd: Command) -> Result<Applied, CoreError> {
    match cmd {
        Command::Hint(req) => hint::request(state, ctx.limits, *req),
        Command::HintStatus { slot, finding, location, status } => {
            hint::set_status(state, ctx.limits, slot, finding, location, status)
        }
        Command::KvSet(w) => {
            if !state.slots.contains_key(&w.slot) {
                return Err(CoreError::UnknownSlot(w.slot));
            }
            let (original, value, bytes) = kv_write(state, ctx, &w).map_err(CoreError::Kv)?;
            let seq = next_seq(state.head)?;
            let KvWrite { slot, key, .. } = *w;
            let events = vec![Event {
                seq,
                slot: Some(slot),
                kind: EventKind::KvSet { key, value: Box::new(value.clone()) },
            }];
            for e in &events {
                state.record(e, ctx.limits)?;
            }
            let reply = Reply::KvSet(Box::new(KvDone { seq, original, value, bytes }));
            Ok(Applied { events, reply, depends_on: Some(seq) })
        }
        Command::Ack { slot, applied } => {
            let applied = state.restore_applied(slot, applied)?;
            Ok(Applied { events: Vec::new(), reply: Reply::Acked { applied }, depends_on: None })
        }
        Command::Goal { slot } => {
            if !state.slots.contains_key(&slot) {
                return Err(CoreError::UnknownSlot(slot));
            }
            if let Some(&seq) = state.goals.get(&slot) {
                let depends_on = state.wait_for(seq.0);
                return Ok(Applied { events: Vec::new(), reply: Reply::Goal { seq }, depends_on });
            }
            let seq = next_seq(state.head)?;
            let events = vec![Event { seq, slot: Some(slot), kind: EventKind::Goal }];
            for e in &events {
                state.record(e, ctx.limits)?;
            }
            Ok(Applied { events, reply: Reply::Goal { seq }, depends_on: Some(seq) })
        }
        Command::Check { slot, cid, location } => {
            if !state.slots.contains_key(&slot) {
                return Err(CoreError::UnknownSlot(slot));
            }
            let window = ctx.limits.dedupe_window_secs;
            if let Some(d) = state.dedupe.get(&slot).and_then(|m| m.get(&cid))
                && live(d.at, ctx.now, window)
            {
                let depends_on = state.wait_for(d.seq.0.saturating_add(1));
                return Ok(Applied {
                    events: Vec::new(),
                    reply: Reply::Checked { seq: d.seq },
                    depends_on,
                });
            }
            let (receiver, item) =
                state.place(slot, location).ok_or(CoreError::UnknownLocation(location))?;
            if let Some(at_seq) = state.checked_at(slot, location) {
                let depends_on = state.wait_for(at_seq.0.saturating_add(1));
                return Ok(Applied {
                    events: Vec::new(),
                    reply: Reply::AlreadyChecked,
                    depends_on,
                });
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
                    kind: EventKind::Item { from: slot, location, item, item_seq },
                },
            ];
            for e in &events {
                state.record(e, ctx.limits)?;
            }
            Ok(Applied {
                events,
                reply: Reply::Checked { seq: checked_seq },
                depends_on: Some(item_event_seq),
            })
        }
    }
}

/// Rebuild state from a snapshot and the log events after it.
///
/// Unlike the doc 05 7.1 sketch it takes the limits and returns `Result`. Validate a stored snapshot with
/// [`RoomState::validate`] first. Every event is checked against the placement table.
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
    // Log events are durable by definition.
    snapshot.durable = snapshot.head;
    Ok(snapshot)
}

#[cfg(test)]
mod tests;
