//! The per-seed instance of a built world (design doc 16 section 4): options settled to
//! numbers, then which checks exist, the pool, start items, fixed placements and placement
//! sets, in the engine's instance form. Also the build-time pool test of section 3.4.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

use crate::lower::OptTy;
use crate::world::{CheckM, Errs, GrantM, Model, NOTHING, OptM};

/// An options-only integer expression's value (the build lets nothing else into these).
fn int(v: &Value, o: &HashMap<&str, i64>) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(k) = v.get("opt").and_then(Value::as_str) {
        return o.get(k).copied();
    }
    let a = v.get("int")?.as_array()?;
    let (op, args) = a.split_first()?;
    let xs: Vec<i64> = args.iter().map(|x| int(x, o)).collect::<Option<_>>()?;
    match op.as_str()? {
        "+" => xs.iter().try_fold(0i64, |s, x| s.checked_add(*x)),
        "min" => xs.into_iter().min(),
        "max" => xs.into_iter().max(),
        "*" => xs.first()?.checked_mul(*xs.get(1)?),
        "div" => xs.first()?.checked_div_euclid(*xs.get(1)?),
        _ => None,
    }
}

/// Options-only rules evaluated for one set of option values, each def once.
struct Ev<'m> {
    o: &'m HashMap<&'m str, i64>,
    defs: &'m BTreeMap<String, Value>,
    memo: HashMap<&'m str, bool>,
}

impl<'m> Ev<'m> {
    /// An options-only rule's value. Recursion follows the rule's nesting, defs included,
    /// which the build bounds.
    fn rule(&mut self, v: &'m Value) -> Option<bool> {
        let (k, a) = v.as_object()?.iter().next()?;
        Some(match k.as_str() {
            "true" => true,
            "false" => false,
            "all" | "any" => {
                let mut out = k == "all";
                for x in a.as_array()? {
                    let b = self.rule(x)?;
                    out = if k == "all" { out && b } else { out || b };
                }
                out
            }
            "not" => !self.rule(a)?,
            "ref" => {
                let name = a.as_str()?;
                if let Some(b) = self.memo.get(name) {
                    return Some(*b);
                }
                let b = self.rule(self.defs.get(name)?)?;
                self.memo.insert(name, b);
                b
            }
            op => {
                let ix = rule_ops::CMP_OPS.iter().position(|x| *x == op)?;
                let [x, y] = a.as_array()?.as_slice() else { return None };
                rule_ops::compare(u8::try_from(ix).ok()?, int(x, self.o)?, int(y, self.o)?)
            }
        })
    }
}

/// `free` slots split by weight, largest remainders first, ties by key: no random draw.
fn split(free: i64, weights: &[(String, i64)]) -> Vec<(String, i64)> {
    let total: i64 = weights.iter().map(|w| w.1).sum();
    let mut out: Vec<(String, i64, i64)> =
        weights.iter().map(|(k, w)| (k.clone(), free * w / total, free * w % total)).collect();
    let left = free - out.iter().map(|x| x.1).sum::<i64>();
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by(|a, b| out[*b].2.cmp(&out[*a].2).then(a.cmp(b)));
    for k in order.into_iter().take(usize::try_from(left).unwrap_or(0)) {
        out[k].1 += 1;
    }
    out.into_iter().map(|(k, n, _)| (k, n)).collect()
}

/// One seed's settled world: options to numbers, and what exists, is fixed, granted and
/// placed, counted and checked before any copy is written out.
struct Settled<'m> {
    o: HashMap<&'m str, i64>,
    included: Vec<&'m CheckM>,
    excluded: Vec<&'m str>,
    granted: Vec<&'m GrantM>,
    assumed: Vec<&'m GrantM>,
    taken: HashMap<&'m str, i64>,
    placed: i64,
    slots: i64,
}

/// Settle `m` for `given` option values.
fn settle<'m>(m: &'m Model, given: &HashMap<String, i64>) -> Result<Settled<'m>, String> {
    let mut o: HashMap<&str, i64> = HashMap::new();
    for opt in &m.options {
        let v = given.get(&opt.key).copied().unwrap_or(opt.default);
        let (lo, hi) = bounds(&opt.ty);
        if !(lo..=hi).contains(&v) {
            return Err(format!("option {:?}: {v} is outside {lo} to {hi}", opt.key));
        }
        o.insert(&opt.key, v);
    }
    if let Some(k) = given.keys().find(|k| !o.contains_key(k.as_str())) {
        return Err(format!("unknown option {k:?}"));
    }
    let (mut included, mut excluded, mut granted, mut assumed) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut taken: HashMap<&str, i64> = HashMap::new();
    {
        let mut ev = Ev { o: &o, defs: &m.defs, memo: HashMap::new() };
        let mut holds =
            |v| ev.rule(v).ok_or_else(|| format!("a rule here reads more than options: {v}"));
        for c in &m.checks {
            if !holds(&c.when)? {
                continue;
            }
            included.push(c);
            if holds(&c.no_progression)? {
                excluded.push(c.key.as_str());
            }
            if let Some(i) = &c.holds {
                *taken.entry(i).or_default() += 1;
            }
        }
        for g in &m.start_items {
            if holds(&g.when)? {
                *taken.entry(&g.item).or_default() += g.n;
                granted.push(g);
            }
        }
        for g in &m.assumed {
            if holds(&g.when)? {
                assumed.push(g);
            }
        }
    }
    let mut placed = 0i64;
    for (key, _, count) in &m.items {
        let n = count - taken.get(key.as_str()).copied().unwrap_or(0);
        if n < 0 {
            return Err(format!(
                "item {key:?} has {count} copies, and {} are fixed or start items",
                count - n
            ));
        }
        placed += n;
    }
    let locked = i64::try_from(included.iter().filter(|c| c.holds.is_some()).count()).unwrap_or(0);
    let slots = i64::try_from(included.len()).unwrap_or(i64::MAX) - locked;
    if placed > slots {
        return Err(format!("{placed} items to place and {slots} checks to hold them"));
    }
    let start: i64 = granted.iter().map(|g| g.n).sum();
    let most = config::WORLD_START_COPIES_MAX;
    if start > i64::try_from(most).unwrap_or(i64::MAX) {
        return Err(format!("{start} start item copies, at most {most}"));
    }
    Ok(Settled { o, included, excluded, granted, assumed, taken, placed, slots })
}

/// The per-seed instance of `m` for `given` option values, in the engine's instance shape
/// (`data_fill::Instance`, the player file's `instance`), as JSON: the tool writes it with or
/// without the engine.
pub(crate) fn of(m: &Model, given: &HashMap<String, i64>) -> Result<Value, String> {
    let s = settle(m, given)?;
    let (mut locked, mut location_groups) = (BTreeMap::new(), BTreeMap::new());
    for c in &s.included {
        if let Some(i) = &c.holds {
            locked.insert(c.key.clone(), i.clone());
        }
        if let Some(set) = &c.set {
            location_groups.insert(c.key.clone(), set.clone());
        }
    }
    let start: Vec<&str> =
        s.granted.iter().flat_map(|g| (0..g.n).map(|_| g.item.as_str())).collect();
    let mut logic: BTreeMap<&str, i64> = BTreeMap::new();
    for g in &s.assumed {
        *logic.entry(&g.item).or_default() += g.n;
    }
    let mut pool: Vec<&str> = Vec::new();
    let mut classes: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, class, count) in &m.items {
        let n = count - s.taken.get(key.as_str()).copied().unwrap_or(0);
        pool.extend((0..n).map(|_| key.as_str()));
        classes.insert(key, class);
    }
    let fill = split(s.slots - s.placed, &m.fill_with);
    for (k, n) in &fill {
        pool.extend((0..*n).map(|_| k.as_str()));
    }
    pool.sort_unstable();
    classes.entry(NOTHING).or_insert("filler");
    let options: BTreeMap<&str, i64> = s.o.iter().map(|(k, v)| (*k, *v)).collect();
    Ok(json!({
        "excluded": s.excluded,
        "item_groups": m.confined,
        "locations": s.included.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
        "location_groups": location_groups,
        "locked": locked,
        "logic_start": logic,
        "options": options,
        "pool": pool,
        "pool_class": classes,
        "rolls": {},
        "start_inventory": start,
    }))
}

/// Every option key an options-only rule of `m` reads, through the defs it names (each def
/// walked once).
fn read_options(m: &Model) -> Vec<String> {
    let mut todo: Vec<&Value> = m.checks.iter().map(|c| &c.when).collect();
    todo.extend(m.start_items.iter().chain(&m.assumed).map(|g| &g.when));
    let (mut out, mut seen) = (Vec::new(), std::collections::HashSet::new());
    while let Some(v) = todo.pop() {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    match (k.as_str(), x.as_str()) {
                        ("opt", Some(s)) => out.push(s.to_owned()),
                        ("ref", Some(d)) => {
                            if seen.insert(d)
                                && let Some(body) = m.defs.get(d)
                            {
                                todo.push(body);
                            }
                        }
                        _ => todo.push(x),
                    }
                }
            }
            Value::Array(a) => todo.extend(a),
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Doc 16 section 3.4: too many items for the checks is an error at build time. Every
/// combination of the options the counts read is tried; when there are more than
/// [`config::WORLD_POOL_TEST_MAX_COMBINATIONS`], every combination of each option's lowest and
/// highest value; when those are still too many, each option alone at its two ends (with a
/// warning). Generation checks the actual settings again.
pub(crate) fn pool_test(m: &Model, errs: &mut Errs) {
    let opts: Vec<&OptM> =
        read_options(m).iter().filter_map(|k| m.options.iter().find(|o| o.key == *k)).collect();
    let most = config::WORLD_POOL_TEST_MAX_COMBINATIONS;
    let ends = |o: &OptM| {
        let (lo, hi) = bounds(&o.ty);
        if lo == hi { vec![lo] } else { vec![lo, hi] }
    };
    // Sizes first: a range's values are written out only when they fit.
    let sizes: Vec<u128> = opts
        .iter()
        .map(|o| bounds(&o.ty))
        .map(|(lo, hi)| (hi - lo + 1).unsigned_abs().into())
        .collect();
    let fits = |sizes: &[u128]| {
        sizes.iter().try_fold(1u128, |n, s| n.checked_mul(*s)).is_some_and(|n| n <= most as u128)
    };
    let grid: Vec<Vec<i64>> = if fits(&sizes) {
        opts.iter()
            .map(|o| {
                let (lo, hi) = bounds(&o.ty);
                (lo..=hi).collect()
            })
            .collect()
    } else if fits(&opts.iter().map(|o| ends(o).len() as u128).collect::<Vec<_>>()) {
        opts.iter().map(|o| ends(o)).collect()
    } else {
        errs.warn_whole(
            format!(
                "the pool test tried each option alone at its two ends: over {most} combinations"
            ),
            "generation still checks the pool for the actual settings",
        );
        for o in &opts {
            for v in ends(o) {
                if !fill(m, &[(o.key.clone(), v)], errs) {
                    return;
                }
            }
        }
        return;
    };
    let mut at = vec![0usize; grid.len()];
    loop {
        let given: Vec<(String, i64)> =
            opts.iter().zip(&at).zip(&grid).map(|((o, i), v)| (o.key.clone(), v[*i])).collect();
        if !fill(m, &given, errs) {
            return;
        }
        let Some(k) = (0..at.len()).find(|k| at[*k] + 1 < grid[*k].len()) else { return };
        at[k] += 1;
        at[..k].iter_mut().for_each(|x| *x = 0);
    }
}

/// The value `text` gives option `key`, as an author writes it: `on`/`off` (or `true`/`false`)
/// for a toggle, a value's name for a choice, a whole number for a range.
pub(crate) fn option_value(m: &Model, key: &str, text: &str) -> Result<i64, String> {
    let opt = m.options.iter().find(|o| o.key == key).ok_or_else(|| {
        let known: Vec<&str> = m.options.iter().map(|o| o.key.as_str()).collect();
        format!(
            "unknown option `{key}`; the options are {}",
            if known.is_empty() { "none".into() } else { known.join(", ") }
        )
    })?;
    let v = match (&opt.ty, text) {
        (OptTy::Toggle, "on" | "true") => Some(1),
        (OptTy::Toggle, "off" | "false") => Some(0),
        (OptTy::Toggle, _) => None,
        (OptTy::Choice(vs), t) => {
            vs.iter().position(|v| v == t).and_then(|i| i64::try_from(i).ok())
        }
        (OptTy::Range(..), t) => t.parse().ok(),
    };
    let (lo, hi) = bounds(&opt.ty);
    match v {
        Some(v) if (lo..=hi).contains(&v) => Ok(v),
        _ => Err(format!(
            "option `{key}` cannot be `{text}`; it takes {}",
            match &opt.ty {
                OptTy::Toggle => "on or off".to_owned(),
                OptTy::Choice(vs) => format!("one of {}", vs.join(", ")),
                OptTy::Range(lo, hi) => format!("a whole number from {lo} to {hi}"),
            }
        )),
    }
}

/// An option's value as an author writes it: the inverse of [`option_value`].
pub(crate) fn option_text(m: &Model, key: &str, v: i64) -> String {
    match m.options.iter().find(|o| o.key == key).map(|o| &o.ty) {
        Some(OptTy::Toggle) => (if v == 0 { "off" } else { "on" }).to_owned(),
        Some(OptTy::Choice(vs)) => usize::try_from(v)
            .ok()
            .and_then(|i| vs.get(i))
            .cloned()
            .unwrap_or_else(|| v.to_string()),
        _ => v.to_string(),
    }
}

/// Every option's key and default, in declaration order.
pub(crate) fn option_defaults(m: &Model) -> Vec<(String, i64)> {
    m.options.iter().map(|o| (o.key.clone(), o.default)).collect()
}

/// An option's lowest and highest value.
fn bounds(ty: &OptTy) -> (i64, i64) {
    match ty {
        OptTy::Toggle => (0, 1),
        OptTy::Choice(vs) => (0, i64::try_from(vs.len()).unwrap_or(1) - 1),
        OptTy::Range(lo, hi) => (*lo, *hi),
    }
}

/// Whether the pool fits with `given`; when not, the error, naming the settings.
fn fill(m: &Model, given: &[(String, i64)], errs: &mut Errs) -> bool {
    let Err(e) = settle(m, &given.iter().cloned().collect()).map(|_| ()) else { return true };
    let mut set: Vec<String> = given.iter().map(|(k, v)| format!("{k} = {v}")).collect();
    set.sort();
    let with = if set.is_empty() { String::new() } else { format!(" with {}", set.join(", ")) };
    errs.whole(
        format!("the pool does not fit{with}: {e}"),
        "lower item counts or start items, or add checks",
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn free_slots_split_by_weight_with_the_remainder_to_the_largest_parts() {
        let w =
            |v: &[(&str, i64)]| v.iter().map(|(k, n)| ((*k).to_owned(), *n)).collect::<Vec<_>>();
        assert_eq!(split(10, &w(&[("coin", 9), ("gust", 1)])), w(&[("coin", 9), ("gust", 1)]));
        assert_eq!(split(3, &w(&[("coin", 9), ("gust", 1)])), w(&[("coin", 3), ("gust", 0)]));
        assert_eq!(split(3, &w(&[("a", 1), ("b", 1)])), w(&[("a", 2), ("b", 1)]));
        assert_eq!(split(0, &w(&[("a", 1)])), w(&[("a", 0)]));
    }

    #[test]
    fn options_only_rules_evaluate_as_the_engine_compares() {
        let o: HashMap<&str, i64> = [("k", 2)].into();
        let defs: BTreeMap<String, Value> =
            [("two".to_owned(), json!({"eq": [{"opt": "k"}, 2]}))].into();
        let cases = [
            (json!({"ge": [{"opt": "k"}, 2]}), Some(true)),
            (json!({"not": {"eq": [{"opt": "k"}, 2]}}), Some(false)),
            (json!({"any": []}), Some(false)),
            (json!({"all": [{"lt": [{"int": ["+", {"opt": "k"}, 1]}, 4]}]}), Some(true)),
            (json!({"all": [{"ref": "two"}, {"ref": "two"}]}), Some(true)),
            (json!({"has": "x"}), None),
            (json!({"ref": "nope"}), None),
        ];
        for (v, want) in &cases {
            let mut ev = Ev { o: &o, defs: &defs, memo: HashMap::new() };
            assert_eq!(ev.rule(v), *want, "{v}");
        }
    }
}
