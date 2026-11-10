//! A reference interpreter for the IR. It defines what programs mean: the tests run each program
//! here before and after optimization, and on an emulated Game Boy, and compare what `debug()` printed.

use crate::ir::*;

pub struct Machine<'a> {
    m: &'a Module,
    globals: Vec<u16>,
    arrays: Vec<Vec<u16>>,
    map: Vec<u8>,
    frame: u16,
    pub out: String,
    steps: u64,
    limit: u64,
    stopped: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// `stop()` was called.
    Stopped,
    /// The step limit ran out.
    Limit,
    /// An array index was out of range.
    Fault(String),
}

pub fn width_mask(wide: bool) -> u16 {
    if wide { 0xFFFF } else { 0xFF }
}

fn signed(v: u16, wide: bool) -> i32 {
    if wide {
        v as i16 as i32
    } else {
        v as u8 as i8 as i32
    }
}

/// Unsigned division as the runtime does it: x / 0 is all ones and x % 0 is x.
fn divu(a: u16, b: u16, wide: bool) -> (u16, u16) {
    match (a.checked_div(b), a.checked_rem(b)) {
        (Some(q), Some(r)) => (q, r),
        _ => (width_mask(wide), a),
    }
}

/// Signed division truncates toward zero; the remainder has the sign of the dividend.
fn divs(a: u16, b: u16, wide: bool) -> (u16, u16) {
    let m = width_mask(wide);
    let neg_a = signed(a, wide) < 0;
    let neg_b = signed(b, wide) < 0;
    let ua = if neg_a { a.wrapping_neg() & m } else { a };
    let ub = if neg_b { b.wrapping_neg() & m } else { b };
    let (q, r) = divu(ua, ub, wide);
    let q = if neg_a != neg_b {
        q.wrapping_neg() & m
    } else {
        q
    };
    let r = if neg_a { r.wrapping_neg() & m } else { r };
    (q, r)
}

pub fn binop(op: Bin, a: u16, b: u16, wide: bool) -> u16 {
    let m = width_mask(wide);
    let bits = if wide { 16 } else { 8 };
    let count = (b & 0xFF) as u32;
    let r = match op {
        Bin::Add => a.wrapping_add(b),
        Bin::Sub => a.wrapping_sub(b),
        Bin::Mul => a.wrapping_mul(b),
        Bin::DivU => divu(a, b, wide).0,
        Bin::ModU => divu(a, b, wide).1,
        Bin::DivS => divs(a, b, wide).0,
        Bin::ModS => divs(a, b, wide).1,
        Bin::And => a & b,
        Bin::Or => a | b,
        Bin::Xor => a ^ b,
        Bin::Shl if count >= bits => 0,
        Bin::Shl => a << count,
        Bin::ShrU if count >= bits => 0,
        Bin::ShrU => a >> count,
        Bin::ShrS => (signed(a, wide) >> count.min(bits - 1)) as u16,
    };
    r & m
}

pub fn compare(cc: Cc, a: u16, b: u16, wide: bool) -> bool {
    match cc {
        Cc::Eq => a == b,
        Cc::Ne => a != b,
        Cc::LtU => a < b,
        Cc::LeU => a <= b,
        Cc::LtS => signed(a, wide) < signed(b, wide),
        Cc::LeS => signed(a, wide) <= signed(b, wide),
    }
}

pub fn sext(a: u16) -> u16 {
    a as u8 as i8 as i16 as u16
}

impl<'a> Machine<'a> {
    pub fn new(m: &'a Module, limit: u64) -> Machine<'a> {
        Machine {
            m,
            globals: vec![0; m.globals.len()],
            arrays: m
                .arrays
                .iter()
                .map(|a| a.rom.clone().unwrap_or_else(|| vec![0; a.len as usize]))
                .collect(),
            map: vec![0x20; 32 * 32],
            frame: 0,
            out: String::new(),
            steps: 0,
            limit,
            stopped: false,
        }
    }

    /// Runs the globals' initializers, `_init`, then `_update` for `frames` frames.
    pub fn run(&mut self, frames: u32) -> Stop {
        let m = self.m;
        let mut order = vec![m.init_globals];
        order.extend(m.init);
        for f in order {
            if let Err(stop) = self.call(f, &[]) {
                return stop;
            }
        }
        if let Some(u) = m.update {
            for _ in 0..frames {
                self.frame = self.frame.wrapping_add(1);
                if let Err(stop) = self.call(u, &[]) {
                    return stop;
                }
            }
        }
        Stop::Limit
    }

    fn call(&mut self, fi: usize, args: &[u16]) -> Result<u16, Stop> {
        let f = &self.m.funcs[fi];
        let mut regs = vec![0u16; f.regs.len()];
        for (p, a) in f.params.iter().zip(args) {
            regs[p.0 as usize] = *a;
        }
        let val = |regs: &[u16], v: Val| match v {
            Val::R(r) => regs[r.0 as usize],
            Val::K(k) => k,
        };
        let mut b = 0;
        loop {
            for ins in &f.blocks[b].insts {
                self.steps += 1;
                if self.steps > self.limit {
                    return Err(Stop::Limit);
                }
                let w = |r: Reg| f.wide(r);
                match &ins.k {
                    Inst::Mov(d, a) => regs[d.0 as usize] = val(&regs, *a) & width_mask(w(*d)),
                    Inst::Bin(op, d, a, bv) => {
                        regs[d.0 as usize] = binop(*op, val(&regs, *a), val(&regs, *bv), w(*d))
                    }
                    Inst::Neg(d, a) => {
                        regs[d.0 as usize] = val(&regs, *a).wrapping_neg() & width_mask(w(*d))
                    }
                    Inst::Not(d, a) => regs[d.0 as usize] = !val(&regs, *a) & width_mask(w(*d)),
                    Inst::Cmp(cc, d, a, bv, wide) => {
                        regs[d.0 as usize] =
                            compare(*cc, val(&regs, *a), val(&regs, *bv), *wide) as u16
                    }
                    Inst::Zext(d, a) => regs[d.0 as usize] = val(&regs, *a) & 0xFF,
                    Inst::Sext(d, a) => regs[d.0 as usize] = sext(val(&regs, *a)),
                    Inst::Trunc(d, a) => regs[d.0 as usize] = val(&regs, *a) & 0xFF,
                    Inst::LoadG(d, g) => regs[d.0 as usize] = self.globals[*g],
                    Inst::StoreG(g, a) => {
                        self.globals[*g] = val(&regs, *a) & width_mask(self.m.globals[*g].wide)
                    }
                    Inst::LoadA(d, a, i) => {
                        let i = val(&regs, *i) as usize;
                        let arr = &self.arrays[*a];
                        let Some(v) = arr.get(i) else {
                            return Err(Stop::Fault(format!(
                                "{}[{i}] is out of range",
                                self.m.arrays[*a].name
                            )));
                        };
                        regs[d.0 as usize] = *v;
                    }
                    Inst::StoreA(a, i, v) => {
                        let i = val(&regs, *i) as usize;
                        let v = val(&regs, *v) & width_mask(self.m.arrays[*a].wide);
                        let name = &self.m.arrays[*a].name;
                        match self.arrays[*a].get_mut(i) {
                            Some(slot) => *slot = v,
                            None => {
                                return Err(Stop::Fault(format!("{name}[{i}] is out of range")));
                            }
                        }
                    }
                    Inst::Call(d, g, args) => {
                        let args: Vec<u16> = args.iter().map(|a| val(&regs, *a)).collect();
                        let r = self.call(*g, &args)?;
                        if let Some(d) = d {
                            regs[d.0 as usize] = r & width_mask(w(*d));
                        }
                    }
                    Inst::Sys(d, s, args, text) => {
                        let args: Vec<u16> = args.iter().map(|a| val(&regs, *a)).collect();
                        let r = self.sys(*s, &args, *text)?;
                        if let Some(d) = d {
                            regs[d.0 as usize] = r;
                        }
                    }
                }
            }
            match &f.blocks[b].term {
                Term::Jmp(t) => b = *t,
                Term::Br(v, t, e) => b = if val(&regs, *v) != 0 { *t } else { *e },
                Term::Ret(v) => return Ok(v.map_or(0, |v| val(&regs, v))),
            }
            self.steps += 1;
            if self.steps > self.limit {
                return Err(Stop::Limit);
            }
        }
    }

    /// Like the runtime, coordinates wrap around the 32x32 background.
    fn put(&mut self, x: u16, y: u16, t: u8) {
        self.map[((y & 31) * 32 + (x & 31)) as usize] = t;
    }

    fn sys(&mut self, s: Sys, a: &[u16], text: Option<usize>) -> Result<u16, Stop> {
        Ok(match s {
            Sys::Joy | Sys::JoyPressed | Sys::Rnd => 0,
            Sys::Frames => self.frame,
            Sys::Vsync => {
                self.frame = self.frame.wrapping_add(1);
                0
            }
            Sys::Tile => {
                self.put(a[0], a[1], a[2] as u8);
                0
            }
            Sys::GetTile => self.map[((a[1] & 31) * 32 + (a[0] & 31)) as usize] as u16,
            Sys::Text => {
                let text = self.m.strings[text.unwrap()].clone();
                for (k, ch) in text.bytes().enumerate() {
                    self.put(a[0].wrapping_add(k as u16) & 0xFF, a[1], ch);
                }
                0
            }
            Sys::Num => {
                let digits = format!("{:<5}", a[2]);
                for (k, ch) in digits.bytes().enumerate() {
                    self.put(a[0].wrapping_add(k as u16) & 0xFF, a[1], ch);
                }
                0
            }
            Sys::Cls => {
                self.map.fill(0x20);
                0
            }
            Sys::DebugU8 | Sys::DebugU16 => {
                self.out += &format!("{}\n", a[0]);
                0
            }
            Sys::DebugI8 => {
                self.out += &format!("{}\n", a[0] as u8 as i8);
                0
            }
            Sys::DebugI16 => {
                self.out += &format!("{}\n", a[0] as i16);
                0
            }
            Sys::Stop => {
                self.stopped = true;
                return Err(Stop::Stopped);
            }
            Sys::Spr | Sys::Hide | Sys::Beep | Sys::Noise | Sys::Scroll => 0,
        })
    }
}
