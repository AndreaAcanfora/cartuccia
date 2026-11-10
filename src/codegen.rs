//! IR to SM83 machine code.
//!
//! - Every variable has a fixed address (there is no recursion), and functions that are never
//!   active at the same time share their memory: frames are overlaid along the call graph.
//! - The most used 8-bit variables, weighted by loop depth, go in HRAM, where `ldh` is a byte
//!   shorter and a cycle faster than `ld`.
//! - A temporary used by the very next instruction never touches memory: it stays in A (8 bits)
//!   or HL (16 bits). A comparison feeding a branch becomes `cp` + a conditional jump.
//! - The generator tracks what every CPU register holds (a constant, or the byte at an address)
//!   across instructions and into blocks whose predecessors agree, and skips redundant loads.

use crate::diag::{Error, Span};
use crate::ir::*;
use crate::opt::used_funcs;
use crate::runtime::{self, Hooks, Runtime};
use crate::sm83::{Alu, Asm, Cond, Ins, Label, R8, R16, Rot};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Known {
    Mem(u16),
    Imm(u8),
}

type State = [Option<Known>; 8];

/// Where an 8-bit operand is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Src {
    Imm(u8),
    Mem(u16),
    /// The value the previous instruction left in a register.
    In(R8),
}

/// Per-function code size and memory, for the stats panel.
#[derive(Clone, Debug, Default)]
pub struct FuncInfo {
    pub name: String,
    pub label: Label,
    pub wram: u16,
    pub hram: u16,
}

pub struct Generated {
    pub asm: Asm,
    pub funcs: Vec<FuncInfo>,
    /// Index into `asm.code` where the game's code starts (after the runtime).
    pub user_start: usize,
    pub ram_used: u16,
    pub hram_used: u16,
}

fn max_of(wide: bool, signed: bool) -> u16 {
    match (wide, signed) {
        (false, false) => 0xFF,
        (false, true) => 0x7F,
        (true, false) => 0xFFFF,
        (true, true) => 0x7FFF,
    }
}

fn mask_of(wide: bool) -> u16 {
    if wide { 0xFFFF } else { 0xFF }
}

fn is_hram(addr: u16) -> bool {
    addr >= 0xFF80
}

pub fn generate(m: &Module, source: &str) -> Result<Generated, Error> {
    let lines: Vec<&str> = source.lines().collect();
    let used = used_funcs(m);
    let mut a = Asm::default();
    let func_labels: Vec<Label> = m
        .funcs
        .iter()
        .map(|f| a.new_label(f.name.clone()))
        .collect();
    let gfx_label = a.new_label("gfx");
    let hooks = Hooks {
        init_globals: func_labels[m.init_globals],
        init: m.init.map(|f| func_labels[f]),
        update: m.update.map(|f| func_labels[f]),
        gfx: gfx_label,
        gfx_tiles: m.tiles.len() as u16,
    };
    let rt = runtime::emit(&mut a, &hooks);
    let user_start = a.code.len();

    // RAM: globals, arrays, then the frames.
    let mut next = runtime::USER_RAM;
    let mut global_addr = Vec::new();
    for g in &m.globals {
        global_addr.push(next);
        next += if g.wide { 2 } else { 1 };
    }
    let mut array_place = Vec::new();
    for arr in &m.arrays {
        let bytes = arr.len * if arr.wide { 2 } else { 1 };
        if arr.rom.is_some() {
            array_place.push(Place::Rom(a.new_label(format!("{}[]", arr.name))));
        } else {
            if next as u32 + bytes > runtime::USER_RAM_END as u32 {
                return Err(out_of_ram(m, next as u32 + bytes));
            }
            array_place.push(Place::Ram(next));
            next += bytes as u16;
        }
    }
    let frames_base = next;

    let analyses: Vec<Option<Analysis>> = m
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| used[i].then(|| analyze(f, &global_addr)))
        .collect();

    // Overlay frames along the call graph: a function's frame starts after the frames of
    // every function that can be active while it runs.
    let order = topo_order(m, &used);
    let mut base_w = vec![0u16; m.funcs.len()];
    let mut base_h = vec![0u16; m.funcs.len()];
    let mut slots: Vec<Vec<Option<u16>>> = vec![Vec::new(); m.funcs.len()];
    let mut infos = vec![FuncInfo::default(); m.funcs.len()];
    let mut ram_end = frames_base;
    let mut hram_end = runtime::HRAM_USER as u16;
    for &fi in &order {
        let f = &m.funcs[fi];
        let an = analyses[fi].as_ref().unwrap();
        let (s, w, h) = allocate(f, an, frames_base + base_w[fi], base_h[fi]);
        slots[fi] = s;
        infos[fi] = FuncInfo {
            name: f.name.clone(),
            label: func_labels[fi],
            wram: w,
            hram: h,
        };
        ram_end = ram_end.max(frames_base + base_w[fi] + w);
        hram_end = hram_end.max(runtime::HRAM_USER as u16 + base_h[fi] + h);
        for callee in callees(f) {
            base_w[callee] = base_w[callee].max(base_w[fi] + w);
            base_h[callee] = base_h[callee].max(base_h[fi] + h);
        }
    }
    if ram_end > runtime::USER_RAM_END {
        return Err(out_of_ram(m, ram_end as u32));
    }

    let param_addr: Vec<Vec<u16>> = m
        .funcs
        .iter()
        .enumerate()
        .map(|(fi, f)| {
            if !used[fi] {
                return Vec::new();
            }
            f.params
                .iter()
                .map(|p| slots[fi][p.0 as usize].expect("parameters have a slot"))
                .collect()
        })
        .collect();

    for (fi, f) in m.funcs.iter().enumerate() {
        if !used[fi] {
            continue;
        }
        let mut g = Gen {
            m,
            rt: &rt,
            a: &mut a,
            f,
            an: analyses[fi].as_ref().unwrap(),
            slots: &slots[fi],
            func_labels: &func_labels,
            param_addr: &param_addr,
            global_addr: &global_addr,
            arrays: &array_place,
            regs: [None; 8],
            zf_a: false,
            protect_a: false,
            protect_hl: false,
            incoming: vec![Vec::new(); f.blocks.len()],
            block_labels: Vec::new(),
            lines: &lines,
            last_line: 0,
            strings: HashMap::new(),
        };
        g.function(func_labels[fi]);
    }

    // Data: strings, constant arrays, graphics.
    a.emit(Ins::Comment("data".into()));
    for (i, s) in m.strings.iter().enumerate() {
        let l = match a.names.iter().position(|n| n == &format!("str{i}")) {
            Some(p) => p as Label,
            None => continue,
        };
        a.place(l);
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        a.emit(Ins::Db(bytes));
    }
    for (arr, place) in m.arrays.iter().zip(&array_place) {
        if let (Some(values), Place::Rom(l)) = (&arr.rom, place) {
            a.place(*l);
            let bytes: Vec<u8> = if arr.wide {
                values.iter().flat_map(|v| v.to_le_bytes()).collect()
            } else {
                values.iter().map(|v| *v as u8).collect()
            };
            a.emit(Ins::Db(bytes));
        }
    }
    a.place(gfx_label);
    if !m.tiles.is_empty() {
        a.emit(Ins::Db(m.tiles.iter().flatten().copied().collect()));
    }

    let mut funcs: Vec<FuncInfo> = order.iter().map(|&fi| infos[fi].clone()).collect();
    funcs.sort_by_key(|f| f.label);
    Ok(Generated {
        asm: a,
        funcs,
        user_start,
        ram_used: ram_end - runtime::USER_RAM,
        hram_used: hram_end - runtime::HRAM_USER as u16,
    })
}

fn out_of_ram(m: &Module, end: u32) -> Error {
    let biggest = m
        .arrays
        .iter()
        .filter(|a| a.rom.is_none())
        .max_by_key(|a| a.len * if a.wide { 2 } else { 1 });
    let mut e = Error::new(
        Span {
            line: 1,
            col: 1,
            ..Span::default()
        },
        format!(
            "out of memory: the game needs {} bytes of RAM, the Game Boy has {} for games",
            end - runtime::USER_RAM as u32,
            runtime::USER_RAM_END - runtime::USER_RAM
        ),
    );
    if let Some(arr) = biggest {
        e = e.note(
            None,
            format!("the biggest array is `{}` ({} elements)", arr.name, arr.len),
        );
    }
    e
}

#[derive(Clone, Copy)]
enum Place {
    Ram(u16),
    Rom(Label),
}

fn callees(f: &Func) -> Vec<usize> {
    let mut out = Vec::new();
    for b in &f.blocks {
        for i in &b.insts {
            if let Inst::Call(_, g, _) = i.k
                && !out.contains(&g)
            {
                out.push(g);
            }
        }
    }
    out
}

/// Used functions, callers before callees.
fn topo_order(m: &Module, used: &[bool]) -> Vec<usize> {
    let n = m.funcs.len();
    let mut indegree = vec![0usize; n];
    for (fi, f) in m.funcs.iter().enumerate() {
        if used[fi] {
            for g in callees(f) {
                indegree[g] += 1;
            }
        }
    }
    let mut ready: Vec<usize> = (0..n).filter(|&f| used[f] && indegree[f] == 0).collect();
    let mut order = Vec::new();
    while let Some(f) = ready.pop() {
        order.push(f);
        for g in callees(&m.funcs[f]) {
            indegree[g] -= 1;
            if indegree[g] == 0 {
                ready.push(g);
            }
        }
    }
    order
}

// ---------------------------------------------------------------------------------------------
// Analysis

struct Analysis {
    /// The register's value stays in A or HL for the next instruction.
    fused: Vec<bool>,
    /// A comparison that feeds the branch right after it: it becomes flags, not a value.
    flags: Vec<bool>,
    /// A load whose only use is the next instruction: it reads the address directly.
    alias: Vec<Option<u16>>,
    weight: Vec<u32>,
}

fn analyze(f: &Func, global_addr: &[u16]) -> Analysis {
    let n = f.regs.len();
    let mut uses = vec![0u32; n];
    let mut weight = vec![0u32; n];
    let depth = loop_depth(f);
    for (b, block) in f.blocks.iter().enumerate() {
        let w = 4u32.pow(depth[b].min(5));
        for ins in &block.insts {
            for v in ins.k.uses() {
                if let Val::R(r) = v {
                    uses[r.0 as usize] += 1;
                    weight[r.0 as usize] += w;
                }
            }
            if let Some(d) = ins.k.def() {
                weight[d.0 as usize] += w;
            }
        }
        for v in block.term.uses() {
            if let Val::R(r) = v {
                uses[r.0 as usize] += 1;
                weight[r.0 as usize] += w;
            }
        }
    }
    let mut fused = vec![false; n];
    let mut flags = vec![false; n];
    let mut alias: Vec<Option<u16>> = vec![None; n];
    for block in &f.blocks {
        // A global read once, later in the block, with no store to it or call in between:
        // the use reads the global itself.
        for (p, ins) in block.insts.iter().enumerate() {
            let Inst::LoadG(t, g) = ins.k else { continue };
            if uses[t.0 as usize] != 1 {
                continue;
            }
            let mut q = p + 1;
            let found = loop {
                let Some(next) = block.insts.get(q) else {
                    break block.term.uses().contains(&Val::R(t));
                };
                if next.k.uses().contains(&Val::R(t)) {
                    break true;
                }
                if matches!(next.k, Inst::Call(..))
                    || matches!(next.k, Inst::StoreG(h, _) if h == g)
                {
                    break false;
                }
                q += 1;
            };
            if found {
                alias[t.0 as usize] = Some(global_addr[g]);
            }
        }
        for (p, ins) in block.insts.iter().enumerate() {
            let Some(t) = ins.k.def() else { continue };
            if alias[t.0 as usize].is_some() {
                continue;
            }
            if uses[t.0 as usize] != 1 || f.params.contains(&t) {
                continue;
            }
            let next_uses = match block.insts.get(p + 1) {
                Some(next) => next.k.uses(),
                None => block.term.uses(),
            };
            if !next_uses.contains(&Val::R(t)) {
                continue;
            }
            let at_branch = p + 1 == block.insts.len() && matches!(block.term, Term::Br(..));
            match &ins.k {
                Inst::Cmp(..) if at_branch => flags[t.0 as usize] = true,
                Inst::Sys(_, Sys::Joy, ..) => {
                    alias[t.0 as usize] = Some(0xFF00 | runtime::H_JOY as u16)
                }
                Inst::Sys(_, Sys::JoyPressed, ..) => {
                    alias[t.0 as usize] = Some(0xFF00 | runtime::H_JOYNEW as u16)
                }
                Inst::Sys(_, Sys::Frames, ..) => {
                    alias[t.0 as usize] = Some(0xFF00 | runtime::H_FRAME as u16)
                }
                _ => fused[t.0 as usize] = true,
            }
        }
    }
    Analysis {
        fused,
        flags,
        alias,
        weight,
    }
}

/// How many loops contain each block (back edges found by depth-first search).
fn loop_depth(f: &Func) -> Vec<u32> {
    let nb = f.blocks.len();
    let mut depth = vec![0u32; nb];
    let mut state = vec![0u8; nb];
    let mut back = Vec::new();
    fn dfs(f: &Func, b: usize, state: &mut [u8], back: &mut Vec<(usize, usize)>) {
        state[b] = 1;
        for s in f.blocks[b].term.succs() {
            if state[s] == 1 {
                back.push((b, s));
            } else if state[s] == 0 {
                dfs(f, s, state, back);
            }
        }
        state[b] = 2;
    }
    dfs(f, 0, &mut state, &mut back);
    let preds = f.preds();
    for (tail, head) in back {
        let mut body = vec![false; nb];
        body[head] = true;
        let mut stack = vec![tail];
        while let Some(b) = stack.pop() {
            if std::mem::replace(&mut body[b], true) {
                continue;
            }
            stack.extend(preds[b].iter().copied());
        }
        for (b, inside) in body.iter().enumerate() {
            if *inside {
                depth[b] += 1;
            }
        }
    }
    depth
}

/// Gives each register that needs memory an address: the hottest ones in HRAM.
fn allocate(f: &Func, an: &Analysis, wram: u16, hram_base: u16) -> (Vec<Option<u16>>, u16, u16) {
    const HRAM_PER_FUNCTION: u16 = 24;
    let n = f.regs.len();
    let mut slots = vec![None; n];
    let mut needs: Vec<usize> = (0..n)
        .filter(|&r| !an.fused[r] && !an.flags[r] && an.alias[r].is_none())
        .filter(|&r| an.weight[r] > 0 || f.params.contains(&Reg(r as u32)))
        .collect();
    needs.sort_by_key(|&r| {
        let wide = f.regs[r].wide;
        std::cmp::Reverse(if wide { an.weight[r] / 2 } else { an.weight[r] })
    });
    let hram_room = (runtime::HRAM_USER_END as u16 - runtime::HRAM_USER as u16)
        .saturating_sub(hram_base)
        .min(HRAM_PER_FUNCTION);
    let (mut w, mut h) = (0u16, 0u16);
    for r in needs {
        let size = if f.regs[r].wide { 2 } else { 1 };
        if h + size <= hram_room && an.weight[r] >= 2 {
            slots[r] = Some(0xFF00 | (runtime::HRAM_USER as u16 + hram_base + h));
            h += size;
        } else {
            slots[r] = Some(wram + w);
            w += size;
        }
    }
    (slots, w, h)
}

// ---------------------------------------------------------------------------------------------
// Code generation

struct Gen<'a> {
    m: &'a Module,
    rt: &'a Runtime,
    a: &'a mut Asm,
    f: &'a Func,
    an: &'a Analysis,
    slots: &'a [Option<u16>],
    func_labels: &'a [Label],
    param_addr: &'a [Vec<u16>],
    global_addr: &'a [u16],
    arrays: &'a [Place],
    regs: State,
    /// The Z flag says whether A is zero.
    zf_a: bool,
    /// Loads must leave A / HL alone (they hold an argument already in place).
    protect_a: bool,
    protect_hl: bool,
    /// Register states at the jumps into each block.
    incoming: Vec<Vec<(usize, State)>>,
    block_labels: Vec<Label>,
    lines: &'a [&'a str],
    last_line: u32,
    strings: HashMap<usize, Label>,
}

const fn idx(r: R8) -> usize {
    r as usize
}

impl Gen<'_> {
    // ---- emitting, with the register cache kept up to date

    fn hl(&self) -> Option<u16> {
        match (self.regs[idx(R8::H)], self.regs[idx(R8::L)]) {
            (Some(Known::Imm(h)), Some(Known::Imm(l))) => Some((h as u16) << 8 | l as u16),
            _ => None,
        }
    }

    fn forget_mem(&mut self, addr: Option<u16>) {
        for r in &mut self.regs {
            if let Some(Known::Mem(x)) = r
                && addr.is_none_or(|a| a == *x)
            {
                *r = None;
            }
        }
    }

    fn set_hl_known(&mut self, v: Option<u16>) {
        self.regs[idx(R8::H)] = v.map(|v| Known::Imm((v >> 8) as u8));
        self.regs[idx(R8::L)] = v.map(|v| Known::Imm(v as u8));
    }

    fn emit(&mut self, i: Ins) {
        use Ins::*;
        match &i {
            Ld(d, s) => {
                if *d == R8::M {
                    let addr = self.hl();
                    self.forget_mem(addr);
                    if let Some(a) = addr
                        && *s != R8::H
                        && *s != R8::L
                    {
                        self.regs[idx(*s)] = Some(Known::Mem(a));
                    }
                } else {
                    self.regs[idx(*d)] = if *s == R8::M {
                        self.hl().map(Known::Mem)
                    } else {
                        self.regs[idx(*s)]
                    };
                    if *d == R8::A {
                        self.zf_a = false;
                    }
                }
            }
            Ldi(d, n) => {
                if *d == R8::M {
                    let addr = self.hl();
                    self.forget_mem(addr);
                } else {
                    self.regs[idx(*d)] = Some(Known::Imm(*n));
                    if *d == R8::A {
                        self.zf_a = false;
                    }
                }
            }
            Ld16(r, n) => self.set_pair(*r, Some(*n)),
            Ld16L(r, ..) => self.set_pair(*r, None),
            LdAMem(a) => {
                self.regs[idx(R8::A)] = Some(Known::Mem(*a));
                self.zf_a = false;
            }
            LdhA(n) => {
                self.regs[idx(R8::A)] = Some(Known::Mem(0xFF00 | *n as u16));
                self.zf_a = false;
            }
            LdMemA(a) => {
                self.forget_mem(Some(*a));
                self.regs[idx(R8::A)] = Some(Known::Mem(*a));
            }
            LdhW(n) => {
                let a = 0xFF00 | *n as u16;
                self.forget_mem(Some(a));
                self.regs[idx(R8::A)] = Some(Known::Mem(a));
            }
            LdAInd(_) => {
                self.regs[idx(R8::A)] = None;
                self.zf_a = false;
            }
            LdIndA(_) => self.forget_mem(None),
            LdAHli | LdAHld => {
                let hl = self.hl();
                self.regs[idx(R8::A)] = hl.map(Known::Mem);
                self.zf_a = false;
                let step = if matches!(i, LdAHli) { 1 } else { 0xFFFF };
                self.set_hl_known(hl.map(|v| v.wrapping_add(step)));
            }
            LdHliA | LdHldA => {
                let hl = self.hl();
                self.forget_mem(hl);
                let step = if matches!(i, LdHliA) { 1 } else { 0xFFFF };
                self.set_hl_known(hl.map(|v| v.wrapping_add(step)));
            }
            Alu(op, r) => {
                let keep =
                    matches!(op, crate::sm83::Alu::And | crate::sm83::Alu::Or) && *r == R8::A;
                if *op == crate::sm83::Alu::Xor && *r == R8::A {
                    self.regs[idx(R8::A)] = Some(Known::Imm(0));
                } else if !keep && *op != crate::sm83::Alu::Cp {
                    self.regs[idx(R8::A)] = None;
                }
                self.zf_a = *op != crate::sm83::Alu::Cp;
            }
            AluI(op, _) => {
                if *op != crate::sm83::Alu::Cp {
                    self.regs[idx(R8::A)] = None;
                }
                self.zf_a = *op != crate::sm83::Alu::Cp;
            }
            Inc(r) | Dec(r) => {
                if *r == R8::M {
                    let a = self.hl();
                    self.forget_mem(a);
                    self.zf_a = false;
                } else {
                    self.regs[idx(*r)] = None;
                    self.zf_a = *r == R8::A;
                }
            }
            Inc16(r) | Dec16(r) => {
                let step = if matches!(i, Inc16(_)) { 1 } else { 0xFFFF };
                if *r == R16::HL {
                    let hl = self.hl();
                    self.set_hl_known(hl.map(|v| v.wrapping_add(step)));
                } else {
                    self.set_pair(*r, None);
                }
            }
            AddHl(_) => self.set_hl_known(None),
            Push(_) | Jp(..) | Nop | Di | Ei | Halt | Label(_) | Comment(_) | Org(_) | Db(_)
            | Dw(_) | Ret(_) | Reti | JpHl | Scf | Ccf => {}
            Pop(p) => match p {
                crate::sm83::Pair::AF => {
                    self.regs[idx(R8::A)] = None;
                    self.zf_a = false;
                }
                crate::sm83::Pair::BC => self.set_pair(R16::BC, None),
                crate::sm83::Pair::DE => self.set_pair(R16::DE, None),
                crate::sm83::Pair::HL => self.set_pair(R16::HL, None),
            },
            Call(..) | CallAddr(_) => {
                self.regs = [None; 8];
                self.zf_a = false;
            }
            Cb(_, r) | Res(_, r) | Set(_, r) => {
                if *r == R8::M {
                    let a = self.hl();
                    self.forget_mem(a);
                } else {
                    self.regs[idx(*r)] = None;
                }
                self.zf_a = matches!(i, Cb(..)) && *r == R8::A;
            }
            Bit(..) => self.zf_a = false,
            Rlca | Rrca | Rla | Rra | Cpl => {
                self.regs[idx(R8::A)] = None;
                self.zf_a = false;
            }
            LdSpHl => {}
        }
        self.a.emit(i);
    }

    fn set_pair(&mut self, r: R16, v: Option<u16>) {
        let (hi, lo) = match r {
            R16::BC => (R8::B, R8::C),
            R16::DE => (R8::D, R8::E),
            R16::HL => (R8::H, R8::L),
            R16::SP => return,
        };
        self.regs[idx(hi)] = v.map(|v| Known::Imm((v >> 8) as u8));
        self.regs[idx(lo)] = v.map(|v| Known::Imm(v as u8));
    }

    // ---- operands

    fn slot(&self, r: Reg) -> u16 {
        self.slots[r.0 as usize]
            .unwrap_or_else(|| panic!("{}: register %{} has no memory slot", self.f.name, r.0))
    }

    fn fused(&self, r: Reg) -> bool {
        self.an.fused[r.0 as usize]
    }

    /// Byte `byte` (0 low, 1 high) of a value.
    fn src(&self, v: Val, byte: u16) -> Src {
        match v {
            Val::K(k) => Src::Imm((k >> (8 * byte)) as u8),
            Val::R(r) => {
                if let Some(addr) = self.an.alias[r.0 as usize] {
                    Src::Mem(addr + byte)
                } else if self.fused(r) {
                    if self.f.wide(r) {
                        Src::In(if byte == 0 { R8::L } else { R8::H })
                    } else {
                        Src::In(R8::A)
                    }
                } else {
                    Src::Mem(self.slot(r) + byte)
                }
            }
        }
    }

    fn holds(&self, k: Known) -> Option<R8> {
        if self.regs[idx(R8::A)] == Some(k) {
            return Some(R8::A);
        }
        [R8::B, R8::C, R8::D, R8::E, R8::H, R8::L]
            .into_iter()
            .find(|r| self.regs[idx(*r)] == Some(k))
    }

    fn set_hl(&mut self, addr: u16) {
        if self.hl() == Some(addr) {
            return;
        }
        if self.regs[idx(R8::H)] == Some(Known::Imm((addr >> 8) as u8)) {
            self.emit(Ins::Ldi(R8::L, addr as u8));
        } else {
            self.emit(Ins::Ld16(R16::HL, addr));
        }
    }

    /// Puts an 8-bit value in `dst`, without touching A or HL when they are protected.
    fn load8(&mut self, s: Src, dst: R8) {
        match s {
            Src::In(r) => {
                if r != dst {
                    self.emit(Ins::Ld(dst, r));
                }
            }
            Src::Imm(n) => {
                if self.regs[idx(dst)] == Some(Known::Imm(n)) {
                    return;
                }
                if let Some(r) = self.holds(Known::Imm(n)) {
                    self.emit(Ins::Ld(dst, r));
                } else if dst == R8::A && n == 0 {
                    self.emit(Ins::Alu(Alu::Xor, R8::A));
                } else {
                    self.emit(Ins::Ldi(dst, n));
                }
            }
            Src::Mem(addr) => {
                let k = Known::Mem(addr);
                if self.regs[idx(dst)] == Some(k) {
                    return;
                }
                if let Some(r) = self.holds(k) {
                    self.emit(Ins::Ld(dst, r));
                    return;
                }
                if dst == R8::A {
                    if is_hram(addr) {
                        self.emit(Ins::LdhA(addr as u8));
                    } else {
                        self.emit(Ins::LdAMem(addr));
                    }
                } else if !self.protect_hl && dst != R8::H && dst != R8::L {
                    self.set_hl(addr);
                    self.emit(Ins::Ld(dst, R8::M));
                } else {
                    assert!(
                        !self.protect_a,
                        "{}: no free register to load into",
                        self.f.name
                    );
                    self.load8(Src::Mem(addr), R8::A);
                    self.emit(Ins::Ld(dst, R8::A));
                }
            }
        }
    }

    fn load_a(&mut self, v: Val) {
        let s = self.src(v, 0);
        self.load8(s, R8::A);
    }

    /// Puts a 16-bit value in HL, DE or BC. Loading DE or BC keeps HL when `keep_hl`.
    fn load16(&mut self, v: Val, pair: R16) {
        let (hi, lo) = match pair {
            R16::HL => (R8::H, R8::L),
            R16::DE => (R8::D, R8::E),
            R16::BC => (R8::B, R8::C),
            R16::SP => unreachable!(),
        };
        let (s_lo, s_hi) = (self.src(v, 0), self.src(v, 1));
        match (s_lo, s_hi) {
            (Src::In(_), _) => {
                if pair != R16::HL {
                    self.emit(Ins::Ld(hi, R8::H));
                    self.emit(Ins::Ld(lo, R8::L));
                }
            }
            (Src::Imm(l), Src::Imm(h)) => {
                let n = (h as u16) << 8 | l as u16;
                let have = self.regs[idx(hi)] == Some(Known::Imm(h))
                    && self.regs[idx(lo)] == Some(Known::Imm(l));
                if !have {
                    self.emit(Ins::Ld16(pair, n));
                }
            }
            (Src::Mem(addr), _) => {
                if self.regs[idx(lo)] == Some(Known::Mem(addr))
                    && self.regs[idx(hi)] == Some(Known::Mem(addr + 1))
                {
                    return;
                }
                if pair == R16::HL && !is_hram(addr) && !self.protect_a {
                    self.set_hl(addr);
                    self.emit(Ins::LdAHli);
                    self.emit(Ins::Ld(R8::H, R8::M));
                    self.emit(Ins::Ld(R8::L, R8::A));
                    self.regs[idx(R8::L)] = Some(Known::Mem(addr));
                    self.regs[idx(R8::H)] = Some(Known::Mem(addr + 1));
                } else {
                    let keep = self.protect_hl;
                    self.protect_hl = keep || pair == R16::HL;
                    self.load8(Src::Mem(addr), lo);
                    self.load8(Src::Mem(addr + 1), hi);
                    self.protect_hl = keep;
                }
            }
            _ => unreachable!(),
        }
    }

    fn store8(&mut self, d: Reg) {
        if self.fused(d) {
            return;
        }
        let addr = self.slot(d);
        if is_hram(addr) {
            self.emit(Ins::LdhW(addr as u8));
        } else {
            self.emit(Ins::LdMemA(addr));
        }
    }

    fn store_a_at(&mut self, addr: u16) {
        if is_hram(addr) {
            self.emit(Ins::LdhW(addr as u8));
        } else {
            self.emit(Ins::LdMemA(addr));
        }
    }

    /// Stores HL at `addr`.
    fn store_hl_at(&mut self, addr: u16) {
        self.emit(Ins::Ld(R8::A, R8::L));
        self.store_a_at(addr);
        self.emit(Ins::Ld(R8::A, R8::H));
        self.store_a_at(addr + 1);
        self.regs[idx(R8::L)] = Some(Known::Mem(addr));
        self.regs[idx(R8::H)] = Some(Known::Mem(addr + 1));
    }

    fn store16(&mut self, d: Reg) {
        if self.fused(d) {
            return;
        }
        let addr = self.slot(d);
        self.store_hl_at(addr);
    }

    /// Stores a 16-bit value at `addr` (through A for constants, HL otherwise).
    fn store_val16(&mut self, v: Val, addr: u16) {
        if let Val::K(k) = v {
            self.load8(Src::Imm(k as u8), R8::A);
            self.store_a_at(addr);
            self.load8(Src::Imm((k >> 8) as u8), R8::A);
            self.store_a_at(addr + 1);
        } else {
            self.load16(v, R16::HL);
            self.store_hl_at(addr);
        }
    }

    // ---- structure

    fn function(&mut self, label: Label) {
        let f = self.f;
        self.a.emit(Ins::Comment(String::new()));
        self.a.place(label);
        self.block_labels = (0..f.blocks.len())
            .map(|b| self.a.new_label(format!("{}.b{b}", f.name)))
            .collect();
        let preds = f.preds();
        for b in 0..f.blocks.len() {
            // What the registers hold here: what every jump into the block agrees on.
            let state = if b == 0 {
                [None; 8]
            } else if !preds[b].is_empty()
                && preds[b]
                    .iter()
                    .all(|p| self.incoming[b].iter().any(|(q, _)| q == p))
            {
                let mut s = self.incoming[b][0].1;
                for (_, other) in &self.incoming[b][1..] {
                    for r in 0..8 {
                        if s[r] != other[r] {
                            s[r] = None;
                        }
                    }
                }
                s
            } else {
                [None; 8]
            };
            self.regs = state;
            self.zf_a = false;
            self.a.place(self.block_labels[b]);
            for (k, ins) in f.blocks[b].insts.iter().enumerate() {
                self.line(ins.line);
                self.inst(b, k);
            }
            self.term(b);
        }
    }

    fn line(&mut self, line: u32) {
        if line != self.last_line && line > 0 {
            self.last_line = line;
            let text = self.lines.get(line as usize - 1).map_or("", |l| l.trim());
            self.a.emit(Ins::Comment(format!("{line}: {text}")));
        }
    }

    fn jump(&mut self, from: usize, to: usize, cond: Option<Cond>) {
        let state = self.regs;
        self.incoming[to].push((from, state));
        self.emit(Ins::Jp(cond, self.block_labels[to]));
    }

    fn fall(&mut self, from: usize, to: usize) {
        let state = self.regs;
        self.incoming[to].push((from, state));
    }

    fn branch(&mut self, b: usize, cond: Cond, t: usize, e: usize) {
        let next = b + 1;
        if e == next {
            self.jump(b, t, Some(cond));
            self.fall(b, e);
        } else if t == next {
            self.jump(b, e, Some(cond.invert()));
            self.fall(b, t);
        } else {
            self.jump(b, t, Some(cond));
            self.jump(b, e, None);
        }
    }

    fn term(&mut self, b: usize) {
        let block = &self.f.blocks[b];
        if let Some(line) = Some(block.line).filter(|&l| l != 0 && block.insts.is_empty()) {
            self.line(line);
        }
        match block.term.clone() {
            Term::Jmp(t) => {
                if t == b + 1 {
                    self.fall(b, t);
                } else {
                    self.jump(b, t, None);
                }
            }
            Term::Br(v, t, e) => match v {
                Val::K(k) => {
                    let to = if k != 0 { t } else { e };
                    if to == b + 1 {
                        self.fall(b, to);
                    } else {
                        self.jump(b, to, None);
                    }
                }
                Val::R(r) if self.an.flags[r.0 as usize] => {
                    let Some(crate::ir::Ins {
                        k: Inst::Cmp(cc, _, x, y, wide),
                        ..
                    }) = block.insts.last().cloned()
                    else {
                        unreachable!("flag register without its comparison")
                    };
                    let cond = self.compare(cc, x, y, wide);
                    self.branch(b, cond, t, e);
                }
                _ => {
                    let in_a = matches!(v, Val::R(r) if self.fused(r));
                    self.load_a(v);
                    if !(in_a && self.zf_a) {
                        self.emit(Ins::Alu(Alu::Or, R8::A));
                    }
                    self.branch(b, Cond::NZ, t, e);
                }
            },
            Term::Ret(v) => {
                if let Some(v) = v {
                    if self.f.ret == Some(true) {
                        self.load16(v, R16::HL);
                    } else {
                        self.load_a(v);
                    }
                }
                self.emit(Ins::Ret(None));
            }
        }
    }

    // ---- instructions

    fn inst(&mut self, b: usize, k: usize) {
        let ins = self.f.blocks[b].insts[k].k.clone();
        if let Some(d) = ins.def()
            && (self.an.alias[d.0 as usize].is_some() || self.an.flags[d.0 as usize])
        {
            return; // computed where it is used
        }
        let wide = |r: Reg| self.f.wide(r);
        match ins {
            Inst::Mov(d, v) => {
                if wide(d) {
                    if !self.fused(d)
                        && let Val::K(_) = v
                    {
                        let addr = self.slot(d);
                        self.store_val16(v, addr);
                    } else {
                        self.load16(v, R16::HL);
                        self.store16(d);
                    }
                } else {
                    self.load_a(v);
                    self.store8(d);
                }
            }
            Inst::Bin(op, d, x, y) => {
                if op.is_shift() {
                    self.shift(op, d, x, y);
                } else if matches!(op, Bin::Mul | Bin::DivU | Bin::DivS | Bin::ModU | Bin::ModS) {
                    self.muldiv(op, d, x, y);
                } else if wide(d) {
                    self.bin16(op, d, x, y);
                } else {
                    self.bin8(op, d, x, y);
                }
            }
            Inst::Neg(d, x) | Inst::Not(d, x) => {
                let neg = matches!(ins, Inst::Neg(..));
                if wide(d) {
                    self.load16(x, R16::HL);
                    if neg {
                        self.emit(Ins::Alu(Alu::Xor, R8::A));
                        self.emit(Ins::Alu(Alu::Sub, R8::L));
                        self.emit(Ins::Ld(R8::L, R8::A));
                        self.emit(Ins::Ldi(R8::A, 0));
                        self.emit(Ins::Alu(Alu::Sbc, R8::H));
                        self.emit(Ins::Ld(R8::H, R8::A));
                    } else {
                        for r in [R8::L, R8::H] {
                            self.emit(Ins::Ld(R8::A, r));
                            self.emit(Ins::Cpl);
                            self.emit(Ins::Ld(r, R8::A));
                        }
                    }
                    self.store16(d);
                } else {
                    self.load_a(x);
                    self.emit(Ins::Cpl);
                    if neg {
                        self.emit(Ins::Inc(R8::A));
                    }
                    self.store8(d);
                }
            }
            Inst::Cmp(cc, d, x, y, w) => {
                let cond = self.compare(cc, x, y, w);
                self.materialize(cond);
                self.store8(d);
            }
            Inst::Zext(d, x) | Inst::Sext(d, x) => {
                self.load_a(x);
                self.emit(Ins::Ld(R8::L, R8::A));
                if matches!(ins, Inst::Sext(..)) {
                    self.emit(Ins::Alu(Alu::Add, R8::A));
                    self.emit(Ins::Alu(Alu::Sbc, R8::A));
                    self.emit(Ins::Ld(R8::H, R8::A));
                } else {
                    self.emit(Ins::Ldi(R8::H, 0));
                }
                self.store16(d);
            }
            Inst::Trunc(d, x) => {
                let s = self.src(x, 0);
                self.load8(s, R8::A);
                self.store8(d);
            }
            Inst::LoadG(d, g) => {
                let addr = self.global_addr[g];
                if wide(d) {
                    self.load_mem16(addr);
                    self.store16(d);
                } else {
                    self.load8(Src::Mem(addr), R8::A);
                    self.store8(d);
                }
            }
            Inst::StoreG(g, v) => {
                let addr = self.global_addr[g];
                if self.m.globals[g].wide {
                    self.store_val16(v, addr);
                } else {
                    self.load_a(v);
                    self.store_a_at(addr);
                }
            }
            Inst::LoadA(d, arr, i) => self.load_elem(d, arr, i),
            Inst::StoreA(arr, i, v) => self.store_elem(arr, i, v),
            Inst::Call(d, g, args) => {
                let params = &self.param_addr[g];
                let wides: Vec<bool> = self.m.funcs[g]
                    .params
                    .iter()
                    .map(|p| self.m.funcs[g].wide(*p))
                    .collect();
                // A value still in A or HL goes first.
                let order: Vec<usize> = {
                    let mut o: Vec<usize> = (0..args.len()).collect();
                    o.sort_by_key(|&i| !matches!(args[i], Val::R(r) if self.fused(r)));
                    o
                };
                for i in order {
                    let addr = params[i];
                    if wides[i] {
                        self.store_val16(args[i], addr);
                    } else {
                        self.load_a(args[i]);
                        self.store_a_at(addr);
                    }
                }
                self.emit(Ins::Call(None, self.func_labels[g]));
                if let Some(d) = d {
                    if wide(d) {
                        self.store16(d);
                    } else {
                        self.store8(d);
                    }
                }
            }
            Inst::Sys(d, s, args, text) => self.sys(d, s, &args, text),
        }
    }

    fn load_mem16(&mut self, addr: u16) {
        if self.regs[idx(R8::L)] == Some(Known::Mem(addr))
            && self.regs[idx(R8::H)] == Some(Known::Mem(addr + 1))
        {
            return;
        }
        if is_hram(addr) {
            self.load8(Src::Mem(addr), R8::A);
            self.emit(Ins::Ld(R8::L, R8::A));
            self.load8(Src::Mem(addr + 1), R8::A);
            self.emit(Ins::Ld(R8::H, R8::A));
        } else {
            self.set_hl(addr);
            self.emit(Ins::LdAHli);
            self.emit(Ins::Ld(R8::H, R8::M));
            self.emit(Ins::Ld(R8::L, R8::A));
        }
        self.regs[idx(R8::L)] = Some(Known::Mem(addr));
        self.regs[idx(R8::H)] = Some(Known::Mem(addr + 1));
    }

    fn alu_of(op: Bin) -> Alu {
        match op {
            Bin::Add => Alu::Add,
            Bin::Sub => Alu::Sub,
            Bin::And => Alu::And,
            Bin::Or => Alu::Or,
            Bin::Xor => Alu::Xor,
            _ => unreachable!(),
        }
    }

    /// `alu` A with an operand that is not in A.
    fn alu_operand(&mut self, alu: Alu, s: Src) {
        match s {
            Src::Imm(n) => self.emit(Ins::AluI(alu, n)),
            Src::In(r) => self.emit(Ins::Alu(alu, r)),
            Src::Mem(addr) => {
                if let Some(r) = self.holds(Known::Mem(addr)) {
                    self.emit(Ins::Alu(alu, r));
                } else {
                    self.set_hl(addr);
                    self.emit(Ins::Alu(alu, R8::M));
                }
            }
        }
    }

    fn bin8(&mut self, op: Bin, d: Reg, x: Val, y: Val) {
        let alu = Self::alu_of(op);
        let (mut x, mut y) = (x, y);
        if matches!(self.src(y, 0), Src::In(_)) {
            if op.commutative() {
                std::mem::swap(&mut x, &mut y);
            } else {
                self.emit(Ins::Ld(R8::C, R8::A));
                self.load_a(x);
                self.emit(Ins::Alu(alu, R8::C));
                self.store8(d);
                return;
            }
        }
        self.load_a(x);
        match (op, self.src(y, 0)) {
            (Bin::Add, Src::Imm(1)) => self.emit(Ins::Inc(R8::A)),
            (Bin::Add, Src::Imm(0xFF)) => self.emit(Ins::Dec(R8::A)),
            (_, s) => self.alu_operand(alu, s),
        }
        self.store8(d);
    }

    /// HL = x, DE = y, whichever of them is already in HL.
    fn pair_operands(&mut self, x: Val, y: Val) {
        let x_in = matches!(self.src(x, 0), Src::In(_));
        let y_in = matches!(self.src(y, 0), Src::In(_));
        if y_in {
            self.emit(Ins::Ld(R8::D, R8::H));
            self.emit(Ins::Ld(R8::E, R8::L));
            self.load16(x, R16::HL);
        } else if x_in {
            self.protect_hl = true;
            self.load16(y, R16::DE);
            self.protect_hl = false;
        } else {
            self.load16(y, R16::DE);
            self.load16(x, R16::HL);
        }
    }

    fn bin16(&mut self, op: Bin, d: Reg, x: Val, y: Val) {
        let (mut x, mut y) = (x, y);
        if op.commutative() && matches!(self.src(y, 0), Src::In(_)) {
            std::mem::swap(&mut x, &mut y);
        }
        match (op, y) {
            (Bin::Add, Val::K(n)) if n <= 3 || n >= 0xFFFD => {
                self.load16(x, R16::HL);
                if n <= 3 {
                    for _ in 0..n {
                        self.emit(Ins::Inc16(R16::HL));
                    }
                } else {
                    for _ in 0..n.wrapping_neg() {
                        self.emit(Ins::Dec16(R16::HL));
                    }
                }
            }
            (Bin::Add, _) => {
                self.pair_operands(x, y);
                self.emit(Ins::AddHl(R16::DE));
            }
            (Bin::Sub, _) => {
                self.pair_operands(x, y);
                self.emit(Ins::Ld(R8::A, R8::L));
                self.emit(Ins::Alu(Alu::Sub, R8::E));
                self.emit(Ins::Ld(R8::L, R8::A));
                self.emit(Ins::Ld(R8::A, R8::H));
                self.emit(Ins::Alu(Alu::Sbc, R8::D));
                self.emit(Ins::Ld(R8::H, R8::A));
            }
            (_, Val::K(n)) => {
                let alu = Self::alu_of(op);
                self.load16(x, R16::HL);
                for (r, byte) in [(R8::L, n as u8), (R8::H, (n >> 8) as u8)] {
                    match (alu, byte) {
                        (Alu::And, 0xFF) | (Alu::Or | Alu::Xor, 0) => {}
                        (Alu::And, 0) => self.emit(Ins::Ldi(r, 0)),
                        (Alu::Or, 0xFF) => self.emit(Ins::Ldi(r, 0xFF)),
                        _ => {
                            self.emit(Ins::Ld(R8::A, r));
                            self.emit(Ins::AluI(alu, byte));
                            self.emit(Ins::Ld(r, R8::A));
                        }
                    }
                }
            }
            _ => {
                let alu = Self::alu_of(op);
                self.pair_operands(x, y);
                for (r, s) in [(R8::L, R8::E), (R8::H, R8::D)] {
                    self.emit(Ins::Ld(R8::A, r));
                    self.emit(Ins::Alu(alu, s));
                    self.emit(Ins::Ld(r, R8::A));
                }
            }
        }
        self.store16(d);
    }

    fn shift(&mut self, op: Bin, d: Reg, x: Val, y: Val) {
        let wide = self.f.wide(d);
        if let Val::K(n) = y {
            let n = n & 0xFF;
            if wide {
                self.load16(x, R16::HL);
                self.shift16_const(op, n);
                self.store16(d);
            } else {
                self.load_a(x);
                self.shift8_const(op, n);
                self.store8(d);
            }
            return;
        }
        let name = match (op, wide) {
            (Bin::Shl, false) => "rt_shl8",
            (Bin::ShrU, false) => "rt_shr8",
            (Bin::ShrS, false) => "rt_sar8",
            (Bin::Shl, true) => "rt_shl16",
            (Bin::ShrU, true) => "rt_shr16",
            _ => "rt_sar16",
        };
        let y_in = matches!(self.src(y, 0), Src::In(_));
        let x_in = matches!(self.src(x, 0), Src::In(_));
        if y_in {
            self.emit(Ins::Ld(R8::B, R8::A));
            if wide {
                self.load16(x, R16::HL);
            } else {
                self.load_a(x);
            }
        } else if x_in {
            let s = self.src(y, 0);
            if wide {
                self.protect_hl = true;
            } else {
                self.protect_a = true;
            }
            self.load8(s, R8::B);
            self.protect_hl = false;
            self.protect_a = false;
        } else {
            let s = self.src(y, 0);
            self.load8(s, R8::B);
            if wide {
                self.load16(x, R16::HL);
            } else {
                self.load_a(x);
            }
        }
        self.emit(Ins::Call(None, self.rt.get(name)));
        if wide {
            self.store16(d);
        } else {
            self.store8(d);
        }
    }

    fn shift8_const(&mut self, op: Bin, n: u16) {
        match op {
            Bin::Shl if n >= 8 => self.emit(Ins::Ldi(R8::A, 0)),
            Bin::ShrU if n >= 8 => self.emit(Ins::Ldi(R8::A, 0)),
            Bin::ShrS if n >= 7 => {
                self.emit(Ins::Alu(Alu::Add, R8::A));
                self.emit(Ins::Alu(Alu::Sbc, R8::A));
            }
            Bin::Shl | Bin::ShrU if n >= 4 => {
                self.emit(Ins::Cb(Rot::Swap, R8::A));
                let mask = if op == Bin::Shl { 0xF0 } else { 0x0F };
                self.emit(Ins::AluI(Alu::And, mask));
                self.shift8_const(op, n - 4);
            }
            Bin::Shl => (0..n).for_each(|_| self.emit(Ins::Alu(Alu::Add, R8::A))),
            Bin::ShrU => (0..n).for_each(|_| self.emit(Ins::Cb(Rot::Srl, R8::A))),
            _ => (0..n).for_each(|_| self.emit(Ins::Cb(Rot::Sra, R8::A))),
        }
    }

    fn shift16_const(&mut self, op: Bin, n: u16) {
        match op {
            Bin::Shl if n >= 16 => self.emit(Ins::Ld16(R16::HL, 0)),
            Bin::Shl if n >= 8 => {
                self.emit(Ins::Ld(R8::H, R8::L));
                self.emit(Ins::Ldi(R8::L, 0));
                (8..n).for_each(|_| self.emit(Ins::Cb(Rot::Sla, R8::H)));
            }
            Bin::Shl => (0..n).for_each(|_| self.emit(Ins::AddHl(R16::HL))),
            Bin::ShrU if n >= 16 => self.emit(Ins::Ld16(R16::HL, 0)),
            Bin::ShrU if n >= 8 => {
                self.emit(Ins::Ld(R8::L, R8::H));
                self.emit(Ins::Ldi(R8::H, 0));
                (8..n).for_each(|_| self.emit(Ins::Cb(Rot::Srl, R8::L)));
            }
            Bin::ShrU => (0..n).for_each(|_| {
                self.emit(Ins::Cb(Rot::Srl, R8::H));
                self.emit(Ins::Cb(Rot::Rr, R8::L));
            }),
            _ if n >= 15 => {
                self.emit(Ins::Ld(R8::A, R8::H));
                self.emit(Ins::Alu(Alu::Add, R8::A));
                self.emit(Ins::Alu(Alu::Sbc, R8::A));
                self.emit(Ins::Ld(R8::H, R8::A));
                self.emit(Ins::Ld(R8::L, R8::A));
            }
            _ if n >= 8 => {
                self.emit(Ins::Ld(R8::L, R8::H));
                self.emit(Ins::Ld(R8::A, R8::H));
                self.emit(Ins::Alu(Alu::Add, R8::A));
                self.emit(Ins::Alu(Alu::Sbc, R8::A));
                self.emit(Ins::Ld(R8::H, R8::A));
                (8..n).for_each(|_| self.emit(Ins::Cb(Rot::Sra, R8::L)));
            }
            _ => (0..n).for_each(|_| {
                self.emit(Ins::Cb(Rot::Sra, R8::H));
                self.emit(Ins::Cb(Rot::Rr, R8::L));
            }),
        }
    }

    /// Widens an 8-bit value into a register pair, through A.
    fn widen(&mut self, v: Val, hi: R8, lo: R8, signed: bool) {
        self.load_a(v);
        self.emit(Ins::Ld(lo, R8::A));
        if signed {
            self.emit(Ins::Alu(Alu::Add, R8::A));
            self.emit(Ins::Alu(Alu::Sbc, R8::A));
            self.emit(Ins::Ld(hi, R8::A));
        } else {
            self.emit(Ins::Ldi(hi, 0));
        }
    }

    fn muldiv(&mut self, op: Bin, d: Reg, x: Val, y: Val) {
        let routine = match op {
            Bin::Mul => "rt_mul16",
            Bin::DivU => "rt_divu16",
            Bin::DivS => "rt_divs16",
            Bin::ModU => "rt_modu16",
            _ => "rt_mods16",
        };
        let signed = matches!(op, Bin::DivS | Bin::ModS);
        if self.f.wide(d) {
            self.pair_operands(x, y);
        } else if matches!(self.src(y, 0), Src::In(_)) {
            self.widen(y, R8::D, R8::E, signed);
            self.widen(x, R8::H, R8::L, signed);
        } else {
            self.widen(x, R8::H, R8::L, signed);
            self.widen(y, R8::D, R8::E, signed);
        }
        self.emit(Ins::Call(None, self.rt.get(routine)));
        if self.f.wide(d) {
            self.store16(d);
        } else {
            self.emit(Ins::Ld(R8::A, R8::L));
            self.store8(d);
        }
    }

    // ---- comparisons

    /// Emits a comparison and returns the flag condition that holds when it is true.
    fn compare(&mut self, cc: Cc, x: Val, y: Val, wide: bool) -> Cond {
        match cc {
            Cc::Eq | Cc::Ne => {
                if wide {
                    self.eq16(x, y);
                } else {
                    self.eq8(x, y);
                }
                if cc == Cc::Eq { Cond::Z } else { Cond::NZ }
            }
            Cc::LtU | Cc::LtS => {
                let signed = cc == Cc::LtS;
                if let (Val::K(n), false) = (x, matches!(y, Val::K(_)))
                    && n != max_of(wide, signed)
                {
                    // n < y is not (y < n + 1): the constant goes in `cp`
                    self.less(y, Val::K(n.wrapping_add(1) & mask_of(wide)), wide, signed);
                    return Cond::NC;
                }
                self.less(x, y, wide, signed);
                Cond::C
            }
            Cc::LeU | Cc::LeS => {
                let signed = cc == Cc::LeS;
                if let Val::K(n) = y
                    && n != max_of(wide, signed)
                {
                    // x <= n is x < n + 1
                    self.less(x, Val::K(n.wrapping_add(1) & mask_of(wide)), wide, signed);
                    Cond::C
                } else {
                    // x <= y is not (y < x)
                    self.less(y, x, wide, signed);
                    Cond::NC
                }
            }
        }
    }

    fn eq8(&mut self, x: Val, y: Val) {
        let (x, y) = if matches!(self.src(y, 0), Src::In(_)) {
            (y, x)
        } else {
            (x, y)
        };
        let x_in = matches!(self.src(x, 0), Src::In(_));
        self.load_a(x);
        match self.src(y, 0) {
            Src::Imm(0) => {
                if !(x_in && self.zf_a) {
                    self.emit(Ins::Alu(Alu::Or, R8::A));
                }
            }
            s => self.alu_operand(Alu::Cp, s),
        }
    }

    fn eq16(&mut self, x: Val, y: Val) {
        let (x, y) = if matches!(self.src(y, 0), Src::In(_)) {
            (y, x)
        } else {
            (x, y)
        };
        if y == Val::K(0) {
            self.load16(x, R16::HL);
            self.emit(Ins::Ld(R8::A, R8::L));
            self.emit(Ins::Alu(Alu::Or, R8::H));
            self.zf_a = false;
            return;
        }
        let done = self.a.new_label("eq16");
        if let Val::K(n) = y {
            self.load16(x, R16::HL);
            self.emit(Ins::Ld(R8::A, R8::L));
            self.emit(Ins::AluI(Alu::Cp, n as u8));
            self.emit(Ins::Jp(Some(Cond::NZ), done));
            self.emit(Ins::Ld(R8::A, R8::H));
            self.emit(Ins::AluI(Alu::Cp, (n >> 8) as u8));
        } else {
            self.pair_operands(x, y);
            self.emit(Ins::Ld(R8::A, R8::L));
            self.emit(Ins::Alu(Alu::Cp, R8::E));
            self.emit(Ins::Jp(Some(Cond::NZ), done));
            self.emit(Ins::Ld(R8::A, R8::H));
            self.emit(Ins::Alu(Alu::Cp, R8::D));
        }
        self.a.place(done);
        self.regs[idx(R8::A)] = None;
    }

    /// Sets carry when x < y.
    fn less(&mut self, x: Val, y: Val, wide: bool, signed: bool) {
        if wide {
            let flip = |n: u16| if signed { n ^ 0x8000 } else { n };
            if let Val::K(n) = y {
                self.load16(x, R16::HL);
                if signed {
                    self.flip_high(R8::H);
                }
                let n = flip(n);
                self.emit(Ins::Ld(R8::A, R8::L));
                self.emit(Ins::AluI(Alu::Sub, n as u8));
                self.emit(Ins::Ld(R8::A, R8::H));
                self.emit(Ins::AluI(Alu::Sbc, (n >> 8) as u8));
            } else {
                self.pair_operands(x, y);
                if signed {
                    self.flip_high(R8::H);
                    self.flip_high(R8::D);
                }
                self.emit(Ins::Ld(R8::A, R8::L));
                self.emit(Ins::Alu(Alu::Sub, R8::E));
                self.emit(Ins::Ld(R8::A, R8::H));
                self.emit(Ins::Alu(Alu::Sbc, R8::D));
            }
            self.regs[idx(R8::A)] = None;
            return;
        }
        if !signed {
            if matches!(self.src(y, 0), Src::In(_)) {
                self.emit(Ins::Ld(R8::C, R8::A));
                self.load_a(x);
                self.emit(Ins::Alu(Alu::Cp, R8::C));
            } else {
                self.load_a(x);
                let s = self.src(y, 0);
                self.alu_operand(Alu::Cp, s);
            }
            return;
        }
        // signed: compare with the sign bits flipped
        if let Val::K(n) = y {
            self.load_a(x);
            self.emit(Ins::AluI(Alu::Xor, 0x80));
            self.emit(Ins::AluI(Alu::Cp, (n as u8) ^ 0x80));
        } else if matches!(self.src(x, 0), Src::In(_)) {
            self.emit(Ins::AluI(Alu::Xor, 0x80));
            self.emit(Ins::Ld(R8::E, R8::A));
            self.load_a(y);
            self.emit(Ins::AluI(Alu::Xor, 0x80));
            self.emit(Ins::Ld(R8::C, R8::A));
            self.emit(Ins::Ld(R8::A, R8::E));
            self.emit(Ins::Alu(Alu::Cp, R8::C));
        } else {
            self.load_a(y);
            self.emit(Ins::AluI(Alu::Xor, 0x80));
            self.emit(Ins::Ld(R8::C, R8::A));
            self.load_a(x);
            self.emit(Ins::AluI(Alu::Xor, 0x80));
            self.emit(Ins::Alu(Alu::Cp, R8::C));
        }
    }

    fn flip_high(&mut self, r: R8) {
        self.emit(Ins::Ld(R8::A, r));
        self.emit(Ins::AluI(Alu::Xor, 0x80));
        self.emit(Ins::Ld(r, R8::A));
    }

    /// A = 1 when `cond` holds, 0 otherwise.
    fn materialize(&mut self, cond: Cond) {
        match cond {
            Cond::C => {
                self.emit(Ins::Alu(Alu::Sbc, R8::A));
                self.emit(Ins::AluI(Alu::And, 1));
            }
            Cond::NC => {
                self.emit(Ins::Alu(Alu::Sbc, R8::A));
                self.emit(Ins::Inc(R8::A));
            }
            Cond::Z | Cond::NZ => {
                let skip = self.a.new_label("bool");
                self.emit(Ins::Ldi(R8::A, 0));
                self.emit(Ins::Jp(Some(cond.invert()), skip));
                self.emit(Ins::Inc(R8::A));
                self.a.place(skip);
                self.regs[idx(R8::A)] = None;
                self.zf_a = false;
            }
        }
    }

    // ---- arrays

    /// The address of an element: a constant, or computed into HL.
    fn elem_addr(&mut self, arr: usize, i: Val) -> Option<u16> {
        let wide = self.m.arrays[arr].wide;
        let size = if wide { 2 } else { 1 };
        let place = self.arrays[arr];
        if let Val::K(k) = i {
            match place {
                Place::Ram(base) => return Some(base.wrapping_add(k.wrapping_mul(size))),
                Place::Rom(l) => {
                    self.emit(Ins::Ld16L(R16::HL, l, k.wrapping_mul(size)));
                    return None;
                }
            }
        }
        let Val::R(r) = i else { unreachable!() };
        if self.f.wide(r) {
            self.load16(i, R16::HL);
        } else {
            self.load_a(i);
            self.emit(Ins::Ld(R8::L, R8::A));
            self.emit(Ins::Ldi(R8::H, 0));
        }
        if wide {
            self.emit(Ins::AddHl(R16::HL));
        }
        match place {
            Place::Ram(base) => self.emit(Ins::Ld16(R16::DE, base)),
            Place::Rom(l) => self.emit(Ins::Ld16L(R16::DE, l, 0)),
        }
        self.emit(Ins::AddHl(R16::DE));
        None
    }

    fn load_elem(&mut self, d: Reg, arr: usize, i: Val) {
        let wide = self.m.arrays[arr].wide;
        match self.elem_addr(arr, i) {
            Some(addr) => {
                if wide {
                    self.load_mem16(addr);
                    self.store16(d);
                } else {
                    self.load8(Src::Mem(addr), R8::A);
                    self.store8(d);
                }
            }
            None => {
                if wide {
                    self.emit(Ins::LdAHli);
                    self.emit(Ins::Ld(R8::H, R8::M));
                    self.emit(Ins::Ld(R8::L, R8::A));
                    self.store16(d);
                } else {
                    self.emit(Ins::Ld(R8::A, R8::M));
                    self.store8(d);
                }
            }
        }
    }

    fn store_elem(&mut self, arr: usize, i: Val, v: Val) {
        let wide = self.m.arrays[arr].wide;
        let v_in = matches!(self.src(v, 0), Src::In(_));
        if let (Val::K(k), Place::Ram(base)) = (i, self.arrays[arr]) {
            let addr = base.wrapping_add(k.wrapping_mul(if wide { 2 } else { 1 }));
            if wide {
                self.store_val16(v, addr);
            } else {
                self.load_a(v);
                self.store_a_at(addr);
            }
            // a store to an array can change what a variable-index load cached
            self.forget_mem(Some(addr));
            if !wide {
                self.regs[idx(R8::A)] = Some(Known::Mem(addr));
            }
            return;
        }
        if wide {
            if v_in {
                self.emit(Ins::Ld(R8::B, R8::H));
                self.emit(Ins::Ld(R8::C, R8::L));
                self.elem_addr(arr, i);
            } else {
                self.elem_addr(arr, i);
                if !matches!(v, Val::K(_)) {
                    self.protect_hl = true;
                    self.load16(v, R16::BC);
                    self.protect_hl = false;
                }
            }
            match v {
                Val::K(k) => {
                    self.emit(Ins::Ldi(R8::M, k as u8));
                    self.emit(Ins::Inc16(R16::HL));
                    self.emit(Ins::Ldi(R8::M, (k >> 8) as u8));
                }
                _ => {
                    self.emit(Ins::Ld(R8::M, R8::C));
                    self.emit(Ins::Inc16(R16::HL));
                    self.emit(Ins::Ld(R8::M, R8::B));
                }
            }
        } else if v_in {
            self.emit(Ins::Ld(R8::C, R8::A));
            self.elem_addr(arr, i);
            self.emit(Ins::Ld(R8::M, R8::C));
        } else {
            self.elem_addr(arr, i);
            match self.src(v, 0) {
                Src::Imm(n) => self.emit(Ins::Ldi(R8::M, n)),
                s => {
                    self.protect_hl = true;
                    self.load8(s, R8::A);
                    self.protect_hl = false;
                    self.emit(Ins::Ld(R8::M, R8::A));
                }
            }
        }
        self.forget_mem(None);
    }

    // ---- runtime calls

    fn sys(&mut self, d: Option<Reg>, s: Sys, args: &[Val], text: Option<usize>) {
        use R8::*;
        enum T {
            R(R8),
            HL,
        }
        let (name, targets): (&str, Vec<T>) = match s {
            Sys::Joy | Sys::JoyPressed | Sys::Frames => {
                let addr = match s {
                    Sys::Joy => runtime::H_JOY,
                    Sys::JoyPressed => runtime::H_JOYNEW,
                    _ => runtime::H_FRAME,
                };
                let d = d.expect("result used");
                if s == Sys::Frames {
                    self.load_mem16(0xFF00 | addr as u16);
                    self.store16(d);
                } else {
                    self.load8(Src::Mem(0xFF00 | addr as u16), A);
                    self.store8(d);
                }
                return;
            }
            Sys::Spr => ("rt_spr", vec![T::R(A), T::R(B), T::R(C), T::R(D)]),
            Sys::Hide => ("rt_hide", vec![T::R(A)]),
            Sys::Tile => ("rt_tile", vec![T::R(B), T::R(C), T::R(A)]),
            Sys::GetTile => ("rt_gettile", vec![T::R(B), T::R(C)]),
            Sys::Text => ("rt_text", vec![T::R(B), T::R(C)]),
            Sys::Num => ("rt_num", vec![T::R(B), T::R(C), T::HL]),
            Sys::Cls => ("rt_cls", vec![]),
            Sys::Rnd => ("rt_rnd", vec![T::R(A)]),
            Sys::Beep => ("rt_beep", vec![T::R(A), T::R(B)]),
            Sys::Noise => ("rt_noise", vec![T::R(A)]),
            Sys::Scroll => ("rt_scroll", vec![T::R(B), T::R(C)]),
            Sys::Vsync => ("rt_vsync", vec![]),
            Sys::DebugU8 => ("rt_debug_u8", vec![T::R(A)]),
            Sys::DebugI8 => ("rt_debug_i8", vec![T::R(A)]),
            Sys::DebugU16 => ("rt_debug_u16", vec![T::HL]),
            Sys::DebugI16 => ("rt_debug_i16", vec![T::HL]),
            Sys::Stop => ("rt_stop", vec![]),
        };
        let mut done = vec![false; args.len()];
        // 1. the argument still in A or HL
        for (i, (v, t)) in args.iter().zip(&targets).enumerate() {
            if let Val::R(r) = v
                && self.fused(*r)
            {
                match t {
                    T::HL => self.protect_hl = true,
                    T::R(A) => self.protect_a = true,
                    T::R(r) => {
                        self.emit(Ins::Ld(*r, A));
                    }
                }
                done[i] = true;
            }
        }
        // 2. 16 bits, 3. registers other than A, 4. A
        for (i, (v, t)) in args.iter().zip(&targets).enumerate() {
            if !done[i] && matches!(t, T::HL) {
                self.load16(*v, R16::HL);
                self.protect_hl = true;
                done[i] = true;
            }
        }
        for (i, (v, t)) in args.iter().zip(&targets).enumerate() {
            if let (false, T::R(r)) = (done[i], t)
                && *r != A
            {
                let s = self.src(*v, 0);
                self.load8(s, *r);
                done[i] = true;
            }
        }
        for (i, (v, t)) in args.iter().zip(&targets).enumerate() {
            if let (false, T::R(A)) = (done[i], t) {
                let s = self.src(*v, 0);
                self.load8(s, A);
            }
        }
        if let Some(t) = text {
            let label = *self.strings.entry(t).or_insert_with(|| {
                match self.a.names.iter().position(|n| n == &format!("str{t}")) {
                    Some(p) => p as Label,
                    None => self.a.new_label(format!("str{t}")),
                }
            });
            self.emit(Ins::Ld16L(R16::DE, label, 0));
        }
        self.protect_a = false;
        self.protect_hl = false;
        self.emit(Ins::Call(None, self.rt.get(name)));
        if let Some(d) = d {
            if self.f.wide(d) {
                self.store16(d);
            } else {
                self.store8(d);
            }
        }
    }
}
