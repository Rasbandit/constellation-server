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
    got: Vec<u32>,
    /// Network: items on the way, delivered in random order, some twice.
    inbox: Vec<(ItemSeq, u32)>,
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
    sent: Vec<(SlotNo, Cid, u32)>,
    next_cid: u64,
    replies: BTreeMap<(SlotNo, Cid), (Seq, u64)>,
    acked: Vec<(Seq, SlotNo, Cid, u32)>,
    committed_head: u64,
    committed_items: BTreeMap<SlotNo, u64>,
    checked: BTreeSet<u32>,
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
        let placement: Placement = (0..LOCATIONS)
            .map(|l| {
                let slot = u32::try_from(rng.below(3)).unwrap_or(0).saturating_add(1);
                (l, (SlotNo(slot), l.saturating_add(1000)))
            })
            .collect();
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
            next_cid: 1,
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
        // flush cursors, snapshot, advance time, crash, takeover, zombie write
        const W: [u64; 13] = [20, 8, 3, 8, 30, 25, 3, 4, 2, 6, 2, 2, 3];
        match self.rng.weighted(&W) {
            0 => {
                let slot = self.random_slot();
                let location = self.rng.below_u32(LOCATIONS);
                let cid = Cid(self.next_cid);
                self.next_cid = self.next_cid.saturating_add(1);
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
                        location.saturating_add(1) % LOCATIONS
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
            7 => self.flush_cursors()?,
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
                location: LOCATIONS.saturating_add(self.rng.below_u32(5)),
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

    /// Apply, append, and only then reply (doc 05 8.1 rule 2).
    fn submit(&mut self, cmd: Command, hostile: bool) -> Res {
        let window_hit = match cmd {
            Command::Check { slot, cid, .. } => self
                .replies
                .get(&(slot, cid))
                .filter(|(_, at)| self.now < at.saturating_add(self.limits.dedupe_window_secs))
                .map(|(seq, _)| *seq),
            Command::Ack { .. } => None,
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
        }
        if let (Command::Check { location, .. }, Reply::AlreadyChecked) = (cmd, applied.reply) {
            ensure!(
                self.checked.contains(&location),
                "AlreadyChecked for location {location} the log never checked"
            );
            ensure!(applied.events.is_empty(), "AlreadyChecked emitted events");
        }
        if applied.events.is_empty() {
            return Ok(());
        }
        if self.rng.chance(4) {
            // Crash after apply, before the append: nothing was committed or replied.
            return self.rehydrate(false);
        }
        let cursors = self.dirty_cursors();
        let batch = AppendBatch {
            room: ROOM,
            epoch: self.owner.epoch,
            events: &applied.events,
            cursors: &cursors,
        };
        let receipt = block_on(self.store.append(batch))
            .map_err(|e| format!("live owner append failed: {e:?}"))?;
        self.committed(&cursors);
        ensure!(
            receipt.head.0 == self.committed_head.saturating_add(count(applied.events.len())),
            "receipt head {} disagrees with the committed count",
            receipt.head.0
        );
        self.committed_head = receipt.head.0;
        self.stats.events = self.stats.events.saturating_add(count(applied.events.len()));
        for e in &applied.events {
            match e.kind {
                EventKind::Checked { location, .. } => {
                    ensure!(self.checked.insert(location), "location {location} committed twice");
                }
                EventKind::Item { location, item_seq, .. } => {
                    let slot = e.slot.ok_or("item without slot")?;
                    self.committed_items.insert(slot, item_seq.0);
                    self.clients.entry(slot).or_default().inbox.push((item_seq, location));
                }
            }
        }
        if let (Command::Check { slot, cid, location }, Reply::Checked { seq }) =
            (cmd, applied.reply)
        {
            self.replies.insert((slot, cid), (seq, self.now));
            self.acked.push((seq, slot, cid, location));
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

    fn flush_cursors(&mut self) -> Res {
        let cursors = self.dirty_cursors();
        let batch =
            AppendBatch { room: ROOM, epoch: self.owner.epoch, events: &[], cursors: &cursors };
        block_on(self.store.append(batch)).map_err(|e| format!("cursor flush failed: {e:?}"))?;
        self.committed(&cursors);
        Ok(())
    }

    fn snapshot(&mut self) -> Res {
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
            Some(s) => serde_json::from_slice::<RoomState>(&s.data).map_err(dbg)?,
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
        let location = self.rng.below_u32(LOCATIONS);
        let cmd = Command::Check { slot, cid: Cid(self.next_cid), location };
        self.next_cid = self.next_cid.saturating_add(1);
        let ctx = Ctx { now: Timestamp(self.now), limits: &self.limits };
        let events = apply(&mut z.state, &ctx, cmd).map(|a| a.events).unwrap_or_default();
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
    fn receive(&mut self, slot: SlotNo, item_seq: ItemSeq, location: u32) -> Res {
        let c = self.clients.get_mut(&slot).ok_or("client")?;
        if item_seq.0 == c.last_applied.saturating_add(1) {
            ensure!(
                !c.got.contains(&location),
                "item for location {location} delivered twice to slot {}",
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
            if let EventKind::Item { location, item_seq, .. } = e.kind {
                if item_seq.0 > c.last_applied {
                    c.inbox.push((item_seq, location));
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
                    && matches!(e.kind, EventKind::Checked { cid: c, location: l, .. } if c == *cid && l == *location)),
                "a replied check (slot {}, cid {}, seq {}) is not in the log",
                slot.0,
                cid.0,
                seq.0
            );
        }
        for (slot, c) in &self.clients {
            let items: Vec<u32> = events
                .iter()
                .filter(|e| e.slot == Some(*slot))
                .filter_map(|e| match e.kind {
                    EventKind::Item { location, .. } => Some(location),
                    EventKind::Checked { .. } => None,
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
        self.flush_cursors()?;
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
