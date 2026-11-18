//! Hints (docs/protocol-plan.md section 2): which locations a slot knows the item of, their
//! status, and the points that buy them.
//!
//! A hint is keyed by the slot whose world holds the location and the location, as the
//! room's dense indexes (slot position, location index), so the same hint asked for twice is
//! one hint; ids appear only in commands, events and the [`Hint`] view. The item and receiver come from the placement, and
//! whether it is found from the checks, so neither is stored. Points follow the reference
//! server's rule (`MultiServer.py:816-819, 1929-1936`): each checked location of a slot's own
//! world earns the room's points per check, and each paid hint costs a share of that world's
//! size.
//!
//! The hints a seed starts with ([`RoomState::with_hints`]) are part of the fresh state, built
//! from the bundle on every start, so they are never logged.

#![allow(
    clippy::redundant_pub_crate,
    reason = "rustc's unreachable_pub (denied) needs crate visibility in this private module"
)]

use crate::{Applied, CoreError, Reply, RoomState, next_seq};
use alloc::{boxed::Box, collections::BTreeSet, vec::Vec};
use proto::{Event, EventKind, HintAsk, HintRequest, Seq, SlotNo};
use serde::{Deserialize, Serialize};

/// Status values (protocol doc, "`HintStatus`").
pub mod status {
    /// No status chosen.
    pub const UNSPECIFIED: u8 = 0;
    /// The receiver does not need it soon.
    pub const NO_PRIORITY: u8 = 10;
    /// The receiver would rather not get it (traps).
    pub const AVOID: u8 = 20;
    /// The receiver wants it.
    pub const PRIORITY: u8 = 30;
    /// The location is checked. Never set by a request; it follows from the checks.
    pub const FOUND: u8 = 40;
}

/// Whether `s` is a status a hint may be stored with.
#[must_use]
pub const fn valid(s: u8) -> bool {
    matches!(
        s,
        status::UNSPECIFIED
            | status::NO_PRIORITY
            | status::AVOID
            | status::PRIORITY
            | status::FOUND
    )
}

/// Whether a status update may set `s`: a stored status, but never found (only a check finds).
const fn settable(s: u8) -> bool {
    valid(s) && s != status::FOUND
}

/// The room's hint economy, from the seed's server options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintRules {
    /// Percent of a slot's world size one hint costs, 0 to 100 (0: hints are free).
    pub cost_pct: u8,
    /// Points earned per checked location of the slot's own world.
    pub check_points: u32,
}

impl HintRules {
    /// Points one hint costs a slot whose world has `locations` locations: 0 when hints are
    /// free, else at least 1. Computed in floating point like the reference (`hint_cost *
    /// 0.01 * size`, truncated, `MultiServer.py:816-819`), so 29 percent of 100 is 28 there and
    /// here.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::as_conversions,
        reason = "the reference truncates a float; a non-negative product fits once truncated"
    )]
    pub fn cost(self, locations: usize) -> u64 {
        if self.cost_pct == 0 {
            return 0;
        }
        let share = f64::from(self.cost_pct) * 0.01 * locations as f64;
        (share as u64).max(1)
    }

    /// A slot's points: earned by its checks, less what its paid hints cost. Negative only if
    /// the cost could rise after hints were paid, which a fixed seed never does.
    #[must_use]
    pub fn points(self, locations: usize, checked: usize, used: u64) -> i64 {
        let earned =
            i64::from(self.check_points).saturating_mul(i64::try_from(checked).unwrap_or(i64::MAX));
        let spent = i64::try_from(self.cost(locations).saturating_mul(used)).unwrap_or(i64::MAX);
        earned.saturating_sub(spent)
    }
}

/// A stored hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HintRec {
    /// The status asked for (the shown status is found once the location is checked).
    pub(crate) status: u8,
    /// The last event that wrote it; 0 for a hint the seed started with.
    pub(crate) seq: Seq,
}

/// A hint as clients see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hint {
    /// The slot whose world holds the location.
    pub finding: SlotNo,
    /// The location.
    pub location: i64,
    /// The slot the item goes to.
    pub receiver: SlotNo,
    /// The item, in the receiver's world.
    pub item: i64,
    /// Found when the location is checked, else the stored status.
    pub status: u8,
    /// The location is checked.
    pub found: bool,
}

/// One hint in a [`Hinted`] reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HintShown {
    /// The hint.
    pub hint: Hint,
    /// It was not known before this request (it may still not be stored: a found one asked
    /// without `keep_found`).
    pub new: bool,
}

/// The reply to a hint request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hinted {
    /// What to show, in request order: every known hint asked for, every new found one, and
    /// every new unfound one that was created.
    pub shown: Vec<HintShown>,
    /// New unfound hints left out because they would cost points the slot lacks (or because
    /// one paid hint per request is the rule).
    pub unpaid: usize,
    /// The slot's points after the request.
    pub points: i64,
    /// What one hint costs the slot.
    pub cost: u64,
}

/// Why a hint command was refused. Nothing changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintRefusal {
    /// Not a status value, or found (which only a check sets).
    BadStatus(u8),
    /// Only the hint's receiver may change its status.
    NoPermission,
}

/// Every stored hint by (finding slot position, location index), sorted: 24 bytes a hint. A
/// snapshot keeps it as that list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HintTable(Vec<(u32, u32, HintRec)>);

impl HintTable {
    fn find(&self, pos: usize, i: usize) -> Result<usize, usize> {
        let key = (u32::try_from(pos).unwrap_or(u32::MAX), u32::try_from(i).unwrap_or(u32::MAX));
        self.0.binary_search_by_key(&key, |e| (e.0, e.1))
    }

    fn get(&self, pos: usize, i: usize) -> Option<HintRec> {
        self.find(pos, i).ok().and_then(|k| self.0.get(k)).map(|e| e.2)
    }

    /// Store `rec` unless a hint is there already; whether it was stored.
    fn add(&mut self, pos: usize, i: usize, rec: HintRec) -> bool {
        let Err(k) = self.find(pos, i) else { return false };
        let key = (u32::try_from(pos).unwrap_or(u32::MAX), u32::try_from(i).unwrap_or(u32::MAX));
        self.0.insert(k, (key.0, key.1, rec));
        true
    }

    fn get_mut(&mut self, pos: usize, i: usize) -> Option<&mut HintRec> {
        let k = self.find(pos, i).ok()?;
        self.0.get_mut(k).map(|e| &mut e.2)
    }

    fn iter(&self) -> impl Iterator<Item = (usize, usize, HintRec)> + '_ {
        self.0.iter().map(|&(p, i, r)| (p as usize, i as usize, r))
    }
}

impl RoomState {
    /// The room's hint economy and the hints the seed starts with, each `(finding slot,
    /// location, status)`; a hint named twice is kept once, the first time.
    ///
    /// # Errors
    /// `UnknownLocation` for a hint outside the placement, `Hint(BadStatus)` for a status
    /// that is not a value.
    pub fn with_hints(
        mut self,
        rules: HintRules,
        start: impl IntoIterator<Item = (SlotNo, i64, u8)>,
    ) -> Result<Self, CoreError> {
        self.hint_rules = rules;
        for (finding, location, s) in start {
            let (pos, i, ..) =
                self.place(finding, location).ok_or(CoreError::UnknownLocation(location))?;
            if !valid(s) {
                return Err(CoreError::Hint(HintRefusal::BadStatus(s)));
            }
            self.hints.add(pos, i, HintRec { status: s, seq: Seq(0) });
        }
        Ok(self)
    }

    /// The room's hint economy.
    #[must_use]
    pub const fn hint_rules(&self) -> HintRules {
        self.hint_rules
    }

    /// Locations in `slot`'s world.
    fn world_size(&self, slot: SlotNo) -> usize {
        let w = &self.world.0;
        w.slots().binary_search(&slot).map_or(0, |pos| w.placed(pos))
    }

    /// What one hint costs `slot`.
    #[must_use]
    pub fn hint_cost(&self, slot: SlotNo) -> u64 {
        self.hint_rules.cost(self.world_size(slot))
    }

    /// `slot`'s hint points now.
    #[must_use]
    pub fn hint_points(&self, slot: SlotNo) -> i64 {
        let used = self.hints_used.get(&slot).copied().unwrap_or(0);
        self.hint_rules.points(self.world_size(slot), self.checked_count(slot), used)
    }

    /// The hint at location `i` of slot position `pos` in ids, as clients see it.
    fn hint_view(&self, pos: usize, i: usize, stored: u8) -> Option<Hint> {
        let w = &self.world.0;
        let (to, item) = w.target(pos, i)?;
        let found = self.is_checked(pos, i);
        let status = if found { status::FOUND } else { stored };
        let (finding, receiver) = (*w.slots().get(pos)?, *w.slots().get(to)?);
        Some(Hint { finding, location: w.location(pos, i)?, receiver, item, status, found })
    }

    /// Whether `finding`'s `location` has a hint (found or not).
    #[must_use]
    pub fn is_hinted(&self, finding: SlotNo, location: i64) -> bool {
        self.place(finding, location).is_some_and(|(pos, i, ..)| self.hints.get(pos, i).is_some())
    }

    /// Every hint `slot` finds or receives, in (finding slot, location) order: the value of
    /// its `_read_hints_` key.
    #[must_use]
    pub fn hints_of(&self, slot: SlotNo) -> Vec<Hint> {
        self.hints
            .iter()
            .filter_map(|(pos, i, r)| self.hint_view(pos, i, r.status))
            .filter(|h| h.finding == slot || h.receiver == slot)
            .collect()
    }

    /// The one place a hint event changes state (called from `record`).
    pub(crate) fn record_hint(&mut self, slot: SlotNo, ev: &Event) -> Result<(), CoreError> {
        if self.awaiting.is_some() {
            return Err(CoreError::PlacementMismatch);
        }
        let writes = self.hint_writes.checked_add(1).ok_or(CoreError::SeqOverflow)?;
        match ev.kind {
            EventKind::HintAdded { finding, location, status: s, paid } => {
                let (pos, i, ..) =
                    self.place(finding, location).ok_or(CoreError::UnknownLocation(location))?;
                if !valid(s) {
                    return Err(CoreError::Hint(HintRefusal::BadStatus(s)));
                }
                if self.hints.get(pos, i).is_some() {
                    return Err(CoreError::BadEvent);
                }
                if paid {
                    let used = self.hints_used.entry(slot).or_insert(0);
                    *used = used.checked_add(1).ok_or(CoreError::SeqOverflow)?;
                }
                self.hints.add(pos, i, HintRec { status: s, seq: ev.seq });
            }
            EventKind::HintStatus { finding, location, status: s } => {
                if !settable(s) {
                    return Err(CoreError::Hint(HintRefusal::BadStatus(s)));
                }
                let (pos, i, ..) = self.place(finding, location).ok_or(CoreError::BadEvent)?;
                let rec = self.hints.get_mut(pos, i).ok_or(CoreError::BadEvent)?;
                *rec = HintRec { status: s, seq: ev.seq };
            }
            _ => return Err(CoreError::BadEvent),
        }
        self.hint_writes = writes;
        Ok(())
    }

    /// Snapshot rules for the hint table (part of [`RoomState::validate`]).
    pub(crate) fn validate_hints(&self) -> Result<(), &'static str> {
        let w = &self.world.0;
        let sorted = self.hints.0.windows(2).all(|p| (p[0].0, p[0].1) < (p[1].0, p[1].1));
        let bad_rec = |(pos, i, r): (usize, usize, HintRec)| {
            i >= w.placed(pos) || !valid(r.status) || r.seq > self.head
        };
        if !sorted || self.hints.iter().any(bad_rec) {
            return Err("hint outside the placement, with a bad status or beyond the head");
        }
        if self.hints_used.keys().any(|s| !self.slots.contains_key(s)) {
            return Err("hints used by a missing slot");
        }
        if self.hints_used.values().fold(0, |a: u64, &u| a.saturating_add(u)) > self.hint_writes {
            return Err("more paid hints than hint events");
        }
        Ok(())
    }
}

/// One asked hint: the ask, where it is (slot position, location index), the `(status, paid)`
/// of the event that creates it (none for a known one or a found one that is not kept), and
/// the seq it rests on.
type Planned = (HintAsk, (usize, usize), Option<(u8, bool)>, Seq);

/// Apply a hint request: see [`Hinted`] for the reply. Known hints are shown and never
/// changed; a new found hint is kept only with `keep_found`; new unfound hints are all created
/// when `pay` is off or hints are free, else the first one when the slot has the points.
pub(crate) fn request(
    state: &mut RoomState,
    limits: &config::Limits,
    req: HintRequest,
) -> Result<Applied, CoreError> {
    let HintRequest { slot, asks, pay, keep_found } = req;
    if !state.slots.contains_key(&slot) {
        return Err(CoreError::UnknownSlot(slot));
    }
    let mut seen = BTreeSet::new();
    let mut asked: Vec<(HintAsk, (usize, usize))> = Vec::new();
    for a in asks {
        let (pos, i, ..) =
            state.place(a.finding, a.location).ok_or(CoreError::UnknownLocation(a.location))?;
        if !valid(a.status) || a.status == status::FOUND {
            return Err(CoreError::Hint(HintRefusal::BadStatus(a.status)));
        }
        if seen.insert((pos, i)) {
            asked.push((a, (pos, i)));
        }
    }
    let cost = state.hint_cost(slot);
    let points = state.hint_points(slot);
    let unfound = asked
        .iter()
        .filter(|(_, (p, i))| state.hints.get(*p, *i).is_none() && !state.is_checked(*p, *i))
        .count();
    let affordable = if !pay || cost == 0 {
        unfound
    } else {
        usize::from(points >= i64::try_from(cost).unwrap_or(i64::MAX))
    };
    // Plan first, so a counter overflow fails before anything changes.
    let mut plan: Vec<Planned> = Vec::with_capacity(asked.len());
    let mut head = state.head;
    let mut wait = Seq(0);
    let mut paid = 0;
    for (a, (p, i)) in asked {
        if let Some(rec) = state.hints.get(p, i) {
            wait = wait.max(rec.seq);
            plan.push((a, (p, i), None, rec.seq));
        } else if state.is_checked(p, i) {
            if keep_found {
                head = next_seq(head)?;
                plan.push((a, (p, i), Some((status::FOUND, false)), head));
            } else {
                plan.push((a, (p, i), None, Seq(0)));
            }
        } else if paid < affordable {
            paid = paid.saturating_add(1);
            head = next_seq(head)?;
            plan.push((a, (p, i), Some((a.status, pay)), head));
        }
    }
    let mut events = Vec::new();
    let mut shown = Vec::with_capacity(plan.len());
    for (a, (p, i), create, seq) in plan {
        let known = state.hints.get(p, i);
        if let Some((s, paid)) = create {
            let kind =
                EventKind::HintAdded { finding: a.finding, location: a.location, status: s, paid };
            let ev = Event { seq, slot: Some(slot), kind };
            state.record(&ev, limits)?;
            events.push(ev);
        }
        let stored = known.or_else(|| state.hints.get(p, i)).map_or(a.status, |r| r.status);
        if let Some(hint) = state.hint_view(p, i, stored) {
            shown.push(HintShown { hint, new: known.is_none() });
        }
    }
    let unpaid = unfound.saturating_sub(paid);
    let depends_on = events.last().map(|e| e.seq).or_else(|| state.wait_for(wait.0));
    let points = state.hint_points(slot);
    let reply = Reply::Hinted(Box::new(Hinted { shown, unpaid, points, cost }));
    Ok(Applied { events, reply, depends_on })
}

/// Change a known hint's status. Unknown hints, a found hint and an unchanged status are
/// answered with no event (the reference ignores them, `MultiServer.py:2159-2201`).
pub(crate) fn set_status(
    state: &mut RoomState,
    limits: &config::Limits,
    slot: SlotNo,
    finding: SlotNo,
    location: i64,
    s: u8,
) -> Result<Applied, CoreError> {
    if !state.slots.contains_key(&slot) {
        return Err(CoreError::UnknownSlot(slot));
    }
    let unchanged = |state: &RoomState, seq: Seq| Applied {
        events: Vec::new(),
        reply: Reply::HintStatus { changed: false },
        depends_on: state.wait_for(seq.0),
    };
    let Some((pos, i, receiver, _)) = state.place(finding, location) else {
        return Ok(unchanged(state, Seq(0)));
    };
    let Some(rec) = state.hints.get(pos, i) else {
        return Ok(unchanged(state, Seq(0)));
    };
    // The reference checks who asks before it reads the status.
    if receiver != slot {
        return Err(CoreError::Hint(HintRefusal::NoPermission));
    }
    if !settable(s) {
        return Err(CoreError::Hint(HintRefusal::BadStatus(s)));
    }
    if state.is_checked(pos, i) || rec.status == s {
        return Ok(unchanged(state, rec.seq));
    }
    let seq = next_seq(state.head)?;
    let ev = Event {
        seq,
        slot: Some(slot),
        kind: EventKind::HintStatus { finding, location, status: s },
    };
    state.record(&ev, limits)?;
    Ok(Applied {
        events: alloc::vec![ev],
        reply: Reply::HintStatus { changed: true },
        depends_on: Some(seq),
    })
}

#[cfg(test)]
mod tests;
