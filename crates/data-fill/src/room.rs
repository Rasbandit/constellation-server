//! A room: several players' worlds filled together, items crossing between them.
//!
//! Each player has one reachability state ([`Reach`], the truth-maintenance model of
//! `reach.rs`) over their own world. When an item placed in world B belongs to player A, B's
//! state does not collect it; it sends it to the room, which gives it to A's state as a new
//! copy found at the rank of the location that held it. A's state then goes on incrementally,
//! and what it reaches may send items to others in turn. Taking an item back (assumed fill) is
//! the same walk in reverse: the owner's state retracts what depended on the copy, the
//! locations it no longer reaches tell the states holding their items to retract in turn, and
//! the facts of every state are checked lowest rank first, across states.
//!
//! All states take ranks from one sequence (the room hands it from state to state), so a rank
//! orders derivations over the whole room and a fact's support is checked against the room as
//! of its rank, exactly as for one player. No full rescan happens after the first solve.
//!
//! Per-player state is owned by the room and shared data (`World`, `Plan`) is immutable and
//! `Sync`; only the fill is sequential. Choices where the legacy multiplayer fill has options
//! (see `docs/world-rules-section.md`): rounds of one item per player; one shared pool and one shuffled list of locations
//! over all players; an item with a placement group stays in its owner's world; progression
//! never goes on an excluded location; every player's goal must hold (accessibility is not
//! required beyond that); no local-item option (#91), no accessibility modes or progression balancing (#92).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use serde::Deserialize;
use spec_rng::SpecRng;

use crate::fill::{FillError, Instance, Plan};
use crate::par::map_ordered;
use crate::reach::{Msg, Reach, Stats};
use crate::spheres::{RoomSpheres, room_from_states};
use crate::world::World;

/// One player's compiled world, instance and which items count as held.
#[derive(Debug, Clone, Copy)]
pub struct Seat<'a> {
    /// The player's world.
    pub w: &'a World,
    /// Their resolved instance.
    pub plan: &'a Plan,
    /// Per world item: whether it counts as held (`Instance::progression`).
    pub progression: &'a [bool],
}

/// An item at a location, and the player it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Given {
    /// Owner (player index, from 0).
    pub to: u16,
    /// Item index in the owner's world.
    pub item: u16,
}

/// Resolve every player's instance against their world, on up to `workers` threads. The plans
/// come back in player order whatever the worker count; the first error by player index wins.
///
/// # Errors
/// [`FillError::Instance`] naming the player.
pub fn plans(players: &[(&World, &Instance)], workers: usize) -> Result<Vec<Plan>, FillError> {
    too_many(players.len())?;
    map_ordered(players, workers, |(w, inst)| Plan::new(w, inst))
        .into_iter()
        .enumerate()
        .map(|(p, r)| r.map_err(|e| prefix(p, e)))
        .collect()
}

/// Compile every player's manifest into a world, on up to `workers` threads, in player order.
///
/// # Errors
/// The first failing player's message, by player index.
pub fn worlds(manifests: &[&serde_json::Value], workers: usize) -> Result<Vec<World>, String> {
    too_many(manifests.len()).map_err(|e| format!("{e:?}"))?;
    map_ordered(manifests, workers, |m| World::load(m))
        .into_iter()
        .enumerate()
        .map(|(p, r)| r.map_err(|e| format!("player {}: {e}", p + 1)))
        .collect()
}

/// Players' instances from JSON.
///
/// # Errors
/// A value that is not an instance, naming the player.
pub fn instances(raw: &[serde_json::Value]) -> Result<Vec<Instance>, String> {
    too_many(raw.len()).map_err(|e| format!("{e:?}"))?;
    raw.iter()
        .enumerate()
        .map(|(p, v)| {
            Instance::deserialize(v).map_err(|e| format!("player {}: instance: {e}", p + 1))
        })
        .collect()
}

/// Every player's compiled world, resolved plan and progression flags, in player order: what
/// [`Seat`]s borrow.
#[derive(Debug)]
pub struct Compiled {
    /// Per player: the world.
    pub worlds: Vec<World>,
    /// Per player: the resolved instance.
    pub plans: Vec<Plan>,
    /// Per player, per world item: whether it counts as held.
    pub progression: Vec<Vec<bool>>,
}

impl Compiled {
    /// Compile each manifest ([`worlds`]) and resolve each player's instance against it
    /// ([`plans`]), on up to `workers` threads.
    ///
    /// # Errors
    /// As [`worlds`] and [`plans`], or when the two lists differ in length.
    pub fn new(
        manifests: &[&serde_json::Value],
        insts: &[&Instance],
        workers: usize,
    ) -> Result<Self, String> {
        if manifests.len() != insts.len() {
            return Err(format!("{} manifests, {} instances", manifests.len(), insts.len()));
        }
        let worlds = worlds(manifests, workers)?;
        let pairs: Vec<(&World, &Instance)> = worlds.iter().zip(insts.iter().copied()).collect();
        let plans = plans(&pairs, workers).map_err(|e| format!("{e:?}"))?;
        let progression = worlds.iter().zip(insts).map(|(w, i)| i.progression(w)).collect();
        Ok(Compiled { worlds, plans, progression })
    }

    /// One seat per player.
    #[must_use]
    pub fn seats(&self) -> Vec<Seat<'_>> {
        (0..self.plans.len())
            .map(|p| Seat {
                w: &self.worlds[p],
                plan: &self.plans[p],
                progression: &self.progression[p],
            })
            .collect()
    }
}

/// A room takes 1 to `config::ROOM_MAX_PLAYERS` players.
///
/// # Errors
/// [`FillError::Instance`] outside that range.
pub fn too_many(players: usize) -> Result<(), FillError> {
    if players == 0 || players > config::ROOM_MAX_PLAYERS {
        return Err(FillError::Instance(format!(
            "a room takes 1 to {} players, got {players}",
            config::ROOM_MAX_PLAYERS
        )));
    }
    Ok(())
}

fn prefix(p: usize, e: FillError) -> FillError {
    match e {
        FillError::Instance(m) => FillError::Instance(format!("player {}: {m}", p + 1)),
        other => other,
    }
}

/// Result of a room fill.
#[derive(Debug, Clone)]
pub struct RoomFilled {
    /// Per player's world, per location: the item there and its owner.
    pub placed: Vec<Vec<Option<Given>>>,
    /// Full fills started (1 means no retry).
    pub attempts: u32,
    /// Fresh solves of the whole room.
    pub solves: u32,
    /// Steps whose removal retracted something.
    pub rederived: u32,
    /// Groups of items placed together against one state (rounds), in the last attempt.
    pub rounds: u32,
    /// Work counters across all players' states.
    pub stats: Stats,
    /// Per player: the goal holds with everything placed collected.
    pub beatable: Vec<bool>,
    /// Per player: existing locations reached with everything collected.
    pub reached: Vec<usize>,
}

impl RoomFilled {
    /// Every player's goal holds.
    #[must_use]
    pub fn all_beatable(&self) -> bool {
        self.beatable.iter().all(|b| *b)
    }
}

/// Fill a room. Retries the whole fill with the next draws from `rng` when stuck.
///
/// # Errors
/// [`FillError`] when every attempt is stuck, the counts do not match, or there are no or too
/// many players.
pub fn fill_room(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
) -> Result<RoomFilled, FillError> {
    fill_with(seats, rng, max_attempts, true, false).map(|x| x.0)
}

/// [`fill_room`] and the room's spheres, from the fill's own final states.
///
/// # Errors
/// As [`fill_room`].
pub fn fill_room_spheres(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
) -> Result<(RoomFilled, RoomSpheres), FillError> {
    let (f, s) = fill_with(seats, rng, max_attempts, true, true)?;
    Ok((f, s.unwrap_or_else(|| unreachable!("asked for spheres"))))
}

/// Play the room from the start items: per player, is the goal reached and how many existing
/// locations (what [`RoomFilled`] reports, from a fresh solve).
///
/// # Errors
/// [`FillError::Instance`] for seats or placements that do not fit the room (see [`check_placed`]).
pub fn validate_room(
    seats: &[Seat<'_>],
    placed: &[Vec<Option<Given>>],
) -> Result<(Vec<bool>, Vec<usize>), FillError> {
    check_placed(seats, placed)?;
    let mut room = Linked::new(seats);
    room.solve(&vec![Vec::new(); seats.len()], placed);
    Ok(room.report())
}

/// Whether the placement rules let `g` sit at `loc` of `player`'s world (logic is not
/// consulted): an item with a placement group stays in its owner's world and group, and no
/// progression goes on an excluded location.
///
/// `false` for a player, owner or location that is not in the room.
#[must_use]
pub fn permits(seats: &[Seat<'_>], g: Given, player: u16, loc: u32) -> bool {
    let (Some(owner), Some(here)) = (seats.get(usize::from(g.to)), seats.get(usize::from(player)))
    else {
        return false;
    };
    let Some(excluded) = here.plan.excluded.get(loc as usize) else { return false };
    let progression = owner.progression.get(usize::from(g.item)).copied().unwrap_or(false);
    confined_ok(seats, g, player, loc) && !(progression && *excluded)
}

/// Per player's world, per location: the locked item (owned by that player).
#[must_use]
pub fn locked_placed(seats: &[Seat<'_>]) -> Vec<Vec<Option<Given>>> {
    seats
        .iter()
        .enumerate()
        .map(|(p, s)| {
            s.plan
                .locked_placed(s.w)
                .into_iter()
                .map(|i| i.map(|item| Given { to: u16::try_from(p).unwrap_or(0), item }))
                .collect()
        })
        .collect()
}

/// The room as seed-bundle placements, one list per slot (player order, locations ascending):
/// `receiver` is the owner's slot (from 1), `item` an id in the owner's world, `sphere` the
/// round of `spheres` that reached the location (from 0) when given. Flags carry the
/// progression bit only; [`placements_flagged`] sets every class.
///
/// # Errors
/// [`FillError::Instance`] for seats or placements that do not fit the room (see [`check_placed`]).
pub fn placements(
    seats: &[Seat<'_>],
    placed: &[Vec<Option<Given>>],
    spheres: Option<&RoomSpheres>,
) -> Result<Vec<Vec<seed_bundle::Placed>>, FillError> {
    let flags: Vec<Vec<u8>> =
        seats.iter().map(|s| s.progression.iter().map(|p| u8::from(*p)).collect()).collect();
    placements_flagged(seats, placed, spheres, &flags)
}

/// [`placements`] with each item's full classification bits: `flags[owner][item]` (1
/// progression, 2 useful, 4 trap; see `Instance::flags`). An item outside the list gets 0.
///
/// # Errors
/// As [`placements`].
pub fn placements_flagged(
    seats: &[Seat<'_>],
    placed: &[Vec<Option<Given>>],
    spheres: Option<&RoomSpheres>,
    flags: &[Vec<u8>],
) -> Result<Vec<Vec<seed_bundle::Placed>>, FillError> {
    check_placed(seats, placed)?;
    let mut round = std::collections::HashMap::new();
    for (k, s) in spheres.iter().flat_map(|s| s.spheres.iter()).enumerate() {
        round.extend(s.iter().map(|x| (*x, u32::try_from(k).unwrap_or(u32::MAX))));
    }
    Ok(placed
        .iter()
        .enumerate()
        .map(|(p, row)| {
            row.iter()
                .enumerate()
                .filter_map(|(l, g)| {
                    let g = (*g)?;
                    let (l, p) = (u32::try_from(l).unwrap_or(0), u16::try_from(p).unwrap_or(0));
                    let owner = &seats[usize::from(g.to)];
                    let flag =
                        flags.get(usize::from(g.to)).and_then(|f| f.get(usize::from(g.item)));
                    Some(seed_bundle::Placed {
                        location: seats[usize::from(p)].w.loc_id(l),
                        item: owner.w.item_id(g.item),
                        receiver: u32::from(g.to) + 1,
                        flags: flag.copied().unwrap_or(0),
                        sphere: round.get(&(p, l)).copied(),
                    })
                })
                .collect()
        })
        .collect())
}

/// Per owner: progression items lying in other players' worlds, and all their progression
/// items placed (locked ones included).
///
/// # Errors
/// [`FillError::Instance`] for seats or placements that do not fit the room (see [`check_placed`]).
pub fn cross_world_share(
    seats: &[Seat<'_>],
    placed: &[Vec<Option<Given>>],
) -> Result<Vec<(usize, usize)>, FillError> {
    check_placed(seats, placed)?;
    let mut out = vec![(0, 0); seats.len()];
    for (p, row) in placed.iter().enumerate() {
        for g in row.iter().flatten() {
            if seats[usize::from(g.to)].progression.get(usize::from(g.item)).copied() == Some(true)
            {
                let o = &mut out[usize::from(g.to)];
                o.0 += usize::from(usize::from(g.to) != p);
                o.1 += 1;
            }
        }
    }
    Ok(out)
}

/// The seats fit together: a room's size, and each plan and progression list belong to the
/// seat's world (so no index a state later takes from them can be out of range).
pub(crate) fn check(seats: &[Seat<'_>]) -> Result<(), FillError> {
    too_many(seats.len())?;
    let bad = |p: usize, what: &str| {
        Err(FillError::Instance(format!("player {}: {what} does not fit the world", p + 1)))
    };
    for (p, s) in seats.iter().enumerate() {
        let (items, locs) = (s.w.items(), s.w.locations());
        let item = |i: &u16| usize::from(*i) < items;
        if s.progression.len() != items {
            return bad(p, "progression list");
        }
        let plan = s.plan;
        if !plan.included.iter().all(|l| (*l as usize) < locs)
            || plan.excluded.len() != locs
            || !plan.progression.iter().chain(&plan.rest).all(item)
            || !plan.locked.iter().all(|(l, i)| (*l as usize) < locs && item(i))
        {
            return bad(p, "plan");
        }
    }
    Ok(())
}

/// The placements fit the seats: one row per seat of its world's size, owners in the room,
/// items in the owner's world.
///
/// # Errors
/// [`FillError::Instance`] naming the first thing that does not fit.
pub fn check_placed(seats: &[Seat<'_>], placed: &[Vec<Option<Given>>]) -> Result<(), FillError> {
    check(seats)?;
    let err = |m: String| Err(FillError::Instance(m));
    if placed.len() != seats.len() {
        return err(format!("placements for {} slots, the room has {}", placed.len(), seats.len()));
    }
    for (p, row) in placed.iter().enumerate() {
        if row.len() != seats[p].w.locations() {
            return err(format!(
                "slot {}: {} locations, the world has {}",
                p + 1,
                row.len(),
                seats[p].w.locations()
            ));
        }
        for g in row.iter().flatten() {
            match seats.get(usize::from(g.to)) {
                Some(o) if usize::from(g.item) < o.w.items() => {}
                _ => {
                    return err(format!(
                        "slot {}: item {} of player {} is not in the room",
                        p + 1,
                        g.item,
                        u32::from(g.to) + 1
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The fill. `extend` off solves the whole room fresh at every step (the reference the
/// carried states are tested against).
fn fill_with(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
    extend: bool,
    want_spheres: bool,
) -> Result<(RoomFilled, Option<RoomSpheres>), FillError> {
    check(seats)?;
    let mut last = None;
    let mut s = Steps {
        room: Linked::new(seats),
        held: vec![Vec::new(); seats.len()],
        solves: 0,
        rederived: 0,
        rounds: 0,
        stats: Stats::default(),
        extend,
    };
    for attempt in 1..=max_attempts {
        match s.try_fill(rng) {
            Ok(placed) => {
                let (beatable, reached) = s.room.report();
                let sp = want_spheres.then(|| room_from_states(seats, &mut s.room.models, &placed));
                let filled = RoomFilled {
                    attempts: attempt,
                    solves: s.solves,
                    rederived: s.rederived,
                    rounds: s.rounds,
                    stats: s.stats,
                    beatable,
                    reached,
                    placed,
                };
                return Ok((filled, sp));
            }
            Err(FillError::Stuck { item, .. }) => last = Some(item),
            Err(e) => return Err(e),
        }
    }
    Err(FillError::Stuck { item: last.unwrap_or_default(), attempts: max_attempts })
}

/// Every player's state, linked.
struct Linked<'a, 'w> {
    seats: &'a [Seat<'w>],
    models: Vec<Reach<'w>>,
    /// Next rank of the room.
    seq: u32,
    /// Per player: lost facts or was handed copies during this removal.
    touched: Vec<bool>,
    touched_list: Vec<usize>,
    /// Per player: gated to re-derivation (see `Reach::begin_rederive`).
    gated: Vec<bool>,
    gated_list: Vec<usize>,
    rederiving: bool,
    /// Players with something to run, lowest first (`queued`: already in `ready`).
    ready: BinaryHeap<Reverse<usize>>,
    queued: Vec<bool>,
    /// Players whose outbox may hold messages nobody has delivered yet.
    outboxes: Vec<usize>,
    /// Suspects over all states, lowest rank first: `(rank, player)`. An entry is current when
    /// the player's own lowest suspect still has that rank.
    suspects: BinaryHeap<Reverse<(u32, usize)>>,
    /// Players handed a `Move` since the last look (their suspects changed).
    moved: Vec<usize>,
}

impl<'a, 'w> Linked<'a, 'w> {
    fn new(seats: &'a [Seat<'w>]) -> Self {
        Linked {
            seats,
            models: seats.iter().map(|s| Reach::new(s.w, s.plan)).collect(),
            seq: 1,
            touched: vec![false; seats.len()],
            touched_list: Vec::new(),
            gated: vec![false; seats.len()],
            gated_list: Vec::new(),
            rederiving: false,
            ready: BinaryHeap::new(),
            queued: vec![false; seats.len()],
            outboxes: Vec::new(),
            suspects: BinaryHeap::new(),
            moved: Vec::new(),
        }
    }

    /// Player `p` has something to run.
    fn mark(&mut self, p: usize) {
        if !std::mem::replace(&mut self.queued[p], true) {
            self.ready.push(Reverse(p));
        }
    }

    /// A fresh solve of the whole room: each player holds the start items plus `extra[p]`,
    /// and every placed item is collected when its location is reached.
    fn solve(&mut self, extra: &[Vec<u16>], placed: &[Vec<Option<Given>>]) {
        // Rank 1 is the first fact of the first state: the room's sequence starts over.
        self.seq = 1;
        for (p, held) in extra.iter().enumerate() {
            let seq = self.seq;
            self.with(p, |m| m.reset_from(seq));
            self.seats[p].plan.prime(&mut self.models[p], held);
            self.mark(p);
        }
        for (p, row) in placed.iter().enumerate() {
            for (l, g) in row.iter().enumerate() {
                if let Some(g) = g {
                    self.hold(p, u32::try_from(l).unwrap_or(0), *g);
                }
            }
        }
        for p in 0..self.models.len() {
            self.with(p, Reach::begin);
        }
        self.settle();
    }

    /// Location `loc` of player `p`'s world holds `g`.
    fn hold(&mut self, p: usize, loc: u32, g: Given) {
        if usize::from(g.to) == p {
            self.models[p].hold(loc, g.item);
            self.mark(p);
        } else {
            self.models[p].hold_away(loc, g.to, g.item);
            self.outboxes.push(p);
        }
    }

    /// Run states and pass their messages on until nothing is left to do.
    fn settle(&mut self) {
        loop {
            // Ascending by player, as every delivery has always been: ranks and counters follow.
            self.outboxes.sort_unstable();
            self.outboxes.dedup();
            for i in 0..self.outboxes.len() {
                self.deliver(self.outboxes[i]);
            }
            self.outboxes.clear();
            let Some(Reverse(p)) = self.ready.pop() else { return };
            self.queued[p] = false;
            if !self.models[p].idle() {
                self.with(p, Reach::run);
                self.deliver(p);
            }
        }
    }

    /// `f` on state `p` with the room's rank sequence.
    fn with<T>(&mut self, p: usize, f: impl FnOnce(&mut Reach<'w>) -> T) -> T {
        let m = &mut self.models[p];
        m.set_seq(self.seq);
        let out = f(m);
        self.seq = m.seq();
        out
    }

    /// Hand state `p`'s messages to their receivers.
    fn deliver(&mut self, p: usize) {
        let mut i = 0;
        while i < self.models[p].outbox.len() {
            let msg = self.models[p].outbox[i];
            i += 1;
            let to = match msg {
                Msg::Give { to, item, rank } => {
                    self.models[usize::from(to)].give(item, rank);
                    self.mark(usize::from(to));
                    to
                }
                Msg::Move { to, item, from, rank } => {
                    self.models[usize::from(to)].shift(item, from, rank);
                    self.moved.push(usize::from(to));
                    to
                }
            };
            let q = usize::from(to);
            debug_assert_ne!(q, p, "an item for the same player is held in place");
            if !std::mem::replace(&mut self.touched[q], true) {
                self.touched_list.push(q);
            }
            // A state that lost nothing but is handed copies while re-deriving must not
            // derive more than the smaller room supports: it is gated like the others.
            if self.rederiving && !self.gated[q] {
                self.gated[q] = true;
                self.gated_list.push(q);
                self.models[q].gate();
            }
        }
        self.models[p].outbox.clear();
    }

    /// Queue player `p`'s lowest suspect.
    fn suspect(&mut self, p: usize) {
        if let Some(k) = self.models[p].next_suspect() {
            self.suspects.push(Reverse((k, p)));
        }
    }

    /// Take one assumed copy of `item` back from `owner`, across the room.
    fn take_back(&mut self, owner: u16, item: u16) -> bool {
        let o = usize::from(owner);
        if !self.models[o].start_removal(item) {
            return false;
        }
        // Retractions over the room: the cascade reaches other players' states.
        let retracted = |m: &[Reach<'_>]| m.iter().map(|m| m.stats.retractions).sum::<u64>();
        let before = retracted(&self.models);
        for q in self.touched_list.drain(..) {
            self.touched[q] = false;
        }
        self.touched[o] = true;
        self.touched_list.push(o);
        self.suspects.clear();
        self.moved.clear();
        self.suspect(o);
        // Check suspects lowest rank first over all states: a check at rank k needs every
        // retraction below k done, in whichever state it happened. (A check made too early is
        // queued again when a lower fact is retracted, so the order is for cost, not for
        // exactness; the ranks themselves are what exactness rests on, see the cycle test.)
        while let Some(Reverse((k, p))) = self.suspects.pop() {
            if self.models[p].next_suspect() != Some(k) {
                continue; // stale: the player's lowest suspect has a current entry of its own
            }
            self.models[p].cascade_one();
            self.deliver(p);
            self.suspect(p);
            while let Some(q) = self.moved.pop() {
                self.suspect(q);
            }
        }
        let n = retracted(&self.models) - before;
        self.models[o].end_cascade(n);
        self.rederiving = true;
        // Ascending by player: re-deriving hands out ranks.
        self.touched_list.sort_unstable();
        for i in 0..self.touched_list.len() {
            let p = self.touched_list[i];
            if !std::mem::replace(&mut self.gated[p], true) {
                self.gated_list.push(p);
            }
            self.with(p, Reach::begin_rederive);
            self.mark(p);
        }
        self.settle();
        self.rederiving = false;
        for p in self.gated_list.drain(..) {
            self.gated[p] = false;
            self.models[p].end_rederive();
        }
        true
    }

    fn reset_stats(&mut self) {
        self.models.iter_mut().for_each(|m| m.stats = Stats::default());
    }

    fn report(&self) -> (Vec<bool>, Vec<usize>) {
        let beatable = self.models.iter().map(Reach::goal).collect();
        let reached = self
            .seats
            .iter()
            .zip(&self.models)
            .map(|(s, m)| s.plan.included.iter().filter(|l| m.reached(**l)).count())
            .collect();
        (beatable, reached)
    }

    fn absorb(&self, into: &mut Stats) {
        self.models.iter().for_each(|m| into.absorb(&m.stats));
    }
}

struct Steps<'a, 'w> {
    room: Linked<'a, 'w>,
    /// Per player: items assumed held, rebuilt by a fresh solve.
    held: Vec<Vec<u16>>,
    solves: u32,
    rederived: u32,
    rounds: u32,
    stats: Stats,
    extend: bool,
}

impl Steps<'_, '_> {
    fn try_fill(&mut self, rng: &mut SpecRng) -> Result<Vec<Vec<Option<Given>>>, FillError> {
        let seats = self.room.seats;
        self.rounds = 0;
        let mut placed = locked_placed(seats);
        // Every player's free locations and items, in player order.
        let mut free: Vec<(u16, u32)> = Vec::new();
        let (mut prog, mut rest): (Vec<Given>, Vec<Given>) = (Vec::new(), Vec::new());
        for (p, s) in seats.iter().enumerate() {
            let p = u16::try_from(p).unwrap_or(0);
            free.extend(
                s.plan
                    .included
                    .iter()
                    .copied()
                    .filter(|l| placed[usize::from(p)][*l as usize].is_none())
                    .map(|l| (p, l)),
            );
            prog.extend(s.plan.progression.iter().map(|i| Given { to: p, item: *i }));
            rest.extend(s.plan.rest.iter().map(|i| Given { to: p, item: *i }));
        }
        let items = prog.len() + rest.len();
        if items != free.len() {
            return Err(FillError::Count { items, locations: free.len() });
        }
        // Confined items first, as the world's pre_fill does, each into its own group.
        let (mut confined, mut prog): (Vec<Given>, Vec<Given>) = prog
            .into_iter()
            .partition(|g| seats[usize::from(g.to)].plan.item_group.contains_key(&g.item));
        rng.shuffle(&mut confined);
        rng.shuffle(&mut prog);
        let mut locs = free.clone();
        rng.shuffle(&mut locs);
        let mut prev = Vec::new();
        self.restrictive(&groups(confined, false), &prog, &mut locs, &mut placed, &mut prev)?;
        locs.retain(|(p, l)| !seats[usize::from(*p)].plan.excluded[*l as usize]);
        self.restrictive(&groups(prog, true), &[], &mut locs, &mut placed, &mut prev)?;
        // Everything else goes anywhere left, excluded locations included.
        rng.shuffle(&mut rest);
        let mut left: Vec<(u16, u32)> = free
            .into_iter()
            .filter(|(p, l)| placed[usize::from(*p)][*l as usize].is_none())
            .collect();
        rng.shuffle(&mut left);
        for ((p, l), g) in left.into_iter().zip(rest) {
            placed[usize::from(p)][l as usize] = Some(g);
        }
        // The finished room, for the beatability check: collect the last placed item and the
        // rest, which only adds to the fixed point.
        let room = &mut self.room;
        match prev.is_empty() {
            false if self.extend => {
                room.reset_stats();
                for (p, row) in placed.iter().enumerate() {
                    for (l, g) in row.iter().enumerate() {
                        if let Some(g) = g {
                            room.hold(p, u32::try_from(l).unwrap_or(0), *g);
                        }
                    }
                }
                room.settle();
                room.absorb(&mut self.stats);
            }
            _ => {
                room.solve(&vec![Vec::new(); seats.len()], &placed);
                self.solves += 1;
            }
        }
        Ok(placed)
    }

    /// Place each group in turn against a state that holds none of the group (but everything
    /// not yet placed outside it, and everything placed): every item goes to the first location
    /// of `locs` that is reached in that state and allows it.
    fn restrictive(
        &mut self,
        groups: &[Vec<Given>],
        also_held: &[Given],
        locs: &mut Vec<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        prev: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        let seats = self.room.seats;
        for (k, group) in groups.iter().enumerate() {
            self.rounds += 1;
            let carried = if !prev.is_empty() && self.extend {
                // The last group goes where it was put, then this group is taken back.
                self.room.reset_stats();
                for ((p, l), g) in prev.drain(..) {
                    self.room.hold(usize::from(p), l, g);
                }
                self.room.settle();
                for g in group {
                    self.rederived += u32::from(self.room.take_back(g.to, g.item));
                }
                self.room.absorb(&mut self.stats);
                true
            } else {
                false
            };
            if !carried {
                self.held.iter_mut().for_each(Vec::clear);
                for g in groups[k + 1..].iter().flatten().chain(also_held) {
                    self.held[usize::from(g.to)].push(g.item);
                }
                self.room.solve(&self.held, placed);
                self.solves += 1;
                self.room.absorb(&mut self.stats);
            }
            prev.clear();
            for item in group {
                let pos = locs.iter().position(|(p, l)| {
                    self.room.models[usize::from(*p)].reached(*l)
                        && confined_ok(seats, *item, *p, *l)
                });
                let Some(pos) = pos else {
                    let w = seats[usize::from(item.to)].w;
                    return Err(FillError::Stuck {
                        item: w.item_key(item.item).to_owned(),
                        attempts: 1,
                    });
                };
                let (p, l) = locs.remove(pos);
                placed[usize::from(p)][l as usize] = Some(*item);
                prev.push(((p, l), *item));
            }
        }
        Ok(())
    }
}

/// The order items are placed in, each inner list placed together. One at a time, last of the
/// pool first; or `per_player`: rounds that take the last item of every player's own pile, the
/// piles in the order players first appear in the pool (the reference multiplayer fill's
/// default). The items of a round are placed against a state that holds none of them, which
/// favours earlier locations; with one player a round is one item.
fn groups(pool: Vec<Given>, per_player: bool) -> Vec<Vec<Given>> {
    if !per_player {
        return pool.into_iter().rev().map(|g| vec![g]).collect();
    }
    // Piles in the order players first appear; `pile[owner]` indexes them.
    let mut piles: Vec<Vec<Given>> = Vec::new();
    let mut pile: Vec<Option<usize>> = Vec::new();
    for g in pool {
        let o = usize::from(g.to);
        if pile.len() <= o {
            pile.resize(o + 1, None);
        }
        let k = *pile[o].get_or_insert(piles.len());
        if k == piles.len() {
            piles.push(Vec::new());
        }
        piles[k].push(g);
    }
    let mut out = Vec::new();
    loop {
        let round: Vec<Given> = piles.iter_mut().filter_map(Vec::pop).collect();
        if round.is_empty() {
            return out;
        }
        out.push(round);
    }
}

/// The placement group rule alone (progression on excluded locations is handled by the
/// phases' location lists).
fn confined_ok(seats: &[Seat<'_>], g: Given, player: u16, loc: u32) -> bool {
    let owner = &seats[usize::from(g.to)];
    !owner.plan.item_group.contains_key(&g.item)
        || (player == g.to && owner.plan.allowed(g.item, loc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::tests::world_with;
    use crate::reach::assert_same;

    fn rng(seed: u64) -> SpecRng {
        SpecRng::from_u64(seed)
    }

    /// Three players of the richer test world (thresholds, a held-up `any`, events, clocks):
    /// with and without clocks, so the players' worlds differ.
    struct Three {
        worlds: Vec<World>,
        plans: Vec<Plan>,
        progression: Vec<Vec<bool>>,
    }

    impl Three {
        fn new() -> Self {
            let built: Vec<(World, Instance)> =
                [false, true, false].iter().map(|c| world_with(*c)).collect();
            let plans = built.iter().map(|(w, i)| Plan::new(w, i).unwrap()).collect();
            let progression = built.iter().map(|(w, i)| i.progression(w)).collect();
            Three { worlds: built.into_iter().map(|x| x.0).collect(), plans, progression }
        }

        fn seats(&self) -> Vec<Seat<'_>> {
            (0..3)
                .map(|p| Seat {
                    w: &self.worlds[p],
                    plan: &self.plans[p],
                    progression: &self.progression[p],
                })
                .collect()
        }
    }

    /// Each player's hall door needs their own key, and that key lies in the next player's hall:
    /// a cycle through three worlds whose only support is the start copies. Taking the three
    /// copies back one by one must end with every hall closed in every state, though each fact
    /// is supported by another state's fact of lower rank only through the cycle.
    #[test]
    fn removal_does_not_keep_support_that_runs_through_other_players() {
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/room_world.json")).unwrap();
        let w = World::load(&doc["manifest"]).unwrap();
        let inst: Instance = serde_json::from_value(doc["instances"][0].clone()).unwrap();
        let plans: Vec<Plan> = (0..3).map(|_| Plan::new(&w, &inst).unwrap()).collect();
        let prog = inst.progression(&w);
        let seats: Vec<Seat<'_>> =
            plans.iter().map(|plan| Seat { w: &w, plan, progression: &prog }).collect();
        let (key, l3) = (w.item("key_a").unwrap(), w.loc("l3").unwrap());
        let mut placed = locked_placed(&seats);
        for p in 0..3u16 {
            placed[usize::from(p)][l3 as usize] = Some(Given { to: (p + 1) % 3, item: key });
        }
        let mut held: Vec<Vec<u16>> = vec![w.item("key_b").map(|b| vec![key, b]).unwrap(); 3];
        let mut room = Linked::new(&seats);
        room.solve(&held, &placed);
        assert!(room.models.iter().all(|m| m.reached(l3)), "all halls open with every key held");
        for p in 0..3u16 {
            held[usize::from(p)].remove(0);
            room.take_back(p, key);
            let mut fresh = Linked::new(&seats);
            fresh.solve(&held, &placed);
            for (q, m) in room.models.iter().enumerate() {
                assert_same(
                    &w,
                    m,
                    &fresh.models[q],
                    &format!("after taking back player {p}'s key, state {q}"),
                );
            }
        }
        assert!(room.models.iter().all(|m| !m.reached(l3)), "the cycle supports nothing");
    }

    #[test]
    fn rounds_take_the_last_item_of_each_players_pile() {
        let g = |to, item| Given { to, item };
        let pool = vec![g(1, 10), g(0, 1), g(1, 11), g(0, 2), g(1, 12)];
        // Piles in order of first appearance: player 1 [10, 11, 12], player 0 [1, 2].
        assert_eq!(
            groups(pool.clone(), true),
            vec![vec![g(1, 12), g(0, 2)], vec![g(1, 11), g(0, 1)], vec![g(1, 10)]]
        );
        // One at a time: the pool from its end.
        assert_eq!(
            groups(pool, false).concat(),
            vec![g(1, 12), g(0, 2), g(1, 11), g(0, 1), g(1, 10)]
        );
        assert!(groups(Vec::new(), true).is_empty());
        // One player: a round is one item, the same order as one at a time.
        let one = vec![g(0, 5), g(0, 6), g(0, 7)];
        assert_eq!(groups(one.clone(), true), groups(one, false));
    }

    #[test]
    fn carrying_the_room_keeps_every_placement() {
        let t = Three::new();
        let seats = t.seats();
        let (mut with, mut without, mut rederived, mut crossed) = (0, 0, 0, 0);
        for seed in 1u64..=100 {
            let (a, sa) =
                fill_with(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS, true, true).unwrap();
            let (b, _) =
                fill_with(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS, false, false).unwrap();
            assert_eq!(a.placed, b.placed, "seed {seed}");
            assert_eq!(a.attempts, b.attempts, "seed {seed}");
            assert_eq!(
                (a.beatable.clone(), a.reached.clone()),
                validate_room(&seats, &a.placed).unwrap(),
                "seed {seed}"
            );
            assert_eq!(
                sa,
                Some(crate::spheres::room_spheres(&seats, &a.placed).unwrap()),
                "seed {seed}"
            );
            with += a.solves;
            without += b.solves;
            rederived += a.rederived;
            crossed += a
                .placed
                .iter()
                .enumerate()
                .flat_map(|(p, r)| r.iter().flatten().map(move |g| usize::from(g.to) != p))
                .filter(|x| *x)
                .count();
        }
        assert!(with < without, "reuse saved no solve: {with} vs {without}");
        assert!(rederived > 0, "no step retracted anything");
        assert!(crossed > 0, "no item crossed worlds");
    }

    #[test]
    fn taking_items_back_across_players_matches_a_fresh_solve() {
        let t = Three::new();
        let seats = t.seats();
        let (mut lit, mut crossed_back, mut spread, mut churned) = (false, false, false, false);
        for seed in 1u64..=100 {
            let mut r = rng(seed);
            let mut items: Vec<Given> = Vec::new();
            for (p, s) in seats.iter().enumerate() {
                items.extend(
                    s.plan
                        .progression
                        .iter()
                        .map(|i| Given { to: u16::try_from(p).unwrap(), item: *i }),
                );
            }
            r.shuffle(&mut items);
            let mut locs: Vec<(u16, u32)> = Vec::new();
            for (p, s) in seats.iter().enumerate() {
                locs.extend(s.plan.included.iter().map(|l| (u16::try_from(p).unwrap(), *l)));
            }
            r.shuffle(&mut locs);
            // A third of the items placed anywhere in the room, the rest held from the start
            // and taken back one by one.
            let cut = items.len() / 3;
            let mut placed = locked_placed(&seats);
            for ((p, l), g) in locs.iter().zip(&items[..cut]) {
                placed[usize::from(*p)][*l as usize] = Some(*g);
            }
            let mut held: Vec<Vec<u16>> = vec![Vec::new(); 3];
            for g in &items[cut..] {
                held[usize::from(g.to)].push(g.item);
            }
            let mut room = Linked::new(&seats);
            room.solve(&held, &placed);
            for g in items[cut..].iter().rev() {
                let pos = held[usize::from(g.to)].iter().rposition(|i| *i == g.item).unwrap();
                held[usize::from(g.to)].remove(pos);
                let sum = |room: &Linked<'_, '_>, f: fn(&Stats) -> u64| -> u64 {
                    room.models.iter().map(|m| f(&m.stats)).sum()
                };
                let (gone, back) = (sum(&room, |s| s.retractions), sum(&room, |s| s.churn));
                room.take_back(g.to, g.item);
                // One removal's counters cover every state it reached, not only the owner's.
                let (gone, back) =
                    (sum(&room, |s| s.retractions) - gone, sum(&room, |s| s.churn) - back);
                let most = room.models.iter().map(|m| m.stats.max_retractions).max().unwrap();
                assert!(most >= gone, "seed {seed}: max {most} below this removal's {gone}");
                assert!(back <= gone, "seed {seed}: churn {back} above retractions {gone}");
                spread |= room.models.iter().filter(|m| m.stats.retractions > 0).count() > 1;
                churned |= back > 0 && back < gone;
                let mut fresh = Linked::new(&seats);
                fresh.solve(&held, &placed);
                for (p, seat) in seats.iter().enumerate() {
                    let what = format!(
                        "seed {seed}, player {p}, {} left",
                        held.iter().map(Vec::len).sum::<usize>()
                    );
                    assert_same(seat.w, &room.models[p], &fresh.models[p], &what);
                }
                lit |= room.models.iter().any(|m| m.counters.iter().any(|c| *c >= 2));
                crossed_back |= room.models.iter().any(|m| m.stats.retractions > 0)
                    && placed
                        .iter()
                        .enumerate()
                        .any(|(p, row)| row.iter().flatten().any(|g| usize::from(g.to) != p));
            }
        }
        assert!(lit && crossed_back, "counters and cross-world retractions were exercised");
        assert!(spread && churned, "retractions in several states, and partial churn, were seen");
    }
}
