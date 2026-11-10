//! The optimizer. Passes, run until nothing changes:
//!
//! - inlining of small functions and of functions called once
//! - sparse conditional constant propagation (constants through branches and loops)
//! - algebraic simplification and strength reduction (`x * 8` becomes `x << 3`: the SM83 has no multiply)
//! - copy propagation and coalescing of temporaries
//! - dead code elimination by liveness
//! - control-flow simplification: unreachable blocks, jump threading, block merging

use crate::interp::{binop, compare, sext, width_mask};
use crate::ir::*;
use std::collections::{HashMap, HashSet};

/// Functions up to this many IR instructions are inlined at every call.
const INLINE_SIZE: usize = 12;

pub fn optimize(m: &mut Module) {
    for f in &mut m.funcs {
        simplify_cfg(f);
    }
    inline(m);
    for f in &mut m.funcs {
        for _ in 0..10 {
            let mut changed = false;
            changed |= const_prop(f);
            changed |= simplify_insts(f);
            changed |= coalesce(f);
            changed |= copy_prop(f);
            changed |= dce(f);
            changed |= simplify_cfg(f);
            if !changed {
                break;
            }
        }
    }
}

/// Functions reachable from the entry points; the others are left out of the ROM.
pub fn used_funcs(m: &Module) -> Vec<bool> {
    let mut used = vec![false; m.funcs.len()];
    let mut stack: Vec<usize> = vec![m.init_globals];
    stack.extend(m.init);
    stack.extend(m.update);
    while let Some(f) = stack.pop() {
        if std::mem::replace(&mut used[f], true) {
            continue;
        }
        for b in &m.funcs[f].blocks {
            for ins in &b.insts {
                if let Inst::Call(_, g, _) = ins.k {
                    stack.push(g);
                }
            }
        }
    }
    used
}

// ---------------------------------------------------------------------------------------------
// Control flow

pub fn simplify_cfg(f: &mut Func) -> bool {
    let mut changed = false;
    loop {
        let mut again = false;
        for b in 0..f.blocks.len() {
            // A branch to the same place either way, or on a constant, is a jump.
            if let Term::Br(v, t, e) = f.blocks[b].term.clone() {
                let to = match v {
                    _ if t == e => Some(t),
                    Val::K(k) => Some(if k != 0 { t } else { e }),
                    _ => None,
                };
                if let Some(to) = to {
                    f.blocks[b].term = Term::Jmp(to);
                    again = true;
                }
            }
            // Jump threading: skip empty blocks that only jump on.
            let mut term = f.blocks[b].term.clone();
            for s in term.succs_mut() {
                let mut seen = 0;
                while let (true, Term::Jmp(next)) =
                    (f.blocks[*s].insts.is_empty(), &f.blocks[*s].term)
                {
                    if *next == *s || seen > f.blocks.len() {
                        break;
                    }
                    *s = *next;
                    seen += 1;
                    again = true;
                }
            }
            f.blocks[b].term = term;
        }
        // An empty block that returns can be copied into the jumps that reach it.
        for b in 0..f.blocks.len() {
            if let Term::Jmp(s) = f.blocks[b].term
                && f.blocks[s].insts.is_empty()
                && let Term::Ret(v) = &f.blocks[s].term
            {
                f.blocks[b].term = Term::Ret(*v);
                again = true;
            }
        }
        // Merge a block into its only predecessor when that predecessor only jumps to it.
        let preds = f.preds();
        for b in 0..f.blocks.len() {
            if let Term::Jmp(s) = f.blocks[b].term
                && s != b
                && s != 0
                && preds[s].len() == 1
            {
                let moved = std::mem::take(&mut f.blocks[s].insts);
                let term = std::mem::replace(&mut f.blocks[s].term, Term::Jmp(s));
                f.blocks[b].insts.extend(moved);
                f.blocks[b].term = term;
                again = true;
                break; // predecessors changed
            }
        }
        again |= remove_unreachable(f);
        if !again {
            return changed;
        }
        changed = true;
    }
}

fn remove_unreachable(f: &mut Func) -> bool {
    let mut seen = vec![false; f.blocks.len()];
    let mut stack = vec![0];
    let mut order = Vec::new();
    while let Some(b) = stack.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        order.push(b);
        for s in f.blocks[b].term.succs().into_iter().rev() {
            stack.push(s);
        }
    }
    if order.len() == f.blocks.len() && order.windows(2).all(|w| w[0] < w[1]) {
        return false;
    }
    // Renumber in depth-first order, which also gives the code generator a good layout.
    let mut new_id = vec![usize::MAX; f.blocks.len()];
    for (i, &b) in order.iter().enumerate() {
        new_id[b] = i;
    }
    let old = std::mem::take(&mut f.blocks);
    let mut blocks: Vec<Option<Block>> = old.into_iter().map(Some).collect();
    for &b in &order {
        let mut block = blocks[b].take().unwrap();
        for s in block.term.succs_mut() {
            *s = new_id[*s];
        }
        f.blocks.push(block);
    }
    true
}

/// Blocks in reverse postorder.
fn rpo(f: &Func) -> Vec<BlockId> {
    fn visit(f: &Func, b: BlockId, seen: &mut [bool], out: &mut Vec<BlockId>) {
        seen[b] = true;
        for s in f.blocks[b].term.succs() {
            if !seen[s] {
                visit(f, s, seen, out);
            }
        }
        out.push(b);
    }
    let mut seen = vec![false; f.blocks.len()];
    let mut out = Vec::new();
    visit(f, 0, &mut seen, &mut out);
    out.reverse();
    out
}

// ---------------------------------------------------------------------------------------------
// Sparse conditional constant propagation

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lat {
    Undef,
    K(u16),
    Var,
}

fn meet(a: Lat, b: Lat) -> Lat {
    match (a, b) {
        (Lat::Undef, x) | (x, Lat::Undef) => x,
        (Lat::K(x), Lat::K(y)) if x == y => Lat::K(x),
        _ => Lat::Var,
    }
}

fn lat_of(state: &[Lat], v: Val) -> Lat {
    match v {
        Val::K(k) => Lat::K(k),
        Val::R(r) => state[r.0 as usize],
    }
}

/// The value an instruction computes from constant operands, if it is pure.
fn fold(f: &Func, k: &Inst, get: impl Fn(Val) -> Lat) -> Lat {
    let all = |vals: &[Val]| -> Option<Vec<u16>> {
        let mut out = Vec::new();
        for v in vals {
            match get(*v) {
                Lat::K(k) => out.push(k),
                _ => return None,
            }
        }
        Some(out)
    };
    let any_undef = k.uses().iter().any(|v| get(*v) == Lat::Undef);
    let result = match k {
        Inst::Mov(d, a) => all(&[*a]).map(|v| v[0] & width_mask(f.wide(*d))),
        Inst::Bin(op, d, a, b) => all(&[*a, *b]).map(|v| binop(*op, v[0], v[1], f.wide(*d))),
        Inst::Neg(d, a) => all(&[*a]).map(|v| v[0].wrapping_neg() & width_mask(f.wide(*d))),
        Inst::Not(d, a) => all(&[*a]).map(|v| !v[0] & width_mask(f.wide(*d))),
        Inst::Cmp(cc, _, a, b, wide) => {
            all(&[*a, *b]).map(|v| compare(*cc, v[0], v[1], *wide) as u16)
        }
        Inst::Zext(_, a) | Inst::Trunc(_, a) => all(&[*a]).map(|v| v[0] & 0xFF),
        Inst::Sext(_, a) => all(&[*a]).map(|v| sext(v[0])),
        _ => return Lat::Var,
    };
    match result {
        Some(k) => Lat::K(k),
        None if any_undef => Lat::Undef,
        None => Lat::Var,
    }
}

fn transfer(f: &Func, k: &Inst, state: &mut [Lat]) {
    if let Some(d) = k.def() {
        state[d.0 as usize] = fold(f, k, |v| lat_of(state, v));
    }
}

pub fn const_prop(f: &mut Func) -> bool {
    let n = f.regs.len();
    let nb = f.blocks.len();
    let mut entry = vec![Lat::Undef; n];
    for p in &f.params {
        entry[p.0 as usize] = Lat::Var;
    }
    let mut ins: Vec<Option<Vec<Lat>>> = vec![None; nb];
    let mut outs: Vec<Option<Vec<Lat>>> = vec![None; nb];
    let mut edges: HashSet<(BlockId, BlockId)> = HashSet::new();
    let preds = f.preds();
    let order = rpo(f);
    let mut changed_any = true;
    let mut rounds = 0;
    while changed_any && rounds < 100 {
        changed_any = false;
        rounds += 1;
        for &b in &order {
            let state_in = if b == 0 {
                Some(entry.clone())
            } else {
                let mut acc: Option<Vec<Lat>> = None;
                for &p in &preds[b] {
                    if !edges.contains(&(p, b)) {
                        continue;
                    }
                    if let Some(out) = &outs[p] {
                        acc = Some(match acc {
                            None => out.clone(),
                            Some(a) => a.iter().zip(out).map(|(x, y)| meet(*x, *y)).collect(),
                        });
                    }
                }
                acc
            };
            let Some(mut state) = state_in else { continue };
            ins[b] = Some(state.clone());
            for i in &f.blocks[b].insts {
                transfer(f, &i.k, &mut state);
            }
            let succs: Vec<BlockId> = match &f.blocks[b].term {
                Term::Br(v, t, e) => match lat_of(&state, *v) {
                    Lat::K(k) => vec![if k != 0 { *t } else { *e }],
                    Lat::Undef => vec![],
                    Lat::Var => vec![*t, *e],
                },
                term => term.succs(),
            };
            for s in succs {
                changed_any |= edges.insert((b, s));
            }
            if outs[b].as_ref() != Some(&state) {
                outs[b] = Some(state);
                changed_any = true;
            }
        }
    }
    // Rewrite: constants replace registers, folded instructions become moves.
    let mut changed = false;
    for b in 0..nb {
        let Some(mut state) = ins[b].clone() else {
            continue;
        };
        for idx in 0..f.blocks[b].insts.len() {
            let mut k = f.blocks[b].insts[idx].k.clone();
            for u in k.uses_mut() {
                if let Val::R(r) = *u
                    && let Lat::K(c) = state[r.0 as usize]
                {
                    *u = Val::K(c);
                    changed = true;
                }
            }
            transfer(f, &k, &mut state);
            if let Some(d) = k.def()
                && !k.has_effect()
                && !matches!(k, Inst::Mov(_, Val::K(_)))
                && let Lat::K(c) = state[d.0 as usize]
                && !matches!(
                    k,
                    Inst::Sys(..) | Inst::Call(..) | Inst::LoadG(..) | Inst::LoadA(..)
                )
            {
                k = Inst::Mov(d, Val::K(c));
                changed = true;
            }
            f.blocks[b].insts[idx].k = k;
        }
        for u in f.blocks[b].term.uses_mut() {
            if let Val::R(r) = *u
                && let Lat::K(c) = state[r.0 as usize]
            {
                *u = Val::K(c);
                changed = true;
            }
        }
    }
    changed
}

// ---------------------------------------------------------------------------------------------
// Algebraic simplification and strength reduction

fn simplify_insts(f: &mut Func) -> bool {
    let mut changed = false;
    for b in 0..f.blocks.len() {
        let insts = std::mem::take(&mut f.blocks[b].insts);
        let mut out = Vec::with_capacity(insts.len());
        for ins in insts {
            let line = ins.line;
            let new = simplify(f, ins.k.clone());
            if new.len() != 1 || new[0] != ins.k {
                changed = true;
            }
            out.extend(new.into_iter().map(|k| Ins { k, line }));
        }
        f.blocks[b].insts = out;
    }
    changed
}

fn simplify(f: &mut Func, k: Inst) -> Vec<Inst> {
    use Val::K;
    let Inst::Bin(op, d, a, b) = k else {
        if let Inst::Cmp(cc, d, a, b, w) = k {
            // Constants on the right, where the SM83's `cp n` wants them.
            if let (K(_), Val::R(_)) = (a, b)
                && matches!(cc, Cc::Eq | Cc::Ne)
            {
                return vec![Inst::Cmp(cc, d, b, a, w)];
            }
            match (cc, a, b) {
                (Cc::LtU, _, K(0)) => return vec![Inst::Mov(d, K(0))],
                (Cc::LeU, K(0), _) => return vec![Inst::Mov(d, K(1))],
                _ => {}
            }
        }
        return vec![k];
    };
    let wide = f.wide(d);
    let m = width_mask(wide);
    // Constants on the right.
    let (a, b) = match (a, b) {
        (K(_), Val::R(_)) if op.commutative() => (b, a),
        _ => (a, b),
    };
    let mov = |v: Val| vec![Inst::Mov(d, v)];
    match (op, b) {
        (Bin::Add | Bin::Sub | Bin::Or | Bin::Xor | Bin::Shl | Bin::ShrU | Bin::ShrS, K(0)) => {
            return mov(a);
        }
        (Bin::Mul | Bin::And, K(0)) => return mov(K(0)),
        (Bin::Mul | Bin::DivU | Bin::DivS, K(1)) => return mov(a),
        (Bin::And, K(x)) if x == m => return mov(a),
        (Bin::Or, K(x)) if x == m => return mov(K(m)),
        (Bin::ModU, K(1)) => return mov(K(0)),
        (Bin::Mul, K(x)) if x.is_power_of_two() => {
            return vec![Inst::Bin(Bin::Shl, d, a, K(x.trailing_zeros() as u16))];
        }
        (Bin::Mul, K(x)) if x.count_ones() == 2 && x > 0 => {
            // x * 10 = (x << 3) + (x << 1)
            let lo = x.trailing_zeros() as u16;
            let hi = 15 - x.leading_zeros() as u16;
            let t1 = f.new_reg(wide, "");
            let t2 = f.new_reg(wide, "");
            return vec![
                Inst::Bin(Bin::Shl, t1, a, K(hi)),
                Inst::Bin(Bin::Shl, t2, a, K(lo)),
                Inst::Bin(Bin::Add, d, Val::R(t1), Val::R(t2)),
            ];
        }
        (Bin::DivU, K(x)) if x.is_power_of_two() => {
            return vec![Inst::Bin(Bin::ShrU, d, a, K(x.trailing_zeros() as u16))];
        }
        (Bin::ModU, K(x)) if x.is_power_of_two() => {
            return vec![Inst::Bin(Bin::And, d, a, K(x - 1))];
        }
        (Bin::Sub, K(x)) => return vec![Inst::Bin(Bin::Add, d, a, K(x.wrapping_neg() & m))],
        _ => {}
    }
    if a == b {
        match op {
            Bin::Sub | Bin::Xor => return mov(K(0)),
            Bin::And | Bin::Or => return mov(a),
            _ => {}
        }
    }
    vec![Inst::Bin(op, d, a, b)]
}

// ---------------------------------------------------------------------------------------------
// Copies

fn copy_prop(f: &mut Func) -> bool {
    let mut changed = false;
    for block in &mut f.blocks {
        let mut copies: HashMap<Reg, Val> = HashMap::new();
        for ins in &mut block.insts {
            for u in ins.k.uses_mut() {
                if let Val::R(r) = *u
                    && let Some(v) = copies.get(&r)
                {
                    *u = *v;
                    changed = true;
                }
            }
            if let Some(d) = ins.k.def() {
                copies.retain(|k, v| *k != d && *v != Val::R(d));
                if let Inst::Mov(_, v) = ins.k
                    && v != Val::R(d)
                {
                    copies.insert(d, v);
                }
            }
        }
        for u in block.term.uses_mut() {
            if let Val::R(r) = *u
                && let Some(v) = copies.get(&r)
            {
                *u = *v;
                changed = true;
            }
        }
    }
    changed
}

fn use_counts(f: &Func) -> Vec<u32> {
    let mut n = vec![0u32; f.regs.len()];
    for b in &f.blocks {
        for ins in &b.insts {
            for v in ins.k.uses() {
                if let Val::R(r) = v {
                    n[r.0 as usize] += 1;
                }
            }
        }
        for v in b.term.uses() {
            if let Val::R(r) = v {
                n[r.0 as usize] += 1;
            }
        }
    }
    n
}

/// `t = a + b; x = t` becomes `x = a + b` when t is a temporary used only there.
fn coalesce(f: &mut Func) -> bool {
    let uses = use_counts(f);
    let mut changed = false;
    for b in 0..f.blocks.len() {
        let mut k = 0;
        while k < f.blocks[b].insts.len() {
            let Inst::Mov(x, Val::R(t)) = f.blocks[b].insts[k].k else {
                k += 1;
                continue;
            };
            if uses[t.0 as usize] != 1
                || !f.regs[t.0 as usize].name.is_empty()
                || f.wide(t) != f.wide(x)
                || f.params.contains(&t)
            {
                k += 1;
                continue;
            }
            let insts = &f.blocks[b].insts;
            let Some(p) = (0..k).rev().find(|&p| insts[p].k.def() == Some(t)) else {
                k += 1;
                continue;
            };
            let touches_x = insts[p + 1..k]
                .iter()
                .any(|i| i.k.def() == Some(x) || i.k.uses().contains(&Val::R(x)));
            if touches_x {
                k += 1;
                continue;
            }
            *f.blocks[b].insts[p].k.def_mut().unwrap() = x;
            f.blocks[b].insts.remove(k);
            changed = true;
        }
    }
    changed
}

// ---------------------------------------------------------------------------------------------
// Dead code

pub fn liveness(f: &Func) -> (Vec<HashSet<Reg>>, Vec<HashSet<Reg>>) {
    let nb = f.blocks.len();
    let mut live_in = vec![HashSet::new(); nb];
    let mut live_out: Vec<HashSet<Reg>> = vec![HashSet::new(); nb];
    let order = rpo(f);
    loop {
        let mut changed = false;
        for &b in order.iter().rev() {
            let mut out = HashSet::new();
            for s in f.blocks[b].term.succs() {
                out.extend(live_in[s].iter().copied());
            }
            let mut live = out.clone();
            for v in f.blocks[b].term.uses() {
                if let Val::R(r) = v {
                    live.insert(r);
                }
            }
            for ins in f.blocks[b].insts.iter().rev() {
                if let Some(d) = ins.k.def() {
                    live.remove(&d);
                }
                for v in ins.k.uses() {
                    if let Val::R(r) = v {
                        live.insert(r);
                    }
                }
            }
            if live != live_in[b] || out != live_out[b] {
                live_in[b] = live;
                live_out[b] = out;
                changed = true;
            }
        }
        if !changed {
            return (live_in, live_out);
        }
    }
}

fn dce(f: &mut Func) -> bool {
    let (_, live_out) = liveness(f);
    let mut changed = false;
    for (b, out) in live_out.iter().enumerate() {
        let mut live = out.clone();
        for v in f.blocks[b].term.uses() {
            if let Val::R(r) = v {
                live.insert(r);
            }
        }
        let insts = std::mem::take(&mut f.blocks[b].insts);
        let mut kept = Vec::with_capacity(insts.len());
        for mut ins in insts.into_iter().rev() {
            let dead = ins.k.def().is_some_and(|d| !live.contains(&d));
            if dead {
                if !ins.k.has_effect() {
                    changed = true;
                    continue;
                }
                if let Inst::Call(d, ..) | Inst::Sys(d, ..) = &mut ins.k {
                    *d = None;
                    changed = true;
                }
            }
            if let Inst::Mov(d, Val::R(s)) = ins.k
                && d == s
            {
                changed = true;
                continue;
            }
            if let Some(d) = ins.k.def() {
                live.remove(&d);
            }
            for v in ins.k.uses() {
                if let Val::R(r) = v {
                    live.insert(r);
                }
            }
            kept.push(ins);
        }
        kept.reverse();
        f.blocks[b].insts = kept;
    }
    changed
}

// ---------------------------------------------------------------------------------------------
// Inlining

fn call_counts(m: &Module) -> Vec<u32> {
    let mut n = vec![0u32; m.funcs.len()];
    for f in &m.funcs {
        for b in &f.blocks {
            for ins in &b.insts {
                if let Inst::Call(_, g, _) = ins.k {
                    n[g] += 1;
                }
            }
        }
    }
    n
}

fn has_calls(f: &Func) -> bool {
    f.blocks
        .iter()
        .any(|b| b.insts.iter().any(|i| matches!(i.k, Inst::Call(..))))
}

fn inline(m: &mut Module) {
    // Leaves first: once a function's own calls are inlined it may become a leaf itself.
    for _ in 0..m.funcs.len() + 1 {
        let counts = call_counts(m);
        let mut did = false;
        for caller in 0..m.funcs.len() {
            loop {
                let site = find_site(m, caller, &counts);
                let Some((b, k, callee)) = site else { break };
                let body = m.funcs[callee].clone();
                inline_at(&mut m.funcs[caller], b, k, &body);
                did = true;
            }
        }
        if !did {
            break;
        }
    }
}

fn find_site(m: &Module, caller: usize, counts: &[u32]) -> Option<(BlockId, usize, usize)> {
    let f = &m.funcs[caller];
    for (b, block) in f.blocks.iter().enumerate() {
        for (k, ins) in block.insts.iter().enumerate() {
            if let Inst::Call(_, g, _) = ins.k {
                let callee = &m.funcs[g];
                let entry = Some(g) == m.init || Some(g) == m.update;
                let small = callee.size() <= INLINE_SIZE;
                if g != caller && !entry && !has_calls(callee) && (small || counts[g] == 1) {
                    return Some((b, k, g));
                }
            }
        }
    }
    None
}

fn inline_at(f: &mut Func, b: BlockId, k: usize, callee: &Func) {
    let Inst::Call(dest, _, args) = f.blocks[b].insts[k].k.clone() else {
        unreachable!()
    };
    let line = f.blocks[b].insts[k].line;
    // Split the block after the call.
    let rest: Vec<Ins> = f.blocks[b].insts.drain(k..).skip(1).collect();
    let cont = f.blocks.len();
    let term = std::mem::replace(&mut f.blocks[b].term, Term::Jmp(cont));
    f.blocks.push(Block {
        insts: rest,
        term,
        line,
    });
    // Copy the callee's registers and blocks.
    let reg_base = f.regs.len() as u32;
    for info in &callee.regs {
        let name = if info.name.is_empty() {
            String::new()
        } else {
            format!("{}.{}", callee.name, info.name)
        };
        f.regs.push(RegInfo {
            wide: info.wide,
            name,
        });
    }
    let map_reg = |r: Reg| Reg(r.0 + reg_base);
    let map_val = |v: Val| match v {
        Val::R(r) => Val::R(map_reg(r)),
        k => k,
    };
    let block_base = f.blocks.len();
    for (p, a) in callee.params.iter().zip(&args) {
        f.blocks[b].insts.push(Ins {
            k: Inst::Mov(map_reg(*p), *a),
            line,
        });
    }
    f.blocks[b].term = Term::Jmp(block_base);
    for block in &callee.blocks {
        let mut insts = Vec::new();
        for ins in &block.insts {
            let mut k = ins.k.clone();
            for u in k.uses_mut() {
                *u = map_val(*u);
            }
            if let Some(d) = k.def_mut() {
                *d = map_reg(*d);
            }
            insts.push(Ins { k, line: ins.line });
        }
        let term = match &block.term {
            Term::Ret(v) => {
                if let (Some(d), Some(v)) = (dest, v) {
                    insts.push(Ins {
                        k: Inst::Mov(d, map_val(*v)),
                        line: block.line,
                    });
                }
                Term::Jmp(cont)
            }
            Term::Jmp(t) => Term::Jmp(t + block_base),
            Term::Br(v, t, e) => Term::Br(map_val(*v), t + block_base, e + block_base),
        };
        f.blocks.push(Block {
            insts,
            term,
            line: block.line,
        });
    }
}
