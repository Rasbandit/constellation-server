//! The joint layout solve: every exit the `layout` shuffles of one world list, connected in
//! one growth from the start region (doc 17 section 4.5).

use std::collections::{HashMap, HashSet};

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
    /// Its position among its shuffle's slots, and the first door of that shuffle.
    local: usize,
    first: usize,
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
    /// Per region: how many doors it reaches by fixed exits.
    opens: Vec<u32>,
}

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
        for (shuffle, sh) in decl.shuffles.iter().enumerate().filter(|x| x.1.layout) {
            let first = doors.len();
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
        let mut opens = vec![0u32; regions];
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
                    behind.push(found);
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
            behind[d.behind].iter().for_each(|r| opens[usize::from(*r)] += 1);
        }
        let layout = Layout { doors, fixed, doors_in, behind, opens };
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
    /// every exit site it wrote with its new target, and records the result in `table`.
    ///
    /// The preference is also the look-ahead of doc 17 section 4.5 step 4: when the door
    /// being connected is the last open one, a thing that opens another door is in the first
    /// tier and is taken; when none is allowed there, no choice could go on.
    ///
    /// A door counts as usable as soon as its region is placed: whether its own rule can be
    /// met there is left to the whole-world check, which runs the real solver on the result
    /// (growing a running solve exit by exit: see #211). The far side of a coupled door
    /// follows its near side whatever its own group is.
    pub(super) fn grow(
        &self,
        decl: &Decl,
        rng: &mut SpecRng,
        start: usize,
        table: &mut Table,
    ) -> Result<Vec<(usize, u32)>, String> {
        let n = self.doors.len();
        let key = |d: &Door| decl.exits[d.exit].key.as_str();
        let mut placed = vec![false; self.fixed.len()];
        let mut opens = self.opens.clone();
        let mut used = vec![false; n];
        // Open doors in placed regions.
        let mut open: Vec<usize> = Vec::new();
        let mut out = Vec::with_capacity(2 * n);
        let reach = |from: usize, placed: &mut Vec<bool>, open: &mut Vec<usize>| {
            let mut stack = vec![from];
            while let Some(r) = stack.pop() {
                if std::mem::replace(&mut placed[r], true) {
                    continue;
                }
                open.extend_from_slice(&self.doors_in[r]);
                stack.extend(self.fixed[r].iter().map(|t| usize::from(*t)));
            }
        };
        reach(start, &mut placed, &mut open);
        let mut candidates = Vec::new();
        for _ in 0..n {
            let Some(k) = pick(rng, open.len()) else {
                return Err("exits are left in regions the layout cannot reach".to_owned());
            };
            let x = &self.doors[open.swap_remove(k)];
            let sh = &decl.shuffles[x.shuffle];
            let near = &self.behind[x.behind];
            let mut best = 3;
            for (t, thing) in self.doors.iter().enumerate().skip(x.first).take(sh.slots.len()) {
                if used[t] || !sh.allows(x.local, thing.local) {
                    continue;
                }
                let to = thing.target;
                // The door being connected is no longer one the new region opens.
                let own = near.binary_search(&u16::try_from(to).unwrap_or(u16::MAX)).is_ok();
                let tier = match (placed[to], opens[to] > u32::from(own)) {
                    (false, true) => 0,
                    (false, false) => 1,
                    (true, _) => 2,
                };
                if tier < best {
                    best = tier;
                    candidates.clear();
                }
                if tier == best {
                    candidates.push(t);
                }
            }
            let Some(t) = pick(rng, candidates.len()).map(|k| candidates[k]) else {
                return Err(format!("no allowed thing is left for exit {:?}", key(x)));
            };
            let thing = &self.doors[t];
            used[t] = true;
            let row = table.entry(sh.name.clone()).or_default();
            row.insert(key(x).to_owned(), key(thing).to_owned());
            out.push((x.site, u32::try_from(thing.target).unwrap_or(0)));
            // Coupled: the far side of where this door now leads comes back here.
            if let (Some((site, far)), Some(own)) = (thing.partner, decl.exits[x.exit].partner) {
                out.push((site, u32::try_from(x.region).unwrap_or(0)));
                row.insert(decl.exits[far].key.clone(), decl.exits[own].key.clone());
            }
            near.iter().for_each(|r| opens[usize::from(*r)] -= 1);
            reach(thing.target, &mut placed, &mut open);
        }
        Ok(out)
    }
}
