//! Lowers the checked syntax tree to the IR. Conditions become branches directly
//! (`a and b` never materializes a bool), loops become blocks, built-ins become runtime calls.

use crate::ast::*;
use crate::check::{Checked, Global};
use crate::ir::*;
use std::collections::HashMap;

pub fn lower(prog: &Program, c: &Checked) -> Module {
    let mut globals = Vec::new();
    let mut arrays = Vec::new();
    let mut gidx = HashMap::new();
    let mut aidx = HashMap::new();
    for name in &c.global_order {
        match &c.globals[name] {
            Global::Var(p) => {
                gidx.insert(name.clone(), globals.len());
                globals.push(GlobalVar {
                    name: name.clone(),
                    wide: p.wide(),
                });
            }
            Global::Array { elem, len, values } => {
                aidx.insert(name.clone(), arrays.len());
                arrays.push(Array {
                    name: name.clone(),
                    wide: elem.wide(),
                    len: *len,
                    rom: values
                        .as_ref()
                        .map(|v| v.iter().map(|&x| mask(x, elem.wide())).collect()),
                });
            }
            _ => {}
        }
    }
    let mut l = Lower {
        c,
        module: Module {
            funcs: Vec::new(),
            globals,
            arrays,
            strings: Vec::new(),
            tiles: c.tiles.clone(),
            init_globals: c.funcs.len(),
            init: None,
            update: None,
        },
        f: empty_func("", None),
        cur: 0,
        scopes: Vec::new(),
        loops: Vec::new(),
        line: 0,
        gidx,
        aidx,
    };
    for (fi, sig) in c.funcs.iter().enumerate() {
        let Item::Function(f) = &prog.items[sig.item] else {
            unreachable!()
        };
        l.f = empty_func(&f.name, sig.ret.map(|p| p.wide()));
        l.cur = l.new_block(f.span.line);
        l.scopes = vec![HashMap::new()];
        for (p, ty) in f.params.iter().zip(&sig.params) {
            let r = l.f.new_reg(ty.wide(), &p.name);
            l.f.params.push(r);
            l.scopes[0].insert(p.name.clone(), r);
        }
        l.block(&f.body);
        let ret = sig.ret.map(|_| Val::K(0));
        l.term(Term::Ret(ret));
        let func = std::mem::replace(&mut l.f, empty_func("", None));
        match f.name.as_str() {
            "_init" => l.module.init = Some(fi),
            "_update" => l.module.update = Some(fi),
            _ => {}
        }
        l.module.funcs.push(func);
    }
    // The global initializers run first, as a function of their own.
    l.f = empty_func("(globals)", None);
    l.cur = l.new_block(1);
    l.scopes = vec![HashMap::new()];
    for item in &prog.items {
        if let Item::Global {
            name,
            init: Some(init),
            constant: false,
            span,
            ..
        } = item
        {
            l.line = span.line;
            let v = l.expr(init);
            let g = l.gidx[name];
            l.emit(Inst::StoreG(g, v));
        }
    }
    l.term(Term::Ret(None));
    let func = std::mem::replace(&mut l.f, empty_func("", None));
    l.module.funcs.push(func);
    l.module
}

fn empty_func(name: &str, ret: Option<bool>) -> Func {
    Func {
        name: name.to_string(),
        params: Vec::new(),
        ret,
        regs: Vec::new(),
        blocks: Vec::new(),
    }
}

pub fn mask(v: i32, wide: bool) -> u16 {
    if wide { v as u16 } else { (v as u16) & 0xFF }
}

struct Lower<'a> {
    c: &'a Checked,
    module: Module,
    f: Func,
    cur: BlockId,
    scopes: Vec<HashMap<String, Reg>>,
    /// Exit blocks of the loops being lowered, for `break`.
    loops: Vec<BlockId>,
    line: u32,
    gidx: HashMap<String, usize>,
    aidx: HashMap<String, usize>,
}

impl Lower<'_> {
    fn new_block(&mut self, line: u32) -> BlockId {
        self.f.blocks.push(Block {
            insts: Vec::new(),
            term: Term::Ret(None),
            line,
        });
        self.f.blocks.len() - 1
    }

    fn emit(&mut self, k: Inst) {
        let line = self.line;
        self.f.blocks[self.cur].insts.push(Ins { k, line });
    }

    /// Ends the current block. Code that follows (after a `return` or `break`) goes into a fresh
    /// block nothing jumps to; the optimizer deletes it.
    fn term(&mut self, t: Term) {
        self.f.blocks[self.cur].term = t;
        self.cur = self.new_block(self.line);
    }

    fn ty(&self, e: &Expr) -> Prim {
        self.c.expr_ty[e.id as usize]
    }

    fn temp(&mut self, wide: bool) -> Reg {
        self.f.new_reg(wide, "")
    }

    fn lookup(&self, name: &str) -> Option<Reg> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn block(&mut self, stmts: &[Stmt]) {
        self.scopes.push(HashMap::new());
        for s in stmts {
            self.stmt(s);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        self.line = s.span.line;
        match &s.kind {
            StmtKind::Local { name, init, .. } => {
                let ty = self.c.decl_ty[&s.span.start];
                let v = match init {
                    Some(e) => self.expr(e),
                    None => Val::K(0),
                };
                let r = self.f.new_reg(ty.wide(), name);
                self.emit(Inst::Mov(r, v));
                self.scopes.last_mut().unwrap().insert(name.clone(), r);
            }
            StmtKind::Assign { target, op, value } => {
                let wide = self.ty(value).wide();
                let signed = self.ty(value).signed();
                let combine = |l: &mut Self, old: Val, v: Val| -> Val {
                    match op {
                        None => v,
                        Some(op) => {
                            let t = l.temp(wide);
                            let bin = arith(*op, signed);
                            l.emit(Inst::Bin(bin, t, old, v));
                            Val::R(t)
                        }
                    }
                };
                match target {
                    Target::Name(name) => {
                        if let Some(r) = self.lookup(name) {
                            let v = self.expr(value);
                            match op {
                                None => self.emit(Inst::Mov(r, v)),
                                Some(op) => {
                                    self.emit(Inst::Bin(arith(*op, signed), r, Val::R(r), v))
                                }
                            }
                        } else {
                            let g = self.gidx[name];
                            let old = if op.is_some() {
                                let t = self.temp(wide);
                                self.emit(Inst::LoadG(t, g));
                                Val::R(t)
                            } else {
                                Val::K(0)
                            };
                            let v = self.expr(value);
                            let v = combine(self, old, v);
                            self.emit(Inst::StoreG(g, v));
                        }
                    }
                    Target::Index(name, idx) => {
                        let a = self.aidx[name];
                        let i = self.expr(idx);
                        let old = if op.is_some() {
                            let t = self.temp(wide);
                            self.emit(Inst::LoadA(t, a, i));
                            Val::R(t)
                        } else {
                            Val::K(0)
                        };
                        let v = self.expr(value);
                        let v = combine(self, old, v);
                        self.emit(Inst::StoreA(a, i, v));
                    }
                }
            }
            StmtKind::If { arms, otherwise } => {
                let join = self.new_block(s.span.line);
                for (cond, body) in arms {
                    let then = self.new_block(cond.span.line);
                    let next = self.new_block(cond.span.line);
                    self.line = cond.span.line;
                    self.cond(cond, then, next);
                    self.cur = then;
                    self.block(body);
                    self.term(Term::Jmp(join));
                    self.cur = next;
                }
                self.block(otherwise);
                self.term(Term::Jmp(join));
                self.cur = join;
            }
            StmtKind::While { cond, body } => {
                let head = self.new_block(cond.span.line);
                let inner = self.new_block(s.span.line);
                let exit = self.new_block(s.span.line);
                self.term(Term::Jmp(head));
                self.cur = head;
                self.cond(cond, inner, exit);
                self.cur = inner;
                self.loops.push(exit);
                self.block(body);
                self.loops.pop();
                self.line = s.span.line;
                self.term(Term::Jmp(head));
                self.cur = exit;
            }
            StmtKind::For {
                var,
                from,
                to,
                step,
                body,
            } => {
                // i = from; lim = to; if i <= lim: loop { body; if lim - i < step: break; i += step }
                // Counting by the distance left never overflows, even up to 255 in a u8.
                let ty = self.c.decl_ty[&s.span.start];
                let wide = ty.wide();
                let from_v = self.expr(from);
                let i = self.f.new_reg(wide, var);
                self.emit(Inst::Mov(i, from_v));
                let to_v = self.expr(to);
                let lim = match to_v {
                    Val::K(_) => to_v,
                    Val::R(_) => {
                        let t = self.f.new_reg(wide, "limit");
                        self.emit(Inst::Mov(t, to_v));
                        Val::R(t)
                    }
                };
                let le = if ty.signed() { Cc::LeS } else { Cc::LeU };
                let enter = self.temp(false);
                let (a, b) = if *step > 0 {
                    (Val::R(i), lim)
                } else {
                    (lim, Val::R(i))
                };
                self.emit(Inst::Cmp(le, enter, a, b, wide));
                let inner = self.new_block(s.span.line);
                let latch = self.new_block(s.span.line);
                let exit = self.new_block(s.span.line);
                self.term(Term::Br(Val::R(enter), inner, exit));
                self.cur = inner;
                self.scopes.push(HashMap::from([(var.clone(), i)]));
                self.loops.push(exit);
                self.block(body);
                self.loops.pop();
                self.scopes.pop();
                self.line = s.span.line;
                self.term(Term::Jmp(latch));
                self.cur = latch;
                let done = self.temp(false);
                let n = step.unsigned_abs() as u16;
                if n == 1 {
                    self.emit(Inst::Cmp(Cc::Eq, done, Val::R(i), lim, wide));
                } else {
                    let left = self.temp(wide);
                    let (a, b) = if *step > 0 {
                        (lim, Val::R(i))
                    } else {
                        (Val::R(i), lim)
                    };
                    self.emit(Inst::Bin(Bin::Sub, left, a, b));
                    self.emit(Inst::Cmp(Cc::LtU, done, Val::R(left), Val::K(n), wide));
                }
                let next = self.new_block(s.span.line);
                self.term(Term::Br(Val::R(done), exit, next));
                self.cur = next;
                let op = if *step > 0 { Bin::Add } else { Bin::Sub };
                self.emit(Inst::Bin(op, i, Val::R(i), Val::K(n)));
                self.term(Term::Jmp(inner));
                self.cur = exit;
            }
            StmtKind::Break => {
                let exit = *self.loops.last().unwrap();
                self.term(Term::Jmp(exit));
            }
            StmtKind::Return(value) => {
                let v = value.as_ref().map(|e| self.expr(e));
                self.term(Term::Ret(v));
            }
            StmtKind::Call(e) => {
                self.call(e, false);
            }
        }
    }

    /// Branches to `t` if `e` is true, to `f` otherwise.
    fn cond(&mut self, e: &Expr, t: BlockId, f: BlockId) {
        match &e.kind {
            ExprKind::Binary(BinOp::LogicAnd, a, b) => {
                let mid = self.new_block(e.span.line);
                self.cond(a, mid, f);
                self.cur = mid;
                self.cond(b, t, f);
            }
            ExprKind::Binary(BinOp::LogicOr, a, b) => {
                let mid = self.new_block(e.span.line);
                self.cond(a, t, mid);
                self.cur = mid;
                self.cond(b, t, f);
            }
            ExprKind::Unary(UnOp::Not, x) => self.cond(x, f, t),
            ExprKind::Bool(b) => self.term(Term::Jmp(if *b { t } else { f })),
            _ => {
                let v = self.expr(e);
                self.term(Term::Br(v, t, f));
            }
        }
    }

    fn expr(&mut self, e: &Expr) -> Val {
        let ty = self.ty(e);
        let wide = ty.wide();
        match &e.kind {
            ExprKind::Int(n) => Val::K(mask(*n as i32, wide)),
            ExprKind::Bool(b) => Val::K(*b as u16),
            ExprKind::Str(_) => unreachable!("checked"),
            ExprKind::Name(name) => {
                if let Some(r) = self.lookup(name) {
                    return Val::R(r);
                }
                if let Some((_, m)) = crate::builtins::BUTTONS.iter().find(|b| b.0 == name) {
                    return Val::K(*m as u16);
                }
                match &self.c.globals[name] {
                    Global::Var(_) => {
                        let t = self.temp(wide);
                        let g = self.gidx[name];
                        self.emit(Inst::LoadG(t, g));
                        Val::R(t)
                    }
                    Global::Const(v, _) => Val::K(mask(*v, wide)),
                    Global::Gfx(t) => Val::K(mask(*t as i32, wide)),
                    _ => unreachable!("checked"),
                }
            }
            ExprKind::Index(name, idx) => {
                let a = self.aidx[name];
                let i = self.expr(idx);
                let t = self.temp(wide);
                self.emit(Inst::LoadA(t, a, i));
                Val::R(t)
            }
            ExprKind::Call(..) => self.call(e, true).unwrap(),
            ExprKind::Unary(op, x) => {
                if let (UnOp::Neg, ExprKind::Int(n)) = (op, &x.kind) {
                    return Val::K(mask(-(*n as i32), wide));
                }
                let v = self.expr(x);
                let t = self.temp(wide);
                match op {
                    UnOp::Neg => self.emit(Inst::Neg(t, v)),
                    UnOp::BitNot => self.emit(Inst::Not(t, v)),
                    UnOp::Not => self.emit(Inst::Bin(Bin::Xor, t, v, Val::K(1))),
                }
                Val::R(t)
            }
            ExprKind::Binary(BinOp::LogicAnd | BinOp::LogicOr, ..) => {
                let r = self.temp(false);
                let yes = self.new_block(e.span.line);
                let no = self.new_block(e.span.line);
                let join = self.new_block(e.span.line);
                self.cond(e, yes, no);
                self.cur = yes;
                self.emit(Inst::Mov(r, Val::K(1)));
                self.term(Term::Jmp(join));
                self.cur = no;
                self.emit(Inst::Mov(r, Val::K(0)));
                self.term(Term::Jmp(join));
                self.cur = join;
                Val::R(r)
            }
            ExprKind::Binary(op, a, b) if op.is_compare() => {
                let opty = self.ty(a);
                let (va, vb) = (self.expr(a), self.expr(b));
                let s = opty.signed();
                let (cc, x, y) = match op {
                    BinOp::Eq => (Cc::Eq, va, vb),
                    BinOp::Ne => (Cc::Ne, va, vb),
                    BinOp::Lt => (if s { Cc::LtS } else { Cc::LtU }, va, vb),
                    BinOp::Le => (if s { Cc::LeS } else { Cc::LeU }, va, vb),
                    BinOp::Gt => (if s { Cc::LtS } else { Cc::LtU }, vb, va),
                    BinOp::Ge => (if s { Cc::LeS } else { Cc::LeU }, vb, va),
                    _ => unreachable!(),
                };
                let t = self.temp(false);
                self.emit(Inst::Cmp(cc, t, x, y, opty.wide()));
                Val::R(t)
            }
            ExprKind::Binary(op, a, b) => {
                let (va, vb) = (self.expr(a), self.expr(b));
                // A shift count is used as a byte.
                let vb = if op_is_shift(*op) && self.ty(b).wide() {
                    self.convert(vb, Prim::U16, Prim::U8)
                } else {
                    vb
                };
                let t = self.temp(wide);
                self.emit(Inst::Bin(arith(*op, ty.signed()), t, va, vb));
                Val::R(t)
            }
        }
    }

    fn convert(&mut self, v: Val, from: Prim, to: Prim) -> Val {
        let (fw, tw) = (from.wide(), to.wide());
        if fw == tw {
            return v;
        }
        let t = self.temp(tw);
        if tw {
            if from.signed() {
                self.emit(Inst::Sext(t, v));
            } else {
                self.emit(Inst::Zext(t, v));
            }
        } else {
            self.emit(Inst::Trunc(t, v));
        }
        Val::R(t)
    }

    fn string(&mut self, s: &str) -> usize {
        match self.module.strings.iter().position(|x| x == s) {
            Some(i) => i,
            None => {
                self.module.strings.push(s.to_string());
                self.module.strings.len() - 1
            }
        }
    }

    fn call(&mut self, e: &Expr, value: bool) -> Option<Val> {
        let ExprKind::Call(name, args) = &e.kind else {
            unreachable!()
        };
        let ty = self.ty(e);
        if let Some(Global::Func(fi)) = self.c.globals.get(name) {
            let fi = *fi;
            let vals: Vec<Val> = args.iter().map(|a| self.expr(a)).collect();
            let d = self.c.funcs[fi].ret.map(|p| self.temp(p.wide()));
            self.emit(Inst::Call(d, fi, vals));
            return d.map(Val::R);
        }
        let sys = |l: &mut Self, s: Sys, args: &[Expr], text: Option<usize>| -> Option<Val> {
            let vals: Vec<Val> = args.iter().map(|a| l.expr(a)).collect();
            let d = s.signature().1.map(|w| l.temp(w));
            l.emit(Inst::Sys(d, s, vals, text));
            d.map(Val::R)
        };
        let result = match name.as_str() {
            "btn" | "btnp" => {
                let m = self.expr(&args[0]);
                let s = if name == "btn" {
                    Sys::Joy
                } else {
                    Sys::JoyPressed
                };
                let j = self.temp(false);
                self.emit(Inst::Sys(Some(j), s, vec![], None));
                let bits = self.temp(false);
                self.emit(Inst::Bin(Bin::And, bits, Val::R(j), m));
                let t = self.temp(false);
                self.emit(Inst::Cmp(Cc::Ne, t, Val::R(bits), Val::K(0), false));
                Some(Val::R(t))
            }
            "text" => {
                let ExprKind::Str(s) = &args[2].kind else {
                    unreachable!()
                };
                let idx = self.string(s);
                sys(self, Sys::Text, &args[..2], Some(idx))
            }
            "debug" => {
                let s = match self.ty(&args[0]) {
                    Prim::U8 | Prim::Bool => Sys::DebugU8,
                    Prim::I8 => Sys::DebugI8,
                    Prim::U16 => Sys::DebugU16,
                    Prim::I16 => Sys::DebugI16,
                };
                sys(self, s, args, None)
            }
            "u8" | "i8" | "u16" | "i16" => {
                let from = self.ty(&args[0]);
                let v = self.expr(&args[0]);
                Some(self.convert(v, from, ty))
            }
            "min" | "max" | "abs" => {
                let a = self.expr(&args[0]);
                let r = self.temp(ty.wide());
                self.emit(Inst::Mov(r, a));
                let c = self.temp(false);
                let s = ty.signed();
                let lt = if s { Cc::LtS } else { Cc::LtU };
                let replace = match name.as_str() {
                    "abs" if !s => return Some(a),
                    "abs" => {
                        self.emit(Inst::Cmp(Cc::LtS, c, a, Val::K(0), ty.wide()));
                        None
                    }
                    other => {
                        let b = self.expr(&args[1]);
                        let (x, y) = if other == "min" { (b, a) } else { (a, b) };
                        self.emit(Inst::Cmp(lt, c, x, y, ty.wide()));
                        Some(b)
                    }
                };
                let set = self.new_block(e.span.line);
                let join = self.new_block(e.span.line);
                self.term(Term::Br(Val::R(c), set, join));
                self.cur = set;
                match replace {
                    Some(b) => self.emit(Inst::Mov(r, b)),
                    None => self.emit(Inst::Neg(r, a)),
                }
                self.term(Term::Jmp(join));
                self.cur = join;
                Some(Val::R(r))
            }
            other => {
                let s = match other {
                    "spr" => Sys::Spr,
                    "hide" => Sys::Hide,
                    "tile" => Sys::Tile,
                    "gettile" => Sys::GetTile,
                    "num" => Sys::Num,
                    "cls" => Sys::Cls,
                    "rnd" => Sys::Rnd,
                    "beep" => Sys::Beep,
                    "noise" => Sys::Noise,
                    "scroll" => Sys::Scroll,
                    "frames" => Sys::Frames,
                    "vsync" => Sys::Vsync,
                    "stop" => Sys::Stop,
                    _ => unreachable!("unknown builtin {other}"),
                };
                sys(self, s, args, None)
            }
        };
        if value { result } else { None }
    }
}

fn op_is_shift(op: BinOp) -> bool {
    matches!(op, BinOp::Shl | BinOp::Shr)
}

fn arith(op: BinOp, signed: bool) -> Bin {
    match op {
        BinOp::Add => Bin::Add,
        BinOp::Sub => Bin::Sub,
        BinOp::Mul => Bin::Mul,
        BinOp::Div if signed => Bin::DivS,
        BinOp::Div => Bin::DivU,
        BinOp::Mod if signed => Bin::ModS,
        BinOp::Mod => Bin::ModU,
        BinOp::And => Bin::And,
        BinOp::Or => Bin::Or,
        BinOp::Xor => Bin::Xor,
        BinOp::Shl => Bin::Shl,
        BinOp::Shr if signed => Bin::ShrS,
        BinOp::Shr => Bin::ShrU,
        _ => unreachable!("not arithmetic: {op:?}"),
    }
}
