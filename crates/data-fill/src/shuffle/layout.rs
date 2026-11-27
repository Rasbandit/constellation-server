//! The joint layout solve: every exit the `layout` shuffles of one world list, connected in
//! one growth from the start region (doc 17 section 4.5).

use std::collections::{BTreeMap, HashMap, HashSet};

use spec_rng::SpecRng;

use super::decl::Decl;
use super::draw::pick;
use super::{R, Table};
use crate::section::EXITS;
use crate::world::World;

/// One exit a `layout` shuffle lists: a slot, and the thing "arrive where it led".
#[derive(Debug)]
struct Door {
    /// Index in the declaration's exits.
    exit: usize,
    site: usize,
    region: usize,
    /// Where it leads in the unshuffled game.
    target: usize,
    shuffle: usize,
    /// Its position among its shuffle's slots, the first door of that shuffle, and the first
    /// word of that shuffle in the sets of things [`Layout::grow`] keeps (bit `local` from
    /// there is this door as a thing).
    local: usize,
    first: usize,
    word: usize,
    /// For a door side of a coupled shuffle: the far side's site and exit.
    partner: Option<(usize, usize)>,
    /// Index in [`Layout::behind`].
    behind: usize,
}

/// What the joint layout solve needs of a world, made once at load.
#[derive(Debug)]
pub(super) struct Layout {
    doors: Vec<Door>,
    /// Per region: where its exits that no shuffle moves lead.
    fixed: Vec<Vec<u16>>,
    /// Per region: the doors in it.
    doors_in: Vec<Vec<usize>>,
    /// Per distinct door region: the regions that reach it by fixed exits, itself included,
    /// ascending.
    behind: Vec<Vec<u16>>,
    /// Per region: how many regions with a door it reaches by fixed exits, and per list of
    /// `behind`: the doors in its region.
    opens: Vec<u32>,
    doors_at: Vec<u32>,
    /// Per exit of the declaration: its place among them by key.
    rank: Vec<usize>,
    /// Per region: the things that lead to it, as bits of the sets of things.
    into: Vec<Vec<usize>>,
    /// The set of things that lead to a region with a door in reach (`opens` above 0), and
    /// the words a set of things has.
    live: Vec<u64>,
}

/// One connection of a growth: the shuffle, the slot's exit and the thing's exit.
pub(super) type Pair = (usize, usize, usize);

impl Layout {
    /// The layout of `decl`'s `layout` shuffles on world `w`, whose keyed exits are at `sites`
    /// (site, region, target); `None` when no shuffle moves an exit.
    pub(super) fn new(
        w: &World,
        decl: &Decl,
        sites: &[(usize, usize, usize)],
    ) -> R<Option<Layout>> {
        let mut doors = Vec::new();
        // Sites a shuffle writes: its slots, and the far sides that follow.
        let mut moved = HashSet::new();
        let mut words = 0;
        for (shuffle, sh) in decl.shuffles.iter().enumerate().filter(|x| x.1.layout) {
            let (first, word) = (doors.len(), words);
            words += sh.exits.len().div_ceil(64);
            for (local, exit) in sh.exits.iter().copied().enumerate() {
                let (site, region, target) = sites[exit];
                let partner = decl.exits[exit].partner.filter(|_| sh.coupled);
                let partner = partner.map(|p| (sites[p].0, p));
                moved.insert(site);
                moved.extend(partner.map(|p| p.0));
                let behind = 0;
                doors.push(Door {
                    exit,
                    site,
                    region,
                    target,
                    shuffle,
                    local,
                    first,
                    word,
                    partner,
                    behind,
                });
            }
        }
        if doors.is_empty() {
            return Ok(None);
        }
        let regions = w.regions();
        let small = |r: usize| u16::try_from(r).unwrap_or(u16::MAX);
        let (mut fixed, mut back) = (vec![Vec::new(); regions], vec![Vec::new(); regions]);
        for (r, out) in fixed.iter_mut().enumerate() {
            for site in w.group(r, EXITS).filter(|site| !moved.contains(site)) {
                let to = w.site_target(site) as usize;
                out.push(small(to));
                back[to].push(r);
            }
        }
        let mut doors_in = vec![Vec::new(); regions];
        let mut behind: Vec<Vec<u16>> = Vec::new();
        let mut of_region: HashMap<usize, usize> = HashMap::new();
        let mut seen = vec![usize::MAX; regions];
        let (mut opens, mut doors_at) = (vec![0u32; regions], Vec::new());
        // Regions listed and exits walked, against one budget.
        let mut steps = 0usize;
        for (k, d) in doors.iter_mut().enumerate() {
            doors_in[d.region].push(k);
            d.behind = match of_region.get(&d.region) {
                Some(b) => *b,
                None => {
                    // Walk the fixed exits backwards from the door's region.
                    let b = behind.len();
                    let mut found = vec![d.region];
                    seen[d.region] = b;
                    let mut at = 0;
                    while let Some(r) = found.get(at).copied() {
                        steps += back[r].len();
                        for from in back[r].iter().copied() {
                            if seen[from] != b {
                                seen[from] = b;
                                found.push(from);
                            }
                        }
                        at += 1;
                    }
                    let mut found: Vec<u16> = found.into_iter().map(small).collect();
                    found.sort_unstable();
                    found.iter().for_each(|r| opens[usize::from(*r)] += 1);
                    behind.push(found);
                    doors_at.push(0);
                    of_region.insert(d.region, b);
                    b
                }
            };
            steps += behind[d.behind].len();
            if steps > config::SHUFFLE_MAX_BEHIND {
                return Err(format!(
                    "shuffles: the regions that reach a shuffled exit's region by exits that are \
                     not shuffled take more than {} steps to list",
                    config::SHUFFLE_MAX_BEHIND
                ));
            }
            doors_at[d.behind] += 1;
        }
        let (mut into, mut live) = (vec![Vec::new(); regions], vec![0u64; words]);
        for d in &doors {
            let bit = d.word * 64 + d.local;
            into[d.target].push(bit);
            live[bit / 64] |= u64::from(opens[d.target] > 0) << (bit % 64);
        }
        let mut by_key: Vec<usize> = (0..decl.exits.len()).collect();
        by_key.sort_unstable_by_key(|e| &decl.exits[*e].key);
        let mut rank = vec![0; by_key.len()];
        by_key.iter().enumerate().for_each(|(at, e)| rank[*e] = at);
        let layout = Layout { doors, fixed, doors_in, behind, opens, doors_at, rank, into, live };
        layout.listed_from_the_near_side(w, decl)?;
        Ok(Some(layout))
    }

    /// Every door is in a region the unshuffled layout reaches from the start by fixed exits
    /// and by doors walked from their listed side. A door that is not can never be connected:
    /// the growth only uses doors in regions it has placed, and the far side of a coupled
    /// door is not one. That is how a coupled door listed from its far side shows.
    fn listed_from_the_near_side(&self, w: &World, decl: &Decl) -> R<()> {
        let mut placed = vec![false; self.fixed.len()];
        let mut stack = vec![w.start as usize];
        while let Some(r) = stack.pop() {
            if std::mem::replace(&mut placed[r], true) {
                continue;
            }
            stack.extend(self.fixed[r].iter().map(|t| usize::from(*t)));
            stack.extend(self.doors_in[r].iter().map(|d| self.doors[*d].target));
        }
        let Some(d) = self.doors.iter().find(|d| !placed[d.region]) else { return Ok(()) };
        Err(format!(
            "shuffles.{}: exit {:?} is in region {:?}, which the unshuffled layout reaches from \
             the start only through the far side of a shuffled door, if at all; a coupled \
             shuffle lists the side of each door that leads away from the start",
            decl.shuffles[d.shuffle].name,
            decl.exits[d.exit].key,
            w.region_key(u32::try_from(d.region).unwrap_or(0))
        ))
    }

    /// The joint layout solve: grow from the start, each step connecting one open door in a
    /// region already placed to an allowed thing, preferring a branch that leads to a new
    /// region, then a dead end that leads to a new region, then anything allowed. Returns
    /// every exit site it wrote with its new target, and lists what it connected in `pairs`
    /// ([`Layout::record`] names them).
    ///
    /// The preference is also the look-ahead of doc 17 section 4.5 step 4: when the door
    /// being connected is the last open one, a thing that opens another door is in the first
    /// tier and is taken; when none is allowed there, no choice could go on.
    ///
    /// A door counts as usable as soon as its region is placed: whether its own rule can be
    /// met there is left to the whole-world check, which runs the real solver on the result
    /// (growing a running solve exit by exit: see #211). The far side of a coupled door
    /// follows its near side whatever its own group is.
    ///
    /// The things still to choose from are kept as three sets, a bit per thing: not yet used,
    /// leading to a region not yet placed, and leading to a region with an open door in
    /// reach. A step reads its candidates from them a word at a time, in the order of the
    /// things, so it costs the words of its shuffle and not its things.
    pub(super) fn grow(
        &self,
        decl: &Decl,
        rng: &mut SpecRng,
        start: usize,
        pairs: &mut Vec<Pair>,
    ) -> Result<Vec<(usize, u32)>, String> {
        let n = self.doors.len();
        let key = |d: &Door| decl.exits[d.exit].key.as_str();
        let mut placed = vec![false; self.fixed.len()];
        let (mut opens, mut doors_at) = (self.opens.clone(), self.doors_at.clone());
        let (mut free, mut fresh) =
            (vec![u64::MAX; self.live.len()], vec![u64::MAX; self.live.len()]);
        let mut live = self.live.clone();
        // Take the things that lead to region `r` out of a set.
        let drop = |set: &mut [u64], r: usize| {
            self.into[r].iter().for_each(|bit| set[bit / 64] &= !(1 << (bit % 64)));
        };
        // Open doors in placed regions.
        let mut open: Vec<usize> = Vec::new();
        let mut out = Vec::with_capacity(2 * n);
        let reach =
            |from: usize, placed: &mut Vec<bool>, open: &mut Vec<usize>, fresh: &mut [u64]| {
                let mut stack = vec![from];
                while let Some(r) = stack.pop() {
                    if std::mem::replace(&mut placed[r], true) {
                        continue;
                    }
                    drop(fresh, r);
                    open.extend_from_slice(&self.doors_in[r]);
                    stack.extend(self.fixed[r].iter().map(|t| usize::from(*t)));
                }
            };
        reach(start, &mut placed, &mut open, &mut fresh);
        let ones = |bits: u64| usize::try_from(bits.count_ones()).unwrap_or(0);
        for _ in 0..n {
            let Some(k) = pick(rng, open.len()) else {
                return Err("exits are left in regions the layout cannot reach".to_owned());
            };
            let x = &self.doors[open.swap_remove(k)];
            let sh = &decl.shuffles[x.shuffle];
            // The door being connected is no longer one a region opens: when it was the last
            // of its region, that region is one fewer for every region that reaches it.
            doors_at[x.behind] -= 1;
            let emptied =
                if doors_at[x.behind] == 0 { self.behind[x.behind].as_slice() } else { &[] };
            for r in emptied.iter().map(|r| usize::from(*r)) {
                opens[r] -= 1;
                if opens[r] == 0 {
                    drop(&mut live, r);
                }
            }
            // Word `i` of the allowed free things of one tier: a branch to a new region, a
            // dead end to a new region, anything.
            let tier = |i: usize, tier: usize| {
                let (allowed, at) = (sh.row(x.local, i), x.word + i);
                allowed & free[at] & [fresh[at] & live[at], fresh[at] & !live[at], !fresh[at]][tier]
            };
            let words = 0..sh.slots.len().div_ceil(64);
            let count = |t: usize| words.clone().map(|i| ones(tier(i, t))).sum::<usize>();
            let best = (0..3).find(|t| count(*t) > 0).unwrap_or(2);
            // The candidate drawn, counted in the order of the things.
            let mut skip = pick(rng, count(best));
            let local = words.clone().find_map(|i| {
                let (mut bits, k) = (tier(i, best), skip?);
                if let Some(rest) = k.checked_sub(ones(bits)) {
                    skip = Some(rest);
                    return None;
                }
                (0..k).for_each(|_| bits &= bits - 1);
                Some(i * 64 + usize::try_from(bits.trailing_zeros()).unwrap_or(0))
            });
            let Some(thing) = local.and_then(|j| self.doors.get(x.first + j)) else {
                return Err(format!("no allowed thing is left for exit {:?}", key(x)));
            };
            free[x.word + thing.local / 64] &= !(1 << (thing.local % 64));
            pairs.push((x.shuffle, x.exit, thing.exit));
            out.push((x.site, u32::try_from(thing.target).unwrap_or(0)));
            // Coupled: the far side of where this door now leads comes back here.
            if let (Some((site, far)), Some(own)) = (thing.partner, decl.exits[x.exit].partner) {
                out.push((site, u32::try_from(x.region).unwrap_or(0)));
                pairs.push((x.shuffle, far, own));
            }
            reach(thing.target, &mut placed, &mut open, &mut fresh);
        }
        Ok(out)
    }

    /// Write what a growth connected into `table`: per shuffle, slot key to thing key. The
    /// pairs are put in key order by the exits' ranks, so no key is compared to sort them.
    pub(super) fn record(&self, decl: &Decl, mut pairs: Vec<Pair>, table: &mut Table) {
        let key = |exit: usize| decl.exits[exit].key.clone();
        pairs.sort_unstable_by_key(|p| (p.0, self.rank[p.1]));
        for of_shuffle in pairs.chunk_by(|a, b| a.0 == b.0) {
            let row: BTreeMap<_, _> = of_shuffle.iter().map(|p| (key(p.1), key(p.2))).collect();
            table.entry(decl.shuffles[of_shuffle[0].0].name.clone()).or_default().extend(row);
        }
    }
}
