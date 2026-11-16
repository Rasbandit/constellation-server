//! One simulated room: a live owner, an optional zombie, a lossy network and receiving clients.
mod invariants;
mod rng;

use config::Limits;
use invariants::{check_log, ensure};
use log::{AppendBatch, AppendError, CursorUpdate, LogStore, MemLogStore, Snapshot};
use pollster::block_on;
use proto::{Cid, Command, Epoch, Event, EventKind, ItemSeq, RoomId, Seq, SlotNo, Timestamp};
use rng::Rng;
use room_core::{Ctx, Placement, Reply, RoomState, apply, fold};
use std::collections::{BTreeMap, BTreeSet};

const ROOM: RoomId = RoomId(1);
const SLOTS: u32 = 3;
const LOCATIONS: u32 = 40;

/// A location in one slot's world: (checking slot, location id).
type Loc = (SlotNo, i64);

type Res<T = ()> = Result<T, String>;

/// What a run exercised, so a test can prove the faults were really injected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Events committed.
    pub events: u64,
    /// Crash-and-rehydrate cycles, including crashes between apply and append.
    pub crashes: u64,
    /// Racing second owners that took over.
    pub takeovers: u64,
    /// Writes by a stale owner that the store refused.
    pub fenced_writes: u64,
    /// Repeated cids answered from the dedupe index.
    pub deduped: u64,
    /// Replies that had to wait for an append (the hold rule on `room_core::Applied`).
    pub held_replies: u64,
    /// Events applied but thrown away by a crash before they were appended.
    pub lost_events: u64,
}

/// A reply the room has produced but not yet released to the caller.
#[derive(Debug, Clone, Copy)]
struct HeldReply {
    cmd: Command,
    reply: Reply,
    depends_on: Option<Seq>,
    /// The command emitted events (a first check), as opposed to answering from earlier ones.
    fresh: bool,
}

#[derive(Debug)]
struct Owner {
    epoch: Epoch,
    state: RoomState,
}

#[derive(Debug, Default)]
struct Client {
    last_applied: u64,
    /// Locations applied, in order.
    got: Vec<Loc>,
    /// Network: items on the way, delivered in random order, some twice.
    inbox: Vec<(ItemSeq, Loc)>,
}

/// One simulated room. Drive it with [`World::step`], finish with [`World::finish`]; [`crate::run_seed`] does both.
#[derive(Debug)]
pub struct World {
    rng: Rng,
    limits: Limits,
    store: MemLogStore,
    placement: Placement,
    now: u64,
    owner: Owner,
    zombie: Option<Owner>,
    inflight: Vec<(Command, bool)>,
    sent: Vec<(SlotNo, Cid, i64)>,
    next_cid: BTreeMap<SlotNo, u64>,
    /// Applied to the live state, not yet appended: the group-commit window.
    pending: Vec<Event>,
    held: Vec<HeldReply>,
    /// Fault injection for the sim's own test: ignore `depends_on`.
    release_early: bool,
    replies: BTreeMap<(SlotNo, Cid), (Seq, u64)>,
    /// Released `Checked` replies: seq, slot, cid, and the location when the command was fresh.
    acked: Vec<(Seq, SlotNo, Cid, Option<i64>)>,
    committed_head: u64,
    committed_items: BTreeMap<SlotNo, u64>,
    checked: BTreeSet<Loc>,
    durable_applied: BTreeMap<SlotNo, ItemSeq>,
    seen_applied: BTreeMap<SlotNo, ItemSeq>,
    clients: BTreeMap<SlotNo, Client>,
    /// What the run injected so far.
    pub stats: Stats,
}

fn slot_ids() -> impl Iterator<Item = SlotNo> {
    (1..=SLOTS).map(SlotNo)
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn dbg<E: core::fmt::Debug>(e: E) -> String {
    format!("{e:?}")
}

impl World {
    /// A fresh room for `seed`. The seed fixes the placement table and, through `step`, the schedule.
    ///
    /// # Errors
    /// A message when the room cannot be built.
    pub fn new(seed: u64, limits: Limits) -> Res<Self> {
        let mut rng = Rng::new(seed);
        // Every slot's world has locations 0..LOCATIONS (the same ids, different locations).
        let mut placement = Placement::new();
        for from in slot_ids() {
            let world = placement.entry(from).or_default();
            for l in 0..LOCATIONS {
                let to = u32::try_from(rng.below(3)).unwrap_or(0).saturating_add(1);
                let item = i64::from(l).saturating_add(i64::from(from.0).saturating_mul(1000));
                world.insert(i64::from(l), (SlotNo(to), item));
            }
        }
        let state = RoomState::new(slot_ids(), placement.clone()).map_err(dbg)?;
        let store = MemLogStore::new();
        let epoch = store.claim(ROOM);
        Ok(Self {
            rng,
            limits,
            store,
            placement,
            now: 0,
            owner: Owner { epoch, state },
            zombie: None,
            inflight: Vec::new(),
            sent: Vec::new(),
            next_cid: BTreeMap::new(),
            pending: Vec::new(),
            held: Vec::new(),
            release_early: false,
            replies: BTreeMap::new(),
            acked: Vec::new(),
            committed_head: 0,
            committed_items: BTreeMap::new(),
            checked: BTreeSet::new(),
            durable_applied: BTreeMap::new(),
            seen_applied: BTreeMap::new(),
            clients: slot_ids().map(|s| (s, Client::default())).collect(),
            stats: Stats::default(),
        })
    }

    /// Per-slot cid counters, so the same cid value is in use on several slots at once.
    fn fresh_cid(&mut self, slot: SlotNo) -> Cid {
        let n = self.next_cid.entry(slot).or_insert(1);
        let cid = Cid(*n);
        *n = n.saturating_add(1);
        cid
    }

    /// Planted bug for the sim's own test: replies are released without waiting.
    #[cfg(test)]
    pub(crate) const fn release_early(mut self) -> Self {
        self.release_early = true;
        self
    }

    fn random_slot(&mut self) -> SlotNo {
        SlotNo(u32::try_from(self.rng.below(3)).unwrap_or(0).saturating_add(1))
    }

    fn all_events(&self) -> Res<Vec<Event>> {
        block_on(self.store.read_tail(ROOM, Seq(0), usize::MAX)).map_err(dbg)
    }

    /// One random action of the schedule.
    /// One random action, then the cheap invariants.
    ///
    /// # Errors
    /// The broken invariant.
    pub fn step(&mut self) -> Res {
        // new check, resend, hostile, client ack, deliver command, client step, resync,
        // flush the batch (events and cursors), snapshot, advance time, crash, takeover, zombie write
        const W: [u64; 13] = [20, 8, 3, 8, 30, 25, 3, 4, 2, 6, 2, 2, 3];
        match self.rng.weighted(&W) {
            0 => {
                let slot = self.random_slot();
                let location = i64::from(self.rng.below_u32(LOCATIONS));
                let cid = self.fresh_cid(slot);
                self.sent.push((slot, cid, location));
                self.inflight.push((Command::Check { slot, cid, location }, false));
            }
            1 => {
                if let Some(&(slot, cid, location)) = self.sent.get(self.rng.below(self.sent.len()))
                {
                    // Same cid, sometimes a different location: the cid still wins in the window.
                    let location = if self.rng.chance(50) {
                        location
                    } else {
                        location.saturating_add(1).rem_euclid(i64::from(LOCATIONS))
                    };
                    self.inflight.push((Command::Check { slot, cid, location }, false));
                }
            }
            2 => self.hostile()?,
            3 => {
                let slot = self.random_slot();
                let applied = ItemSeq(self.clients.get(&slot).map_or(0, |c| c.last_applied));
                self.inflight.push((Command::Ack { slot, applied }, false));
            }
            4 => self.deliver_command()?,
            5 => {
                let slot = self.random_slot();
                self.client_step(slot)?;
            }
            6 => {
                let slot = self.random_slot();
                self.resync(slot)?;
            }
            7 => self.flush_batch()?,
            8 => self.snapshot()?,
            9 => {
                self.now = self.now.saturating_add(self.rng.below_u64(30));
                if self.rng.chance(15) {
                    // Jump past the dedupe window so expiry is exercised.
                    self.now = self.now.saturating_add(self.limits.dedupe_window_secs);
                }
            }
            10 => self.rehydrate(false)?,
            11 => self.rehydrate(true)?,
            _ => self.zombie_write()?,
        }
        self.check_cursors()
    }

    /// Invalid input is judged against the state it meets, so it is submitted at once, not queued.
    fn hostile(&mut self) -> Res {
        let slot = self.random_slot();
        let head = self.owner.state.cursor(slot).map_or(0, |c| c.item_head.0);
        let cmd = match self.rng.below(4) {
            0 => Command::Check { slot: SlotNo(0), cid: Cid(u64::MAX), location: 0 },
            1 => Command::Check {
                slot,
                cid: Cid(u64::MAX),
                location: i64::from(LOCATIONS.saturating_add(self.rng.below_u32(5))),
            },
            2 => Command::Ack { slot: SlotNo(SLOTS.saturating_add(1)), applied: ItemSeq(0) },
            _ => Command::Ack {
                slot,
                applied: ItemSeq(head.saturating_add(1).saturating_add(self.rng.below_u64(5))),
            },
        };
        self.submit(cmd, true)
    }

    fn deliver_command(&mut self) -> Res {
        if self.inflight.is_empty() {
            return Ok(());
        }
        let i = self.rng.below(self.inflight.len());
        // 30%: the network duplicates it, so it stays queued for a second delivery.
        let (cmd, hostile) = if self.rng.chance(30) {
            self.inflight.get(i).copied().ok_or("queue index")?
        } else {
            self.inflight.swap_remove(i)
        };
        self.submit(cmd, hostile)
    }

    /// Apply now, append later (group commit), and release the reply only when `depends_on` is
    /// durable (doc 05 8.1 rule 2 and the hold rule on `room_core::Applied`).
    fn submit(&mut self, cmd: Command, hostile: bool) -> Res {
        let window_hit = match cmd {
            Command::Check { slot, cid, .. } => self
                .replies
                .get(&(slot, cid))
                .filter(|(_, at)| self.now < at.saturating_add(self.limits.dedupe_window_secs))
                .map(|(seq, _)| *seq),
            Command::Ack { .. } | Command::Goal { .. } => None,
        };
        let before = self.owner.state.clone();
        let ctx = Ctx { now: Timestamp(self.now), limits: &self.limits };
        let out = apply(&mut self.owner.state, &ctx, cmd);
        let applied = match out {
            Err(e) => {
                ensure!(
                    self.owner.state == before,
                    "apply({cmd:?}) failed with {e:?} but changed the state"
                );
                ensure!(hostile, "honest command {cmd:?} was refused: {e:?}");
                return Ok(());
            }
            Ok(a) => a,
        };
        ensure!(!hostile, "hostile command {cmd:?} was accepted");
        if let Some(seq) = window_hit {
            ensure!(
                applied.reply == Reply::Checked { seq } && applied.events.is_empty(),
                "repeat of {cmd:?} inside the window did not return the original"
            );
            self.stats.deduped = self.stats.deduped.saturating_add(1);
        } else if let (Command::Check { slot, cid, .. }, Reply::Checked { seq }) =
            (cmd, applied.reply)
        {
            // No dedupe hit was due, so this must be a new command with a brand new seq.
            ensure!(
                applied.events.first().map(|e| e.seq) == Some(seq) && seq > before.head(),
                "{cmd:?} had no dedupe hit but replied with the old seq {seq:?}"
            );
            self.replies.insert((slot, cid), (seq, self.now));
        }
        if matches!(applied.reply, Reply::AlreadyChecked) {
            ensure!(applied.events.is_empty(), "AlreadyChecked emitted events");
        }
        let held = HeldReply {
            cmd,
            reply: applied.reply,
            depends_on: applied.depends_on,
            fresh: !applied.events.is_empty(),
        };
        self.pending.extend(applied.events);
        if held.depends_on.is_none() || self.release_early {
            self.release(&held)?;
        } else {
            self.stats.held_replies = self.stats.held_replies.saturating_add(1);
            self.held.push(held);
        }
        if !self.pending.is_empty() && self.rng.chance(35) {
            self.flush_batch()?;
        }
        Ok(())
    }

    /// Hand a reply to the caller. Nothing may be released for an event that is not durable.
    fn release(&mut self, h: &HeldReply) -> Res {
        if let Some(d) = h.depends_on {
            ensure!(
                d.0 <= self.committed_head,
                "reply {:?} to {:?} released before durable: it depends on seq {} and the log ends at {}",
                h.reply,
                h.cmd,
                d.0,
                self.committed_head
            );
        }
        match (h.cmd, h.reply) {
            (Command::Check { slot, cid, location }, Reply::Checked { seq }) => {
                self.acked.push((seq, slot, cid, h.fresh.then_some(location)));
            }
            (Command::Check { slot, location, .. }, Reply::AlreadyChecked) => ensure!(
                self.checked.contains(&(slot, location)),
                "AlreadyChecked released for location {location} the log never checked"
            ),
            _ => {}
        }
        Ok(())
    }

    /// Append everything pending (whole commands) plus dirty cursors, mark it durable, and
    /// release the replies that were waiting for it.
    fn flush_batch(&mut self) -> Res {
        let cursors = self.dirty_cursors();
        if self.pending.is_empty() && cursors.is_empty() {
            return Ok(());
        }
        let events = core::mem::take(&mut self.pending);
        let batch =
            AppendBatch { room: ROOM, epoch: self.owner.epoch, events: &events, cursors: &cursors };
        let receipt = block_on(self.store.append(batch))
            .map_err(|e| format!("live owner append failed: {e:?}"))?;
        self.committed(&cursors);
        ensure!(
            receipt.head.0 == self.committed_head.saturating_add(count(events.len())),
            "receipt head {} disagrees with the committed count",
            receipt.head.0
        );
        self.committed_head = receipt.head.0;
        self.stats.events = self.stats.events.saturating_add(count(events.len()));
        for e in &events {
            match e.kind {
                EventKind::Checked { location, .. } => {
                    let slot = e.slot.ok_or("check without slot")?;
                    ensure!(
                        self.checked.insert((slot, location)),
                        "location {location} committed twice"
                    );
                }
                EventKind::Item { from, location, item_seq, .. } => {
                    let slot = e.slot.ok_or("item without slot")?;
                    self.committed_items.insert(slot, item_seq.0);
                    self.clients.entry(slot).or_default().inbox.push((item_seq, (from, location)));
                }
                EventKind::Goal => {}
            }
        }
        self.owner.state.mark_durable(Seq(self.committed_head)).map_err(dbg)?;
        for h in core::mem::take(&mut self.held) {
            self.release(&h)?;
        }
        Ok(())
    }

    /// Cursors the live state holds above what the store has.
    fn dirty_cursors(&self) -> Vec<CursorUpdate> {
        slot_ids()
            .filter_map(|slot| {
                let applied = self.owner.state.cursor(slot)?.applied;
                (applied > self.durable_applied.get(&slot).copied().unwrap_or_default())
                    .then_some(CursorUpdate { slot, applied })
            })
            .collect()
    }

    fn committed(&mut self, cursors: &[CursorUpdate]) {
        for c in cursors {
            self.durable_applied.insert(c.slot, c.applied);
        }
    }

    fn snapshot(&mut self) -> Res {
        // A snapshot covers only durable state, so the group-commit window closes first.
        self.flush_batch()?;
        let data = serde_json::to_vec(&self.owner.state).map_err(dbg)?;
        let snap = Snapshot { seq: self.owner.state.head(), data };
        block_on(self.store.snapshot_put(ROOM, snap, self.owner.epoch))
            .map_err(|e| format!("snapshot failed: {e:?}"))?;
        // The snapshot carries the live applied cursors, so they are durable now.
        for slot in slot_ids() {
            if let Some(c) = self.owner.state.cursor(slot) {
                let d = self.durable_applied.entry(slot).or_default();
                *d = (*d).max(c.applied);
            }
        }
        Ok(())
    }

    /// Drop all live state and rebuild by fold from the log. With `race`, the old owner lives on
    /// as a zombie holding a stale epoch.
    fn rehydrate(&mut self, race: bool) -> Res {
        let epoch = self.store.claim(ROOM);
        let base = match block_on(self.store.snapshot_get(ROOM)).map_err(dbg)? {
            Some(s) => {
                let state = serde_json::from_slice::<RoomState>(&s.data).map_err(dbg)?;
                state.validate(s.seq).map_err(|e| format!("stored snapshot rejected: {e}"))?;
                state
            }
            None => RoomState::new(slot_ids(), self.placement.clone()).map_err(dbg)?,
        };
        let tail = block_on(self.store.read_tail(ROOM, base.head(), usize::MAX)).map_err(dbg)?;
        let mut state = fold(base, &self.limits, tail).map_err(dbg)?;
        for c in block_on(self.store.read_cursors(ROOM)).map_err(dbg)? {
            state.restore_applied(c.slot, c.applied).map_err(dbg)?;
        }
        ensure!(
            state.head().0 == self.committed_head,
            "rehydrated head {} != committed {}",
            state.head().0,
            self.committed_head
        );
        for slot in slot_ids() {
            let c = state.cursor(slot).ok_or("slot vanished")?;
            ensure!(
                c.item_head.0 == self.committed_items.get(&slot).copied().unwrap_or(0),
                "slot {} item_head {} lost or invented items across the crash",
                slot.0,
                c.item_head.0
            );
            ensure!(
                c.applied == self.durable_applied.get(&slot).copied().unwrap_or_default(),
                "slot {} applied {} is not the durable cursor",
                slot.0,
                c.applied.0
            );
            self.seen_applied.insert(slot, c.applied);
        }
        // The group-commit window dies with the process: unappended events and the replies held
        // for them are gone, and nothing was ever released for them.
        self.stats.lost_events = self.stats.lost_events.saturating_add(count(self.pending.len()));
        self.pending.clear();
        self.held.clear();
        self.replies.retain(|_, (seq, _)| seq.0 <= self.committed_head);
        let old = core::mem::replace(&mut self.owner, Owner { epoch, state });
        self.zombie = race.then_some(old);
        self.stats.crashes = self.stats.crashes.saturating_add(1);
        if race {
            self.stats.takeovers = self.stats.takeovers.saturating_add(1);
        }
        self.check_durable()
    }

    /// The old owner wakes up and tries to write. Nothing it sends may land.
    fn zombie_write(&mut self) -> Res {
        let Some(mut z) = self.zombie.take() else { return Ok(()) };
        let slot = self.random_slot();
        let location = i64::from(self.rng.below_u32(LOCATIONS));
        let cid = self.fresh_cid(slot);
        let cmd = Command::Check { slot, cid, location };
        let ctx = Ctx { now: Timestamp(self.now), limits: &self.limits };
        let events = match apply(&mut z.state, &ctx, cmd) {
            Ok(a) => a.events,
            Err(e) => return Err(format!("the zombie's honest {cmd:?} was refused: {e:?}")),
        };
        let before = (self.all_events()?, block_on(self.store.read_cursors(ROOM)).map_err(dbg)?);
        // A garbage cursor too: a fence that only guards events would let this through.
        let junk = [CursorUpdate { slot, applied: ItemSeq(u64::MAX) }];
        let batch = AppendBatch { room: ROOM, epoch: z.epoch, events: &events, cursors: &junk };
        let current = self.owner.epoch;
        ensure!(
            block_on(self.store.append(batch)) == Err(AppendError::Fenced { current }),
            "a fenced writer's append was not refused with Fenced"
        );
        let data = serde_json::to_vec(&z.state).map_err(dbg)?;
        let snap = Snapshot { seq: Seq(0), data };
        ensure!(
            block_on(self.store.snapshot_put(ROOM, snap, z.epoch))
                == Err(AppendError::Fenced { current }),
            "a fenced writer's snapshot was not refused"
        );
        let after = (self.all_events()?, block_on(self.store.read_cursors(ROOM)).map_err(dbg)?);
        ensure!(before == after, "a fenced writer changed the store");
        self.stats.fenced_writes = self.stats.fenced_writes.saturating_add(1);
        Ok(())
    }

    fn client_step(&mut self, slot: SlotNo) -> Res {
        let len = self.clients.get(&slot).map_or(0, |c| c.inbox.len());
        if len == 0 {
            return Ok(());
        }
        let i = self.rng.below(len);
        let dup = self.rng.chance(30);
        let c = self.clients.get_mut(&slot).ok_or("client")?;
        let (item_seq, location) =
            if dup { c.inbox.get(i).copied().ok_or("inbox")? } else { c.inbox.swap_remove(i) };
        self.receive(slot, item_seq, location)
    }

    /// The receiver rule (doc 02 D3): apply only `last_applied + 1`; drop duplicates; resync on a gap.
    fn receive(&mut self, slot: SlotNo, item_seq: ItemSeq, location: Loc) -> Res {
        let c = self.clients.get_mut(&slot).ok_or("client")?;
        if item_seq.0 == c.last_applied.saturating_add(1) {
            ensure!(
                !c.got.contains(&location),
                "item for location {location:?} delivered twice to slot {}",
                slot.0
            );
            c.got.push(location);
            c.last_applied = item_seq.0;
            Ok(())
        } else if item_seq.0 > c.last_applied {
            self.resync(slot)
        } else {
            Ok(())
        }
    }

    /// Reconnect: the server replays every item above the client's applied value, whatever room seq it holds.
    fn resync(&mut self, slot: SlotNo) -> Res {
        let events = self.all_events()?;
        let c = self.clients.get_mut(&slot).ok_or("client")?;
        for e in events.iter().filter(|e| e.slot == Some(slot)) {
            if let EventKind::Item { from, location, item_seq, .. } = e.kind {
                if item_seq.0 > c.last_applied {
                    c.inbox.push((item_seq, (from, location)));
                }
            }
        }
        Ok(())
    }

    /// Cheap invariants after every step: `applied` between its old value and the head.
    fn check_cursors(&mut self) -> Res {
        for slot in slot_ids() {
            let c = self.owner.state.cursor(slot).ok_or("slot vanished")?;
            ensure!(
                c.applied <= c.item_head,
                "slot {} applied {} is ahead of head {}",
                slot.0,
                c.applied.0,
                c.item_head.0
            );
            let seen = self.seen_applied.insert(slot, c.applied).unwrap_or_default();
            ensure!(
                c.applied >= seen,
                "slot {} applied went backwards: {} to {}",
                slot.0,
                seen.0,
                c.applied.0
            );
        }
        Ok(())
    }

    /// Full log invariants plus no acked loss.
    fn check_durable(&self) -> Res {
        let events = self.all_events()?;
        check_log(&events, &self.placement)?;
        for (seq, slot, cid, location) in &self.acked {
            let e = events.get(usize::try_from(seq.0.saturating_sub(1)).unwrap_or(usize::MAX));
            ensure!(
                e.is_some_and(|e| e.seq == *seq
                    && e.slot == Some(*slot)
                    && matches!(e.kind, EventKind::Checked { cid: c, location: l, .. }
                        if c == *cid && location.is_none_or(|want| want == l))),
                "a replied check (slot {}, cid {}, seq {}) is not in the log",
                slot.0,
                cid.0,
                seq.0
            );
        }
        for (slot, c) in &self.clients {
            let items: Vec<Loc> = events
                .iter()
                .filter(|e| e.slot == Some(*slot))
                .filter_map(|e| match e.kind {
                    EventKind::Item { from, location, .. } => Some((from, location)),
                    EventKind::Checked { .. } | EventKind::Goal => None,
                })
                .collect();
            ensure!(
                items.starts_with(&c.got),
                "slot {} applied items that are not the log's, in order",
                slot.0
            );
        }
        Ok(())
    }

    /// Stop injecting faults, deliver everything, and check that nothing was lost end to end.
    /// Deliver everything, crash once more, and check nothing was lost end to end.
    ///
    /// # Errors
    /// The broken invariant.
    pub fn finish(&mut self) -> Res {
        while !self.inflight.is_empty() {
            self.deliver_command()?;
            // A duplicate keeps a command queued; drain the queue for real after a few rounds.
            if self.rng.chance(20) {
                self.inflight.clear();
            }
        }
        self.flush_batch()?;
        for slot in slot_ids() {
            self.resync(slot)?;
            let c = self.clients.get_mut(&slot).ok_or("client")?;
            c.inbox.sort_unstable();
            while !self.clients.get(&slot).ok_or("client")?.inbox.is_empty() {
                let c = self.clients.get_mut(&slot).ok_or("client")?;
                let (item_seq, location) = c.inbox.remove(0);
                self.receive(slot, item_seq, location)?;
            }
            let applied = ItemSeq(self.clients.get(&slot).ok_or("client")?.last_applied);
            let ctx = Ctx { now: Timestamp(self.now), limits: &self.limits };
            let out =
                apply(&mut self.owner.state, &ctx, Command::Ack { slot, applied }).map_err(dbg)?;
            ensure!(
                out.reply == Reply::Acked { applied },
                "final ack of slot {} not accepted",
                slot.0
            );
        }
        self.flush_batch()?;
        ensure!(self.held.is_empty(), "replies still held after the final flush");
        self.rehydrate(false)?;
        for slot in slot_ids() {
            let head = self.committed_items.get(&slot).copied().unwrap_or(0);
            let c = self.clients.get(&slot).ok_or("client")?;
            ensure!(
                c.last_applied == head,
                "slot {}: client applied {} of {head} items: lost delivery",
                slot.0,
                c.last_applied
            );
            let cur = self.owner.state.cursor(slot).ok_or("slot")?;
            ensure!(
                cur.applied.0 == head,
                "slot {}: acked cursor {} lost across the final crash (head {head})",
                slot.0,
                cur.applied.0
            );
        }
        Ok(())
    }
}
