//! Generic shuffles, first slice (design doc 17 of the docs repo, decision D-137): a shuffle
//! assigns things to slots under a permission table that rows only narrow, and its result
//! folds into the seed's world before the item fill, which stays the exact fill and does not
//! know a shuffle ran.
//!
//! Two kinds are built. A **`none`** shuffle has slots it names itself and things that may
//! carry a rule; a rule reads the result with `{"at": [shuffle, slot]}`, which the compiler
//! ([`crate::section`]) turns into a test of a per-seed option, or into a reference to one
//! shared def per slot that lists every candidate's rule, so the per-seed fold (`crate::fold`)
//! leaves exactly the assigned thing's rule. A **`layout`** shuffle moves exits: every exit it
//! lists is a slot ("leaving through this exit") and a thing ("arriving where this exit leads
//! in the unshuffled game"). Two exits that name one `door` are its two sides; a coupled
//! shuffle lists the side that leads away from the start and the other follows, so a door
//! leads back where it was entered. An exit with no second side is one-way and trades only
//! with one-way exits.
//!
//! Per world and seed, [`stage`] runs the `none` shuffles in the order their reads give (a
//! cycle is a load error), then one joint layout solve over every `layout` shuffle, then the
//! whole-world check: holding every item of the pool, every region of the world and every
//! check that exists this seed is reached and the goal holds. A failed attempt starts all of
//! it again on the next attempt's streams, at most [`rando_limits::SHUFFLE_MAX_ATTEMPTS`] times.
//! Streams are keyed by seed, player, shuffle name and attempt, never by position, and are
//! apart from the fill's.
//!
//! This file is our own code. Its entrance model (typed groups with a table of who may
//! connect to whom, one-way and two-way exits, coupling, judging a layout with every item
//! held, and placing branches before dead ends) is informed by Archipelago's generic entrance
//! randomization as design doc 17 section 1.2 describes it, citing their `entrance_rando.py`
//! and `BaseClasses.py` at commit `4f9545f7` (read for that doc on 2026-11-26). Those
//! citations were not checked against their source again for this file: not verified here.
//!
//! Refused at load, each by name, until it is built: the world's `require` and `kinds`, a
//! shuffle's `when`, `pin`, `pool`, `kind`, `after`, `scope` and `vanilla`, counts, weights
//! and repeats, payloads other than a rule, start and item slots, `reach` shuffles, reading a
//! `layout` result, and an exit shuffle on a world with a time context (design doc 17 section
//! 9). What comes next is listed in #211.

mod decl;
mod draw;
mod layout;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use serde_json::Value;
use spec_rng::SpecRng;

pub(crate) use self::decl::Decl;
use self::draw::assign;
use self::layout::Layout;
use crate::fill::{Instance, Plan};
use crate::room::{Compiled, too_many};
use crate::section::EXITS;
use crate::world::World;

type R<T> = Result<T, String>;

/// Per shuffle, slot to thing, by name: what a seed records of one world's shuffles. For a
/// coupled door both sides are listed, the side that followed too.
pub type Table = BTreeMap<String, BTreeMap<String, String>>;

/// The stream key of the joint layout solve, and its name in the run order.
const LAYOUT: &str = "layout";

/// What a fill is told about a world whose shuffles have not run for the seed.
pub const UNSTAGED: &str = "the world declares shuffles, which have to run for the seed before \
                            a fill: stage it first (`data_fill::shuffle::stage`)";

/// The shuffle section of manifest `m` (`seed_bundle::package::SECTION_SHUFFLES`): its
/// declarations, checked, in the form a compiled world stores and [`World::from_parts`]
/// takes. `None` for a world that declares no shuffle.
///
/// # Errors
/// A message naming the manifest path of what is malformed or not supported yet.
pub fn section(m: &Value) -> R<Option<Vec<u8>>> {
    Decl::of(m)?.map(|d| d.stored()).transpose()
}

/// The order a seed runs the shuffles of `w` in, by name: each `none` shuffle after every
/// shuffle its rules read (ties by name), then `layout`, the one joint solve of every shuffle
/// that moves exits. Empty for a world without shuffles.
#[must_use]
pub fn order(w: &World) -> Vec<String> {
    w.shuffles().map_or_else(Vec::new, |b| b.decl.order_names())
}

/// A world's shuffles bound to its regions, exits and options: checked once at load, shared
/// by every seed.
#[derive(Debug)]
pub(crate) struct Bound {
    decl: Decl,
    layout: Option<Layout>,
}

impl Bound {
    /// Check a stored shuffle section against the world it came with.
    pub(crate) fn bind(w: &World, section: &[u8]) -> R<Bound> {
        // A stored name is quoted in an error at whatever length the section gives it.
        Self::checked(w, section).map_err(decl::clip)
    }

    fn checked(w: &World, section: &[u8]) -> R<Bound> {
        let decl = Decl::load(section)?;
        for option in decl.option_keys() {
            if !(0..w.options()).any(|k| w.option_key(k) == option) {
                return Err(format!("shuffle section: the rules have no option {option:?}"));
            }
        }
        // An exit passes its region's whole time mask on today; whether an exit may narrow it
        // is open (D-129), and a shuffled exit would carry the answer somewhere else.
        let slices = w.time.all.count_ones();
        if let Some(sh) = decl.shuffles.iter().find(|sh| sh.layout)
            && (slices > 1 || w.time.owned.is_some())
        {
            return Err(format!(
                "shuffles.{}: a shuffle that moves exits is not supported on a world with a \
                 time context yet (x.time has {slices} slices)",
                sh.name
            ));
        }
        // Per exit: its site, its region and where it leads.
        let mut sites = Vec::with_capacity(decl.exits.len());
        for e in &decl.exits {
            let r = w.region(&e.region).map_err(|m| format!("exit {:?}: {m}", e.key))? as usize;
            let span = w.group(r, EXITS);
            let site =
                span.start.checked_add(e.index).filter(|x| *x < span.end).ok_or_else(|| {
                    format!("exit {:?}: region {:?} has no exit {}", e.key, e.region, e.index)
                })?;
            sites.push((site, r, w.site_target(site) as usize));
        }
        let distinct: HashSet<usize> = sites.iter().map(|x| x.0).collect();
        if distinct.len() != sites.len() {
            return Err("shuffle section: two keys name one exit".to_owned());
        }
        for (e, here) in decl.exits.iter().zip(&sites) {
            let back = e.partner.and_then(|p| sites.get(p)).map(|far| (far.2, far.1));
            if back.is_some_and(|b| b != (here.1, here.2)) {
                return Err(format!(
                    "door {:?}: its two sides are not each other's way back",
                    e.door.as_deref().unwrap_or_default()
                ));
            }
        }
        let layout = Layout::new(w, &decl, &sites)?;
        Ok(Bound { decl, layout })
    }

    /// Bytes the bound shuffles hold, themselves and on the heap (`World::held_bytes`).
    pub(crate) fn held_bytes(&self) -> usize {
        size_of::<Self>()
            + self.decl.heap_bytes()
            + self.layout.as_ref().map_or(0, Layout::heap_bytes)
    }

    /// The shuffles of one level, quoted, for an error.
    fn names(&self, layout: bool) -> String {
        let names = self.decl.shuffles.iter().filter(|sh| sh.layout == layout);
        let names: Vec<String> = names.map(|sh| format!("{:?}", sh.name)).collect();
        names.join(", ") + if layout { " (layout)" } else { "" }
    }

    /// One attempt (doc 17 section 4.4, steps 1 to 3): the `none` shuffles in order, the joint
    /// layout solve, the fold, the whole-world check.
    fn attempt(
        &self,
        w: &World,
        inst: &mut Cow<'_, Instance>,
        stream: &dyn Fn(&str) -> R<SpecRng>,
    ) -> Result<Staged, Fail> {
        let mut table = Table::new();
        // Wall time of each step, for the bench only (`crate::COUNTED`).
        let (mut nanos, mut at) = ([0u64; STEPS.len()], crate::COUNTED.then(Instant::now));
        let mut lap = |step: usize| {
            if let Some(at) = &mut at {
                let now = Instant::now();
                nanos[step] += u64::try_from((now - *at).as_nanos()).unwrap_or(u64::MAX);
                *at = now;
            }
        };
        for sh in self.decl.order.iter().filter_map(|i| self.decl.shuffles.get(*i)) {
            let mut rng = stream(&sh.name).map_err(Fail::Hard)?;
            let allows = |slot: usize, thing: usize| sh.allows(slot, thing);
            let got = assign(sh.slots.len(), sh.things.len(), &allows, &mut rng)
                .ok_or_else(|| Fail::Hard(format!("shuffle {:?}: no assignment", sh.name)))?;
            let row = table.entry(sh.name.clone()).or_default();
            for ((slot, option), t) in sh.slots.iter().zip(&sh.keys).zip(got) {
                let thing = sh.things.get(t).map_or("", |x| x.name.as_str());
                row.insert(slot.clone(), thing.to_owned());
                inst.to_mut().options.insert(option.clone(), i64::try_from(t).unwrap_or(0));
            }
        }
        lap(0);
        let hard = |e| Fail::Hard(format!("{e:?}"));
        let Some(layout) = &self.layout else {
            let world = w.staged(&[]);
            lap(2);
            let plan = Plan::new(&world, inst).map_err(hard)?;
            lap(3);
            whole_world(&world, &plan).map_err(|why| Fail::Attempt(self.names(false), why))?;
            lap(4);
            return Ok(Staged { world, plan, table, attempts: 1, nanos });
        };
        let (mut rng, mut pairs) = (stream(LAYOUT).map_err(Fail::Hard)?, Vec::new());
        let exits = layout
            .grow(&self.decl, &mut rng, w.start as usize, &mut pairs)
            .map_err(|why| Fail::Attempt(self.names(true), why))?;
        lap(1);
        let world = w.staged(&exits);
        lap(2);
        let plan = Plan::new(&world, inst).map_err(hard)?;
        lap(3);
        let checked = whole_world(&world, &plan);
        lap(4);
        let Err(why) = checked else {
            // Only a layout that passed is written out by name.
            layout.record(&self.decl, pairs, &mut table);
            lap(5);
            return Ok(Staged { world, plan, table, attempts: 1, nanos });
        };
        // Is the layout at fault? The same seed with every exit where the manifest has it.
        let plain = w.staged(&[]);
        let plan = Plan::new(&plain, inst).map_err(hard)?;
        Err(match whole_world(&plain, &plan) {
            Ok(()) => Fail::Attempt(self.names(true), why),
            // Another draw of the `none` shuffles can still change what a rule reads.
            Err(also) if !self.decl.order.is_empty() => {
                Fail::Attempt(self.names(false), format!("{also}, in the unshuffled layout too"))
            }
            Err(also) => Fail::Hard(format!(
                "{also}, in the unshuffled layout too: a world with shuffles needs every region \
                 and every check that exists this seed to be reachable, and no layout can open \
                 what this seed's options close (see #211)"
            )),
        })
    }
}

/// The whole-world check (doc 17 section 4.5): holding every item still in the pool, with a
/// fixed item collected only where its check is reached, every region and every check that
/// exists this seed is reached and the goal holds. Every region of the manifest counts: the
/// engine has no way to say that a region does not exist in a seed.
fn whole_world(w: &World, plan: &Plan) -> Result<(), String> {
    let held: Vec<u16> = plan.progression.iter().chain(&plan.rest).copied().collect();
    let r = plan.state(w, &held, &plan.locked_placed(w));
    if let Some(region) = r.masks.iter().position(|m| *m == 0) {
        let region = w.region_key(u32::try_from(region).unwrap_or(0));
        return Err(format!("region {region:?} is not reached with every item held"));
    }
    if let Some(l) = plan.included.iter().find(|l| !r.reached(**l)) {
        return Err(format!("check {:?} is not reached with every item held", w.loc_key(*l)));
    }
    if !r.goal() {
        return Err("the goal does not hold with every item held".to_owned());
    }
    Ok(())
}

/// Why an attempt ended.
enum Fail {
    /// The shuffles and the reason: the next attempt may pass.
    Attempt(String, String),
    /// Nothing another attempt can change.
    Hard(String),
}

/// One world of one seed with its shuffles folded in: an ordinary world to the fill.
#[derive(Debug)]
pub struct Staged {
    /// The world with this seed's layout: the exits lead where the shuffles put them. It
    /// shares everything else with the world it was staged from.
    pub world: World,
    /// The player's instance resolved against it, with every `none` result folded into the
    /// rules.
    pub plan: Plan,
    /// What each shuffle assigned.
    pub table: Table,
    /// Attempts taken (1: no restart).
    pub attempts: u32,
    /// Wall time of the attempt that passed, nanoseconds per step of [`STEPS`]; all 0
    /// without feature `memory-harness`.
    pub nanos: [u64; STEPS.len()],
}

/// The steps of one attempt, as [`Staged::nanos`] times them: the `none` draws, the joint
/// layout solve, making the seed's world, resolving the instance on it (which a world without
/// shuffles pays too), the whole-world check, and writing the layout's table by name.
pub const STEPS: [&str; 6] = ["draws", "layout", "world", "plan", "check", "table"];

/// Run the shuffles of `w` for `player` (from 0) of the room seeded `seed`. `None` for a world
/// that declares no shuffle: nothing is drawn and the caller fills it as before. The same
/// seed and player give the same result.
///
/// # Errors
/// An instance the world lacks keys for; a region or check that is closed whatever the
/// shuffles do; or no valid result within [`rando_limits::SHUFFLE_MAX_ATTEMPTS`] attempts, where the
/// message names the shuffles, the attempts and what the last attempt failed on.
pub fn stage(w: &World, inst: &Instance, seed: u64, player: u32) -> R<Option<Staged>> {
    let Some(bound) = w.shuffles() else { return Ok(None) };
    let mut root = [0u8; 32];
    root[..8].copy_from_slice(&seed.to_le_bytes());
    let of_world = spec_rng::derive(&root, b"shuffles", player).ok_or("no random stream")?;
    let mut inst = Cow::Borrowed(inst);
    let mut last = (String::new(), String::new());
    let cap = rando_limits::SHUFFLE_MAX_ATTEMPTS;
    for attempt in 1..=cap {
        // Keyed by name and attempt, never by position (doc 17 section 4.3).
        let stream = |name: &str| {
            spec_rng::derive(&of_world, name.as_bytes(), attempt)
                .map(SpecRng::from_seed)
                .ok_or_else(|| format!("shuffle {name:?}: no random stream"))
        };
        match bound.attempt(w, &mut inst, &stream) {
            Ok(staged) => return Ok(Some(Staged { attempts: attempt, ..staged })),
            Err(Fail::Attempt(what, why)) => last = (what, why),
            Err(Fail::Hard(e)) => return Err(e),
        }
    }
    Err(format!("shuffle {}: no valid result in {cap} attempts (the last: {})", last.0, last.1))
}

/// The world of `w` with every exit where the manifest has it, resolved for `inst`: what a
/// seed's staged world is measured against (the same fill, on the layout nothing moved).
/// `None` for a world that declares no shuffle.
///
/// # Errors
/// A world with a `none` shuffle, whose rules read a draw and have no unshuffled form, or an
/// instance the world lacks keys for.
pub fn unshuffled(w: &World, inst: &Instance) -> R<Option<Staged>> {
    let Some(bound) = w.shuffles() else { return Ok(None) };
    if !bound.decl.order.is_empty() {
        return Err(format!(
            "shuffle {}: rules read its draw, so it has no unshuffled form",
            bound.names(false)
        ));
    }
    let world = w.staged(&[]);
    let plan = Plan::new(&world, inst)?;
    Ok(Some(Staged { world, plan, table: Table::new(), attempts: 0, nanos: [0; STEPS.len()] }))
}

/// A room staged: per player their world (player `p` plays
/// `worlds[p % worlds.len()]`, one each or one for all, as [`Compiled`] holds them) with its
/// shuffles run and its plan, and each player's table. A world without shuffles is resolved
/// as it is. Every player has their own streams, and the first error by player wins.
///
/// # Errors
/// As [`stage`] and [`Plan::new`], naming the player.
pub fn room(worlds: &[&World], insts: &[&Instance], seed: u64) -> R<(Compiled, Vec<Table>)> {
    too_many(insts.len())?;
    let players: Vec<usize> = (0..insts.len()).collect();
    let staged = players.iter().map(|p| -> R<Staged> {
        let who = |e: String| crate::room::player(*p, e);
        let w = worlds.get(p % worlds.len().max(1)).ok_or("a room needs a world")?;
        let player = u32::try_from(*p).map_err(|e| e.to_string())?;
        Ok(match stage(w, insts[*p], seed, player).map_err(who)? {
            Some(staged) => staged,
            None => Staged {
                plan: Plan::new(w, insts[*p]).map_err(|e| who(format!("{e:?}")))?,
                world: (*w).clone(),
                table: Table::new(),
                attempts: 0,
                nanos: [0; STEPS.len()],
            },
        })
    });
    let mut c = Compiled { worlds: Vec::new(), plans: Vec::new(), progression: Vec::new() };
    let mut tables = Vec::with_capacity(insts.len());
    for (staged, inst) in staged.into_iter().zip(insts) {
        let staged = staged?;
        c.progression.push(inst.progression(&staged.world));
        c.worlds.push(staged.world);
        c.plans.push(staged.plan);
        tables.push(staged.table);
    }
    Ok((c, tables))
}
