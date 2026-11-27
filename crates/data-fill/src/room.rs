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
//! of its rank, exactly as for one player. On this carried path no full rescan happens after
//! the first solve.
//!
//! A room places its rounds on the
//! split path ([`Path::Split`]): one solve, then every round's state by adding to it and
//! taking the additions back. A third path ([`Path::Forward`]), for a caller that names it,
//! solves the room afresh for every round instead, forward only. A round's state is the least fixed point
//! of monotone rules for what is held and placed, which is unique, and placement reads nothing
//! but what that state reaches, so both paths place every item in the same location. On that
//! path the states of a room run on several threads, each to its own fixed point, and exchange
//! what they found in player order between runs; which thread ran a state changes nothing.
//!
//! Per-player state is owned by the room and shared data (`World`, `Plan`) is immutable and
//! `Sync`; placing is sequential. Choices where the legacy multiplayer fill has options
//! (see `docs/world-rules-section.md`): rounds of one item per player; one shared pool and one shuffled list of locations
//! over all players; each player's items placed deepest first (`depth.rs`), so that what a
//! round takes out never hides the items a world needs first; a round that does not fit its
//! state divided in halves, down to single items; an item that still has nowhere to go given
//! the location of a placed one, which is placed again (`Steps::displace`); a restart with new
//! draws only when that fails too, at most `config::FILL_MAX_ATTEMPTS` attempts; an item with a placement group stays in its owner's world; progression
//! never goes on an excluded location; every player's goal must hold (the exact fill asks for
//! no accessibility beyond that; the opt-in modes have `Dials::goal_only`); no local-item
//! option (#91), no progression balancing (#92).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use serde::Deserialize;
use spec_rng::SpecRng;

use crate::depth;
use crate::fill::{FillError, Instance, Plan};
use crate::level;
use crate::live::Live;
use crate::par::{for_each_mut, map_ordered};
use crate::reach::{Mark, Msg, Reach, Stats};
use crate::section::CHECKS;
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
    /// Per player: the world. Or one world that every player plays ([`Compiled::shared`]).
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
        Self::resolve(worlds(manifests, workers)?, insts, workers)
    }

    /// One compiled `world` that every player plays, each with their own instance: the world
    /// is held once however many players there are.
    ///
    /// # Errors
    /// As [`plans`].
    pub fn shared(world: World, insts: &[&Instance], workers: usize) -> Result<Self, String> {
        Self::resolve(vec![world], insts, workers)
    }

    /// Resolve each player's instance against their world (`worlds`, already loaded: one each,
    /// or one for all), on up to `workers` threads.
    ///
    /// # Errors
    /// As [`plans`].
    pub fn resolve(
        worlds: Vec<World>,
        insts: &[&Instance],
        workers: usize,
    ) -> Result<Self, String> {
        let world = |p: usize| &worlds[p % worlds.len().max(1)];
        let pairs: Vec<(&World, &Instance)> =
            insts.iter().enumerate().map(|(p, i)| (world(p), *i)).collect();
        let plans = plans(&pairs, workers).map_err(|e| format!("{e:?}"))?;
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
    /// The placements are the asked opt-in mode's own (`false`: the exact fill's, asked for or
    /// fallen back to). A seed bundle has no field for this (`docs/seed-bundle-v2.md`).
    pub fast: bool,
    /// Level fill: times the drawn order was reordered and solved again for these placements
    /// (0 when the attempt that made them fell back).
    pub repairs: u32,
    /// Attempts (not seeds) an opt-in mode gave up on and the exact fill placed, or was stuck
    /// on, instead: with `fast` and a count above 0, an earlier attempt fell back, was stuck,
    /// and the mode placed a later one.
    pub fallbacks: u32,
    /// Shuffle fill: progression items moved into reach in these placements (0 with `fast`:
    /// they are the uniform shuffle as drawn; 0 too when the attempt fell back). Exact fill:
    /// placed items that gave their location to an item with nowhere to go and were placed
    /// again.
    pub swaps: u32,
    /// Exact fill: rounds (or parts of one) that did not fit their state and were divided, in
    /// the attempt that made these placements.
    pub divided: u32,
    /// Exact fill: rule evaluations the fill's repairs spent over all its attempts, tried moves
    /// included (`config::FILL_REPAIR_EVALS`, and the solve that crosses it).
    pub repair_evals: u64,
    /// Rule evaluations of the opt-in mode's own solves, out of `stats.evals` (the rest are
    /// the confined items' steps and the final solve that checks and reports the seed).
    pub mode_evals: u64,
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
    /// [`Path::Split`]: journal steps taken back over the fill (each a fact derived or an item
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

/// How the state a round of one item per player is placed against is found. The placements
/// are the same on both paths (see the module text); only the work differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// Carry every state from round to round: the last round's items are held where they were
    /// put, then each item of this round is taken back by truth maintenance.
    Carry,
    /// Solve the room afresh for every round with none of the round's items held, forward
    /// only: nothing is retracted. Confined items, placed one at a time before the rounds, are
    /// still carried. The players' states run on up to `workers` threads; the result does not
    /// depend on the number.
    Forward {
        /// Threads the states of a room run on (at most `config::FILL_MAX_WORKERS`).
        workers: usize,
    },
    /// Solve the room once with none of the rounds' items, then reach every round's state by
    /// adding to it and taking the additions back (`Steps::split`): the rounds are split in
    /// halves again and again, and a half's state holds everything but its own rounds' items.
    /// Forward only, on one thread; a round's state is the same least fixed point.
    Split,
}

impl Path {
    /// The path a room takes, whatever its size and whatever `workers` is: the split path, on
    /// one thread (measured on the `bench-room` job: 100 players fill 5 to 10 times faster
    /// than on the forward path, which four threads do not make up, and one heavy player in
    /// 2.8 ms against 11.6 ms carried, run 45924343167). The other paths stay for a caller
    /// that names them.
    #[must_use]
    pub fn of(_players: usize, _workers: usize) -> Self {
        Path::Split
    }
}

/// The dials of the opt-in fill modes. The defaults (`config::FILL_DEPTH_PERCENT`,
/// `config::FILL_GOAL_ONLY`) are uniform picks and every progression item collectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dials {
    /// Where a progression item goes among the locations it may take, percent: 0 uniform,
    /// above 0 towards locations opened later, below 0 towards earlier ones.
    pub depth: i32,
    /// Only every player's goal has to be reachable, not every progression item.
    pub goal_only: bool,
    /// Not a dial of feel but of work (`config::FILL_SKIP_EMPTY_CHECKS`): the same seed either
    /// way. With `goal_only`, and a caller that asks for no spheres, the final pass over the
    /// skipped checks is left out too, and `RoomFilled::reached` then counts only locations
    /// whose check was evaluated.
    pub skip_empty: bool,
}

impl Dials {
    /// The dials are within their limits (`config::FILL_DEPTH_MAX_PERCENT` either way).
    ///
    /// # Errors
    /// [`FillError::Instance`] naming the limit.
    pub fn check(&self) -> Result<(), FillError> {
        let most = config::FILL_DEPTH_MAX_PERCENT;
        if self.depth.checked_abs().is_none_or(|d| d > most) {
            return Err(FillError::Instance(format!(
                "depth {} is not in -{most} to {most}",
                self.depth
            )));
        }
        Ok(())
    }
}

impl Default for Dials {
    fn default() -> Self {
        Dials {
            depth: config::FILL_DEPTH_PERCENT,
            goal_only: config::FILL_GOAL_ONLY,
            skip_empty: config::FILL_SKIP_EMPTY_CHECKS,
        }
    }
}

/// How the progression items that have no placement group are placed. Confined items before
/// them and everything else after them are placed the same way in every mode. In a fill
/// request: `"exact"`, or `{"level": {}}` with any of the dials inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Algorithm {
    /// Assumed fill: every item is placed against a state that holds what is not yet placed.
    /// The default and the reference.
    #[default]
    Exact,
    /// Level fill (see `level.rs`): draw an order of the items, label every location with the
    /// number of order items held when one forward solve first reaches it, and give item `k` a
    /// location of a level below `k`. About one solve instead of one per item or round; an
    /// order that cannot be made to work falls back to [`Algorithm::Exact`] for that attempt.
    Level(Dials),
    /// Shuffle then unstick: every item goes to a uniformly random permitted location, one
    /// forward solve collects what it reaches, and while progression is out of reach one
    /// unreached progression item at a time changes places with a non-progression item at a
    /// reached location, the solve going on from where it stopped. A seed whose first shuffle
    /// is beatable is that shuffle. Runs on one thread. With no reached location left to
    /// move to, the attempt falls back to [`Algorithm::Exact`].
    Shuffle(Dials),
    /// For measurements only (tests and feature `memory-harness`): the exact fill as it formed
    /// its rounds before issue 212, a round never divided and no item moved; a stuck attempt
    /// starts over in the same way. What [`Algorithm::Exact`] is held against in the bench and
    /// the quality report.
    #[cfg(any(test, feature = "memory-harness"))]
    #[serde(skip)]
    Drawn,
    /// For measurements only: [`Algorithm::Exact`] with every attempt deepest first, the
    /// first one too (what the exact fill does only after a stuck attempt).
    #[cfg(any(test, feature = "memory-harness"))]
    #[serde(skip)]
    Deepest,
    /// For measurements only: [`Algorithm::Exact`] with every attempt in the order drawn, the
    /// restarts too (the exact fill goes deepest first after a stuck attempt).
    #[cfg(any(test, feature = "memory-harness"))]
    #[serde(skip)]
    Repaired,
    /// For measurements only: [`Algorithm::Drawn`] with rounds of one item.
    #[cfg(any(test, feature = "memory-harness"))]
    #[serde(skip)]
    OneItem,
}

/// How an exact fill forms its rounds in one attempt.
#[derive(Debug, Clone, Copy)]
struct Rule {
    /// Each player's items go deepest first (`depth.rs`) instead of in the order drawn.
    deepest: bool,
    /// A round that does not fit its state is divided, and an item with nowhere to go takes
    /// the place of a seated one ([`Steps::displace`]).
    repair: bool,
    /// A round takes one item from every player; `false`: one item.
    per_player: bool,
}

impl Algorithm {
    /// The dials of an opt-in mode.
    #[must_use]
    pub fn dials(self) -> Option<Dials> {
        match self {
            Algorithm::Level(d) | Algorithm::Shuffle(d) => Some(d),
            Algorithm::Exact => None,
            #[cfg(any(test, feature = "memory-harness"))]
            Algorithm::Drawn | Algorithm::Deepest | Algorithm::Repaired | Algorithm::OneItem => {
                None
            }
        }
    }

    /// The rounds of attempt `attempt` (from 1) of an exact fill, or of an opt-in mode's
    /// fallback to it: the order drawn, repaired in place; after a stuck attempt, deepest
    /// first, which gives up some depth of the item chains for a room that does not collapse.
    fn rule(self, attempt: u32) -> Rule {
        let rule = |deepest, repair, per_player| Rule { deepest, repair, per_player };
        match self {
            Algorithm::Exact | Algorithm::Level(_) | Algorithm::Shuffle(_) => {
                rule(attempt > 1, true, true)
            }
            #[cfg(any(test, feature = "memory-harness"))]
            Algorithm::Drawn => rule(false, false, true),
            #[cfg(any(test, feature = "memory-harness"))]
            Algorithm::Deepest => rule(true, true, true),
            #[cfg(any(test, feature = "memory-harness"))]
            Algorithm::Repaired => rule(false, true, true),
            #[cfg(any(test, feature = "memory-harness"))]
            Algorithm::OneItem => rule(false, false, false),
        }
    }
}

/// A mode as command lines name it: `exact`, `level` or `shuffle`, then any of `,depth=N`,
/// `,goal` (goal only) and `,noskip` (evaluate every check as the solve meets it). In
/// measurement builds also `drawn`, `deepest`, `repaired` and `single` ([`Algorithm::Drawn`],
/// [`Algorithm::Deepest`], [`Algorithm::Repaired`], [`Algorithm::OneItem`]).
impl std::str::FromStr for Algorithm {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let mut parts = s.split(',');
        let name = parts.next().unwrap_or("");
        let mut d = Dials::default();
        for part in parts {
            match part.split_once('=') {
                Some(("depth", n)) => d.depth = n.parse().map_err(|_| format!("depth {n:?}"))?,
                None if part == "goal" => d.goal_only = true,
                None if part == "noskip" => d.skip_empty = false,
                _ => return Err(format!("mode setting {part:?}: depth=N, goal or noskip")),
            }
        }
        d.check().map_err(|e| format!("{e:?}"))?;
        match name {
            "exact" if d == Dials::default() => Ok(Algorithm::Exact),
            #[cfg(any(test, feature = "memory-harness"))]
            "drawn" if d == Dials::default() => Ok(Algorithm::Drawn),
            #[cfg(any(test, feature = "memory-harness"))]
            "deepest" if d == Dials::default() => Ok(Algorithm::Deepest),
            #[cfg(any(test, feature = "memory-harness"))]
            "repaired" if d == Dials::default() => Ok(Algorithm::Repaired),
            #[cfg(any(test, feature = "memory-harness"))]
            "single" if d == Dials::default() => Ok(Algorithm::OneItem),
            "level" => Ok(Algorithm::Level(d)),
            "shuffle" => Ok(Algorithm::Shuffle(d)),
            _ => Err(format!("mode {s:?}: exact, level or shuffle (the exact fill has no dials)")),
        }
    }
}

/// Fill a room, on the path a room of its size takes ([`Path::of`]). Retries the whole fill
/// with the next draws from `rng` when stuck. A caller that wants another path names it to
/// [`fill_room_on`].
///
/// # Errors
/// [`FillError`] when every attempt is stuck, the counts do not match, or there are no or too
/// many players.
pub fn fill_room(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
) -> Result<RoomFilled, FillError> {
    let path = Path::of(seats.len(), config::ROOM_FORWARD_WORKERS);
    fill_on(seats, rng, max_attempts, true, path, false).map(|x| x.0)
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
    fill_room_on(seats, rng, max_attempts, Path::of(seats.len(), config::ROOM_FORWARD_WORKERS))
}

/// [`fill_room_spheres`] on a given path, whatever the room's size: for a caller with a worker
/// count of its own ([`Path::of`]), measurements, and the tests that hold the two paths
/// against each other.
///
/// # Errors
/// As [`fill_room`].
pub fn fill_room_on(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
    path: Path,
) -> Result<(RoomFilled, RoomSpheres), FillError> {
    fill_room_by(seats, rng, max_attempts, path, Algorithm::Exact)
}

/// [`fill_room_on`] with the mode that places the progression items. `path` still says how an
/// exact fill finds its states, and how many threads a forward solve of any mode may take.
///
/// # Errors
/// As [`fill_room`], and [`FillError::Invalid`] when an opt-in mode's result fails its own
/// check (a bug).
pub fn fill_room_by(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
    path: Path,
    algorithm: Algorithm,
) -> Result<(RoomFilled, RoomSpheres), FillError> {
    let budget = config::FILL_REPAIR_EVALS;
    let (f, s) = fill_by(seats, rng, (max_attempts, budget), (true, path, algorithm), true)?;
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
    let Some(excluded) = here.plan.excluded.get(loc as usize) else { return false };
    confined_ok(seats, g, player, loc) && !(is_progression(seats, g) && *excluded)
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

/// The fill on the carried path. `extend` off solves the whole room fresh at every step, in
/// rank order (the reference the carried states are tested against).
#[cfg(test)]
fn fill_with(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
    extend: bool,
    want_spheres: bool,
) -> Result<(RoomFilled, Option<RoomSpheres>), FillError> {
    fill_on(seats, rng, max_attempts, extend, Path::Carry, want_spheres)
}

fn fill_on(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    max_attempts: u32,
    extend: bool,
    path: Path,
    want_spheres: bool,
) -> Result<(RoomFilled, Option<RoomSpheres>), FillError> {
    let tries = (max_attempts, config::FILL_REPAIR_EVALS);
    fill_by(seats, rng, tries, (extend, path, Algorithm::Exact), want_spheres)
}

/// [`fill_room_on`] with a budget of repair work of its own instead of
/// `config::FILL_REPAIR_EVALS`.
#[cfg(test)]
pub(crate) fn fill_room_budget(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    (max_attempts, budget): (u32, u64),
    path: Path,
) -> Result<RoomFilled, FillError> {
    fill_by(seats, rng, (max_attempts, budget), (true, path, Algorithm::Exact), false).map(|x| x.0)
}

fn fill_by(
    seats: &[Seat<'_>],
    rng: &mut SpecRng,
    (max_attempts, repair_budget): (u32, u64),
    (extend, path, algorithm): (bool, Path, Algorithm),
    want_spheres: bool,
) -> Result<(RoomFilled, Option<RoomSpheres>), FillError> {
    check(seats)?;
    // The one door every caller's dials come through, a fill request's included.
    algorithm.dials().map_or(Ok(()), |d| d.check())?;
    let mut last = None;
    let mut s = Steps {
        room: Linked::new(seats),
        held: vec![Vec::new(); seats.len()],
        solves: 0,
        rederived: 0,
        rounds: 0,
        stats: Stats::default(),
        extend,
        path,
        algorithm,
        divide: false,
        seated: Vec::new(),
        moved: 0,
        repair_evals: 0,
        repair_budget,
        attempt: 0,
        divided: 0,
        depth: Vec::new(),
        spheres: want_spheres,
        fast: false,
        repairs: 0,
        fallbacks: 0,
        swaps: 0,
        mode_evals: 0,
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
                    rederived: s.rederived,
                    rounds: s.rounds,
                    stats: s.stats,
                    beatable,
                    reached,
                    placed,
                    fast: s.fast,
                    repairs: s.repairs,
                    fallbacks: s.fallbacks,
                    swaps: s.swaps,
                    divided: s.divided,
                    repair_evals: s.repair_evals,
                    mode_evals: s.mode_evals,
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
    /// Settle in supersteps ([`Linked::supersteps`]) on this many threads instead of in rank
    /// order: for states no removal will read.
    forward: Option<usize>,
    /// A solve has just started: every state has its whole solve ahead.
    fresh: bool,
    /// The ranks order every derivation of the room: no superstep has run since the last
    /// solve in rank order. Truth maintenance needs it.
    ranked: bool,
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
            forward: None,
            fresh: false,
            ranked: true,
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
        self.solve_as(extra, placed, None);
    }

    /// [`Linked::solve`]; with `awake`, of progression items only (what is not progression is
    /// not held), where the check of a location that holds nothing sleeps unless
    /// `awake(player, location)`: see [`Reach::sleep`]. Nothing may be taken back from such a
    /// solve.
    fn solve_as(
        &mut self,
        extra: &[Vec<u16>],
        placed: &[Vec<Option<Given>>],
        awake: Option<&dyn Fn(usize, u32) -> bool>,
    ) {
        let seats = self.seats;
        let start = crate::COUNTED.then(std::time::Instant::now);
        let held = |g: &Given| awake.is_none() || is_progression(seats, *g);
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
                if let Some(g) = g.filter(held) {
                    self.hold(p, u32::try_from(l).unwrap_or(0), g);
                }
            }
        }
        if let Some(awake) = awake {
            for (p, m) in self.models.iter_mut().enumerate() {
                m.sleep(|l| awake(p, l));
            }
        }
        for p in 0..self.models.len() {
            self.with(p, Reach::begin);
        }
        self.fresh = true;
        // Every state starts over, so the ranks are the room's again unless supersteps follow.
        self.ranked = true;
        let set_up = start.map(|t| t.elapsed());
        self.settle();
        if let (Some(start), Some(set_up)) = (start, set_up) {
            self.nanos[0] += nanos(set_up);
            self.nanos[1] += nanos(start.elapsed() - set_up);
        }
    }

    /// Level fill's solve. The room is solved with nothing of `order` held (`none`: no extra
    /// items for any player), then the order's items are added one at a time, each run to the
    /// fixed point before the next. Returns the room's next rank with `k` items held, for `k`
    /// from 0 to one less than the items (a location reached at a rank below entry `k` was
    /// reached with at most `k` held), and the number of items held when every goal first held
    /// (the number of items when that needs all of them, or never happens).
    ///
    /// The first solve settles as the room is set to (`forward`); the steps run one state at a
    /// time off the ready list, since one item wakes few states.
    fn levels(
        &mut self,
        order: &[Given],
        none: &[Vec<u16>],
        placed: &[Vec<Option<Given>>],
        awake: &dyn Fn(usize, u32) -> bool,
    ) -> (Vec<u32>, usize) {
        let forward = self.forward;
        self.solve_as(none, placed, Some(awake));
        self.forward = None;
        // Supersteps leave every state ranks of its own: go on above all of them, so a later
        // rank is a later step in every state.
        self.seq = self.models.iter().map(Reach::seq).fold(self.seq, u32::max);
        let mut marks = Vec::with_capacity(order.len());
        let (mut met, mut goal) = (0, order.len());
        for (k, g) in order.iter().enumerate() {
            while met < self.models.len() && self.models[met].goal() {
                met += 1;
            }
            if met == self.models.len() {
                goal = goal.min(k);
            }
            marks.push(self.seq);
            // What only the last item opens is of no use to any item.
            if k + 1 < order.len() {
                self.add_held(*g);
                self.settle();
            }
        }
        self.forward = forward;
        (marks, goal)
    }

    /// Wake every sleeping check of the room and run them: one pass, with everything the
    /// solve collected held.
    fn wake(&mut self) {
        for p in 0..self.models.len() {
            self.models[p].wake();
            self.mark(p);
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
        let fresh = std::mem::take(&mut self.fresh);
        if let Some(workers) = self.forward {
            return self.supersteps(workers, fresh);
        }
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

    /// [`Linked::settle`] without the room's rank order: every state with something to run goes
    /// to its own fixed point, then all their messages are handed over in player order, until
    /// nothing is left. The fixed point is the same one (the rules are monotone); the ranks are
    /// each state's own and order nothing across states, so no removal may follow on these
    /// states before a [`Linked::solve`] in rank order.
    ///
    /// A run reads and writes its own state only, so the runs of one step go on up to `workers`
    /// threads: the steps, and every counter, are the same whatever the number. Threads are
    /// started only for a step with `config::ROOM_FORWARD_MIN_PENDING` evaluations ahead: what
    /// is queued, or every rule site of the room in the first step of a `fresh` solve.
    fn supersteps(&mut self, workers: usize, fresh: bool) {
        self.ranked = false;
        // A fresh state evaluates every rule site once; later steps only what is queued.
        let mut ahead = if fresh { self.seats.iter().map(|s| s.w.site_count()).sum() } else { 0 };
        loop {
            for p in 0..self.models.len() {
                if !self.models[p].outbox.is_empty() {
                    self.deliver(p);
                }
            }
            self.outboxes.clear();
            let mut pending = 0;
            while let Some(Reverse(p)) = self.ready.pop() {
                self.queued[p] = false;
                pending += self.models[p].pending();
            }
            if pending == 0 {
                return;
            }
            let worth = pending.max(ahead) >= config::ROOM_FORWARD_MIN_PENDING;
            ahead = 0;
            for_each_mut(&mut self.models, if worth { workers } else { 1 }, |m| {
                if !m.idle() {
                    m.run();
                }
            });
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
        debug_assert!(self.ranked, "a removal after supersteps: the ranks do not order the room");
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
    rederived: u32,
    rounds: u32,
    stats: Stats,
    extend: bool,
    path: Path,
    algorithm: Algorithm,
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
    /// The attempt being made (from 1), and the groups it divided.
    attempt: u32,
    divided: u32,
    /// Per player, per item: how deep each copy first matters ([`depth::waves`]); filled by
    /// the first attempt that goes deepest first.
    depth: Vec<Vec<Vec<u32>>>,
    /// The caller wants the spheres: every check has to be evaluated by the end.
    spheres: bool,
    /// The last attempt's progression was placed by the opt-in mode, not the exact fill.
    fast: bool,
    repairs: u32,
    fallbacks: u32,
    swaps: u32,
    mode_evals: u64,
    profile: Rounds,
    /// Per player: the reached set of the round before ([`Rounds::changed`]).
    last: Vec<Vec<u64>>,
}

/// A location of a room: the player whose world it is in, and its index there.
type Spot = (u16, u32);
/// A room's placements: per player's world, per location.
type Placed = Vec<Vec<Option<Given>>>;

/// What a finished opt-in fill must show in the final solve, by construction.
struct Proof {
    /// Locations that hold a progression item that has to be collectable.
    reach: Vec<(u16, u32)>,
    /// Every player's goal holds (asked only where the mode's own solve reached them).
    goals: bool,
}

impl Proof {
    /// Whether the final solve (`models`) shows what the mode built: in release builds too,
    /// this is what stands between a bug in a mode and a seed that cannot be finished.
    ///
    /// # Errors
    /// [`FillError::Invalid`], which no retry follows.
    fn holds(&self, models: &[Reach<'_>], mode: Algorithm) -> Result<(), FillError> {
        let lost = self.reach.iter().find(|(p, l)| !models[usize::from(*p)].reached(*l));
        if lost.is_some() || (self.goals && !models.iter().all(Reach::goal)) {
            return Err(FillError::Invalid(format!(
                "{mode:?}: unreachable progression at {lost:?}, or a goal lost"
            )));
        }
        Ok(())
    }
}

impl Steps<'_, '_> {
    fn try_fill(&mut self, rng: &mut SpecRng) -> Result<Vec<Vec<Option<Given>>>, FillError> {
        let seats = self.room.seats;
        (self.rounds, self.divided) = (0, 0);
        // What the seed returned was built with: an abandoned attempt's moves are not in it.
        (self.swaps, self.repairs) = (0, 0);
        // Confined items are carried on either path: their first solve is in rank order.
        self.room.forward = None;
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
        let singles = groups(confined, false);
        let singles: Vec<&[Given]> = singles.iter().map(Vec::as_slice).collect();
        self.restrictive(&singles, &prog, &mut locs, &mut placed, &mut prev)?;
        locs.retain(|(p, l)| !seats[usize::from(p)].plan.excluded[l as usize]);
        // From here on nothing is taken back on the forward path: every round is a fresh solve.
        let forward = match self.path {
            Path::Forward { workers } => Some(workers),
            Path::Carry | Path::Split => None,
        };
        self.room.forward = forward;
        let before = self.stats.evals;
        let proof = match self.algorithm {
            Algorithm::Level(dials) => self.level(&prog, &locs, &mut placed, rng, dials)?,
            Algorithm::Shuffle(dials) => {
                // A try that gives up hands the stream back as it was, as level fill does: the
                // exact fill then places what it would have without the try.
                let drawn = rng.clone();
                let out = self.shuffle((&prog, &rest), (&free, &locs), &placed, rng, dials)?;
                if out.is_none() {
                    *rng = drawn;
                }
                out.map(|(shuffled, proof)| {
                    placed = shuffled;
                    rest.clear();
                    proof
                })
            }
            Algorithm::Exact => None,
            #[cfg(any(test, feature = "memory-harness"))]
            Algorithm::Drawn | Algorithm::Deepest | Algorithm::Repaired | Algorithm::OneItem => {
                None
            }
        };
        if self.algorithm.dials().is_some() {
            // The mode solved the room for itself: nothing is carried past it.
            prev.clear();
            self.room.forward = forward;
            self.fallbacks += u32::from(proof.is_none());
            if proof.is_none() {
                (self.swaps, self.repairs) = (0, 0);
            }
            self.mode_evals += self.stats.evals - before;
        }
        let Rule { deepest, repair, per_player } = self.algorithm.rule(self.attempt);
        self.fast = proof.is_some();
        if proof.is_none() {
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
            let groups = groups(prog, per_player);
            let mut groups: Vec<&[Given]> = groups.iter().map(Vec::as_slice).collect();
            loop {
                let before = self.seated.len() - self.moved;
                let done = if self.path == Path::Split {
                    self.split(&groups, &mut locs, &mut placed, &mut prev)
                } else {
                    self.restrictive(&groups, &[], &mut locs, &mut placed, &mut prev)
                };
                let stuck = match done {
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
        }
        scatter(&free, &mut rest, &mut placed, rng);
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
                let none = vec![Vec::new(); seats.len()];
                match self.algorithm.dials().filter(|_| proof.is_some()) {
                    None => room.solve(&none, &placed),
                    // An opt-in seed: only progression is held, and with `skip_empty` the
                    // other checks wait for one pass at the end.
                    Some(d) => {
                        let keep = !d.skip_empty;
                        room.solve_as(&none, &placed, Some(&|_, _| keep));
                        if d.skip_empty && (self.spheres || !d.goal_only) {
                            room.wake();
                        }
                    }
                }
                self.solves += 1;
            }
        }
        if let Some(proof) = proof {
            self.room.absorb(&mut self.stats);
            let held = proof.holds(&self.room.models, self.algorithm);
            debug_assert!(held.is_ok(), "an opt-in fill is not what it was built to be: {held:?}");
            held?;
        }
        Ok(placed)
    }

    /// Level fill of the progression items `prog` (none has a placement group) over the free
    /// locations `locs` that may hold progression: the order is `prog` as drawn. `None` when no
    /// order tried is feasible; nothing is placed and nothing drawn from `rng` then, so the
    /// exact fill that follows places what it would have without this try.
    fn level(
        &mut self,
        prog: &[Given],
        locs: &Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        rng: &mut SpecRng,
        dials: Dials,
    ) -> Result<Option<Proof>, FillError> {
        let items = prog.len();
        let mut order = prog.to_vec();
        self.held.iter_mut().for_each(Vec::clear);
        // Only a location an item may take needs a level: the other empty ones may sleep.
        let mut level_of: Vec<Vec<bool>> =
            self.room.seats.iter().map(|s| vec![!dials.skip_empty; s.w.locations()]).collect();
        locs.iter().for_each(|(p, l)| level_of[usize::from(p)][l as usize] = true);
        let awake = |p: usize, l: u32| level_of[p][l as usize];
        for round in 0..=config::LEVEL_FILL_MAX_REPAIRS {
            let (marks, goal) = self.room.levels(&order, &self.held, placed, &awake);
            self.solves += 1;
            self.room.absorb(&mut self.stats);
            // Goal only: what opens once every goal holds is as good as open then, and so is
            // what never opens.
            let cap = if dials.goal_only { goal } else { items };
            let mut levels = vec![Vec::new(); items];
            let models = &self.room.models;
            for (p, l) in locs.iter() {
                let rank = models[usize::from(p)].loc_rank(l);
                let v = marks.partition_point(|m| *m <= rank).min(cap);
                if v < items {
                    levels[v].push((p, l));
                }
            }
            // The counting condition is exact only while every order item may take every
            // location of `locs`: confined items were placed before and excluded locations are
            // out of the list. Per-item permissions (local items, #91) would need more.
            debug_assert!(
                order.iter().all(|g| !self.room.seats[usize::from(g.to)].plan.confined(g.item)),
                "an order item with a placement group"
            );
            if level::feasible(&levels) {
                let mut picker = level::Picker::new(levels);
                let mut reach = Vec::with_capacity(items);
                for k in (1..=items).rev() {
                    let Some((v, (p, l))) = picker.take(k, rng, dials.depth) else {
                        debug_assert!(false, "a feasible order ran out of locations at item {k}");
                        return Err(FillError::Invalid(format!("level fill: no place for {k}")));
                    };
                    placed[usize::from(p)][l as usize] = Some(order[k - 1]);
                    if v < goal || !dials.goal_only {
                        reach.push((p, l));
                    }
                }
                return Ok(Some(Proof { reach, goals: dials.goal_only && goal < items }));
            }
            if round == config::LEVEL_FILL_MAX_REPAIRS || !level::repair(&mut order, &levels) {
                break;
            }
            self.repairs += 1;
        }
        Ok(None)
    }

    /// Shuffle then unstick. `prog` (no item has a placement group) goes onto the first of
    /// `locs` (the free locations that may hold progression; both lists are shuffled, so this
    /// is a uniform draw of permitted placements) and `rest` anywhere left. One forward solve
    /// that holds progression only then runs on one thread; whenever it stops with progression
    /// out of reach, each player with an unreached item that a rule on their frontier reads
    /// (any unreached item when no frontier names one) has one such item change places with a
    /// non-progression item at a reached location, and the solve goes on. Rules are monotone
    /// and what moves out is not held, so nothing reached is lost.
    ///
    /// `None`, with `placed` untouched, when no reached location is left to move to.
    fn shuffle(
        &mut self,
        (prog, rest): (&[Given], &[Given]),
        (free, locs): (&[Spot], &Live<Spot>),
        placed: &[Vec<Option<Given>>],
        rng: &mut SpecRng,
        dials: Dials,
    ) -> Result<Option<(Placed, Proof)>, FillError> {
        let seats = self.room.seats;
        let open: Vec<(u16, u32)> = locs.iter().collect();
        if open.len() < prog.len() {
            return Ok(None);
        }
        let mut work = placed.to_vec();
        // Per owner: their progression items not yet reached, with where each lies.
        let mut lost: Vec<Vec<((u16, u32), u16)>> = vec![Vec::new(); seats.len()];
        for ((p, l), g) in open.iter().zip(prog) {
            work[usize::from(*p)][*l as usize] = Some(*g);
            lost[usize::from(g.to)].push(((*p, *l), g.item));
        }
        scatter(free, &mut rest.to_vec(), &mut work, rng);
        // Locations holding an item that may move out, where progression may go: `slot` until
        // a region of theirs is reached, then in `pool` with the round that happened in.
        let mut slot: Vec<Vec<bool>> = seats.iter().map(|s| vec![false; s.w.locations()]).collect();
        open[prog.len()..].iter().for_each(|(p, l)| slot[usize::from(*p)][*l as usize] = true);
        let mut pool: Vec<(u16, u32, u32)> = Vec::new();
        let mut seen: Vec<Vec<bool>> = seats.iter().map(|s| vec![false; s.w.regions()]).collect();
        self.held.iter_mut().for_each(Vec::clear);
        self.room.forward = None;
        let keep = !dials.skip_empty;
        self.room.solve_as(&self.held, &work, Some(&|_, _| keep));
        self.solves += 1;
        let mut wanted: Vec<usize> = Vec::new();
        let mut moves: Vec<(usize, usize)> = Vec::new();
        let mut goals;
        for round in 0u32.. {
            let models = &self.room.models;
            for (p, s) in seats.iter().enumerate() {
                for (r, seen) in seen[p].iter_mut().enumerate() {
                    if models[p].masks[r] != 0 && !std::mem::replace(seen, true) {
                        for site in s.w.group(r, CHECKS) {
                            let l = s.w.site_target(site);
                            if std::mem::take(&mut slot[p][l as usize]) {
                                pool.push((u16::try_from(p).unwrap_or(0), l, round));
                            }
                        }
                    }
                }
            }
            goals = models.iter().all(Reach::goal);
            if goals && dials.goal_only {
                break;
            }
            moves.clear();
            let mut left = 0;
            for (o, mine) in lost.iter_mut().enumerate() {
                let models = &mut self.room.models;
                mine.retain(|((p, l), _)| !models[usize::from(*p)].reached(*l));
                left += mine.len();
                // One answer per kind of item (copies share it): 2 when one more copy opens a
                // rule on the frontier now, 1 when a rule there reads it, 0 otherwise. A move
                // uses up a reached location, so the best kind there is goes first.
                let mut asked: Vec<Option<u8>> = vec![None; seats[o].w.items()];
                let mut rate = |item: u16| {
                    *asked[usize::from(item)].get_or_insert_with(|| match models[o].wants(item) {
                        true => 1 + u8::from(models[o].opens(item, false)),
                        false => 0,
                    })
                };
                let best = mine.iter().map(|x| rate(x.1)).max().filter(|b| *b > 0);
                wanted.clear();
                wanted.extend((0..mine.len()).filter(|i| Some(rate(mine[*i].1)) == best));
                if let Some(n) = rng.below(wanted.len() as u64) {
                    moves.push((o, wanted[usize::try_from(n).unwrap_or(0)]));
                }
            }
            if left == 0 {
                break;
            }
            if moves.is_empty() {
                // No frontier names an unreached item: free any one of them.
                let mut k = usize::try_from(rng.below(left as u64).unwrap_or(0)).unwrap_or(0);
                let o = lost.iter().position(|mine| {
                    let here = k < mine.len();
                    k -= if here { 0 } else { mine.len() };
                    here
                });
                moves.extend(o.map(|o| (o, k)));
            }
            for (o, i) in moves.drain(..) {
                let Some((rp, rl)) = self.target(&mut pool, rng, dials) else {
                    self.room.absorb(&mut self.stats);
                    return Ok(None);
                };
                let ((up, ul), item) = lost[o].swap_remove(i);
                let moved_out = work[usize::from(rp)][rl as usize].take();
                work[usize::from(up)][ul as usize] = moved_out;
                let freed = Given { to: u16::try_from(o).unwrap_or(0), item };
                work[usize::from(rp)][rl as usize] = Some(freed);
                // Where the item lay now holds something that may move: a place for a later one.
                let (w, p) = (seats[usize::from(up)].w, usize::from(up));
                self.room.models[p].unhold(ul, dials.skip_empty);
                let checks = w.sites_into(CHECKS, ul as usize).iter();
                if checks.map(|s| w.site_place(*s as usize).0).any(|r| seen[p][r]) {
                    pool.push((up, ul, round));
                } else {
                    slot[p][ul as usize] = true;
                }
                self.room.hold(usize::from(rp), rl, freed);
                self.swaps += 1;
            }
            self.room.settle();
        }
        self.room.absorb(&mut self.stats);
        let holds = |x: &&(u16, u32)| {
            work[usize::from(x.0)][x.1 as usize].is_some_and(|g| is_progression(seats, g))
        };
        let reach =
            if dials.goal_only { Vec::new() } else { open.iter().filter(holds).copied().collect() };
        let goals = self.room.models.iter().all(Reach::goal);
        Ok(Some((work, Proof { reach, goals })))
    }

    /// Take a reached location out of `pool` for a freed item: uniform over the reached ones
    /// (a random entry is probed until one is reached; after `config::SHUFFLE_FILL_MAX_PROBES`
    /// misses every entry is probed and one of the reached drawn). The depth dial draws more
    /// and keeps the one whose region opened latest (above 0) or earliest (below 0). `None`
    /// when no entry is reached.
    fn target(
        &mut self,
        pool: &mut Vec<(u16, u32, u32)>,
        rng: &mut SpecRng,
        dials: Dials,
    ) -> Option<(u16, u32)> {
        let want = 1 + level::draws(rng, dials.depth)?;
        let mut best: Option<usize> = None;
        let better = |new: u32, old: u32| if dials.depth > 0 { new > old } else { new < old };
        let (mut got, mut misses) = (0, 0);
        while got < want && misses < config::SHUFFLE_FILL_MAX_PROBES {
            let i = usize::try_from(rng.below(pool.len() as u64)?).ok()?;
            let (p, l, round) = pool[i];
            if self.room.with(usize::from(p), |m| m.probe(l)) {
                got += 1;
                if best.is_none_or(|b| better(round, pool[b].2)) {
                    best = Some(i);
                }
            } else {
                misses += 1;
            }
        }
        if best.is_none() {
            let reached: Vec<usize> = (0..pool.len())
                .filter(|i| self.room.with(usize::from(pool[*i].0), |m| m.probe(pool[*i].1)))
                .collect();
            let i = usize::try_from(rng.below(reached.len() as u64)?).ok()?;
            best = Some(reached[i]);
        }
        let (p, l, _) = pool.swap_remove(best?);
        Some((p, l))
    }

    /// Place each group in turn against a state that holds none of the group (but everything
    /// not yet placed outside it, and everything placed): every item goes to the first location
    /// of `locs` that is reached in that state and allows it. The state is one per group: an
    /// item of the group sees the locations the items before it took, never what they open.
    ///
    /// That state is carried from the last group's (its items held where they were put, this
    /// group's taken back), or solved afresh when the room settles forward only. A group that
    /// does not fit its state ([`Steps::fits`]) is divided: its first half is placed with the
    /// second half held too (a fresh solve), then the second half as the next group.
    fn restrictive(
        &mut self,
        groups: &[&[Given]],
        also_held: &[Given],
        locs: &mut Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        prev: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        // The groups still to place, the next one last.
        let mut todo: Vec<&[Given]> = groups.iter().rev().copied().collect();
        // The next group is the first half of a divided one: its state is not the carried one.
        let mut halved = false;
        while let Some(group) = todo.pop() {
            let carry = !prev.is_empty() && self.extend && self.room.forward.is_none();
            if carry && !halved {
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
            } else {
                self.held.iter_mut().for_each(Vec::clear);
                for g in todo.iter().copied().flatten().chain(also_held) {
                    self.held[usize::from(g.to)].push(g.item);
                }
                self.room.solve(&self.held, placed);
                self.solves += 1;
                self.room.absorb(&mut self.stats);
                self.note_reached();
            }
            halved = !self.fits(group, locs);
            if halved {
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
    /// `config::FILL_REPAIR_EVALS` rule evaluations. They run in rank order on every path, so
    /// that count is the same on every path.
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
        // A fresh solve's counters depend on how it settles, not on what came before it: in
        // rank order here whatever the path, the budget below is spent alike on all of them.
        let forward = self.room.forward.take();
        let done = self.repair(item, placed, locs);
        self.room.forward = forward;
        done
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
        // state: charged as this first solve again, the same on every path (the pass's own
        // counters depend on the path).
        let mut pass = 0;
        for k in 0..=config::FILL_DISPLACE_TRIES {
            // A budget of work, not of time: a room that needs more repair than this is stuck.
            if self.repair_evals >= self.repair_budget {
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

    /// The rounds of [`Steps::restrictive`] (`groups`, nothing else held) without a solve per
    /// round. A round's state holds every item of the later rounds and has every item of the
    /// earlier ones where it was put; so a run of rounds `a..=b` shares the state that holds
    /// neither of those for any of its rounds, and everything a round's state has beyond it
    /// is an addition: rules are monotone, so adding to a fixed point and running on gives
    /// the fixed point of the larger input, and taking the additions back
    /// ([`Reach::undo_to`]) gives the smaller one again. The room is solved once with none
    /// of the rounds' items; [`Steps::halve`] then walks the rounds in order, adding for
    /// each half what its rounds share and taking it back after.
    ///
    /// The room is left holding what it held before the rounds, and `prev` the rounds'
    /// placements: the caller adds every placement for the finished room.
    fn split(
        &mut self,
        groups: &[&[Given]],
        locs: &mut Live<(u16, u32)>,
        placed: &mut [Vec<Option<Given>>],
        prev: &mut Vec<((u16, u32), Given)>,
    ) -> Result<(), FillError> {
        if groups.is_empty() {
            return Ok(());
        }
        self.held.iter_mut().for_each(Vec::clear);
        self.room.solve(&self.held, placed);
        self.solves += 1;
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

/// The placement group rule alone; [`permits`] adds that progression takes no excluded
/// location, which is what a round is placed by.
fn confined_ok(seats: &[Seat<'_>], g: Given, player: u16, loc: u32) -> bool {
    let owner = &seats[usize::from(g.to)];
    !owner.plan.confined(g.item) || (player == g.to && owner.plan.allowed(g.item, loc))
}

/// Fill `seed` with the opt-in `mode` and hold the result against the exact fill of the same
/// seed: every placement permitted, the report a fresh solve's, the goals and the locations
/// reached the exact fill's (with full accessibility), no progression out of reach, and the
/// exact fill's own placements where the mode fell back. `None` when the exact fill is stuck on
/// this seed: nothing to hold it against.
#[cfg(test)]
pub(crate) fn assert_fast(
    seats: &[Seat<'_>],
    seed: u64,
    path: Path,
    mode: Algorithm,
) -> Option<RoomFilled> {
    let fill = |mode| {
        fill_room_by(seats, &mut SpecRng::from_u64(seed), config::FILL_MAX_ATTEMPTS, path, mode)
    };
    let (exact, _) = fill(Algorithm::Exact).ok()?;
    let (f, sp) = fill(mode).unwrap_or_else(|e| panic!("seed {seed}, {mode:?}: {e:?}"));
    let what = format!("seed {seed}, {mode:?}, fast {}", f.fast);
    assert_eq!((f.beatable.clone(), f.reached.clone()), validate_room(seats, &f.placed).unwrap());
    assert_eq!(f.beatable, exact.beatable, "{what}");
    let dials = mode.dials().expect("an opt-in mode");
    if !f.fast {
        assert_eq!(f.placed, exact.placed, "{what}: a fallback is the exact fill");
    }
    if !dials.goal_only || !f.fast {
        assert_eq!(f.reached, exact.reached, "{what}");
    }
    let progression = |g: &Given| is_progression(seats, *g);
    for (p, row) in f.placed.iter().enumerate() {
        for (l, g) in row.iter().enumerate() {
            let (p, l) = (u16::try_from(p).unwrap(), u32::try_from(l).unwrap());
            let here = seats[usize::from(p)].plan.included.contains(&l);
            assert_eq!(g.is_some(), here, "{what}: location {l} of player {p}");
            if let Some(g) = g {
                assert!(permits(seats, *g, p, l), "{what}: {g:?} at {l} of player {p}");
                let stranded = progression(g) && sp.unreachable.contains(&(p, l));
                assert!(!stranded || dials.goal_only, "{what}: {g:?} out of reach");
            }
        }
    }
    let count = |f: &RoomFilled| {
        let mut all: Vec<Given> = f.placed.iter().flatten().flatten().copied().collect();
        all.sort_unstable();
        all
    };
    assert_eq!(count(&f), count(&exact), "{what}: the same items are placed");
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::tests::world_with;
    use crate::reach::assert_same;

    /// The hand-written world (thresholds, events, a stay rule, clocks for one player), alone
    /// and as a room of three, plain and with locations excluded and items locked.
    #[test]
    fn level_fill_of_the_hand_written_worlds_is_beatable_and_reaches_what_the_exact_fill_does() {
        hand_written(Algorithm::Level, 500);
    }

    #[test]
    fn shuffle_fill_of_the_hand_written_worlds_is_beatable_and_reaches_what_the_exact_fill_does() {
        hand_written(Algorithm::Shuffle, 500);
    }

    /// `mode` over the hand-written worlds; it has to place more than `most` of the 1080 rooms
    /// itself.
    fn hand_written(mode: fn(Dials) -> Algorithm, most: u32) {
        let (mut fast, mut fell, mut repaired, mut swaps, mut as_drawn) = (0, 0, 0, 0, 0);
        for change in 0..3 {
            let built: Vec<(World, Instance)> = [false, true, false]
                .iter()
                .map(|c| {
                    let (w, mut inst) = world_with(*c);
                    if change == 1 {
                        inst.excluded = ["l1", "l5", "l17"].map(str::to_owned).to_vec();
                    }
                    if change == 2 {
                        // A key locked at the start, a coin locked deep in.
                        inst.locked.insert("l2".to_owned(), "key".to_owned());
                        inst.locked.insert("l12".to_owned(), "coin".to_owned());
                        for k in ["key", "coin"] {
                            let at = inst.pool.iter().position(|x| x == k).unwrap();
                            inst.pool.remove(at);
                        }
                    }
                    (w, inst)
                })
                .collect();
            let plans: Vec<Plan> = built.iter().map(|(w, i)| Plan::new(w, i).unwrap()).collect();
            let prog: Vec<Vec<bool>> = built.iter().map(|(w, i)| i.progression(w)).collect();
            let seats: Vec<Seat<'_>> = (0..3)
                .map(|p| Seat { w: &built[p].0, plan: &plans[p], progression: &prog[p] })
                .collect();
            for seed in 1u64..=60 {
                for room in [&seats[..], &seats[..1], &seats[1..2]] {
                    for goal_only in [false, true] {
                        let dials = Dials {
                            goal_only,
                            depth: [0, 150, -150][change],
                            skip_empty: seed % 2 == 0,
                        };
                        let f = assert_fast(room, seed, Path::Carry, mode(dials)).unwrap();
                        fast += u32::from(f.fast);
                        fell += f.fallbacks;
                        repaired += f.repairs;
                        swaps += f.swaps;
                        as_drawn += u32::from(f.fast && f.swaps == 0);
                    }
                }
            }
        }
        println!(
            "{:?}: fast {fast}, fallbacks {fell}, repairs {repaired}, swaps {swaps}, no swap {as_drawn}",
            mode(Dials::default())
        );
        assert!(fast > most, "the mode placed {fast} of 1080 rooms itself");
        assert!(repaired + swaps > 0, "no order was repaired and no item moved");
    }

    /// Leaving the checks of empty locations for later changes no placement, no report and no
    /// sphere, for either opt-in mode; it only evaluates fewer rules.
    #[test]
    fn skipping_empty_checks_gives_the_same_seed_with_fewer_evaluations() {
        let t = Three::new();
        let seats = t.seats();
        for mode in [Algorithm::Level, Algorithm::Shuffle] {
            let (mut with, mut without) = (0, 0);
            for seed in 1u64..=60 {
                let run = |skip_empty, goal_only| {
                    let d = Dials { skip_empty, goal_only, depth: 0 };
                    fill_room_by(&seats, &mut rng(seed), 20, Path::Carry, mode(d)).unwrap()
                };
                for goal_only in [false, true] {
                    let ((a, sa), (b, sb)) = (run(true, goal_only), run(false, goal_only));
                    assert_eq!(a.placed, b.placed, "seed {seed}");
                    assert_eq!((a.beatable, a.reached), (b.beatable, b.reached), "seed {seed}");
                    assert_eq!(sa, sb, "seed {seed}");
                    assert_eq!((a.fast, a.swaps, a.repairs), (b.fast, b.swaps, b.repairs));
                    with += a.stats.evals;
                    without += b.stats.evals;
                }
            }
            assert!(with < without, "{:?}: {with} against {without}", mode(Dials::default()));
            println!("{:?}: {with} evaluations skipping, {without} not", mode(Dials::default()));
        }
    }

    /// A first shuffle that is beatable is the seed, untouched: with every item held from the
    /// start no rule matters, so that fill is the plain shuffle of the same draws, and every
    /// seed that needed no swap has to equal it.
    #[test]
    fn a_shuffle_that_is_beatable_as_drawn_is_kept_as_drawn() {
        let mut as_drawn = 0;
        for clocks in [false, true] {
            let (w, inst) = world_with(clocks);
            let mut free = inst.clone();
            free.logic_start = inst.pool.iter().map(|k| (k.clone(), 9)).collect();
            let (plan, plain) = (Plan::new(&w, &inst).unwrap(), Plan::new(&w, &free).unwrap());
            let prog = inst.progression(&w);
            let mode = Algorithm::Shuffle(Dials::default());
            for seed in 1u64..=300 {
                let run = |plan| {
                    let seats = [Seat { w: &w, plan, progression: &prog }];
                    fill_room_by(&seats, &mut rng(seed), 20, Path::Carry, mode).unwrap().0
                };
                let (real, shuffled) = (run(&plan), run(&plain));
                assert_eq!((shuffled.fast, shuffled.swaps), (true, 0), "nothing gates anything");
                if real.fast && real.swaps == 0 && real.attempts == 1 {
                    assert_eq!(real.placed, shuffled.placed, "seed {seed}, clocks {clocks}");
                    as_drawn += 1;
                } else if real.fast && real.attempts == 1 {
                    assert_ne!(real.placed, shuffled.placed, "seed {seed}: a swap moved an item");
                }
            }
        }
        assert!(as_drawn > 0, "no first shuffle was beatable");
        println!("{as_drawn} of 600 first shuffles were beatable as drawn");
    }

    /// No reached location holds anything that may move out: the attempt is the exact fill's.
    #[test]
    fn a_shuffle_with_nowhere_to_move_an_item_falls_back_to_the_exact_fill() {
        let (w, mut inst) = world_with(false);
        // Only progression in the pool: nothing may ever move out of a reached location.
        inst.pool.retain(|k| k != "junk");
        inst.locations.truncate(inst.pool.len());
        let plan = Plan::new(&w, &inst).unwrap();
        let prog = inst.progression(&w);
        let seats = [Seat { w: &w, plan: &plan, progression: &prog }];
        let (mut fell, mut filled) = (0, 0);
        for seed in 1u64..=40 {
            let mode = Algorithm::Shuffle(Dials::default());
            match fill_room_by(&seats, &mut rng(seed), 20, Path::Carry, mode) {
                Ok((f, _)) => {
                    assert!(f.fast && f.swaps == 0 || f.fallbacks > 0, "seed {seed}");
                    assert_eq!((f.beatable, f.reached), validate_room(&seats, &f.placed).unwrap());
                    fell += f.fallbacks;
                    filled += 1;
                }
                Err(e) => assert!(matches!(e, FillError::Stuck { .. }), "seed {seed}: {e:?}"),
            }
        }
        assert!(fell > 0 && filled > 0, "{fell} fallbacks, {filled} filled");
    }

    /// The dial buys draws for every pick, and a fill request carries it: beyond the limit it
    /// is refused before any work, by name and at the fill's own door.
    #[test]
    fn a_depth_beyond_the_limit_is_refused() {
        let most = config::FILL_DEPTH_MAX_PERCENT;
        for ok in [most, -most, 0] {
            assert!(format!("level,depth={ok}").parse::<Algorithm>().is_ok(), "{ok}");
        }
        let t = Three::new();
        let seats = t.seats();
        for bad in [most + 1, -most - 1, i32::MAX, i32::MIN] {
            assert!(format!("shuffle,depth={bad}").parse::<Algorithm>().is_err(), "{bad}");
            for mode in [Algorithm::Level, Algorithm::Shuffle] {
                let dials = Dials { depth: bad, ..Dials::default() };
                let e = fill_room_by(&seats, &mut rng(1), 1, Path::Carry, mode(dials)).unwrap_err();
                assert!(matches!(&e, FillError::Instance(m) if m.contains("depth")), "{e:?}");
            }
        }
    }

    /// The guard behind every opt-in seed, driven into its failing branch: progression the
    /// final solve does not reach, or a goal it does not hold, is `Invalid`.
    #[test]
    fn a_seed_whose_progression_the_final_solve_misses_is_invalid() {
        let t = Three::new();
        let seats = t.seats();
        let mut room = Linked::new(&seats);
        // Nothing placed but the locked items: the start is reached, the rest is not.
        room.solve(&vec![Vec::new(); 3], &locked_placed(&seats));
        let (open, shut) = (t.worlds[0].loc("l1").unwrap(), t.worlds[0].loc("l11").unwrap());
        assert!(room.models[0].reached(open) && !room.models[0].reached(shut));
        let mode = Algorithm::Level(Dials::default());
        let holds = |reach, goals| Proof { reach, goals }.holds(&room.models, mode);
        assert_eq!(holds(vec![(0, open)], false), Ok(()));
        for (reach, goals) in [(vec![(0, shut)], false), (vec![(0, open)], true)] {
            let e = holds(reach, goals).unwrap_err();
            assert!(matches!(e, FillError::Invalid(_)), "{e:?}");
        }
    }

    /// The labelling itself, on a chain: a location's level is the number of order items held
    /// when it opens, and what only the last item opens (or nothing opens) is past every level.
    #[test]
    fn levels_are_the_number_of_order_items_held_when_a_location_opens() {
        use serde_json::json;
        let check = |l: &str, rule: serde_json::Value| json!({"loc": l, "rule": rule});
        let t = json!({"true": {}});
        let region = |key: &str, checks: serde_json::Value, exits: serde_json::Value| {
            json!({"key": key, "can_stay": false, "checks": checks, "exits": exits,
                   "events": [], "stays": []})
        };
        let m = json!({
            "options": [],
            "items": [{"key": "a", "id": 1}, {"key": "b", "id": 2}, {"key": "junk", "id": 3}],
            "locations": (0..4).map(|i| json!({"key": format!("l{i}"), "id": i + 1}))
                .collect::<Vec<_>>(),
            "start": "ra", "x.time": {"slices": 1, "start_mask": 1}, "defs": {},
            "regions": [
                region("ra", json!([check("l0", t.clone()), check("l3", json!({"has": "b"}))]),
                       json!([{"to": "rb", "rule": {"has": "a"}}])),
                region("rb", json!([check("l1", t.clone())]),
                       json!([{"to": "rc", "rule": {"has": "b"}}])),
                region("rc", json!([check("l2", t)]), json!([])),
            ],
            "goal": {"reach": "region:rb"}
        });
        let pool = ["a", "b", "junk", "junk"];
        let class = |k: &str| if k == "junk" { "filler" } else { "progression" };
        let inst = Instance {
            locations: (0..4).map(|i| format!("l{i}")).collect(),
            pool: pool.map(str::to_owned).to_vec(),
            pool_class: pool.iter().map(|k| ((*k).to_owned(), class(k).to_owned())).collect(),
            ..Instance::default()
        };
        let w = World::load(&m).unwrap();
        let plan = Plan::new(&w, &inst).unwrap();
        let prog = inst.progression(&w);
        let seats = [Seat { w: &w, plan: &plan, progression: &prog }];
        let (a, b) = (w.item("a").unwrap(), w.item("b").unwrap());
        let levels = |order: [u16; 2]| {
            let order = order.map(|item| Given { to: 0, item });
            let mut room = Linked::new(&seats);
            let (marks, goal) =
                room.levels(&order, &[Vec::new()], &locked_placed(&seats), &|_, _| true);
            let level = |l: u32| marks.partition_point(|m| *m <= room.models[0].loc_rank(l));
            ([0, 1, 2, 3].map(level), goal)
        };
        // a then b: l1 opens with a held; l2 and l3 need b, the last item, which no item can use.
        assert_eq!(levels([a, b]), ([0, 1, 2, 2], 1), "the goal (rb) holds with one item held");
        // b then a: l3 opens with b held; l1 and l2 need a, the last item.
        assert_eq!(levels([b, a]), ([0, 2, 2, 1], 2), "the goal needs the last item");
    }

    #[test]
    fn modes_are_named_with_their_dials() {
        let d = Dials::default();
        assert_eq!("exact".parse(), Ok(Algorithm::Exact));
        assert_eq!("level".parse(), Ok(Algorithm::Level(d)));
        let asked = Dials { depth: -50, goal_only: true, skip_empty: false };
        assert_eq!("shuffle,depth=-50,goal,noskip".parse(), Ok(Algorithm::Shuffle(asked)));
        for bad in ["", "fast", "exact,goal", "level,depth=x", "level,deep"] {
            assert!(bad.parse::<Algorithm>().is_err(), "{bad:?}");
        }
        assert_eq!(
            (d.depth, d.goal_only, d.skip_empty),
            (0, false, true),
            "defaults change nothing"
        );
    }

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

    /// Supersteps leave every state with ranks of its own, which truth maintenance cannot
    /// read across states: a removal after them is refused until a solve in rank order.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a removal after supersteps")]
    fn a_removal_never_follows_a_superstep() {
        let t = Three::new();
        let seats = t.seats();
        let held: Vec<Vec<u16>> = seats.iter().map(|s| s.plan.progression.clone()).collect();
        let mut room = Linked::new(&seats);
        room.forward = Some(1);
        room.solve(&held, &locked_placed(&seats));
        room.take_back(0, held[0][0]);
    }

    #[test]
    fn a_solve_in_rank_order_allows_removals_again() {
        let t = Three::new();
        let seats = t.seats();
        let held: Vec<Vec<u16>> = seats.iter().map(|s| s.plan.progression.clone()).collect();
        let placed = locked_placed(&seats);
        let mut room = Linked::new(&seats);
        room.forward = Some(1);
        room.solve(&held, &placed);
        assert!(!room.ranked);
        room.forward = None;
        room.solve(&held, &placed);
        assert!(room.ranked);
        room.take_back(0, held[0][0]);
    }

    /// The richer hand-written world (thresholds, a held-up `any`, events, clocks on for one
    /// player) fills the same on the forward path as carried, on one thread and on three.
    #[test]
    fn the_forward_path_keeps_every_placement_of_the_three_world() {
        let t = Three::new();
        let seats = t.seats();
        for seed in 1u64..=100 {
            let on = |path| {
                let (f, sp) =
                    fill_on(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS, true, path, true)
                        .unwrap();
                (f.placed, sp, f.attempts, f.rounds, f.beatable, f.reached)
            };
            let carried = on(Path::Carry);
            for workers in [1, 3] {
                assert_eq!(carried, on(Path::Forward { workers }), "seed {seed}, {workers}");
            }
        }
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

    /// What [`Path::Split`] rests on, on random rooms: adding items (held, or put at a
    /// location in any world) to a settled room and settling again gives every state the
    /// fixed point a fresh forward solve of the larger input gives, and taking the additions
    /// back gives the state it was, value for value and rank for rank, and so the fixed point
    /// of the smaller input. Random sequences of additions nested several deep and taken back
    /// in any amount, over many seeds, with and without clocks, items crossing worlds.
    #[test]
    fn adding_to_a_room_and_taking_it_back_matches_a_fresh_forward_solve() {
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
                let c = Compiled::new(&manifests, &insts, 1).unwrap();
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
                    f.forward = Some(1);
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

    /// An opt-in mode that gives up hands the attempt to the exact fill, on whatever path the
    /// room takes: on the split path the seed is the one the carried path gives, fallen back
    /// or not.
    #[test]
    fn a_mode_that_falls_back_places_the_same_on_the_split_path() {
        let (w, mut inst) = world_with(false);
        // Only progression in the pool: a shuffle can never move an item out of the way.
        inst.pool.retain(|k| k != "junk");
        inst.locations.truncate(inst.pool.len());
        let plan = Plan::new(&w, &inst).unwrap();
        let prog = inst.progression(&w);
        let seats = [Seat { w: &w, plan: &plan, progression: &prog }];
        let (mut fell, mut filled) = (0, 0);
        for seed in 1u64..=40 {
            for mode in [Algorithm::Shuffle(Dials::default()), Algorithm::Level(Dials::default())] {
                let on = |path| {
                    fill_room_by(&seats, &mut rng(seed), 20, path, mode)
                        .map(|(f, sp)| (f.placed, f.fast, f.fallbacks, f.attempts, sp))
                        .map_err(|e| format!("{e:?}"))
                };
                let split = on(Path::Split);
                assert_eq!(split, on(Path::Carry), "seed {seed}, {mode:?}");
                if let Ok(f) = split {
                    fell += f.2;
                    filled += 1;
                }
            }
        }
        assert!(fell > 0 && filled > 0, "{fell} fallbacks, {filled} filled");
    }
}
