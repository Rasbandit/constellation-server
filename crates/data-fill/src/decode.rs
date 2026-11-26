//! The compiled section back to a readable manifest, names restored from the game section
//! and the names subsection, so a porter can see what the engine sees. Compiling the result
//! gives the same section bytes.

use serde_json::{Map, Value, json};

use crate::section::{CHECKS, CMP_OPS, EVENTS, EXITS, GROUPS, Node, node, op, operand};
use crate::world::World;

/// The manifest the section encodes (doc 07 shape; rules as the compiler normalizes them:
/// `has_all` becomes `all` of `has`, an empty `all` becomes `true`).
#[must_use]
pub fn manifest(w: &World) -> Value {
    let n16 = |n: usize| (0..n).map(|i| u16::try_from(i).unwrap_or(0));
    let n32 = |n: usize| (0..n).map(|i| u32::try_from(i).unwrap_or(0));
    let items: Vec<Value> =
        n16(w.items()).map(|i| json!({"key": w.item_key(i), "id": w.item_id(i)})).collect();
    let locations: Vec<Value> = n32(w.locations())
        .map(|l| {
            let mut o = json!({"key": w.loc_key(l), "id": w.loc_id(l)});
            if let Some(v) = w.vanilla(l) {
                o["vanilla"] = json!(w.item_key(v));
            }
            o
        })
        .collect();
    let options: Vec<Value> = (0..w.options())
        .map(|k| json!({"key": w.option_key(k), "default": w.option_default(k)}))
        .collect();
    let rolls: Map<String, Value> = w.roll_names().map(|r| (r.to_owned(), json!({}))).collect();
    let defs: Map<String, Value> =
        (0..w.defs()).map(|d| (w.def_key(d).to_owned(), rule(w, w.def(d)))).collect();
    let regions: Vec<Value> = n32(w.regions())
        .map(|r| {
            let ri = r as usize;
            let mut groups = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for (g, out) in groups.iter_mut().enumerate().take(GROUPS) {
                for s in w.group(ri, g) {
                    // The target a solve walks: a seed's staged world moves exits.
                    let (t, at) = (w.site_target(s), w.site(s).1);
                    let target = match g {
                        EXITS => json!({"to": w.region_key(t)}),
                        EVENTS => json!({"event": w.counter_key(t as usize)}),
                        CHECKS => json!({"loc": w.loc_key(t)}),
                        _ => json!({"slice": t}),
                    };
                    let mut o = target;
                    o["rule"] = rule(w, at);
                    out.push(o);
                }
            }
            let [exits, events, checks, stays] = groups;
            json!({"key": w.region_key(r), "can_stay": w.can_stay(ri), "checks": checks,
                   "exits": exits, "events": events, "stays": stays})
        })
        .collect();
    let mut time = json!({"slices": w.time.all.count_ones(), "start_mask": w.time.start_mask});
    if let Some(o) = w.time.owned {
        let halves: Vec<Value> = (0..o.halves)
            .map(|h| {
                let (m, at) = w.half(h);
                // A half-day is a contiguous slice range.
                let (first, last) = (m.trailing_zeros(), 63 - m.leading_zeros().min(63));
                json!({"first": first, "last": last, "rule": rule(w, at)})
            })
            .collect();
        time["owned"] = json!({"when": rule(w, o.when), "none_owned_mask": o.none_owned_mask, "half_days": halves});
    }
    json!({
        "items": items, "locations": locations, "options": options, "rolls": rolls,
        "start": w.region_key(w.start), "x.time": time, "defs": defs, "regions": regions,
        "goal": rule(w, w.goal),
    })
}

/// The rule at `at` as manifest JSON.
#[must_use]
pub fn rule(w: &World, at: u32) -> Value {
    b(w, at as usize).0
}

fn b(w: &World, pc: usize) -> (Value, usize) {
    let code = w.code();
    let Some((n, at)) = node(code, pc, w.widths) else { return (json!({"false": {}}), pc + 1) };
    match n {
        Node::Const(v) => (json!({ (if v { "true" } else { "false" }): {} }), at),
        Node::All(k, _) | Node::Any(k, _) => {
            let mut parts = Vec::new();
            let mut pc = at;
            for _ in 0..k {
                let (v, e) = b(w, pc);
                parts.push(v);
                pc = e;
            }
            (json!({ (if matches!(n, Node::All(..)) { "all" } else { "any" }): parts }), pc)
        }
        Node::Has(i, c) => {
            let key = w.item_key(i);
            // HAS and HAS_N decode the same node kind; only HAS_N carries `n`.
            if code.get(pc) == Some(&op::HAS_N) {
                (json!({"has": key, "n": c}), at)
            } else {
                (json!({"has": key}), at)
            }
        }
        Node::HasAny(k) => {
            let iw = w.widths.item;
            let items: Vec<&str> = (0..usize::from(k))
                .filter_map(|j| operand(code, at + j * usize::from(iw), iw))
                .map(|i| w.item_key(i))
                .collect();
            (json!({"has_any": items}), at + usize::from(k) * usize::from(iw))
        }
        Node::Cmp(o) => {
            let (x, e) = i(w, at);
            let (y, e) = i(w, e);
            (json!({ CMP_OPS[usize::from(o).min(5)]: [x, y] }), e)
        }
        Node::TimeAny(m) => (json!({"x.time_any": w.mask(usize::from(m))}), at),
        Node::Ref(d) => (json!({"ref": w.def_key(usize::from(d))}), at),
        Node::Reach(r) => (json!({"reach": format!("region:{}", w.region_key(u32::from(r)))}), at),
        Node::Not => {
            let (v, e) = b(w, at);
            (json!({"not": v}), e)
        }
        _ => (json!({"false": {}}), at),
    }
}

fn i(w: &World, pc: usize) -> (Value, usize) {
    let Some((n, at)) = node(w.code(), pc, w.widths) else { return (json!(0), pc + 1) };
    match n {
        Node::Int(v) => (json!(v), at),
        Node::Count(x) => (json!({"count": w.item_key(x)}), at),
        Node::Counter(c) => (json!({"counter": w.counter_key(usize::from(c))}), at),
        Node::Group(g) => {
            let mut args = vec![json!("+")];
            args.extend(w.group_members(usize::from(g)).map(|(item, cap)| match cap {
                None => json!({"count": w.item_key(item)}),
                Some(c) => json!({"int": ["min", {"count": w.item_key(item)}, c]}),
            }));
            (json!({"int": args}), at)
        }
        Node::Opt(o) => (json!({"opt": w.option_key(usize::from(o))}), at),
        Node::Roll(s) => {
            let (name, loc) = w.roll_slot(usize::from(s));
            (json!({"x.roll": name, "at": w.loc_key(loc)}), at)
        }
        Node::Fold(o, k) => {
            let mut args = vec![json!(match o {
                op::ADD => "+",
                op::MIN => "min",
                _ => "max",
            })];
            let mut pc = at;
            for _ in 0..k {
                let (v, e) = i(w, pc);
                args.push(v);
                pc = e;
            }
            (json!({"int": args}), pc)
        }
        Node::Pair(o) => {
            let (x, e) = i(w, at);
            let (y, e) = i(w, e);
            (json!({"int": [if o == op::MUL { "*" } else { "div" }, x, y]}), e)
        }
        _ => (json!(0), at),
    }
}
