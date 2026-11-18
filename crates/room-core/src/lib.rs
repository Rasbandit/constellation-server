//! The pure room state machine (doc 05 7.1, doc 02 D3). No clock, no OS rng, no IO.
//!
//! A room is created from a fixed placement table, a [`World`] (checking slot and location to
//! receiving slot and item; a location id is only unique within its slot's world), shared and
//! never copied. Inside, a location is its dense index in its slot's world and checks are
//! bitsets; ids appear only in commands and events (the log keeps ids). `apply`
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
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod kv;

use alloc::{boxed::Box, collections::BTreeMap, string::String, sync::Arc, vec, vec::Vec};
use config::Limits;
use core::fmt;
pub use kv::KvRefusal;
use proto::{Cid, Command, Event, EventKind, ItemSeq, KvWrite, Seq, SlotNo, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Keys with this prefix are read only (protocol doc, "Set"; reference `MultiServer.py:2286`).
pub const RESERVED_KEY_PREFIX: &str = "_read_";

/// Checking slot, then location id in that slot's world, to (receiving slot, item id). Fixed for
/// the life of the room. The map form of a [`World`], for tests and small rooms.
pub type Placement = BTreeMap<SlotNo, BTreeMap<i64, (SlotNo, i64)>>;

/// A room's fixed placement table, read where it is kept (the seed file) and shared by every
/// reader. Slots are named by position in [`World::slots`], locations by index in their slot's
/// world.
pub trait World: fmt::Debug + Send + Sync {
    /// Every slot, strictly ascending.
    fn slots(&self) -> &[SlotNo];
    /// How many locations slot `pos` has.
    fn placed(&self, pos: usize) -> usize;
    /// The index of location id `location` in slot `pos`'s world.
    fn find(&self, pos: usize, location: i64) -> Option<usize>;
    /// Where location `index` of slot `pos` sends its item: (receiving slot position, item id).
    fn target(&self, pos: usize, index: usize) -> Option<(usize, i64)>;
}

/// A [`Placement`] as a [`World`]: per slot, (location, receiver position, item) by location.
#[derive(Debug, Default)]
struct MapWorld {
    slots: Vec<SlotNo>,
    worlds: Vec<Vec<(i64, usize, i64)>>,
}

impl World for MapWorld {
    fn slots(&self) -> &[SlotNo] {
        &self.slots
    }

    fn placed(&self, pos: usize) -> usize {
        self.worlds.get(pos).map_or(0, Vec::len)
    }

    fn find(&self, pos: usize, location: i64) -> Option<usize> {
        self.worlds.get(pos)?.binary_search_by_key(&location, |p| p.0).ok()
    }

    fn target(&self, pos: usize, index: usize) -> Option<(usize, i64)> {
        self.worlds.get(pos)?.get(index).map(|&(_, to, item)| (to, item))
    }
}

/// The room's world. It is fixed input, not state: two states are equal when everything they
/// recorded is, whatever world object they hold (a snapshot is read back onto its room's).
#[derive(Clone)]
struct Shared(Arc<dyn World>);

impl PartialEq for Shared {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Shared {}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "World({} slots)", self.0.slots().len())
    }
}

/// What a snapshot holds before [`RoomState::from_snapshot`] gives it its world.
fn no_world() -> Shared {
    Shared(Arc::new(MapWorld::default()))
}

use bits::{contains as bit, ones};

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

/// One slot's dedupe entries, sorted by cid: 24 bytes an entry, about a third of a map's. A
/// snapshot keeps them as a cid-keyed map, as before.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "BTreeMap<Cid, Dedupe>", into = "BTreeMap<Cid, Dedupe>")]
struct Dedupes(Vec<(Cid, Dedupe)>);

impl From<BTreeMap<Cid, Dedupe>> for Dedupes {
    fn from(m: BTreeMap<Cid, Dedupe>) -> Self {
        Self(m.into_iter().collect())
    }
}

impl From<Dedupes> for BTreeMap<Cid, Dedupe> {
    fn from(d: Dedupes) -> Self {
        d.0.into_iter().collect()
    }
}

impl Dedupes {
    fn get(&self, cid: &Cid) -> Option<&Dedupe> {
        let i = self.0.binary_search_by_key(cid, |e| e.0).ok()?;
        self.0.get(i).map(|e| &e.1)
    }

    fn insert(&mut self, cid: Cid, d: Dedupe) {
        match self.0.binary_search_by_key(&cid, |e| e.0) {
            Ok(i) => {
                if let Some(e) = self.0.get_mut(i) {
                    e.1 = d;
                }
            }
            Err(i) => self.0.insert(i, (cid, d)),
        }
    }

    fn values(&self) -> impl Iterator<Item = &Dedupe> {
        self.0.iter().map(|e| &e.1)
    }
}

/// Everything the room remembers. Serializable as a snapshot, which leaves the world out (read
/// one back with [`RoomState::from_snapshot`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomState {
    #[serde(skip, default = "no_world")]
    world: Shared,
    head: Seq,
    /// Highest `seq` known to be in the log. Live states lag `head` until `mark_durable`.
    durable: Seq,
    /// Checked locations: per slot position, bit `i` is location index `i` of its world.
    checked: Vec<Vec<u64>>,
    /// Checks whose events may not be durable yet: (slot position, location index, `seq` of the
    /// `Checked` event). A repeat waits for them (the hold rule); pruned as `durable` rises.
    #[serde(default)]
    pending: Vec<(u32, u32, Seq)>,
    /// A `Checked` event whose `Item` has not been folded yet (only between the two events).
    awaiting: Option<(SlotNo, i64)>,
    /// Slots that reached their goal and the `seq` of their `Goal` event.
    goals: BTreeMap<SlotNo, Seq>,
    slots: BTreeMap<SlotNo, SlotCursor>,
    /// `(slot, cid)` index, expiring after `dedupe_window_secs` (D-13).
    dedupe: BTreeMap<SlotNo, Dedupes>,
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
    /// A fresh room from a placement map. Every placement source and target must be one of
    /// `slots`.
    ///
    /// # Errors
    /// `UnknownSlot` when the placement names a slot that is not in `slots`.
    pub fn new(
        slots: impl IntoIterator<Item = SlotNo>,
        placement: Placement,
    ) -> Result<Self, CoreError> {
        let mut ids: Vec<SlotNo> = slots.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        let pos = |s: &SlotNo| ids.binary_search(s).map_err(|_| CoreError::UnknownSlot(*s));
        let mut worlds = vec![Vec::new(); ids.len()];
        for (from, m) in &placement {
            let world = worlds.get_mut(pos(from)?).ok_or(CoreError::UnknownSlot(*from))?;
            for (&location, (to, item)) in m {
                world.push((location, pos(to)?, *item));
            }
        }
        Self::from_world(Arc::new(MapWorld { slots: ids, worlds }))
    }

    /// A fresh room on `world`.
    ///
    /// # Errors
    /// `UnknownSlot` when a placement sends to a position outside the world's slots.
    pub fn from_world(world: Arc<dyn World>) -> Result<Self, CoreError> {
        let ids = world.slots();
        let n = ids.len();
        for pos in 0..n {
            let bad =
                (0..world.placed(pos)).find(|&i| world.target(pos, i).is_none_or(|t| t.0 >= n));
            if bad.is_some() {
                return Err(CoreError::UnknownSlot(ids.get(pos).copied().unwrap_or(SlotNo(0))));
            }
        }
        let slots = ids
            .iter()
            .map(|&s| (s, SlotCursor { item_head: ItemSeq(0), applied: ItemSeq(0) }))
            .collect();
        let checked = (0..n).map(|pos| vec![0; world.placed(pos).div_ceil(64)]).collect();
        Ok(Self {
            world: Shared(world),
            head: Seq(0),
            durable: Seq(0),
            checked,
            pending: Vec::new(),
            awaiting: None,
            goals: BTreeMap::new(),
            slots,
            dedupe: BTreeMap::new(),
            kv: BTreeMap::new(),
            kv_bytes: 0,
            kv_writes: 0,
            kv_log_bytes: 0,
        })
    }

    /// A snapshot (the JSON of a [`RoomState`]) on its room's `world`. Check it with
    /// [`RoomState::validate`] before use.
    ///
    /// # Errors
    /// `BadSnapshot` when it is not a snapshot.
    pub fn from_snapshot(world: Arc<dyn World>, json: &[u8]) -> Result<Self, CoreError> {
        let mut s: Self =
            serde_json::from_slice(json).map_err(|_| CoreError::BadSnapshot("not a snapshot"))?;
        s.world = Shared(world);
        Ok(s)
    }

    /// The room's world, shared.
    #[must_use]
    pub fn world(&self) -> Arc<dyn World> {
        Arc::clone(&self.world.0)
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
        let w = &self.world.0;
        if !self.slots.keys().eq(w.slots().iter()) {
            return bad("snapshot slots are not the world's");
        }
        let well_formed = |pos: usize, set: &Vec<u64>| {
            set.len() == w.placed(pos).div_ceil(64) && ones(set).all(|i| i < w.placed(pos))
        };
        if self.checked.len() != w.slots().len()
            || !self.checked.iter().enumerate().all(|(pos, set)| well_formed(pos, set))
        {
            return bad("checked location outside the placement");
        }
        if !self.pending.is_empty() {
            return bad("a check in the snapshot is not durable");
        }
        if self.slots.values().any(|c| c.applied > c.item_head) {
            return bad("applied is above item_head");
        }
        if self.dedupe.keys().any(|s| !self.slots.contains_key(s)) {
            return bad("dedupe entry for a missing slot");
        }
        // A dedupe entry and a check record the `Checked` event, which its `Item` event always
        // follows, so in a valid snapshot both sit below the head.
        if self.dedupe.values().flat_map(Dedupes::values).any(|d| d.seq >= self.head) {
            return bad("dedupe entry at or beyond the head");
        }
        if self.goals.iter().any(|(slot, s)| !self.slots.contains_key(slot) || *s > self.head) {
            return bad("goal for a missing slot or beyond the head");
        }
        let checks = u64::try_from(self.checked_pairs().count()).unwrap_or(u64::MAX);
        let goals = u64::try_from(self.goals.len()).unwrap_or(u64::MAX);
        // One event per kind's unit: two per check, one per goal, one per key write.
        let events = checks.saturating_mul(2).saturating_add(goals).saturating_add(self.kv_writes);
        if self.head.0 != events {
            return bad("head is not two events per check, one per goal and one per key write");
        }
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
        let mut items = vec![0_u64; w.slots().len()];
        for (pos, i) in self.checked_pairs() {
            if let Some(n) = w.target(pos, i).and_then(|(to, _)| items.get_mut(to)) {
                *n = n.saturating_add(1);
            }
        }
        if self.slots.values().zip(items).any(|(c, n)| c.item_head.0 != n) {
            return bad("item_head does not match the checks placed at the slot");
        }
        Ok(())
    }

    /// Every checked `(slot position, location index)`.
    fn checked_pairs(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.checked.iter().enumerate().flat_map(|(pos, set)| ones(set).map(move |i| (pos, i)))
    }

    /// Where `from`'s location `location` is and sends its item: (slot position, location
    /// index, receiving slot, item id).
    fn place(&self, from: SlotNo, location: i64) -> Option<(usize, usize, SlotNo, i64)> {
        let w = &self.world.0;
        let pos = w.slots().binary_search(&from).ok()?;
        let i = w.find(pos, location)?;
        let (to, item) = w.target(pos, i)?;
        Some((pos, i, *w.slots().get(to)?, item))
    }

    fn is_checked(&self, pos: usize, i: usize) -> bool {
        self.checked.get(pos).is_some_and(|set| bit(set, i))
    }

    /// What a repeat of the check at (`pos`, `i`) must wait for: its `Item` event, while it
    /// may not be durable.
    fn check_wait(&self, pos: usize, i: usize) -> Option<Seq> {
        let found = self.pending.iter().find(|p| (p.0 as usize, p.1 as usize) == (pos, i))?;
        self.wait_for(found.2.0.saturating_add(1))
    }

    /// Drop pending checks whose `Item` event is durable.
    fn prune_pending(&mut self) {
        let durable = self.durable.0;
        self.pending.retain(|p| p.2.0.saturating_add(1) > durable);
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
        self.dedupe.values().map(|d| d.0.len()).sum()
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
        self.prune_pending();
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
                    || self.place(from, location).map(|p| (p.2, p.3)) != Some((slot, item))
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
                let (pos, i, ..) =
                    self.place(slot, location).ok_or(CoreError::UnknownLocation(location))?;
                if self.awaiting.is_some() || self.is_checked(pos, i) {
                    return Err(CoreError::PlacementMismatch);
                }
                let word = self.checked.get_mut(pos).and_then(|set| set.get_mut(i / 64));
                *word.ok_or(CoreError::PlacementMismatch)? |= 1 << (i % 64);
                let (p, k) = (u32::try_from(pos), u32::try_from(i));
                self.pending.push((p.unwrap_or(u32::MAX), k.unwrap_or(u32::MAX), ev.seq));
                self.awaiting = Some((slot, location));
                let index = self.dedupe.entry(slot).or_default();
                // shortcut: prunes this slot's whole index on each new check, O(entries);
                // upgrade to a time-ordered index when a room's 24 h window gets large.
                index.0.retain(|(_, d)| live(d.at, at, limits.dedupe_window_secs));
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
            let (pos, i, receiver, item) =
                state.place(slot, location).ok_or(CoreError::UnknownLocation(location))?;
            if state.is_checked(pos, i) {
                let depends_on = state.check_wait(pos, i);
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
    snapshot.prune_pending();
    Ok(snapshot)
}

#[cfg(test)]
mod tests;
