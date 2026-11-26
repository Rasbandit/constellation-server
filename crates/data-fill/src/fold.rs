//! The per-seed fold: the section's bytecode with this seed's option and roll values folded
//! in, and the `has` tests its start items already meet (a solve never holds fewer: only
//! copies a fill added are taken back), so a solve never re-evaluates what the seed already
//! decided. Same bytecode layout; a
//! site whose rule folds to a constant points at offset 0 (true) or 1 (false). Built once per
//! seed, about the size of the section's bytecode.

use std::collections::HashMap;

use crate::section::{Node, compare, node, op, operand};
use crate::world::World;

/// Folded rules for one seed.
#[derive(Debug, Clone, Default)]
pub(crate) struct Folded {
    pub code: Vec<u8>,
    /// Rule offset per site.
    pub sites: Vec<u32>,
    /// Rule offset per def.
    pub defs: Vec<u32>,
    /// Rule offset per half-day.
    pub halves: Vec<u32>,
    pub goal: u32,
}

enum B {
    K(bool),
    Code(Vec<u8>),
}

enum I {
    K(i64),
    Code(Vec<u8>),
}

struct Folder<'a> {
    w: &'a World,
    code: &'a [u8],
    opts: &'a [i32],
    rolls: &'a [i16],
    /// Copies held from the start, per item (may be shorter than the item table: 0 beyond).
    held: &'a [i64],
    defs: Vec<Option<Option<bool>>>,
}

/// Fold every rule of `w` for these values and start copies (`held`, by item).
pub(crate) fn fold(w: &World, opts: &[i32], rolls: &[i16], held: &[i64]) -> Folded {
    let mut f = Folder { w, code: w.code(), opts, rolls, held, defs: vec![None; w.defs()] };
    let mut out = Folded { code: vec![op::TRUE, op::FALSE], ..Folded::default() };
    let mut memo: HashMap<u32, u32> = HashMap::new();
    let mut top = |f: &mut Folder<'_>, out: &mut Folded, at: u32| -> u32 {
        if let Some(x) = memo.get(&at) {
            return *x;
        }
        let x = match f.b(at as usize).0 {
            B::K(v) => u32::from(!v),
            B::Code(c) => {
                let x = u32::try_from(out.code.len()).unwrap_or(1);
                out.code.extend_from_slice(&c);
                x
            }
        };
        memo.insert(at, x);
        x
    };
    out.defs = (0..w.defs()).map(|d| top(&mut f, &mut out, w.def(d))).collect();
    out.sites = (0..w.site_count()).map(|s| top(&mut f, &mut out, w.site(s).1)).collect();
    let halves = w.time.owned.map_or(0, |o| o.halves);
    out.halves = (0..halves).map(|h| top(&mut f, &mut out, w.half(h).1)).collect();
    out.goal = top(&mut f, &mut out, w.goal);
    out
}

impl Folder<'_> {
    fn start(&self, item: u16) -> i64 {
        self.held.get(usize::from(item)).copied().unwrap_or(0)
    }

    fn def(&mut self, d: usize) -> Option<bool> {
        if let Some(k) = self.defs[d] {
            return k;
        }
        let k = match self.b(self.w.def(d) as usize).0 {
            B::K(v) => Some(v),
            B::Code(_) => None,
        };
        self.defs[d] = Some(k);
        k
    }

    fn b(&mut self, pc: usize) -> (B, usize) {
        let Some((n, at)) = node(self.code, pc, self.w.widths) else {
            return (B::K(false), pc + 1);
        };
        let orig = |end: usize, code: &[u8]| B::Code(code[pc..end].to_vec());
        match n {
            Node::Const(v) => (B::K(v), at),
            Node::All(k, len) | Node::Any(k, len) => {
                let all = matches!(n, Node::All(..));
                let stop = at + usize::from(len);
                let mut parts = 0usize;
                let mut body = Vec::new();
                let mut p = at;
                let mut decided = None;
                for _ in 0..k {
                    let (v, e) = self.b(p);
                    p = e;
                    match v {
                        B::K(v) if v == all => {}
                        B::K(v) => {
                            decided = Some(v);
                            break;
                        }
                        B::Code(c) => {
                            parts += 1;
                            body.extend_from_slice(&c);
                        }
                    }
                }
                if let Some(v) = decided {
                    return (B::K(v), stop);
                }
                match parts {
                    0 => (B::K(all), stop),
                    1 => (B::Code(body), stop),
                    m => {
                        // A folded constant can be a byte longer than the read it replaces, so
                        // a body near the limit may outgrow its length field: keep the
                        // original bytes, which read the same values at solve time.
                        let Ok(len) = u16::try_from(body.len()) else {
                            return (orig(stop, self.code), stop);
                        };
                        let mut c =
                            vec![if all { op::ALL } else { op::ANY }, u8::try_from(m).unwrap_or(0)];
                        c.extend_from_slice(&len.to_le_bytes());
                        c.extend_from_slice(&body);
                        (B::Code(c), stop)
                    }
                }
            }
            Node::HasAny(k) => {
                let iw = self.w.widths.item;
                let end = at + usize::from(k) * usize::from(iw);
                let got = (0..usize::from(k))
                    .filter_map(|j| operand(self.code, at + j * usize::from(iw), iw))
                    .any(|i| self.start(i) > 0);
                (if got { B::K(true) } else { orig(end, self.code) }, end)
            }
            Node::Has(i, c) if self.start(i) >= i64::from(c) => (B::K(true), at),
            Node::Has(..) | Node::TimeAny(_) | Node::Reach(_) => (orig(at, self.code), at),
            Node::Ref(d) => match self.def(usize::from(d)) {
                Some(v) => (B::K(v), at),
                None => (orig(at, self.code), at),
            },
            Node::Not => {
                let (v, e) = self.b(at);
                // The loader allows `not` only over option-only rules, which always fold.
                (
                    match v {
                        B::K(v) => B::K(!v),
                        B::Code(_) => orig(e, self.code),
                    },
                    e,
                )
            }
            Node::Cmp(o) => {
                let (x, e1) = self.i(at);
                let (y, e2) = self.i(e1);
                if let (I::K(a), I::K(b)) = (&x, &y) {
                    return (B::K(compare(o, *a, *b)), e2);
                }
                let mut c = vec![op::CMP, o];
                self.emit(&mut c, x, at, e1);
                self.emit(&mut c, y, e1, e2);
                (B::Code(c), e2)
            }
            _ => (B::K(false), at),
        }
    }

    /// Append a folded integer: a constant that fits 16 bits as `INT`, otherwise the original
    /// bytes `pc..end` (they read the same option and roll values at solve time).
    fn emit(&self, out: &mut Vec<u8>, v: I, pc: usize, end: usize) {
        match v {
            I::K(k) => match i16::try_from(k) {
                Ok(k) => {
                    out.push(op::INT);
                    out.extend_from_slice(&k.to_le_bytes());
                }
                Err(_) => out.extend_from_slice(&self.code[pc..end]),
            },
            I::Code(c) => out.extend_from_slice(&c),
        }
    }

    fn i(&mut self, pc: usize) -> (I, usize) {
        let Some((n, at)) = node(self.code, pc, self.w.widths) else { return (I::K(0), pc + 1) };
        match n {
            Node::Int(v) => (I::K(i64::from(v)), at),
            Node::Opt(o) => (I::K(self.opts.get(usize::from(o)).map_or(0, |v| i64::from(*v))), at),
            Node::Roll(s) => {
                (I::K(self.rolls.get(usize::from(s)).map_or(0, |v| i64::from(*v))), at)
            }
            Node::Count(_) | Node::Counter(_) | Node::Group(_) => {
                (I::Code(self.code[pc..at].to_vec()), at)
            }
            Node::Fold(o, k) => {
                let mut args = Vec::new();
                let mut p = at;
                for _ in 0..k {
                    let (v, e) = self.i(p);
                    args.push((v, p, e));
                    p = e;
                }
                if args.iter().all(|(v, ..)| matches!(v, I::K(_))) {
                    let ks = args
                        .iter()
                        .filter_map(|(v, ..)| if let I::K(k) = v { Some(*k) } else { None });
                    let v = match o {
                        op::ADD => ks.fold(0i64, i64::saturating_add),
                        op::MIN => ks.min().unwrap_or(0),
                        _ => ks.max().unwrap_or(0),
                    };
                    return (I::K(v), p);
                }
                let mut c = vec![o, k];
                for (v, a, e) in args {
                    self.emit(&mut c, v, a, e);
                }
                (I::Code(c), p)
            }
            Node::Pair(o) => {
                let (x, e1) = self.i(at);
                let (y, e2) = self.i(e1);
                if let (I::K(a), I::K(b)) = (&x, &y) {
                    let v = if o == op::MUL {
                        a.saturating_mul(*b)
                    } else {
                        a.checked_div_euclid(*b).unwrap_or(0)
                    };
                    return (I::K(v), e2);
                }
                let mut c = vec![o];
                self.emit(&mut c, x, at, e1);
                self.emit(&mut c, y, e1, e2);
                (I::Code(c), e2)
            }
            _ => (I::K(0), at),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::fill::{Instance, Plan};
    use crate::world::World;
    use serde_json::{Value, json};

    /// One region whose check `l{k}` has rule `rules[k]`, over items `a` to `j`, a counter lit
    /// by two events, and a group sum.
    pub(crate) fn world(rules: &[Value]) -> (World, Instance) {
        let keys: Vec<String> = ('a'..='j').map(String::from).collect();
        let checks: Vec<Value> = rules
            .iter()
            .enumerate()
            .map(|(k, r)| json!({"loc": format!("l{k}"), "rule": r}))
            .collect();
        let m = json!({
            "options": [{"key": "hard", "default": 1}],
            "items": keys.iter().enumerate().map(|(k, i)| json!({"key": i, "id": k + 1}))
                .collect::<Vec<_>>(),
            "locations": (0..rules.len()).map(|k| json!({"key": format!("l{k}"), "id": k + 1}))
                .collect::<Vec<_>>(),
            "start": "s",
            "x.time": {"slices": 2, "start_mask": 1},
            "defs": {"d": {"any": [{"has": "a"}, {"has": "b", "n": 2}]}},
            "regions": [{"key": "s", "can_stay": false, "stays": [], "exits": [],
                         "events": [{"event": "lit", "rule": {"has": "c"}},
                                    {"event": "lit", "rule": {"has": "d"}}],
                         "checks": checks}],
            "goal": {"reach": "region:s"}
        });
        let inst = Instance {
            locations: (0..rules.len()).map(|k| format!("l{k}")).collect(),
            ..Instance::default()
        };
        (World::load(&m).unwrap(), inst)
    }

    /// A `has` the start items already meet is folded to a constant: only copies a fill added
    /// are ever taken back, so no state of the seed holds fewer.
    #[test]
    fn has_tests_the_start_items_meet_fold_to_constants() {
        let rules = [
            json!({"has": "a"}),
            json!({"has": "b", "n": 2}),
            json!({"has": "b", "n": 3}),
            json!({"has_any": ["e", "f"]}),
            json!({"all": [{"has": "a"}, {"has": "g"}]}),
            json!({"any": [{"has": "a"}, {"has": "g"}]}),
        ];
        let (w, mut inst) = world(&rules);
        let count = |inst: &Instance, key: &str| {
            let plan = Plan::new(&w, inst).unwrap();
            let reached: Vec<bool> = (0..6).map(|l| plan.solve(&w, &[]).reached(l)).collect();
            (plan.rules_report(&w)[key].as_u64().unwrap(), reached)
        };
        assert_eq!(count(&inst, "constant_sites_with_start_items").0, 0);
        inst.start_inventory = vec!["a".to_owned(), "f".to_owned()];
        inst.logic_start.insert("b".to_owned(), 2);
        assert_eq!(count(&inst, "constant_sites_by_options").0, 0);
        // `l0`, `l1`, `l3` and `l5` are met; `l2` needs a third `b`, `l4` still reads `g`.
        let (constant, reached) = count(&inst, "constant_sites_with_start_items");
        assert_eq!((constant, reached), (4, vec![true, true, false, true, false, true]));
        let plan = Plan::new(&w, &inst).unwrap();
        let g = w.item("g").unwrap();
        assert!(plan.solve(&w, &[g]).reached(4) && !plan.solve(&w, &[g]).reached(2));
    }

    pub(crate) fn any_of(items: [&str; 2]) -> Value {
        json!({"any": [{"has": items[0]}, {"has": items[1]}]})
    }
}
