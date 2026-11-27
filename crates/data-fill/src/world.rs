//! A world as the engine sees it: the compiled rules section and the game section's key
//! tables, read in place by offset. Loading checks every offset, index and def before a
//! solve may start, so the evaluator can trust the bytes. Authors never see this layer; the
//! manifest is compiled by [`crate::section`] and read back by [`crate::decode`].

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use bits::{BitSet, PackedRef, bytes_for, read};
use seed_bundle::package::{SECTION_GAME, SECTION_RULES, SECTION_SHUFFLES, stored};
use serde_json::Value;

use crate::bitrules::Bits;
use crate::section::{
    self, CMP_OPS, EXITS, GROUPS, HEADER, MAX_SLICES, Node, STAYS, SUBSECTIONS, VERSION, Widths,
    node, sub,
};
use crate::shuffle::Bound;

type R<T> = Result<T, String>;

/// A packed list inside the rules section: where its fields start, their width and count.
#[derive(Debug, Clone, Copy, Default)]
struct List {
    at: usize,
    width: u8,
    len: usize,
}

/// A string table: start and length of every string in its section.
#[derive(Debug, Clone, Default)]
struct Names {
    spans: Vec<(u32, u16)>,
    /// Indexes sorted by key, for lookups.
    sorted: Vec<u16>,
}

/// A list shared by every copy of a world: read like a `Vec`, never changed once loaded.
type Shared<T> = Arc<[T]>;

/// Unpacked copies of the indexes a solve reads per step. Shared between the copies of a world
/// ([`Shared`]), so a copy costs pointers and a seed's staged world owns only what it moves.
#[derive(Debug, Clone, Default)]
struct Hot {
    target: Shared<u32>,
    place: Shared<(u16, u8)>,
    starts: Shared<u32>,
    item_sites: (Shared<u32>, Shared<u32>),
    counter_sites: (Shared<u32>, Shared<u32>),
    /// Per group (exits, events, checks): sites by target, the reverse of `target`.
    into: [(Shared<u32>, Shared<u32>); 3],
    time_dep: BitSet,
    def_time: BitSet,
    def_items: BitSet,
    def_counters: BitSet,
}

/// What the loader has already checked: per def 0 unseen, 1 in progress, 2 checked and
/// state-free, 3 checked and reading state; per rule offset whether it reads state and how
/// deep it nests (defs it reaches included). `depth` is the node being checked, `peak` the
/// deepest an evaluation from the current root goes so far.
struct Memo {
    defs: Vec<u8>,
    rules: HashMap<u32, (bool, u32)>,
    depth: u32,
    peak: u32,
    /// The goal is being checked: the one rule that may read regions (`reach`).
    goal: bool,
}

/// What a rule reads, through the defs it references: items and counters as sorted sets, and
/// whether it reads time.
#[derive(Debug, Clone, Default)]
struct Reads {
    items: Vec<u16>,
    counters: Vec<u16>,
    time: bool,
}

impl Memo {
    /// An evaluation from the current root goes `deep` nodes down.
    fn reach(&mut self, deep: u32) -> R<()> {
        self.peak = self.peak.max(deep);
        if deep > config::RULES_MAX_DEPTH {
            return Err(bad("a rule nested too deep"));
        }
        Ok(())
    }
}

/// Clock ownership (clock shuffle), when the manifest models it.
#[derive(Debug, Clone, Copy)]
pub struct Owned {
    /// Option-only rule: whether clocks restrict time this seed.
    pub when: u32,
    /// Owned mask when no half-day is owned.
    pub none_owned_mask: u64,
    /// Half-day count.
    pub halves: usize,
    at: usize,
}

/// Time model (`x.time_slices@0`).
#[derive(Debug, Clone, Copy)]
pub struct Time {
    /// Mask of the start region before any expansion.
    pub start_mask: u64,
    /// All slices.
    pub all: u64,
    /// Clock ownership, if modelled.
    pub owned: Option<Owned>,
}

/// One world version, loaded from its package sections.
#[derive(Debug, Clone)]
pub struct World {
    rules: Shared<u8>,
    game: Shared<u8>,
    pub(crate) widths: Widths,
    counts: [usize; 6],
    /// Start region.
    pub start: u32,
    /// Time model.
    pub time: Time,
    /// Goal rule offset.
    pub(crate) goal: u32,
    code: Range<usize>,
    options: Range<usize>,
    masks: Range<usize>,
    region_flags: usize,
    region_starts: List,
    sites_at: usize,
    site_widths: (u8, u8),
    /// The indexes a solve walks on every step, unpacked once at load (about 70 KB here).
    hot: Hot,
    defs: List,
    item_sites: (List, List),
    counter_sites: (List, List),
    time_sites: usize,
    vanilla: List,
    roll_ids: List,
    roll_locs: List,
    names: Arc<[Names; 7]>,
    loc_ids: usize,
    /// Members of every item group (starts, items, caps + 1 with 0 for none, and weights when
    /// the section has them; every weight is 1 without).
    groups: (List, List, List, Option<List>),
    /// Per item, the groups it is a member of with its cap and weight there (computed at load).
    item_groups: (Shared<u32>, Shared<(u16, u16, u16)>),
    /// The shuffles the world declares, still to run for a seed ([`crate::shuffle::stage`]);
    /// `None` for a world that declares none, and for a seed's staged world.
    shuffles: Option<Arc<Bound>>,
    /// The rules as bit rules, where they fit (computed at load).
    bits: Arc<Bits>,
}

const ITEMS: usize = 0;
const LOCS: usize = 1;
const REGIONS: usize = 2;
const COUNTERS: usize = 3;
const DEFS: usize = 4;
const SITES: usize = 5;
const N_REGIONS: usize = 2;
const N_COUNTERS: usize = 3;
const N_DEFS: usize = 4;
const N_OPTIONS: usize = 5;
const N_ROLLS: usize = 6;

fn bad(what: &str) -> String {
    format!("rules section: {what}")
}

fn u16_at(b: &[u8], at: usize) -> R<u16> {
    b.get(at..at + 2).map(|x| u16::from_le_bytes([x[0], x[1]])).ok_or_else(|| bad("truncated"))
}

fn u32_at(b: &[u8], at: usize) -> R<u32> {
    b.get(at..at + 4)
        .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
        .ok_or_else(|| bad("truncated"))
}

fn u64_at(b: &[u8], at: usize) -> R<u64> {
    b.get(at..at + 8)
        .and_then(|x| x.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| bad("truncated"))
}

/// A packed list header at `at` (`u32 count`, `u8 width`); returns it and the end offset.
fn list(b: &[u8], at: usize, end: usize) -> R<(List, usize)> {
    let len = u32_at(b, at)? as usize;
    let width = *b.get(at + 4).ok_or_else(|| bad("truncated"))?;
    let l = List { at: at + 5, width, len };
    let stop = l.at + bytes_for(len, usize::from(width));
    if width == 0 || width > 32 || stop > end {
        return Err(bad("packed list out of bounds"));
    }
    Ok((l, stop))
}

/// A string list at `at`; returns it and the end offset.
fn strings(b: &[u8], mut at: usize, end: usize) -> R<(Names, usize)> {
    let n = u16_at(b, at)?;
    at += 2;
    let mut spans = Vec::with_capacity(usize::from(n));
    for _ in 0..n {
        let len = u16_at(b, at)?;
        let s = b.get(at + 2..at + 2 + usize::from(len)).ok_or_else(|| bad("truncated names"))?;
        std::str::from_utf8(s).map_err(|_| bad("names are not UTF-8"))?;
        spans.push((u32::try_from(at + 2).map_err(|_| bad("too large"))?, len));
        at += 2 + usize::from(len);
    }
    if at > end {
        return Err(bad("names out of bounds"));
    }
    Ok((Names { spans, sorted: Vec::new() }, at))
}

fn text<'a>(b: &'a [u8], n: &Names, i: usize) -> &'a str {
    n.spans
        .get(i)
        .and_then(|(at, len)| b.get(*at as usize..*at as usize + usize::from(*len)))
        .and_then(|s| std::str::from_utf8(s).ok())
        .unwrap_or("")
}

impl World {
    /// Compile a manifest and load it (the JSON path, once per world version).
    ///
    /// # Errors
    /// The compiler's message, naming the manifest path.
    pub fn load(m: &Value) -> R<Self> {
        Self::from_package(&section::compile(m)?)
    }

    /// Load a package file: its rules and game sections, checked.
    ///
    /// # Errors
    /// A missing section or any offset, index or def that breaks the layout rules.
    pub fn from_package(file: &[u8]) -> R<Self> {
        let section = |id| stored(file, id).map_err(|e| e.to_string());
        let rules = section(SECTION_RULES)?.ok_or("package: no rules section")?;
        let game = section(SECTION_GAME)?.ok_or("package: no game section")?;
        Self::from_parts(rules.to_vec(), game.to_vec(), section(SECTION_SHUFFLES)?)
    }

    /// Load from the sections' bytes, with the shuffle section the world was compiled with
    /// ([`crate::shuffle::section`]) when it declares shuffles: checked against the world's
    /// regions, exits and options.
    ///
    /// # Errors
    /// As [`World::from_package`]; a shuffle section that does not parse or names what the
    /// world lacks; rules that read shuffle results without their shuffle section.
    pub fn from_parts(rules: Vec<u8>, game: Vec<u8>, shuffles: Option<&[u8]>) -> R<Self> {
        let mut w = Self::loaded(rules, game)?;
        match shuffles {
            Some(bytes) => w.shuffles = Some(Arc::new(Bound::bind(&w, bytes)?)),
            // Filled as it is, every slot a rule reads would hold its first thing.
            None if (0..w.options()).any(|k| w.option_key(k).starts_with("at.")) => {
                return Err(bad("`at.` options need the shuffle section they were compiled with"));
            }
            None => {}
        }
        Ok(w)
    }

    /// The shuffles still to run for a seed of this world.
    pub(crate) fn shuffles(&self) -> Option<&Bound> {
        self.shuffles.as_deref()
    }

    /// Whether the world declares shuffles that a seed has to run before its fill.
    #[must_use]
    pub fn has_shuffles(&self) -> bool {
        self.shuffles.is_some()
    }

    /// Whether this world and `other` share one loaded form (a copy, or a seed's staged world
    /// and the world it was staged from).
    #[must_use]
    pub fn shares_with(&self, other: &World) -> bool {
        Arc::ptr_eq(&self.rules, &other.rules)
    }

    /// One seed's world: this one with every exit site of `exits` leading to its new region,
    /// and no shuffle left to run. To the solver it is an ordinary world. It shares the loaded
    /// form and owns only the targets and the exits' reverse index.
    pub(crate) fn staged(&self, exits: &[(usize, u32)]) -> World {
        let mut w = World { shuffles: None, ..self.clone() };
        if !exits.is_empty() {
            let mut target = self.hot.target.to_vec();
            for (site, to) in exits {
                if let Some(t) = target.get_mut(*site) {
                    *t = *to;
                }
            }
            w.hot.into[EXITS] = self.sites_by_target(&target, EXITS, self.counts[REGIONS]);
            w.hot.target = target.into();
        }
        w
    }

    /// Sites of group `g` by target (`n` targets), the reverse of `target`.
    fn sites_by_target(&self, target: &[u32], g: usize, n: usize) -> (Shared<u32>, Shared<u32>) {
        let mut at = vec![0u32; n + 1];
        let of_g = |s: &usize| usize::from(self.hot.place[*s].1) == g;
        let sites = || (0..target.len()).filter(of_g);
        sites().for_each(|s| at[target[s] as usize + 1] += 1);
        (0..n).for_each(|k| at[k + 1] += at[k]);
        let mut fill = at.clone();
        let mut v = vec![0u32; at[n] as usize];
        for s in sites() {
            let t = target[s] as usize;
            v[fill[t] as usize] = u32::try_from(s).unwrap_or(0);
            fill[t] += 1;
        }
        (at.into(), v.into())
    }

    /// Load from the two sections' bytes: a world that declares no shuffle. Rules that read
    /// shuffle results are refused here: without their shuffle section every slot would read
    /// as its first thing ([`World::from_parts`] loads them).
    ///
    /// # Errors
    /// As [`World::from_package`].
    pub fn from_sections(rules: Vec<u8>, game: Vec<u8>) -> R<Self> {
        Self::from_parts(rules, game, None)
    }

    /// The two sections' bytes, checked.
    #[allow(clippy::too_many_lines)] // one pass in section order
    fn loaded(rules: Vec<u8>, game: Vec<u8>) -> R<Self> {
        let b = &rules;
        if u16_at(b, 0)? != VERSION {
            return Err(bad("unknown layout version"));
        }
        let mut counts = [0usize; 6];
        for (k, c) in counts.iter_mut().enumerate() {
            *c = usize::from(u16_at(b, 4 + 2 * k)?);
        }
        let start = u32::from(u16_at(b, 16)?);
        let mut dir: [Range<usize>; SUBSECTIONS] = Default::default();
        for (k, d) in dir.iter_mut().enumerate() {
            let at = u32_at(b, HEADER + 8 * k)? as usize;
            let len = u32_at(b, HEADER + 8 * k + 4)? as usize;
            if at.checked_add(len).is_none_or(|e| e > b.len()) {
                return Err(bad("directory entry out of bounds"));
            }
            *d = at..at + len;
        }
        let widths = Widths::new(counts[ITEMS], counts[DEFS], counts[COUNTERS]);

        let (item_names, at) = strings(&game, 2 + 8 * usize::from(u16_at(&game, 0)?), game.len())?;
        let n_loc = usize::from(u16_at(&game, at)?);
        let loc_ids = at + 2;
        let (loc_names, _) = strings(&game, loc_ids + 8 * n_loc, game.len())?;
        if item_names.spans.len() != counts[ITEMS] || loc_names.spans.len() != counts[LOCS] {
            return Err(bad("item or location count differs from the game section"));
        }
        let nd = dir[sub::NAMES].clone();
        let mut names: [Names; 7] = Default::default();
        names[ITEMS] = item_names;
        names[LOCS] = loc_names;
        let mut at = nd.start;
        for n in &mut names[N_REGIONS..] {
            let (x, e) = strings(b, at, nd.end)?;
            *n = x;
            at = e;
        }
        let want = [counts[REGIONS], counts[COUNTERS], counts[DEFS]];
        if names[N_REGIONS..=N_DEFS].iter().zip(want).any(|(n, c)| n.spans.len() != c) {
            return Err(bad("name tables do not match the header counts"));
        }
        let n_opts = dir[sub::OPTIONS].len() / 4;
        if names[N_OPTIONS].spans.len() != n_opts || start as usize >= counts[REGIONS] {
            return Err(bad("options or start region out of range"));
        }

        let r = dir[sub::ROLLS].clone();
        let (roll_ids, e) = list(b, r.start, r.end)?;
        let (roll_locs, _) = list(b, e, r.end)?;
        let rg = dir[sub::REGIONS].clone();
        let region_flags = rg.start;
        let (region_starts, _) = list(b, rg.start + counts[REGIONS], rg.end)?;
        let s = dir[sub::SITES].clone();
        let site_widths = (*b.get(s.start).unwrap_or(&0), *b.get(s.start + 1).unwrap_or(&0));
        let rec = usize::from(site_widths.0) + usize::from(site_widths.1);
        if s.len() < 2 + bytes_for(counts[SITES], rec) || rec > 57 {
            return Err(bad("site records out of bounds"));
        }
        let d = dir[sub::DEFS].clone();
        let (defs, _) = list(b, d.start, d.end)?;
        let csr = |k: usize| -> R<(List, List)> {
            let x = dir[k].clone();
            let (a, e) = list(b, x.start, x.end)?;
            Ok((a, list(b, e, x.end)?.0))
        };
        let (item_sites, counter_sites) = (csr(sub::ITEM_SITES)?, csr(sub::COUNTER_SITES)?);
        let gd = dir[sub::GROUPS].clone();
        let (g_starts, e) = list(b, gd.start, gd.end)?;
        let (g_items, e) = list(b, e, gd.end)?;
        let (g_caps, e) = list(b, e, gd.end)?;
        let g_weights = (e < gd.end).then(|| list(b, e, gd.end)).transpose()?.map(|w| w.0);
        let lc = dir[sub::LOCATIONS].clone();
        let (vanilla, _) = list(b, lc.start, lc.end)?;
        let t = dir[sub::TIME].start;
        let slices = u32::from(*b.get(t).ok_or_else(|| bad("truncated time"))?);
        if slices == 0 || slices > MAX_SLICES {
            return Err(bad("slice count out of range"));
        }
        let all = if slices == 64 { u64::MAX } else { (1u64 << slices) - 1 };
        let owned = match b.get(t + 9) {
            Some(1) => {
                let halves = usize::from(*b.get(t + 22).ok_or_else(|| bad("truncated time"))?);
                if t + 23 + 12 * halves > dir[sub::TIME].end {
                    return Err(bad("time model out of bounds"));
                }
                Some(Owned {
                    when: u32_at(b, t + 10)?,
                    none_owned_mask: u64_at(b, t + 14)?,
                    halves,
                    at: t + 23,
                })
            }
            Some(0) => None,
            _ => return Err(bad("time model flag")),
        };
        let mut w = World {
            widths,
            counts,
            start,
            time: Time { start_mask: u64_at(b, t + 1)?, all, owned },
            goal: u32_at(b, dir[sub::GOAL].start)?,
            code: dir[sub::CODE].clone(),
            options: dir[sub::OPTIONS].clone(),
            masks: dir[sub::MASKS].clone(),
            region_flags,
            region_starts,
            sites_at: s.start + 2,
            site_widths,
            hot: Hot::default(),
            defs,
            item_sites,
            counter_sites,
            time_sites: dir[sub::TIME_SITES].start,
            vanilla,
            roll_ids,
            roll_locs,
            names: Arc::new(names),
            loc_ids,
            groups: (g_starts, g_items, g_caps, g_weights),
            item_groups: Default::default(),
            shuffles: None,
            bits: Arc::new(Bits::clone(&crate::bitrules::NONE)),
            game: game.into(),
            rules: Shared::default(),
        };
        w.rules = rules.into();
        w.check_groups()?;
        w.hot.starts = (0..w.region_starts.len).map(|k| w.at(w.region_starts, k)).collect();
        let mut memo = w.check()?;
        w.hot.item_sites = w.csr(w.item_sites);
        w.hot.counter_sites = w.csr(w.counter_sites);
        let mut td = BitSet::new(w.counts[SITES]);
        let tb = w.rules.get(w.time_sites..).unwrap_or_default();
        (0..w.counts[SITES]).filter(|s| read(tb, *s, 1) == 1).for_each(|s| {
            td.insert(s);
        });
        w.hot.time_dep = td;
        w.check_index(&mut memo)?;
        w.bits = Arc::new(Bits::of(&w));
        for k in [ITEMS, LOCS, N_REGIONS] {
            let mut v: Vec<u16> = (0..u16::try_from(w.names[k].spans.len()).unwrap_or(0)).collect();
            let bytes = if k == N_REGIONS { &w.rules } else { &w.game };
            v.sort_by(|x, y| {
                text(bytes, &w.names[k], usize::from(*x)).cmp(text(
                    bytes,
                    &w.names[k],
                    usize::from(*y),
                ))
            });
            Arc::make_mut(&mut w.names)[k].sorted = v;
        }
        w.pack();
        Ok(w)
    }

    /// A copy of this world that owns everything a solve reads, list after list in one run of
    /// memory, where [`Clone`] shares the lists. A room of several worlds without shuffles
    /// fills on such copies, taken one after another, so the worlds sit together as they do
    /// in a room that clones them: a fill on 100 shared worlds measured up to 4% slower than
    /// on copies (PR 207). One world for the whole room is shared as loaded, and so is a
    /// seed's staged world.
    #[must_use]
    pub fn packed(&self) -> Self {
        let mut w = self.clone();
        w.pack();
        w
    }

    /// Copy every list once more, one after another. The loader does it as its last step: it
    /// makes the lists between its own scratch work, which leaves them apart in memory (1% of
    /// a one world fill, measured, PR 207).
    fn pack(&mut self) {
        fn again<T: Clone>(x: &mut Shared<T>) {
            *x = Shared::from(&x[..]);
        }
        again(&mut self.rules);
        again(&mut self.game);
        let h = &mut self.hot;
        again(&mut h.target);
        again(&mut h.place);
        again(&mut h.starts);
        for pair in [&mut h.item_sites, &mut h.counter_sites].into_iter().chain(&mut h.into) {
            again(&mut pair.0);
            again(&mut pair.1);
        }
        self.hot = Hot {
            time_dep: h.time_dep.clone(),
            def_time: h.def_time.clone(),
            def_items: h.def_items.clone(),
            def_counters: h.def_counters.clone(),
            ..std::mem::take(h)
        };
        self.names = Arc::new((*self.names).clone());
        again(&mut self.item_groups.0);
        again(&mut self.item_groups.1);
        self.bits = Arc::new(Bits::clone(&self.bits));
    }

    /// Group member lists, and the per-item membership index built from them.
    fn check_groups(&mut self) -> R<()> {
        let (starts, items, caps, weights) = self.groups;
        let n = starts.len.saturating_sub(1);
        if starts.len == 0
            || self.at(starts, 0) != 0
            || self.at(starts, n) as usize != items.len
            || caps.len != items.len
            || weights.is_some_and(|w| w.len != items.len)
        {
            return Err(bad("item group lists"));
        }
        let mut per: Vec<Vec<(u16, u16, u16)>> = vec![Vec::new(); self.counts[ITEMS]];
        for g in 0..n {
            let (a, z) = (self.at(starts, g) as usize, self.at(starts, g + 1) as usize);
            if a > z {
                return Err(bad("item group lists are not ascending"));
            }
            if z - a > config::RULE_GROUP_MAX_MEMBERS {
                return Err(bad("item group has too many members"));
            }
            for k in a..z {
                let i = self.at(items, k) as usize;
                let cap = self.at(caps, k);
                let cap =
                    if cap == 0 { u16::MAX } else { u16::try_from(cap - 1).unwrap_or(u16::MAX) };
                let weight = self.weight(k);
                if i64::from(weight) > config::RULE_WEIGHT_MAX {
                    return Err(bad("group weight out of range"));
                }
                per.get_mut(i).ok_or_else(|| bad("group member out of range"))?.push((
                    u16::try_from(g).unwrap_or(0),
                    cap,
                    u16::try_from(weight).unwrap_or(0),
                ));
            }
        }
        let mut at = vec![0u32];
        per.iter().for_each(|p| at.push(at[at.len() - 1] + u32::try_from(p.len()).unwrap_or(0)));
        self.item_groups = (at.into(), per.concat().into());
        Ok(())
    }

    /// Item group count.
    pub(crate) fn groups(&self) -> usize {
        self.groups.0.len.saturating_sub(1)
    }

    /// Weight of group member `k` (in member order over every group): 1 without a weights list.
    fn weight(&self, k: usize) -> u32 {
        self.groups.3.map_or(1, |w| self.at(w, k))
    }

    /// Members of group `g`: item, cap (`None` for an uncapped count) and weight.
    pub(crate) fn group_members(
        &self,
        g: usize,
    ) -> impl Iterator<Item = (u16, Option<u16>, u16)> + '_ {
        let (starts, items, caps, _) = self.groups;
        (self.at(starts, g) as usize..self.at(starts, g + 1) as usize).map(move |k| {
            let cap = self.at(caps, k);
            (
                u16::try_from(self.at(items, k)).unwrap_or(0),
                cap.checked_sub(1).map(|c| u16::try_from(c).unwrap_or(0)),
                u16::try_from(self.weight(k)).unwrap_or(0),
            )
        })
    }

    /// Groups item `i` is a member of, with its cap (`u16::MAX` for none) and weight there.
    pub(crate) fn item_groups(&self, i: usize) -> &[(u16, u16, u16)] {
        let (at, m) = &self.item_groups;
        &m[at[i] as usize..at[i + 1] as usize]
    }

    /// Every layout rule a solve relies on (`docs/world-rules-section.md`, rules for writers).
    fn check(&mut self) -> R<Memo> {
        let c = self.counts;
        let mut memo =
            Memo { defs: vec![0; c[DEFS]], rules: HashMap::new(), depth: 0, peak: 0, goal: false };
        let starts = self.region_starts;
        if starts.len != GROUPS * c[REGIONS] + 1
            || self.at(starts, 0) != 0
            || self.at(starts, starts.len - 1) as usize != c[SITES]
        {
            return Err(bad("region site ranges"));
        }
        let halves = self.time.owned.map_or(0, |o| o.halves);
        let (mut targets, mut places) = (Vec::new(), Vec::new());
        for r in 0..c[REGIONS] {
            for g in 0..GROUPS {
                let span = self.group(r, g);
                if span.start > span.end {
                    return Err(bad("region site ranges are not ascending"));
                }
                for s in span {
                    let (target, rule) = self.site(s);
                    let bound =
                        [c[REGIONS], c[COUNTERS], c[LOCS], self.time.all.count_ones() as usize][g];
                    if target as usize >= bound {
                        return Err(bad("site target out of range"));
                    }
                    self.rule_ok(rule, &mut memo)?;
                    targets.push(target);
                    places.push((u16::try_from(r).unwrap_or(0), u8::try_from(g).unwrap_or(0)));
                }
            }
        }
        (self.hot.target, self.hot.place) = (targets.into(), places.into());
        for (g, n) in [c[REGIONS], c[COUNTERS], c[LOCS]].into_iter().enumerate() {
            self.hot.into[g] = self.sites_by_target(&self.hot.target, g, n);
        }
        if self.defs.len != c[DEFS] || self.vanilla.len != c[LOCS] {
            return Err(bad("def or location table length"));
        }
        for d in 0..c[DEFS] {
            self.def_ok(u16::try_from(d).unwrap_or(0), &mut memo)?;
        }
        if (0..c[LOCS]).any(|l| self.at(self.vanilla, l) as usize > c[ITEMS]) {
            return Err(bad("vanilla item out of range"));
        }
        for ((starts, list), n) in [(self.item_sites, c[ITEMS]), (self.counter_sites, c[COUNTERS])]
        {
            if starts.len != n + 1 || self.at(starts, n) as usize != list.len {
                return Err(bad("dependency index length"));
            }
            if (0..n).any(|k| self.at(starts, k) > self.at(starts, k + 1))
                || (0..list.len).any(|k| self.at(list, k) as usize >= c[SITES] + halves)
            {
                return Err(bad("dependency index out of range"));
            }
        }
        if self.rules.len() < self.time_sites + c[SITES].div_ceil(64) * 8 {
            return Err(bad("time-dependent sites out of bounds"));
        }
        if self.roll_ids.len != self.roll_locs.len
            || (0..self.roll_ids.len).any(|k| {
                self.at(self.roll_ids, k) as usize >= self.names[N_ROLLS].spans.len()
                    || self.at(self.roll_locs, k) as usize >= c[LOCS]
            })
        {
            return Err(bad("roll slots out of range"));
        }
        if !self.masks.len().is_multiple_of(8) {
            return Err(bad("mask table length"));
        }
        if let Some(o) = self.time.owned {
            if self.rule_ok(o.when, &mut memo)? {
                return Err(bad("clock condition reads state"));
            }
            for h in 0..o.halves {
                self.rule_ok(self.half(h).1, &mut memo)?;
            }
        }
        memo.goal = true;
        self.rule_ok(self.goal, &mut memo)?;
        memo.goal = false;
        Ok(memo)
    }

    /// What def `d` reads, memoised in `defs`.
    fn def_reads<'d>(&self, d: usize, memo: &mut Memo, defs: &'d mut [Option<Reads>]) -> &'d Reads {
        if defs[d].is_none() {
            let r = self.reads(self.def(d) as usize, memo, defs);
            defs[d] = Some(r);
        }
        defs[d].get_or_insert_with(Reads::default)
    }

    /// What the rule at `start` reads, through the defs it references. Runs on a checked
    /// section: defs form a DAG and nest at most `config::RULES_MAX_DEPTH`, so this ends.
    fn reads(&self, start: usize, memo: &mut Memo, defs: &mut [Option<Reads>]) -> Reads {
        let code = self.code();
        let end = self.node_ok(code, start, memo, true).map_or(start, |x| x.0);
        let (mut pc, mut out) = (start, Reads::default());
        // Prefix order: decoding node after node visits every node of the rule.
        while pc < end {
            let Some((n, at)) = node(code, pc, self.widths) else { break };
            pc = at;
            match n {
                Node::TimeAny(_) => out.time = true,
                Node::Ref(e) => {
                    let r = self.def_reads(usize::from(e), memo, defs);
                    out.items.extend_from_slice(&r.items);
                    out.counters.extend_from_slice(&r.counters);
                    out.time |= r.time;
                }
                Node::HasAny(k) => {
                    let iw = self.widths.item;
                    for j in 0..usize::from(k) {
                        let i = section::operand(code, at + j * usize::from(iw), iw).unwrap_or(0);
                        out.items.push(i);
                    }
                    pc += usize::from(k) * usize::from(iw);
                }
                Node::Has(i, _) | Node::Count(i) => out.items.push(i),
                Node::Counter(c) => out.counters.push(c),
                Node::Group(g) => out.items.extend(self.group_members(usize::from(g)).map(|m| m.0)),
                _ => {}
            }
        }
        for v in [&mut out.items, &mut out.counters] {
            v.sort_unstable();
            v.dedup();
        }
        out
    }

    /// Rebuild the dependency index (readers per item and counter, time-dependent sites) from
    /// the rules and refuse a section that ships another one: a reader missing from it would
    /// never be rechecked, a silently wrong solve. Also fills the def summaries.
    fn check_index(&mut self, memo: &mut Memo) -> R<()> {
        let c = self.counts;
        let mut defs = vec![None; c[DEFS]];
        let (mut dt, mut di, mut dc) =
            (BitSet::new(c[DEFS]), BitSet::new(c[ITEMS]), BitSet::new(c[COUNTERS]));
        for d in 0..c[DEFS] {
            let r = self.def_reads(d, memo, &mut defs);
            if r.time {
                dt.insert(d);
            }
            r.items.iter().for_each(|i| _ = di.insert(usize::from(*i)));
            r.counters.iter().for_each(|k| _ = dc.insert(usize::from(*k)));
        }
        let halves = self.time.owned.map_or(0, |o| o.halves);
        let (mut items, mut counters) = (vec![Vec::new(); c[ITEMS]], vec![Vec::new(); c[COUNTERS]]);
        let mut td = BitSet::new(c[SITES]);
        for id in 0..c[SITES] + halves {
            let rule = if id < c[SITES] { self.site(id).1 } else { self.half(id - c[SITES]).1 };
            let r = self.reads(rule as usize, memo, &mut defs);
            let site = u32::try_from(id).map_err(|_| bad("too many sites"))?;
            r.items.iter().for_each(|i| items[usize::from(*i)].push(site));
            r.counters.iter().for_each(|k| counters[usize::from(*k)].push(site));
            if r.time && id < c[SITES] {
                td.insert(id);
            }
        }
        let csr = |lists: Vec<Vec<u32>>| {
            let mut at = vec![0u32];
            lists.iter().for_each(|l| {
                at.push(at[at.len() - 1] + u32::try_from(l.len()).unwrap_or(u32::MAX))
            });
            (Shared::from(at), Shared::from(lists.concat()))
        };
        if csr(items) != self.hot.item_sites
            || csr(counters) != self.hot.counter_sites
            || td != self.hot.time_dep
        {
            return Err(bad("the dependency index does not match the rules"));
        }
        (self.hot.def_time, self.hot.def_items, self.hot.def_counters) = (dt, di, dc);
        Ok(())
    }

    /// Defs whose value does not depend on the region's time mask (a solve may cache them).
    #[inline]
    pub(crate) fn def_timeless(&self, d: usize) -> bool {
        !self.hot.def_time.contains(d)
    }

    /// Whether some def reads item `i` (a change to it can change a cached def).
    #[inline]
    pub(crate) fn def_reads_item(&self, i: usize) -> bool {
        self.hot.def_items.contains(i)
    }

    /// Whether some def reads counter `c`.
    #[inline]
    pub(crate) fn def_reads_counter(&self, c: usize) -> bool {
        self.hot.def_counters.contains(c)
    }

    /// Check the rule at `at` once (shared offsets are remembered); true when it reads state
    /// (items, counters, time, regions).
    fn rule_ok(&self, at: u32, memo: &mut Memo) -> R<bool> {
        if let Some((s, h)) = memo.rules.get(&at).copied() {
            memo.reach(memo.depth + h)?;
            return Ok(s);
        }
        let code = self.code();
        let (base, outer) = (memo.depth, memo.peak);
        memo.peak = base;
        let (end, state) = self.node_ok(code, at as usize, memo, true)?;
        if end > code.len() {
            return Err(bad("rule runs past the bytecode"));
        }
        memo.rules.insert(at, (state, memo.peak - base));
        memo.peak = memo.peak.max(outer);
        Ok(state)
    }

    /// Check def `d` once; a def met again while it is being checked reaches itself.
    fn def_ok(&self, d: u16, memo: &mut Memo) -> R<bool> {
        match memo.defs[usize::from(d)] {
            1 => return Err(bad("a def reaches itself")),
            // Checked: the rule memo answers, and counts its depth from here.
            2 | 3 => return self.rule_ok(self.def(usize::from(d)), memo),
            _ => {}
        }
        memo.defs[usize::from(d)] = 1;
        // A def is never the goal itself, so it may not read regions either.
        let goal = std::mem::replace(&mut memo.goal, false);
        let state = self.rule_ok(self.def(usize::from(d)), memo)?;
        memo.goal = goal;
        memo.defs[usize::from(d)] = 2 + u8::from(state);
        Ok(state)
    }

    /// Check the node at `pc` and everything under it; returns the offset after it and whether
    /// it reads state.
    fn node_ok(&self, code: &[u8], pc: usize, memo: &mut Memo, boolean: bool) -> R<(usize, bool)> {
        memo.depth += 1;
        memo.reach(memo.depth)?;
        let out = self.node_body_ok(code, pc, memo, boolean)?;
        memo.depth -= 1;
        Ok(out)
    }

    fn node_body_ok(
        &self,
        code: &[u8],
        pc: usize,
        memo: &mut Memo,
        boolean: bool,
    ) -> R<(usize, bool)> {
        let (n, mut at) =
            node(code, pc, self.widths).ok_or_else(|| bad("bad opcode or truncated rule"))?;
        let c = self.counts;
        let is_bool = !matches!(
            n,
            Node::Int(_)
                | Node::Count(_)
                | Node::Counter(_)
                | Node::Opt(_)
                | Node::Roll(_)
                | Node::Fold(..)
                | Node::Pair(_)
                | Node::Group(_)
        );
        if is_bool != boolean {
            return Err(bad("integer and boolean nodes mixed"));
        }
        let idx = |i: u16, n: usize| {
            if usize::from(i) < n { Ok(()) } else { Err(bad("index out of range")) }
        };
        let mut state = false;
        match n {
            Node::Const(_) | Node::Int(_) => {}
            Node::All(k, len) | Node::Any(k, len) => {
                let stop = at + usize::from(len);
                for _ in 0..k {
                    let (e, s) = self.node_ok(code, at, memo, true)?;
                    at = e;
                    state |= s;
                }
                if at != stop {
                    return Err(bad("child length mismatch"));
                }
            }
            Node::Has(i, _) | Node::Count(i) => {
                idx(i, c[ITEMS])?;
                state = true;
            }
            Node::HasAny(k) => {
                for j in 0..usize::from(k) {
                    let i = section::operand(
                        code,
                        at + j * usize::from(self.widths.item),
                        self.widths.item,
                    )
                    .ok_or_else(|| bad("truncated rule"))?;
                    idx(i, c[ITEMS])?;
                }
                at += usize::from(k) * usize::from(self.widths.item);
                state = true;
            }
            Node::Cmp(op) => {
                if usize::from(op) >= CMP_OPS.len() {
                    return Err(bad("comparison operator"));
                }
                let (e, s1) = self.node_ok(code, at, memo, false)?;
                let (e, s2) = self.node_ok(code, e, memo, false)?;
                if !section::monotone_cmp(usize::from(op), s1, s2) {
                    return Err(bad("a comparison that can turn false as state grows"));
                }
                at = e;
                state = s1 || s2;
            }
            Node::Group(g) => {
                idx(g, self.groups())?;
                state = true;
            }
            Node::TimeAny(m) => {
                idx(u16::from(m), self.masks.len() / 8)?;
                state = true;
            }
            Node::Ref(d) => {
                idx(d, c[DEFS])?;
                state = self.def_ok(d, memo)?;
            }
            Node::Reach(r) => {
                idx(r, c[REGIONS])?;
                if !memo.goal {
                    return Err(bad("`reach` outside the goal"));
                }
                state = true;
            }
            Node::Not => {
                let (e, s) = self.node_ok(code, at, memo, true)?;
                if s {
                    return Err(bad("`not` over a rule that reads state"));
                }
                at = e;
            }
            Node::Counter(k) => {
                idx(k, c[COUNTERS])?;
                state = true;
            }
            Node::Opt(o) => idx(u16::from(o), self.options.len() / 4)?,
            Node::Roll(s) => idx(s, self.roll_ids.len)?,
            Node::Fold(_, k) => {
                for _ in 0..k {
                    let (e, s) = self.node_ok(code, at, memo, false)?;
                    at = e;
                    state |= s;
                }
            }
            Node::Pair(o) => {
                let (e, s) = self.node_ok(code, at, memo, false)?;
                let divisor = node(code, e, self.widths);
                if o == section::op::DIV && !matches!(divisor, Some((Node::Int(d), _)) if d > 0) {
                    return Err(bad("division by a non-constant or non-positive divisor"));
                }
                let (e2, s2) = self.node_ok(code, e, memo, false)?;
                let literal = |pc: usize| matches!(node(code, pc, self.widths), Some((Node::Int(d), _)) if d >= 0);
                if o == section::op::MUL && ((s && !literal(e)) || (s2 && !literal(at))) {
                    return Err(bad("a product with state needs a non-negative constant factor"));
                }
                at = e2;
                state = s || s2;
            }
        }
        Ok((at, state))
    }

    fn at(&self, l: List, i: usize) -> u32 {
        PackedRef::from_bytes(self.rules.get(l.at..).unwrap_or_default(), l.width, l.len)
            .and_then(|p| p.get(i))
            .unwrap_or(0)
    }

    /// For the bench only (feature `memory-harness`): build this world's bit rules again
    /// (what a load does once) and return the bytes they take; the world is not changed.
    #[cfg(feature = "memory-harness")]
    #[must_use]
    pub fn build_bit_rules(&self) -> usize {
        Bits::of(self).bytes()
    }

    /// The rules as bit rules.
    pub(crate) fn bits(&self) -> &Bits {
        &self.bits
    }

    pub(crate) fn code(&self) -> &[u8] {
        &self.rules[self.code.clone()]
    }

    /// Site `s`: its target and rule offset.
    pub(crate) fn site(&self, s: usize) -> (u32, u32) {
        let (tw, rw) = self.site_widths;
        let bit = s * (usize::from(tw) + usize::from(rw));
        let b = self.rules.get(self.sites_at..).unwrap_or_default();
        #[allow(clippy::cast_possible_truncation)] // widths are at most 32 bits each
        (read(b, bit, tw) as u32, read(b, bit + usize::from(tw), rw) as u32)
    }

    /// Region and group of site `s`.
    #[inline]
    pub(crate) fn site_place(&self, s: usize) -> (usize, usize) {
        let (r, g) = self.hot.place[s];
        (usize::from(r), usize::from(g))
    }

    /// Target of site `s` (region, counter, location or slice by group).
    #[inline]
    pub(crate) fn site_target(&self, s: usize) -> u32 {
        self.hot.target[s]
    }

    /// Sites of region `r`, group `g` (exits, events, checks, stays).
    #[inline]
    pub(crate) fn group(&self, r: usize, g: usize) -> Range<usize> {
        let k = GROUPS * r + g;
        self.hot.starts[k] as usize..self.hot.starts[k + 1] as usize
    }

    /// Whether time can pass in region `r`.
    #[must_use]
    pub fn can_stay(&self, r: usize) -> bool {
        self.rules.get(self.region_flags + r).is_some_and(|f| f & 1 == 1)
    }

    pub(crate) fn def(&self, d: usize) -> u32 {
        self.at(self.defs, d)
    }

    pub(crate) fn mask(&self, k: usize) -> u64 {
        u64_at(&self.rules, self.masks.start + 8 * k).unwrap_or(0)
    }

    /// Half-day `h`: its slice mask and rule offset.
    pub(crate) fn half(&self, h: usize) -> (u64, u32) {
        let at = self.time.owned.map_or(0, |o| o.at) + 12 * h;
        (u64_at(&self.rules, at).unwrap_or(0), u32_at(&self.rules, at + 8).unwrap_or(1))
    }

    fn csr(&self, (starts, list): (List, List)) -> (Shared<u32>, Shared<u32>) {
        (
            (0..starts.len).map(|k| self.at(starts, k)).collect(),
            (0..list.len).map(|k| self.at(list, k)).collect(),
        )
    }

    /// Sites (and half-day sites past the region sites) whose rule reads item `i`.
    #[inline]
    pub(crate) fn item_sites(&self, i: usize) -> &[u32] {
        let (at, v) = &self.hot.item_sites;
        &v[at[i] as usize..at[i + 1] as usize]
    }

    /// Sites of group `g` (exits, events or checks) whose target is `t`: the exits into a
    /// region, the events of a counter, the checks of a location.
    #[inline]
    pub(crate) fn sites_into(&self, g: usize, t: usize) -> &[u32] {
        let (at, v) = &self.hot.into[g];
        &v[at[t] as usize..at[t + 1] as usize]
    }

    /// Sites whose rule reads counter `c`.
    #[inline]
    pub(crate) fn counter_sites(&self, c: usize) -> &[u32] {
        let (at, v) = &self.hot.counter_sites;
        &v[at[c] as usize..at[c + 1] as usize]
    }

    #[inline]
    pub(crate) fn time_dep(&self, s: usize) -> bool {
        self.hot.time_dep.contains(s)
    }

    /// Default value of option `k`.
    pub(crate) fn option_default(&self, k: usize) -> i32 {
        u32_at(&self.rules, self.options.start + 4 * k).map_or(0, u32::cast_signed)
    }

    /// Roll slot `k`: roll name and location.
    pub(crate) fn roll_slot(&self, k: usize) -> (&str, u32) {
        (self.text(N_ROLLS, self.at(self.roll_ids, k) as usize), self.at(self.roll_locs, k))
    }

    /// Roll slot count.
    pub(crate) fn roll_slots(&self) -> usize {
        self.roll_ids.len
    }

    fn text(&self, table: usize, i: usize) -> &str {
        let bytes = if table <= LOCS { &self.game } else { &self.rules };
        text(bytes, &self.names[table], i)
    }

    fn find(&self, table: usize, key: &str, what: &str) -> R<u32> {
        let t = &self.names[table];
        t.sorted
            .binary_search_by(|i| self.text(table, usize::from(*i)).cmp(key))
            .map(|k| u32::from(t.sorted[k]))
            .map_err(|_| format!("unknown {what} {key:?}"))
    }

    /// Bytes this world holds, itself and on the heap: the two sections, every index unpacked
    /// from them at load, its shuffles, and the bit rules. What a cache of loaded worlds counts. A field
    /// added to the world, its indexes or its bit rules that owns heap memory belongs here
    /// (`tests/memory.rs` fails when one is missing).
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        use crate::bitrules::vec_bytes as v;
        // A shared list or value: its two counts and what it holds.
        let shared = 2 * size_of::<usize>();
        fn s<T>(x: &Shared<T>) -> usize {
            2 * size_of::<usize>() + size_of_val(&x[..])
        }
        let h = &self.hot;
        let pairs = [&h.item_sites, &h.counter_sites, &h.into[0], &h.into[1], &h.into[2]];
        let sets = [&h.time_dep, &h.def_time, &h.def_items, &h.def_counters];
        size_of::<Self>()
            + s(&self.rules)
            + s(&self.game)
            + s(&h.target)
            + s(&h.place)
            + s(&h.starts)
            + pairs.iter().map(|(a, b)| s(a) + s(b)).sum::<usize>()
            + sets.iter().map(|s| size_of_val(s.words())).sum::<usize>()
            + shared
            + size_of_val(&*self.names)
            + self.names.iter().map(|n| v(&n.spans) + v(&n.sorted)).sum::<usize>()
            + s(&self.item_groups.0)
            + s(&self.item_groups.1)
            + self.shuffles.as_ref().map_or(0, |b| shared + b.held_bytes())
            + shared
            + size_of::<Bits>()
            + self.bits.heap_bytes()
    }

    /// Size of the rules section in bytes.
    #[must_use]
    pub fn rules_bytes(&self) -> usize {
        self.rules.len()
    }

    /// Item count.
    #[must_use]
    pub fn items(&self) -> usize {
        self.counts[ITEMS]
    }

    /// Location count.
    #[must_use]
    pub fn locations(&self) -> usize {
        self.counts[LOCS]
    }

    /// Region count.
    #[must_use]
    pub fn regions(&self) -> usize {
        self.counts[REGIONS]
    }

    /// Counter count.
    #[must_use]
    pub fn counters(&self) -> usize {
        self.counts[COUNTERS]
    }

    /// Site count (half-day dependency sites come after these).
    #[must_use]
    pub fn site_count(&self) -> usize {
        self.counts[SITES]
    }

    /// Def count.
    #[must_use]
    pub fn defs(&self) -> usize {
        self.counts[DEFS]
    }

    /// Option count.
    #[must_use]
    pub fn options(&self) -> usize {
        self.options.len() / 4
    }

    /// Roll names.
    pub fn roll_names(&self) -> impl Iterator<Item = &str> {
        (0..self.names[N_ROLLS].spans.len()).map(|k| self.text(N_ROLLS, k))
    }

    /// Item key.
    #[must_use]
    pub fn item_key(&self, i: u16) -> &str {
        self.text(ITEMS, usize::from(i))
    }

    /// Item wire id.
    #[must_use]
    pub fn item_id(&self, i: u16) -> i64 {
        u64_at(&self.game, 2 + 8 * usize::from(i)).map_or(0, u64::cast_signed)
    }

    /// Location key.
    #[must_use]
    pub fn loc_key(&self, l: u32) -> &str {
        self.text(LOCS, l as usize)
    }

    /// Location wire id.
    #[must_use]
    pub fn loc_id(&self, l: u32) -> i64 {
        u64_at(&self.game, self.loc_ids + 8 * l as usize).map_or(0, u64::cast_signed)
    }

    /// Item the location holds in the unrandomized game.
    #[must_use]
    pub fn vanilla(&self, l: u32) -> Option<u16> {
        let v = self.at(self.vanilla, l as usize) as usize;
        (v < self.items()).then(|| u16::try_from(v).unwrap_or(0))
    }

    /// Region key.
    #[must_use]
    pub fn region_key(&self, r: u32) -> &str {
        self.text(N_REGIONS, r as usize)
    }

    /// Counter key.
    #[must_use]
    pub fn counter_key(&self, c: usize) -> &str {
        self.text(N_COUNTERS, c)
    }

    /// Def key.
    #[must_use]
    pub fn def_key(&self, d: usize) -> &str {
        self.text(N_DEFS, d)
    }

    /// Option key.
    #[must_use]
    pub fn option_key(&self, k: usize) -> &str {
        self.text(N_OPTIONS, k)
    }

    /// Index of an item key.
    ///
    /// # Errors
    /// Unknown key.
    pub fn item(&self, key: &str) -> R<u16> {
        self.find(ITEMS, key, "item").map(|i| u16::try_from(i).unwrap_or(0))
    }

    /// Index of a location key.
    ///
    /// # Errors
    /// Unknown key.
    pub fn loc(&self, key: &str) -> R<u32> {
        self.find(LOCS, key, "location")
    }

    /// Index of a region key.
    ///
    /// # Errors
    /// Unknown key.
    pub fn region(&self, key: &str) -> R<u32> {
        self.find(N_REGIONS, key, "region")
    }

    /// Stay sites of region `r` (target slice, rule).
    pub(crate) fn stays(&self, r: usize) -> Range<usize> {
        self.group(r, STAYS)
    }
}
