//! Which regions exist for each option setting, and the references a region `when` would
//! leave dangling. A region whose `when` is false does not exist that seed: its checks, events
//! and exits are gone (the lowering joins its condition to each). The build then tries every
//! setting of the options the region conditions read (the pool test's settings) and reports,
//! as errors with the setting named, what would make every such seed fail in the engine: a
//! region left with no way in, an event read where none of the regions that fire it exist,
//! and a `reach` of a region that is gone.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::Value;

use crate::instance::{Ev, options_read, setting_text, settings, values};
use crate::lower::truth;
use crate::model::Pos;
use crate::world::{Errs, OptM};

/// The world's map as the presence check reads it, all rules already lowered.
pub(crate) struct Map<'a> {
    pub start: &'a str,
    /// Each region: key, place, options-only condition.
    pub regions: Vec<(&'a str, Pos, Value)>,
    /// Each exit: from and to, rules aside.
    pub exits: Vec<(&'a str, &'a str)>,
    /// Each event and the regions that fire it.
    pub fired: HashMap<&'a str, Vec<&'a str>>,
    /// Each rule that may read an event or reach a region: its place, the region it is in
    /// (none for the goal), its own options-only condition, and the lowered rule.
    pub sites: Vec<(Pos, Option<&'a str>, Value, Value)>,
}

/// What a lowered rule reads that a region condition can remove: events and reached regions.
#[derive(Default, Clone)]
struct Reads {
    events: BTreeSet<String>,
    regions: BTreeSet<String>,
}

fn reads(v: &Value, defs: &BTreeMap<String, Value>, memo: &mut HashMap<String, Reads>) -> Reads {
    let mut out = Reads::default();
    let mut todo = vec![v];
    while let Some(v) = todo.pop() {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    match (k.as_str(), x.as_str()) {
                        ("counter", Some(e)) => {
                            out.events.insert(e.to_owned());
                        }
                        ("reach", Some(r)) => {
                            out.regions.insert(r.strip_prefix("region:").unwrap_or(r).to_owned());
                        }
                        ("ref", Some(d)) => {
                            if !memo.contains_key(d) {
                                // A cycle is the lowering's error; an empty entry stops it here.
                                memo.insert(d.to_owned(), Reads::default());
                                let r =
                                    defs.get(d).map(|b| reads(b, defs, memo)).unwrap_or_default();
                                memo.insert(d.to_owned(), r);
                            }
                            let r = &memo[d];
                            out.events.extend(r.events.iter().cloned());
                            out.regions.extend(r.regions.iter().cloned());
                        }
                        _ => todo.push(x),
                    }
                }
            }
            Value::Array(a) => todo.extend(a),
            _ => {}
        }
    }
    out
}

/// Report what a region `when` leaves dangling, for every setting of the options it reads.
pub(crate) fn check(
    m: &Map<'_>,
    options: &[OptM],
    defs: &BTreeMap<String, Value>,
    errs: &mut Errs,
) {
    if m.regions.iter().all(|r| r.2 == truth()) {
        return;
    }
    let keys = options_read(m.regions.iter().map(|r| &r.2).collect(), defs);
    let (all, partly) = settings(options, &keys);
    if partly {
        errs.warn_whole(
            format!(
                "the region conditions were tried with each option alone at its two ends: over {} combinations",
                config::WORLD_POOL_TEST_MAX_COMBINATIONS
            ),
            "generation still refuses a seed whose options leave a region unreachable",
        );
    }
    let mut memo = HashMap::new();
    let site_reads: Vec<Reads> = m.sites.iter().map(|s| reads(&s.3, defs, &mut memo)).collect();
    let pos_of: HashMap<&str, Pos> = m.regions.iter().map(|r| (r.0, r.1)).collect();
    let mut told: HashSet<(Pos, String)> = HashSet::new();
    for given in &all {
        let o = values(options, given);
        let mut ev = Ev::new(&o, defs);
        let on: HashSet<&str> =
            m.regions.iter().filter(|r| ev.rule(&r.2).unwrap_or(true)).map(|r| r.0).collect();
        let set = setting_text(options, given);
        // Rules aside, every region that exists has a way in from the start through regions
        // that exist too.
        let mut seen = HashSet::from([m.start]);
        let mut todo = vec![m.start];
        while let Some(r) = todo.pop() {
            for (_, to) in m.exits.iter().filter(|x| x.0 == r && on.contains(x.1)) {
                if seen.insert(to) {
                    todo.push(to);
                }
            }
        }
        for (r, pos, _) in m.regions.iter().filter(|r| on.contains(r.0) && !seen.contains(r.0)) {
            if told.insert((*pos, String::new())) {
                errs.err(
                    *pos,
                    format!("region `{r}` cannot be reached with {set}: every way in from the start passes through a region that does not exist then"),
                    "give it the same `when`, or add an exit into it from a region that exists then",
                );
            }
        }
        for ((pos, region, when, _), r) in m.sites.iter().zip(&site_reads) {
            if region.is_some_and(|q| !on.contains(q)) || !ev.rule(when).unwrap_or(true) {
                continue;
            }
            for e in &r.events {
                let Some(firing) = m.fired.get(e.as_str()) else { continue };
                if firing.iter().any(|q| on.contains(q)) || !told.insert((*pos, e.clone())) {
                    continue;
                }
                errs.err(
                    *pos,
                    format!(
                        "`{e}` is fired only in regions that do not exist with {set} ({}), and this rule reads it",
                        firing.iter().map(|q| format!("`{q}`")).collect::<Vec<_>>().join(", ")
                    ),
                    "give this the same `when`, or fire the event in a region that exists then",
                );
            }
            for q in r.regions.iter().filter(|q| pos_of.contains_key(q.as_str())) {
                if !on.contains(q.as_str()) && told.insert((*pos, q.clone())) {
                    errs.err(
                        *pos,
                        format!("this rule reaches region `{q}`, which does not exist with {set}"),
                        "read an event its rooms fire, or drop the region's `when`",
                    );
                }
            }
        }
    }
}
