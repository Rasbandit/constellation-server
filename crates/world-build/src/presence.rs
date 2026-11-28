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
    let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in &m.exits {
        next.entry(from).or_default().push(to);
    }
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
            for to in next.get(r).into_iter().flatten().filter(|t| on.contains(*t)) {
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
        let gone_events: HashSet<&str> = m
            .fired
            .iter()
            .filter(|(_, rs)| !rs.iter().any(|q| on.contains(q)))
            .map(|(e, _)| *e)
            .collect();
        let gone = Gone { events: &gone_events, on: &on, regions: &pos_of, defs };
        let mut memo: HashMap<&str, bool> = HashMap::new();
        for ((pos, region, when, rule), r) in m.sites.iter().zip(&site_reads) {
            if region.is_some_and(|q| !on.contains(q)) || !ev.rule(when).unwrap_or(true) {
                continue;
            }
            let dead_events: Vec<&String> =
                r.events.iter().filter(|e| gone_events.contains(e.as_str())).collect();
            let dead_regions: Vec<&String> = r
                .regions
                .iter()
                .filter(|q| pos_of.contains_key(q.as_str()) && !on.contains(q.as_str()))
                .collect();
            // Only a read the rule cannot do without dangles: one alternative of an `or` may be
            // gone while another still holds.
            if (dead_events.is_empty() && dead_regions.is_empty()) || gone.possible(rule, &mut memo)
            {
                continue;
            }
            for e in dead_events {
                if !told.insert((*pos, e.clone())) {
                    continue;
                }
                let firing = &m.fired[e.as_str()];
                errs.err(
                    *pos,
                    format!(
                        "`{e}` is fired only in regions that do not exist with {set} ({}), and this rule cannot hold without it",
                        firing.iter().map(|q| format!("`{q}`")).collect::<Vec<_>>().join(", ")
                    ),
                    "give this the same `when`, add another way with `or`, or fire the event in a region that exists then",
                );
            }
            for q in dead_regions {
                if told.insert((*pos, q.clone())) {
                    errs.err(
                        *pos,
                        format!("this rule reaches region `{q}`, which does not exist with {set}, and cannot hold without it"),
                        "read an event its rooms fire, add another way with `or`, or drop the region's `when`",
                    );
                }
            }
        }
    }
}

/// What is gone for one option setting: events no existing region fires, and regions that do
/// not exist.
struct Gone<'g> {
    events: &'g HashSet<&'g str>,
    on: &'g HashSet<&'g str>,
    regions: &'g HashMap<&'g str, Pos>,
    defs: &'g BTreeMap<String, Value>,
}

impl<'g> Gone<'g> {
    /// Whether a lowered rule can still hold with what is gone false and everything else
    /// granted: an `or` needs one part that can, an `and` every part.
    fn possible(&self, v: &'g Value, memo: &mut HashMap<&'g str, bool>) -> bool {
        let Some((k, a)) = v.as_object().and_then(|o| o.iter().next()) else { return true };
        match k.as_str() {
            "all" => a.as_array().into_iter().flatten().all(|x| self.possible(x, memo)),
            "any" => a.as_array().into_iter().flatten().any(|x| self.possible(x, memo)),
            "ref" => {
                let Some(d) = a.as_str() else { return true };
                if let Some(b) = memo.get(d) {
                    return *b;
                }
                // A cycle is the lowering's error; it counts as possible here.
                memo.insert(d, true);
                let b = self.defs.get(d).is_none_or(|body| self.possible(body, memo));
                memo.insert(d, b);
                b
            }
            "reach" => a
                .as_str()
                .map(|r| r.strip_prefix("region:").unwrap_or(r))
                .is_none_or(|r| !self.regions.contains_key(r) || self.on.contains(r)),
            // An event's counter of 0 fails `counter(e) >= n` for any n of at least 1.
            "ge" | "gt" => match a.as_array().map(Vec::as_slice) {
                Some([Value::Object(c), n]) if c.len() == 1 => {
                    let gone = c
                        .get("counter")
                        .and_then(Value::as_str)
                        .is_some_and(|e| self.events.contains(e));
                    let need = n.as_i64().unwrap_or(0) + i64::from(k == "gt");
                    !(gone && need >= 1)
                }
                _ => true,
            },
            _ => true,
        }
    }
}
