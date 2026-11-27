//! The per-seed instance of a built world (design doc 16 section 4): options settled to
//! numbers, then which checks exist, the pool, start items, fixed placements and placement
//! sets, in the engine's instance form. Also the build-time pool test of section 3.4.

use std::collections::HashMap;

use data_fill::Instance;
use serde_json::Value;

use crate::lower::OptTy;
use crate::world::{Errs, Model, NOTHING};

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

/// An options-only rule's value.
fn rule(v: &Value, o: &HashMap<&str, i64>) -> Option<bool> {
    let (k, a) = v.as_object()?.iter().next()?;
    Some(match k.as_str() {
        "true" => true,
        "false" => false,
        "all" => {
            a.as_array()?.iter().map(|x| rule(x, o)).collect::<Option<Vec<_>>>()?.iter().all(|x| *x)
        }
        "any" => {
            a.as_array()?.iter().map(|x| rule(x, o)).collect::<Option<Vec<_>>>()?.iter().any(|x| *x)
        }
        "not" => !rule(a, o)?,
        op => {
            let ix = data_fill::section::CMP_OPS.iter().position(|x| *x == op)?;
            let [x, y] = a.as_array()?.as_slice() else { return None };
            data_fill::section::compare(u8::try_from(ix).ok()?, int(x, o)?, int(y, o)?)
        }
    })
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

/// The instance of `m` for `given` option values.
pub(crate) fn of(m: &Model, given: &HashMap<String, i64>) -> Result<Instance, String> {
    let mut o: HashMap<&str, i64> = HashMap::new();
    for opt in &m.options {
        let v = given.get(&opt.key).copied().unwrap_or(opt.default);
        let (lo, hi) = match &opt.ty {
            OptTy::Toggle => (0, 1),
            OptTy::Choice(vs) => (0, i64::try_from(vs.len()).unwrap_or(1) - 1),
            OptTy::Range(lo, hi) => (*lo, *hi),
        };
        if !(lo..=hi).contains(&v) {
            return Err(format!("option {:?}: {v} is outside {lo} to {hi}", opt.key));
        }
        o.insert(&opt.key, v);
    }
    if let Some(k) = given.keys().find(|k| !o.contains_key(k.as_str())) {
        return Err(format!("unknown option {k:?}"));
    }
    let holds =
        |v: &Value| rule(v, &o).ok_or_else(|| format!("a rule here reads more than options: {v}"));
    let mut inst = Instance::default();
    let mut taken: HashMap<&str, i64> = HashMap::new();
    for c in &m.checks {
        if !holds(&c.when)? {
            continue;
        }
        inst.locations.push(c.key.clone());
        if holds(&c.no_progression)? {
            inst.excluded.push(c.key.clone());
        }
        if let Some(i) = &c.holds {
            inst.locked.insert(c.key.clone(), i.clone());
            *taken.entry(i).or_default() += 1;
        }
        if let Some(s) = &c.set {
            inst.location_groups.insert(c.key.clone(), s.clone());
        }
    }
    for g in &m.start_items {
        if holds(&g.when)? {
            inst.start_inventory.extend((0..g.n).map(|_| g.item.clone()));
            *taken.entry(&g.item).or_default() += g.n;
        }
    }
    for g in &m.assumed {
        if holds(&g.when)? {
            *inst.logic_start.entry(g.item.clone()).or_default() += g.n;
        }
    }
    let mut placed = 0i64;
    for (key, class, count) in &m.items {
        let n = count - taken.get(key.as_str()).copied().unwrap_or(0);
        if n < 0 {
            return Err(format!(
                "item {key:?} has {count} copies, and {} are fixed or start items",
                count - n
            ));
        }
        inst.pool.extend((0..n).map(|_| key.clone()));
        inst.pool_class.insert(key.clone(), class.clone());
        placed += n;
    }
    let slots = i64::try_from(inst.locations.len() - inst.locked.len()).unwrap_or(i64::MAX);
    if placed > slots {
        return Err(format!("{placed} items to place and {slots} checks to hold them"));
    }
    for (k, n) in split(slots - placed, &m.fill_with) {
        inst.pool.extend((0..n).map(|_| k.clone()));
    }
    inst.pool.sort();
    inst.pool_class.entry(NOTHING.to_owned()).or_insert_with(|| "filler".to_owned());
    inst.item_groups = m.confined.clone().into_iter().collect();
    inst.options = o.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    Ok(inst)
}

/// Every option key an options-only rule of `m` reads.
fn read_options(m: &Model) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(o) => o.iter().for_each(|(k, x)| match (k.as_str(), x.as_str()) {
                ("opt", Some(s)) => out.push(s.to_owned()),
                _ => walk(x, out),
            }),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in &m.checks {
        walk(&c.when, &mut out);
    }
    for g in m.start_items.iter().chain(&m.assumed) {
        walk(&g.when, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Doc 16 section 3.4: too many items for the checks is an error at build time. Every
/// combination of the options the counts read is tried (a range at its two ends), up to
/// [`config::WORLD_POOL_TEST_MAX_COMBINATIONS`]; past it the test is skipped with a warning.
pub(crate) fn pool_test(m: &Model, errs: &mut Errs) {
    let keys = read_options(m);
    let values: Vec<Vec<i64>> = keys
        .iter()
        .filter_map(|k| m.options.iter().find(|o| o.key == *k))
        .map(|o| match &o.ty {
            OptTy::Toggle => vec![0, 1],
            OptTy::Choice(vs) => (0..i64::try_from(vs.len()).unwrap_or(0)).collect(),
            OptTy::Range(lo, hi) if lo == hi => vec![*lo],
            OptTy::Range(lo, hi) => vec![*lo, *hi],
        })
        .collect();
    let combos = values.iter().try_fold(1usize, |n, v| n.checked_mul(v.len()));
    let most = config::WORLD_POOL_TEST_MAX_COMBINATIONS;
    if combos.is_none_or(|n| n > most) {
        errs.warn_whole(
            format!("the pool test was skipped: over {most} option combinations"),
            "generation still checks the pool for the actual settings",
        );
        return;
    }
    let mut at = vec![0usize; values.len()];
    loop {
        let given: HashMap<String, i64> =
            keys.iter().zip(&at).zip(&values).map(|((k, i), v)| (k.clone(), v[*i])).collect();
        if let Err(e) = of(m, &given) {
            let mut set: Vec<String> = given.iter().map(|(k, v)| format!("{k} = {v}")).collect();
            set.sort();
            let with =
                if set.is_empty() { String::new() } else { format!(" with {}", set.join(", ")) };
            errs.whole(
                format!("the pool does not fit{with}: {e}"),
                "lower item counts or start items, or add checks",
            );
            return;
        }
        let Some(k) = (0..at.len()).find(|k| at[*k] + 1 < values[*k].len()) else { return };
        at[k] += 1;
        at[..k].iter_mut().for_each(|x| *x = 0);
    }
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
        assert_eq!(rule(&json!({"ge": [{"opt": "k"}, 2]}), &o), Some(true));
        assert_eq!(rule(&json!({"not": {"eq": [{"opt": "k"}, 2]}}), &o), Some(false));
        assert_eq!(rule(&json!({"any": []}), &o), Some(false));
        assert_eq!(
            rule(&json!({"all": [{"lt": [{"int": ["+", {"opt": "k"}, 1]}, 4]}]}), &o),
            Some(true)
        );
        assert_eq!(rule(&json!({"has": "x"}), &o), None);
    }
}
