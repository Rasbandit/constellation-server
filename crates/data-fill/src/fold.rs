//! The per-seed fold: the section's bytecode with this seed's option and roll values folded
//! in, so a solve never re-evaluates what the seed already decided. Same bytecode layout; a
//! site whose rule folds to a constant points at offset 0 (true) or 1 (false). Built once per
//! seed, about the size of the section's bytecode.

use std::collections::HashMap;

use crate::section::{Node, compare, node, op};
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
    defs: Vec<Option<Option<bool>>>,
}

/// Fold every rule of `w` for these values.
pub(crate) fn fold(w: &World, opts: &[i32], rolls: &[i16]) -> Folded {
    let mut f = Folder { w, code: w.code(), opts, rolls, defs: vec![None; w.defs()] };
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
                        let mut c =
                            vec![if all { op::ALL } else { op::ANY }, u8::try_from(m).unwrap_or(0)];
                        c.extend_from_slice(&u16::try_from(body.len()).unwrap_or(0).to_le_bytes());
                        c.extend_from_slice(&body);
                        (B::Code(c), stop)
                    }
                }
            }
            Node::HasAny(k) => {
                let end = at + usize::from(k) * usize::from(self.w.widths.item);
                (orig(end, self.code), end)
            }
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
