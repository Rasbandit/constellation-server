//! A room: several players' worlds filled together, items crossing between them.
//!
//! Each player has one reachability state ([`Reach`]) over their own world. When an item placed
//! in world B belongs to player A, B's state does not collect it; it sends it to the room, which
//! gives it to A's state as a new copy. A's state
//! then goes on, and what it reaches may send items to others in turn. All states take ranks
//! from one sequence (the room hands it from state to state).
//!
//! Items are placed in groups (one confined item, or a round), each against a state that holds
//! none of the group: the room is solved once, then every group's state is reached by adding to
//! that solve and taking the additions back ([`Steps::split`]). A group's state is the least
//! fixed point of monotone rules for what is held and placed, which is unique, and placement
//! reads nothing but what that state reaches; the tests hold this against a fresh solve of
//! every group's state, which must place every item in the same location.
//!
//! Per-player state is owned by the room and shared data (`World`, `Plan`) is immutable and
//! `Sync`; placing is sequential, on one thread. Choices where the legacy multiplayer fill has
//! options (see `docs/world-rules-section.md`): rounds of one item per player; one shared pool
//! and one shuffled list of locations over all players; each player's items in the order drawn;
//! a round that does not fit its state divided in halves, down to single items; an item that
//! still has nowhere to go given the location of a placed one, which is placed again
//! (`Steps::displace`); a restart with new draws only when that fails too, each player's items
//! then deepest first (`depth.rs`), at most `config::FILL_MAX_ATTEMPTS` attempts; an item with a
//! placement group stays in its owner's world; progression never goes on an excluded location;
//! every progression item lies where it is reached, so every player's goal holds; no local-item
//! option (#91), no progression balancing (#92).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use serde::Deserialize;
use spec_rng::SpecRng;

use crate::depth;
use crate::fill::{FillError, Instance, Plan};
use crate::live::Live;
use crate::reach::{Mark, Msg, Reach, Stats};
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

/// Resolve every player's instance against their world. The plans come back in player order;
/// the first error by player index wins.
///
/// # Errors
/// [`FillError::Instance`] naming the player.
pub fn plans(players: &[(&World, &Instance)]) -> Result<Vec<Plan>, FillError> {
    too_many(players.len())?;
    players
        .iter()
        .map(|(w, inst)| Plan::new(w, inst))
        .enumerate()
        .map(|(p, r)| r.map_err(|e| prefix(p, e)))
        .collect()
}

/// Compile every player's manifest into a world, in player order.
///
/// # Errors
/// The first failing player's message, by player index.
pub fn worlds(manifests: &[&serde_json::Value]) -> Result<Vec<World>, String> {
    too_many(manifests.len())?;
    manifests
        .iter()
        .map(|m| World::load(m))
        .enumerate()
        .map(|(p, r)| r.map_err(|e| player(p, e)))
        .collect()
}

/// Players' instances from JSON.
///
/// # Errors
/// A value that is not an instance, naming the player.
pub fn instances(raw: &[serde_json::Value]) -> Result<Vec<Instance>, String> {
    too_many(raw.len())?;
    raw.iter()
        .enumerate()
        .map(|(p, v)| Instance::deserialize(v).map_err(|e| player(p, format!("instance: {e}"))))
        .collect()
}

/// A world document as the fixtures and tools keep one: `{"manifest", "instances": [...]}`, one
/// world played by several players, or `{"manifest", "instance"}`, one player. Its manifest
/// and every player's instance: the one reading of that shape.
///
/// # Errors
/// An instance that does not parse, or no or too many players.
pub fn from_doc(doc: &serde_json::Value) -> Result<(&serde_json::Value, Vec<Instance>), String> {
    let one = std::slice::from_ref(&doc["instance"]);
    let list = doc["instances"].as_array().map_or(one, Vec::as_slice);
    Ok((&doc["manifest"], instances(list)?))
}

/// Every player's compiled world, resolved plan and progression flags, in player order: what
/// [`Seat`]s borrow.
#[derive(Debug)]
pub struct Compiled {
    /// Per player: the world. Or one world that every player plays ([`Compiled::shared`]).
    pub worlds: Vec<World>,
    /// Per player: the resolved instance.
    pub plans: Vec<Plan>,
    /// Per player, per world item: whether it counts as held.
    pub progression: Vec<Vec<bool>>,
}

impl Compiled {
    /// Compile each manifest ([`worlds`]) and resolve each player's instance against it
    /// ([`plans`]).
    ///
    /// # Errors
    /// As [`worlds`] and [`plans`], or when the two lists differ in length.
    pub fn new(manifests: &[&serde_json::Value], insts: &[&Instance]) -> Result<Self, String> {
        if manifests.len() != insts.len() {
            return Err(format!("{} manifests, {} instances", manifests.len(), insts.len()));
        }
        Self::resolve(worlds(manifests)?, insts)
    }

    /// A world document ([`from_doc`]) as a room: its one world, compiled once, and every
    /// player's instance on it.
    ///
    /// # Errors
    /// As [`from_doc`], [`World::load`] and [`plans`].
    pub fn from_doc(doc: &serde_json::Value) -> Result<Self, String> {
        let (manifest, insts) = from_doc(doc)?;
        Self::shared(World::load(manifest)?, &insts.iter().collect::<Vec<_>>())
    }

    /// One compiled `world` that every player plays, each with their own instance: the world
    /// is held once however many players there are.
    ///
    /// # Errors
    /// As [`plans`].
    pub fn shared(world: World, insts: &[&Instance]) -> Result<Self, String> {
        Self::resolve(vec![world], insts)
    }

    /// Resolve each player's instance against their world (`worlds`, already loaded: one each,
    /// or one for all).
    ///
    /// # Errors
    /// As [`plans`].
    pub fn resolve(worlds: Vec<World>, insts: &[&Instance]) -> Result<Self, String> {
        let world = |p: usize| &worlds[p % worlds.len().max(1)];
        let pairs: Vec<(&World, &Instance)> =
            insts.iter().enumerate().map(|(p, i)| (world(p), *i)).collect();
        let plans = plans(&pairs)?;
        let progression = pairs.iter().map(|(w, i)| i.progression(w)).collect();
        Ok(Compiled { worlds, plans, progression })
    }

    /// Player `p`'s world.
    #[must_use]
    pub fn world(&self, p: usize) -> &World {
        &self.worlds[p % self.worlds.len().max(1)]
    }

    /// One seat per player.
    #[must_use]
    pub fn seats(&self) -> Vec<Seat<'_>> {
        (0..self.plans.len())
            .map(|p| Seat {
                w: self.world(p),
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

/// `e` as said of player `p` (from 0): the one form of that prefix.
pub(crate) fn player(p: usize, e: impl std::fmt::Display) -> String {
    format!("player {}: {e}", p + 1)
}

fn prefix(p: usize, e: FillError) -> FillError {
    match e {
        FillError::Instance(m) => FillError::Instance(player(p, m)),
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
    /// Groups of items placed together against one state (rounds), in the last attempt.
    pub rounds: u32,
    /// Work counters across all players' states.
    pub stats: Stats,
    /// Per player: the goal holds with everything placed collected.
    pub beatable: Vec<bool>,
    /// Per player: existing locations reached with everything collected.
    pub reached: Vec<usize>,
    /// Placed items that gave their location to an item with nowhere to go and were placed
    /// again, in the attempt that made these placements.
    pub swaps: u32,
    /// Rounds (or parts of one) that did not fit their state and were divided, in the attempt
    /// that made these placements.
    pub divided: u32,
    /// Rule evaluations the fill's repairs spent over all its attempts, tried moves
    /// included (`config::FILL_REPAIR_EVALS`, and the solve that crosses it).
    pub repair_evals: u64,
    /// What the rounds of fresh solves did, for measurements.
    pub profile: Rounds,
}

/// The fresh solves of a fill's rounds (a round solves every player's world, then places one
/// item per player against what is reached), counted, and where the wall time went, measured.
/// Kept only with feature `memory-harness` and in this crate's tests; all zero otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rounds {
    /// Worlds solved, over all rounds.
    pub solved: u64,
    /// Of them: worlds that reached another set of locations than in the round before (the
    /// first round's all count).
    pub changed: u64,
    /// Nanoseconds: setting the states up for a solve (reset, start items, what locations
    /// hold), running them to the fixed point, and picking the round's locations.
    pub nanos: [u64; 3],
    /// Journal steps taken back over the fill (each a fact derived or an item
    /// added, then undone), and the most steps the room's journals held at once.
    pub undone: u64,
    /// See `undone`.
    pub journaled: usize,
}

impl RoomFilled {
    /// Every player's goal holds.
    #[must_use]
    pub fn all_beatable(&self) -> bool {
        self.beatable.iter().all(|b| *b)
    }
}

/// How an exact fill forms its rounds in one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rule {
    /// Each player's items go deepest first (`depth.rs`) instead of in the order drawn.
    pub(crate) deepest: bool,
    /// A round that does not fit its state is divided, and an item with nowhere to go takes
    /// the place of a seated one ([`Steps::displace`]).
    pub(crate) repair: bool,
}

impl Rule {
    /// The rounds of attempt `attempt` (from 1): the order drawn, repaired in place; after a
    /// stuck attempt, deepest first, which gives up some depth of the item chains for a room
    /// that does not collapse.
    pub(crate) fn of(attempt: u32) -> Self {
        Rule { deepest: attempt > 1, repair: true }
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
    let tries = (max_attempts, config::FILL_REPAIR_EVALS, Rule::of as fn(u32) -> Rule);
    fill_by(seats, rng, tries, false, false).map(|x| x.0)
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
    let (f, s) =
        fill_by(seats, rng, (max_attempts, config::FILL_REPAIR_EVALS, Rule::of), false, true)?;
    Ok((f, s.unwrap_or_else(|| unreachable!("asked for spheres"))))
}

/// The fill's cross-check, for tests only (feature `cross-check`, which this crate's
/// integration tests turn on): [`fill_room_spheres`] with every group placed against a fresh
/// solve of its own state instead of by adding to one solve and taking the additions back
/// ([`Steps::fresh_rounds`]). It must place every item where the fill does. It is independent
/// of how a group's state is reached (no journal, no undo); it shares round formation, `fits`,
/// `pick`, repair and the solver, so it does not check the placement rule itself.
///
/// # Errors
/// As [`fill_room`].
#[cfg(any(test, feature = "cross-check"))]
pub fn fill_room_fresh(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
) -> Result<(RoomFilled, RoomSpheres), FillError> {
    let (f, s) =
        fill_by(seats, rng, (max_attempts, config::FILL_REPAIR_EVALS, Rule::of), true, true)?;
    Ok((f, s.unwrap_or_else(|| unreachable!("asked for spheres"))))
}

/// Tests only: [`fill_room_spheres`] with what a test holds the fill against: a budget of
/// repair work of its own (`config::FILL_REPAIR_EVALS` in the product), how every attempt
/// forms its rounds ([`Rule::of`] in the product; a test's baseline otherwise), and with
/// `fresh` the cross-check of [`fill_room_fresh`].
#[cfg(test)]
pub(crate) fn fill_room_test(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
    (budget, rule, fresh): (u64, fn(u32) -> Rule, bool),
) -> Result<(RoomFilled, RoomSpheres), FillError> {
    let (f, s) = fill_by(seats, rng, (max_attempts, budget, rule), fresh, true)?;
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
    let (Some(_), Some(here)) = (seats.get(usize::from(g.to)), seats.get(usize::from(player)))
    else {
        return false;
    };
    let exists = (loc as usize) < here.plan.excluded.len();
    exists && confined_ok(seats, g, player, loc) && here.plan.takes(loc, is_progression(seats, g))
}

/// Whether `g` counts as held: a progression item of its owner (`false` for an owner or item
/// that is not in the room). The one reading of it.
#[must_use]
pub fn is_progression(seats: &[Seat<'_>], g: Given) -> bool {
    let owner = seats.get(usize::from(g.to));
    owner.and_then(|s| s.progression.get(usize::from(g.item))).copied().unwrap_or(false)
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
            if is_progression(seats, *g) {
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
        Err(FillError::Instance(player(p, format!("{what} does not fit the world"))))
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

fn fill_by(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    (max_attempts, repair_budget, rule): (u32, u64, fn(u32) -> Rule),
    fresh: bool,
    want_spheres: bool,
) -> Result<(RoomFilled, Option<RoomSpheres>), FillError> {
    check(seats)?;
    #[cfg(not(any(test, feature = "cross-check")))]
    assert!(!fresh, "the fresh solves are built for tests only");
    let mut last = None;
    let mut s = Steps {
        room: Linked::new(seats),
        held: vec![Vec::new(); seats.len()],
        solves: 0,
        rounds: 0,
        stats: Stats::default(),
        #[cfg(any(test, feature = "cross-check"))]
        fresh,
        rule,
        divide: false,
        seated: Vec::new(),
        moved: 0,
        repair_evals: 0,
        repair_budget,
        unit: None,
        attempt: 0,
        divided: 0,
        depth: Vec::new(),
        swaps: 0,
        profile: Rounds::default(),
        last: vec![Vec::new(); seats.len()],
    };
    for attempt in 1..=max_attempts {
        s.attempt = attempt;
        match s.try_fill(rng) {
            Ok(placed) => {
                let (beatable, reached) = s.room.report();
                let sp = want_spheres.then(|| room_from_states(seats, &mut s.room.models, &placed));
                let filled = RoomFilled {
                    attempts: attempt,
                    solves: s.solves,
                    rounds: s.rounds,
                    stats: s.stats,
                    beatable,
                    reached,
                    placed,
                    swaps: s.swaps,
                    divided: s.divided,
                    repair_evals: s.repair_evals,
                    profile: Rounds {
                        nanos: [s.room.nanos[0], s.room.nanos[1], s.profile.nanos[2]],
                        undone: s.room.undone,
                        ..s.profile
                    },
                };
                return Ok((filled, sp));
            }
            Err(FillError::Stuck { item, .. }) => last = Some(item),
            Err(e) => return Err(e),
        }
    }
    Err(FillError::Stuck { item: last.unwrap_or_default(), attempts: max_attempts })
}

/// A duration as nanoseconds, for the round profile.
fn nanos(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Every player's state, linked.
struct Linked<'a, 'w> {
    seats: &'a [Seat<'w>],
    models: Vec<Reach<'w>>,
    /// Next rank of the room.
    seq: u32,
    /// Players with something to run, lowest first (`queued`: already in `ready`).
    ready: BinaryHeap<Reverse<usize>>,
    queued: Vec<bool>,
    /// Players whose outbox may hold messages nobody has delivered yet.
    outboxes: Vec<usize>,
    /// Nanoseconds of the fresh solves: set up, and settled ([`Rounds::nanos`]).
    nanos: [u64; 2],
    /// Journal steps taken back ([`Linked::undo`]), and the most the room's journals held.
    undone: u64,
    journal_peak: usize,
}

impl<'a, 'w> Linked<'a, 'w> {
    fn new(seats: &'a [Seat<'w>]) -> Self {
        Linked {
            seats,
            models: seats.iter().map(|s| Reach::new(s.w, s.plan).lean()).collect(),
            seq: 1,
            ready: BinaryHeap::new(),
            queued: vec![false; seats.len()],
            outboxes: Vec::new(),
            nanos: [0; 2],
            undone: 0,
            journal_peak: 0,
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
        let start = crate::COUNTED.then(std::time::Instant::now);
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
        let set_up = start.map(|t| t.elapsed());
        self.settle();
        if let (Some(start), Some(set_up)) = (start, set_up) {
            self.nanos[0] += nanos(set_up);
            self.nanos[1] += nanos(start.elapsed() - set_up);
        }
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
        for i in 0..self.models[p].outbox.len() {
            let Msg::Give { to, item } = self.models[p].outbox[i];
            debug_assert_ne!(usize::from(to), p, "an item for the same player is held in place");
            self.models[usize::from(to)].give(item);
            self.mark(usize::from(to));
        }
        self.models[p].outbox.clear();
    }

    /// Where every state's journal stands ([`Reach::mark`]) and the room's next rank, at a
    /// fixed point of the room.
    fn marks(&self) -> (Vec<Mark>, u32) {
        (self.models.iter().map(Reach::mark).collect(), self.seq)
    }

    /// Take back everything added to the room since `marks` ([`Reach::undo_to`]); the room's
    /// ranks go on from where they stood then.
    fn undo(&mut self, (marks, seq): &(Vec<Mark>, u32)) {
        if crate::COUNTED {
            let held: usize = self.models.iter().map(|m| m.mark().steps()).sum();
            self.journal_peak = self.journal_peak.max(held);
        }
        for (m, mark) in self.models.iter_mut().zip(marks) {
            self.undone += m.undo_to(*mark) as u64;
        }
        self.seq = *seq;
    }

    /// One more copy of `g`'s item held by its owner, to run on the next settle.
    fn add_held(&mut self, g: Given) {
        let o = usize::from(g.to);
        self.with(o, |m| m.add(g.item, 1));
        self.mark(o);
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
    rounds: u32,
    stats: Stats,
    /// Tests only: every group is placed against a fresh solve ([`Steps::fresh_rounds`]).
    #[cfg(any(test, feature = "cross-check"))]
    fresh: bool,
    /// How an attempt forms its rounds: [`Rule::of`], or a test's baseline.
    rule: fn(u32) -> Rule,
    /// A group that does not fit the state it is placed against is divided ([`Steps::fits`]).
    divide: bool,
    /// The rounds' placements of this attempt, in the order made, and how many of them are
    /// items placed again by [`Steps::displace`].
    seated: Vec<((u16, u32), Given)>,
    moved: usize,
    /// Rule evaluations [`Steps::displace`] spent in this fill, over all its attempts, and the
    /// most it may (`config::FILL_REPAIR_EVALS`).
    repair_evals: u64,
    repair_budget: u64,
    /// What one division of a round is charged against `repair_budget`: the rule evaluations
    /// of the solve with none of the rounds' items at the start of the attempt's rounds.
    unit: Option<u64>,
    /// The attempt being made (from 1), and the groups it divided.
    attempt: u32,
    divided: u32,
    /// Per player, per item: how deep each copy first matters ([`depth::waves`]); filled by
    /// the first attempt that goes deepest first.
    depth: Vec<Vec<Vec<u32>>>,
    swaps: u32,
    profile: Rounds,
    /// Per player: the reached set of the round before ([`Rounds::changed`]).
    last: Vec<Vec<u64>>,
}

impl Steps<'_, '_> {
    fn try_fill(&mut self, rng: &mut SpecRng) -> Result<Vec<Vec<Option<Given>>>, FillError> {
        let seats = self.room.seats;
        (self.rounds, self.divided) = (0, 0);
        // What the seed returned was built with: an abandoned attempt's moves are not in it.
        self.swaps = 0;
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
        let (mut confined, mut prog): (Vec<Given>, Vec<Given>) =
            prog.into_iter().partition(|g| seats[usize::from(g.to)].plan.confined(g.item));
        rng.shuffle(&mut confined);
        rng.shuffle(&mut prog);
        let mut locs = free.clone();
        rng.shuffle(&mut locs);
        let mut locs = Live::new(locs);
        let mut prev = Vec::new();
        self.divide = false;
        // One at a time, the last drawn first.
        let singles: Vec<&[Given]> = confined.iter().rev().map(std::slice::from_ref).collect();
        self.place(&singles, &prog, (&mut locs, &mut placed), &mut prev)?;
        locs.retain(|(p, l)| !seats[usize::from(p)].plan.excluded[l as usize]);
        let Rule { deepest, repair } = (self.rule)(self.attempt);
        if deepest {
            if self.depth.is_empty() {
                let stats = &mut self.stats;
                self.depth = seats.iter().map(|s| depth::waves(s.w, s.plan, stats)).collect();
            }
            depth::deepest_last(&mut prog, &self.depth);
        }
        self.divide = repair;
        self.seated.clear();
        self.moved = 0;
        self.unit = None;
        let groups = groups(prog);
        let mut groups: Vec<&[Given]> = groups.iter().map(Vec::as_slice).collect();
        loop {
            let before = self.seated.len() - self.moved;
            let stuck = match self.place(&groups, &[], (&mut locs, &mut placed), &mut prev) {
                Ok(()) => break,
                Err(e @ FillError::Stuck { .. }) if repair => e,
                Err(e) => return Err(e),
            };
            // Groups are placed in order and a divided group first half first, so what
            // this pass seated is a prefix of its items: the next one has nowhere to go.
            let mut skip = self.seated.len() - self.moved - before;
            groups.retain_mut(|g| {
                let n = skip.min(g.len());
                (skip, *g) = (skip - n, &g[n..]);
                !g.is_empty()
            });
            let Some((item, rest)) = groups.first().and_then(|g| g.split_first()) else {
                return Err(stuck);
            };
            groups[0] = rest;
            groups.retain(|g| !g.is_empty());
            self.displace(*item, &groups, &mut locs, &mut placed)?;
            // The room was solved for the repair: nothing is carried past it.
            prev.clear();
        }
        scatter(&free, &mut rest, &mut placed, rng);
        // The finished room, for the beatability check: collect the last placed item and the
        // rest, which only adds to the fixed point.
        let room = &mut self.room;
        match prev.is_empty() {
            false => {
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
            true => {
                room.solve(&vec![Vec::new(); seats.len()], &placed);
                self.solves += 1;
            }
        }
        Ok(placed)
    }

    /// Place each group in turn against a state that holds none of the group (but `also_held`,
    /// everything of the later groups, and everything placed): every item goes to the first
    /// location of `locs` that is reached in that state and allows it ([`Steps::pick`]). The
    /// state is one per group: an item of the group sees the locations the items before it
    /// took, never what they open. A group that does not fit its state ([`Steps::fits`]) is
    /// divided in halves. The placements are added to `prev`.
    fn place(
        &mut self,
        groups: &[&[Given]],
        also_held: &[Given],
        (locs, placed): (&mut Live<(u16, u32)>, &mut [Vec<Option<Given>>]),
        prev: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        #[cfg(any(test, feature = "cross-check"))]
        if self.fresh {
            return self.fresh_rounds(groups, also_held, locs, placed, prev);
        }
        self.split(groups, also_held, locs, placed, prev)
    }

    /// Tests only: [`Steps::place`] by a fresh solve of every group's state, with nothing
    /// added and taken back: the cross-check of how [`Steps::split`] reaches a group's state
    /// (it shares round formation, `fits`, `pick` and repair), which must place every item
    /// where this does. A divided group's first half is placed with the second half
    /// held too, then the second half as the next group.
    #[cfg(any(test, feature = "cross-check"))]
    fn fresh_rounds(
        &mut self,
        groups: &[&[Given]],
        also_held: &[Given],
        locs: &mut Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        prev: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        if self.unit.is_none() && !groups.is_empty() {
            // What the split path's first solve counts: none of the groups' items held.
            self.held.iter_mut().for_each(Vec::clear);
            self.room.solve(&self.held, placed);
            self.unit = Some(self.room.models.iter().map(|m| m.stats.evals).sum());
        }
        // The groups still to place, the next one last.
        let mut todo: Vec<&[Given]> = groups.iter().rev().copied().collect();
        while let Some(group) = todo.pop() {
            self.held.iter_mut().for_each(Vec::clear);
            for g in todo.iter().copied().flatten().chain(also_held) {
                self.held[usize::from(g.to)].push(g.item);
            }
            self.room.solve(&self.held, placed);
            self.solves += 1;
            self.room.absorb(&mut self.stats);
            self.note_reached();
            if !self.fits(group, locs) {
                self.charge_division(group)?;
                self.divided += 1;
                let (first, second) = group.split_at(group.len() / 2);
                todo.extend([second, first]);
                continue;
            }
            self.rounds += 1;
            prev.clear();
            self.pick(group, locs, placed, prev)?;
        }
        Ok(())
    }

    /// `item` has nowhere to go: no free location is reached without it. It takes the place
    /// of an item of the rounds whose location is reached without it, and that item is placed
    /// again, against the state that has `item` there and holds neither (the items of `later`
    /// held): the assumed fill's own step, so the seed is beatable for the same reason. The
    /// candidates are the seated items reached without `item`, at most
    /// `config::FILL_DISPLACE_TRIES` of them spread evenly over the order they were seated in,
    /// each one fresh solve; a fill's solves here stop once they have spent
    /// `config::FILL_REPAIR_EVALS` rule evaluations.
    ///
    /// # Errors
    /// [`FillError::Stuck`] when no seated item tried can be placed again, or the attempt's
    /// budget of repair work is spent.
    fn displace(
        &mut self,
        item: Given,
        later: &[&[Given]],
        locs: &mut Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
    ) -> Result<(), FillError> {
        self.held.iter_mut().for_each(Vec::clear);
        for g in later.iter().copied().flatten() {
            self.held[usize::from(g.to)].push(g.item);
        }
        self.repair(item, placed, locs)
    }

    /// The tries of [`Steps::displace`], against `self.held`.
    fn repair(
        &mut self,
        item: Given,
        placed: &mut [Vec<Option<Given>>],
        locs: &mut Live<(u16, u32)>,
    ) -> Result<(), FillError> {
        let seats = self.room.seats;
        let w = seats[usize::from(item.to)].w;
        let stuck = || FillError::Stuck { item: w.item_key(item.item).to_owned(), attempts: 1 };
        let mut tried = Vec::new();
        // The pass over the rounds left after a move starts with a fresh solve of about this
        // state: charged as this first solve again, the same for the fill and its cross-check
        // (the pass's own counters differ between them).
        let mut pass = 0;
        for k in 0..=config::FILL_DISPLACE_TRIES {
            // A budget of work, not of time: within it means spent at most the budget, so a
            // room whose repair has spent more is stuck (the same test as `charge_division`).
            if self.repair_evals > self.repair_budget {
                break;
            }
            self.room.solve(&self.held, placed);
            self.solves += 1;
            // Each solve starts its states' counters over: take them in before the next.
            let before = self.stats.evals;
            self.room.absorb(&mut self.stats);
            let spent = self.stats.evals - before;
            self.repair_evals += spent;
            let models = &self.room.models;
            if k == 0 {
                pass = spent;
                // This state has the round's earlier items collected: it may reach a free
                // location the round's state did not, and then no move is needed.
                let free = locs.take_first(|(q, m)| {
                    models[usize::from(q)].reached(m) && permits(seats, item, q, m)
                });
                if let Some((q, m)) = free {
                    placed[usize::from(q)][m as usize] = Some(item);
                    self.seated.push(((q, m), item));
                    self.repair_evals += pass;
                    return Ok(());
                }
                // The seated items reached without `item`, spread evenly over the order they
                // were seated in (the oldest are the deepest: least lies behind them). A copy
                // of the same item would change nothing.
                let reached: Vec<usize> = (0..self.seated.len())
                    .filter(|i| {
                        let ((p, l), g) = self.seated[*i];
                        g != item && models[usize::from(p)].reached(l) && permits(seats, item, p, l)
                    })
                    .collect();
                let step = reached.len().div_ceil(config::FILL_DISPLACE_TRIES).max(1);
                tried = reached.into_iter().step_by(step).collect();
                tried.reverse();
            } else {
                // `item` sits at the candidate's location and the candidate is out: place it.
                let (i, (p, l), out) = tried.pop().map_or_else(
                    || unreachable!("a try without a candidate"),
                    |i| (i, self.seated[i].0, self.seated[i].1),
                );
                let free = locs.take_first(|(q, m)| {
                    models[usize::from(q)].reached(m) && permits(seats, out, q, m)
                });
                if let Some((q, m)) = free {
                    placed[usize::from(q)][m as usize] = Some(out);
                    self.seated[i].1 = item;
                    self.seated.push(((q, m), out));
                    self.moved += 1;
                    self.swaps += 1;
                    self.repair_evals += pass;
                    return Ok(());
                }
                placed[usize::from(p)][l as usize] = Some(out);
            }
            let Some(&i) = tried.last() else { break };
            let ((p, l), _) = self.seated[i];
            placed[usize::from(p)][l as usize] = Some(item);
        }
        // A candidate may still be moved out here: the attempt ends, and its placements with it.
        Err(stuck())
    }

    /// Charge dividing `group` to the repair budget (one [`Steps::unit`] each): halving is
    /// repair work as much as a move is, and its worst case is rounds of one item.
    ///
    /// # Errors
    /// [`FillError::Stuck`] naming the group's first item once the budget is spent.
    fn charge_division(&mut self, group: &[Given]) -> Result<(), FillError> {
        // Read by the groups' first solve, which comes before any division.
        self.repair_evals += self.unit.unwrap_or(0);
        if self.repair_evals > self.repair_budget {
            let g = group[0];
            let w = self.room.seats[usize::from(g.to)].w;
            return Err(FillError::Stuck { item: w.item_key(g.item).to_owned(), attempts: 1 });
        }
        Ok(())
    }

    /// Whether `group` is placed against the room's state as one: it is one item, or groups
    /// are not divided, or the state reaches a free location for each of its items. A group
    /// that does not fit is divided in halves, down to single items if need be: a round is
    /// never stuck where its items, placed one at a time, would not be.
    fn fits(&self, group: &[Given], locs: &Live<(u16, u32)>) -> bool {
        let models = &self.room.models;
        let reached = |(p, l): &(u16, u32)| models[usize::from(*p)].reached(*l);
        !self.divide
            || group.len() <= 1
            || locs.iter().filter(reached).take(group.len()).count() == group.len()
    }

    /// Count the worlds of a round's state, and those that reached another set of locations
    /// than in the round before ([`Rounds`]); only where `crate::COUNTED`.
    fn note_reached(&mut self) {
        let models = self.room.models.iter().zip(&mut self.last);
        for (m, last) in models.filter(|_| crate::COUNTED) {
            self.profile.solved += 1;
            if last.as_slice() != m.reached_words() {
                self.profile.changed += 1;
                last.clear();
                last.extend_from_slice(m.reached_words());
            }
        }
    }

    /// Place a round: each item of `group` goes to the first location of `locs` that the
    /// room's state reaches and that allows it. The placements are added to `prev`.
    fn pick(
        &mut self,
        group: &[Given],
        locs: &mut Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        prev: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        let seats = self.room.seats;
        let picking = crate::COUNTED.then(std::time::Instant::now);
        for item in group {
            let models = &self.room.models;
            let free = locs.take_first(|(p, l)| {
                // Every item placed here is progression: `permits` keeps it off excluded
                // locations, confined or not.
                models[usize::from(p)].reached(l) && permits(seats, *item, p, l)
            });
            let Some((p, l)) = free else {
                let w = seats[usize::from(item.to)].w;
                return Err(FillError::Stuck {
                    item: w.item_key(item.item).to_owned(),
                    attempts: 1,
                });
            };
            placed[usize::from(p)][l as usize] = Some(*item);
            prev.push(((p, l), *item));
            if self.divide {
                self.seated.push(((p, l), *item));
            }
        }
        self.profile.nanos[2] += picking.map_or(0, |t| nanos(t.elapsed()));
        Ok(())
    }

    /// [`Steps::place`] without a solve per group. A group's state holds every item of the
    /// later groups and has every item of the earlier ones where it was put; so a run of
    /// groups `a..=b` shares the state that holds neither of those for any of its groups, and
    /// everything a group's state has beyond it is an addition: rules are monotone, so adding
    /// to a fixed point and running on gives the fixed point of the larger input, and taking
    /// the additions back ([`Reach::undo_to`]) gives the smaller one again. The room is solved
    /// once with none of the groups' items (`also_held` held); [`Steps::halve`] then walks the
    /// groups in order, adding for each half what its groups share and taking it back after.
    ///
    /// The room is left holding what it held before the groups, and `prev` their placements:
    /// the caller adds every placement for the finished room.
    fn split(
        &mut self,
        groups: &[&[Given]],
        also_held: &[Given],
        locs: &mut Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        prev: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        if groups.is_empty() {
            return Ok(());
        }
        self.held.iter_mut().for_each(Vec::clear);
        for g in also_held {
            self.held[usize::from(g.to)].push(g.item);
        }
        self.room.solve(&self.held, placed);
        self.solves += 1;
        if self.unit.is_none() {
            self.unit = Some(self.room.models.iter().map(|m| m.stats.evals).sum());
        }
        let start = crate::COUNTED.then(std::time::Instant::now);
        let picked = self.profile.nanos[2];
        self.room.models.iter_mut().for_each(Reach::keep_journal);
        prev.clear();
        let done = self.halve(groups, locs, placed, prev);
        self.room.models.iter_mut().for_each(Reach::end_journal);
        self.room.absorb(&mut self.stats);
        self.profile.journaled = self.profile.journaled.max(self.room.journal_peak);
        // What the rounds took beyond the one solve, less the picking: adding and undoing.
        let took = start.map_or(0, |t| nanos(t.elapsed()));
        self.room.nanos[1] += took.saturating_sub(self.profile.nanos[2] - picked);
        done
    }

    /// The `groups` of [`Steps::split`] in order, against a room that holds the items of the
    /// groups after them and has those of the groups before them where they were put. One
    /// group that fits the state ([`Steps::fits`]): place it, adding to `put`. One that does
    /// not: its two halves as groups. Several: the first half with the second half's items
    /// held too, then the second half with the first half's items where they now are; each
    /// addition is taken back when its half is done.
    fn halve(
        &mut self,
        groups: &[&[Given]],
        locs: &mut Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        put: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        if let [group] = groups {
            if !self.fits(group, locs) {
                self.charge_division(group)?;
                self.divided += 1;
                let (first, second) = group.split_at(group.len() / 2);
                return self.halve(&[first, second], locs, placed, put);
            }
            self.rounds += 1;
            self.note_reached();
            return self.pick(group, locs, placed, put);
        }
        let (first, second) = groups.split_at(groups.len().div_ceil(2));
        let marks = self.room.marks();
        for g in second.iter().copied().flatten() {
            self.room.add_held(*g);
        }
        self.room.settle();
        let from = put.len();
        self.halve(first, locs, placed, put)?;
        self.room.undo(&marks);
        for &((p, l), g) in &put[from..] {
            self.room.hold(usize::from(p), l, g);
        }
        self.room.settle();
        self.halve(second, locs, placed, put)?;
        self.room.undo(&marks);
        Ok(())
    }
}

/// Everything else goes anywhere left, excluded locations included: `rest` shuffled onto the
/// locations of `free` that hold nothing yet, shuffled.
fn scatter(
    free: &[(u16, u32)],
    rest: &mut Vec<Given>,
    placed: &mut [Vec<Option<Given>>],
    rng: &mut SpecRng,
) {
    rng.shuffle(rest);
    let mut left: Vec<(u16, u32)> = free
        .iter()
        .copied()
        .filter(|(p, l)| placed[usize::from(*p)][*l as usize].is_none())
        .collect();
    rng.shuffle(&mut left);
    for ((p, l), g) in left.into_iter().zip(rest.drain(..)) {
        placed[usize::from(p)][l as usize] = Some(g);
    }
}

/// The rounds items are placed in, each placed together: the last item of every player's own
/// pile, the piles in the order players first appear in the pool (the reference multiplayer
/// fill's default). The items of a round are placed against a state that holds none of them,
/// which favours earlier locations; with one player a round is one item.
fn groups(pool: Vec<Given>) -> Vec<Vec<Given>> {
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

/// The placement group rule alone; [`permits`] adds that progression takes no excluded
/// location, which is what a round is placed by.
fn confined_ok(seats: &[Seat<'_>], g: Given, player: u16, loc: u32) -> bool {
    let owner = &seats[usize::from(g.to)];
    !owner.plan.confined(g.item) || (player == g.to && owner.plan.allowed(g.item, loc))
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

    /// The richer hand-written world (thresholds, a held-up `any`, events, clocks on for one
    /// player), a room of three: the fill places what a fresh solve of every round places, with
    /// fewer solves; its report is a fresh solve's, its spheres a fresh sweep's, and items cross
    /// worlds.
    #[test]
    fn the_fill_of_the_three_world_places_what_a_solve_per_round_places() {
        let t = Three::new();
        let seats = t.seats();
        let (mut with, mut without, mut crossed) = (0, 0, 0);
        for seed in 1u64..=100 {
            let (a, sa) =
                fill_room_spheres(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS).unwrap();
            let oracle = (config::FILL_REPAIR_EVALS, Rule::of as fn(u32) -> Rule, true);
            let (b, sb) =
                fill_room_test(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS, oracle).unwrap();
            assert_eq!(
                (&a.placed, &sa, a.attempts, a.rounds, &a.beatable, &a.reached),
                (&b.placed, &sb, b.attempts, b.rounds, &b.beatable, &b.reached),
                "seed {seed}"
            );
            assert_eq!(
                (a.beatable.clone(), a.reached.clone()),
                validate_room(&seats, &a.placed).unwrap(),
                "seed {seed}"
            );
            assert_eq!(sa, crate::spheres::room_spheres(&seats, &a.placed).unwrap(), "seed {seed}");
            with += a.solves;
            without += b.solves;
            crossed += a
                .placed
                .iter()
                .enumerate()
                .flat_map(|(p, r)| r.iter().flatten().map(move |g| usize::from(g.to) != p))
                .filter(|x| *x)
                .count();
        }
        assert!(with < without, "reuse saved no solve: {with} vs {without}");
        assert!(crossed > 0, "no item crossed worlds");
    }

    #[test]
    fn rounds_take_the_last_item_of_each_players_pile() {
        let g = |to, item| Given { to, item };
        let pool = vec![g(1, 10), g(0, 1), g(1, 11), g(0, 2), g(1, 12)];
        // Piles in order of first appearance: player 1 [10, 11, 12], player 0 [1, 2].
        assert_eq!(
            groups(pool),
            vec![vec![g(1, 12), g(0, 2)], vec![g(1, 11), g(0, 1)], vec![g(1, 10)]]
        );
        assert!(groups(Vec::new()).is_empty());
        // One player: a round is one item, the pool from its end.
        let one = vec![g(0, 5), g(0, 6), g(0, 7)];
        assert_eq!(groups(one), vec![vec![g(0, 7)], vec![g(0, 6)], vec![g(0, 5)]]);
    }

    /// What [`Steps::split`] rests on, on random rooms: adding items (held, or put at a
    /// location in any world) to a settled room and settling again gives every state the
    /// fixed point a fresh solve of the larger input gives, and taking the additions
    /// back gives the state it was, value for value and rank for rank, and so the fixed point
    /// of the smaller input. Random sequences of additions nested several deep and taken back
    /// in any amount, over many seeds, with and without clocks, items crossing worlds.
    #[test]
    fn adding_to_a_room_and_taking_it_back_matches_a_fresh_solve() {
        use crate::synth::{Hostile, Shape, world};
        let small = Shape { checks: 60, layers: 3, ..Shape::default() };
        let heavy = Shape { checks: 60, layers: 3, ..Shape::heavy() };
        let hostile = |how| Shape { checks: 60, ..Shape::hostile(how) };
        let rooms = [
            (small, 1),
            (small, 3),
            (heavy, 2),
            (heavy, 5),
            (hostile(Hostile::Chain), 3),
            (hostile(Hostile::Keys), 2),
            (hostile(Hostile::Brink), 3),
            (hostile(Hostile::Tight), 2),
            (Shape::wide(), 2),
        ];
        let (mut steps, mut undone, mut lost, mut crossed) = (0, 0u64, 0, 0);
        for (shape, players) in rooms {
            for seed in 1u64..=12 {
                // Every other room of the layered worlds has excluded and locked locations and
                // start items too (placement groups are the fill's business, not the states').
                let built: Vec<_> = (0..players as u64)
                    .map(|p| {
                        let (m, mut inst) = world(&shape, seed, p);
                        if seed % 2 == 0 && shape.hostile.is_none() && !shape.wide {
                            inst.excluded = (0..60).step_by(6).map(|l| format!("l{l}")).collect();
                            let at = inst.pool.iter().position(|k| k == "p1").unwrap();
                            inst.locked.insert("l3".to_owned(), inst.pool.remove(at));
                            inst.start_inventory.push("p1".to_owned());
                            inst.logic_start.insert("p0".to_owned(), 1);
                        }
                        (m, inst)
                    })
                    .collect();
                let manifests: Vec<_> = built.iter().map(|b| &b.0).collect();
                let insts: Vec<_> = built.iter().map(|b| &b.1).collect();
                let c = Compiled::new(&manifests, &insts).unwrap();
                let seats = c.seats();
                let mut r = rng(seed);
                let mut pick = |n: usize| usize::try_from(r.below(n as u64).unwrap()).unwrap();
                let mut items: Vec<Given> = Vec::new();
                let mut locs: Vec<(u16, u32)> = Vec::new();
                for (p, s) in seats.iter().enumerate() {
                    let p = u16::try_from(p).unwrap();
                    items.extend(s.plan.progression.iter().map(|i| Given { to: p, item: *i }));
                    locs.extend(s.plan.included.iter().map(|l| (p, *l)));
                }
                let mut placed = locked_placed(&seats);
                locs.retain(|(p, l)| placed[usize::from(*p)][*l as usize].is_none());
                let mut held: Vec<Vec<u16>> = vec![Vec::new(); seats.len()];
                for _ in 0..items.len() / 4 {
                    let g = items.swap_remove(pick(items.len()));
                    held[usize::from(g.to)].push(g.item);
                }
                let mut room = Linked::new(&seats);
                room.solve(&held, &placed);
                room.models.iter_mut().for_each(Reach::keep_journal);
                let fresh = |held: &[Vec<u16>], placed: &[Vec<Option<Given>>]| {
                    let mut f = Linked::new(&seats);
                    f.solve(held, placed);
                    f
                };
                let reached = |room: &Linked<'_, '_>| -> usize {
                    room.models
                        .iter()
                        .map(|m| {
                            m.reached_words().iter().map(|w| w.count_ones() as usize).sum::<usize>()
                        })
                        .sum()
                };
                // What to come back to: the marks, the states as they were, and the input.
                let mut stack = Vec::new();
                for step in 0..40 {
                    let what = format!("{players} players, seed {seed}, step {step}");
                    let back = !stack.is_empty() && (items.len() < 6 || pick(5) < 2);
                    if back {
                        let keep = pick(stack.len());
                        let (marks, was, h, pl, it, lo): (
                            (Vec<Mark>, u32),
                            Vec<Reach<'_>>,
                            _,
                            _,
                            _,
                            _,
                        ) = stack.swap_remove(keep);
                        stack.truncate(keep);
                        let before = (room.undone, reached(&room));
                        room.undo(&marks);
                        (held, placed, items, locs) = (h, pl, it, lo);
                        undone += room.undone - before.0;
                        lost += usize::from(reached(&room) < before.1);
                        for (p, (m, was)) in room.models.iter().zip(&was).enumerate() {
                            assert_eq!(m.differs(was), None, "{what}, player {p}: undone");
                        }
                    } else {
                        stack.push((
                            room.marks(),
                            room.models.clone(),
                            held.clone(),
                            placed.clone(),
                            items.clone(),
                            locs.clone(),
                        ));
                        for _ in 0..1 + pick(5) {
                            let g = items.swap_remove(pick(items.len()));
                            let o = usize::from(g.to);
                            if pick(2) == 0 {
                                held[o].push(g.item);
                                room.add_held(g);
                            } else {
                                let (p, l) = locs.swap_remove(pick(locs.len()));
                                placed[usize::from(p)][l as usize] = Some(g);
                                room.hold(usize::from(p), l, g);
                                crossed += usize::from(p != g.to);
                            }
                        }
                        room.settle();
                    }
                    steps += 1;
                    let f = fresh(&held, &placed);
                    for (p, seat) in seats.iter().enumerate() {
                        assert_same(
                            seat.w,
                            &room.models[p],
                            &f.models[p],
                            &format!("{what}, player {p}"),
                        );
                    }
                }
            }
        }
        assert!(steps == 4320 && undone > 10_000, "{steps} steps, {undone} journal steps undone");
        assert!(
            lost > 500 && crossed > 1000,
            "{lost} undos lost a location, {crossed} items crossed"
        );
    }
}
