//! The intermediate representation: each function is a control-flow graph of basic blocks,
//! with three-address instructions on virtual registers of 8 or 16 bits.
//!
//! The optimizer rewrites this form; the code generator turns it into SM83 machine code.

use std::fmt::{self, Write};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Reg(pub u32);

pub type BlockId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Val {
    R(Reg),
    /// A constant, already masked to the width where it is used.
    K(u16),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Bin {
    Add,
    Sub,
    Mul,
    DivU,
    DivS,
    ModU,
    ModS,
    And,
    Or,
    Xor,
    Shl,
    ShrU,
    ShrS,
}

impl Bin {
    pub fn name(self) -> &'static str {
        match self {
            Bin::Add => "add",
            Bin::Sub => "sub",
            Bin::Mul => "mul",
            Bin::DivU => "divu",
            Bin::DivS => "divs",
            Bin::ModU => "modu",
            Bin::ModS => "mods",
            Bin::And => "and",
            Bin::Or => "or",
            Bin::Xor => "xor",
            Bin::Shl => "shl",
            Bin::ShrU => "shru",
            Bin::ShrS => "shrs",
        }
    }

    pub fn is_shift(self) -> bool {
        matches!(self, Bin::Shl | Bin::ShrU | Bin::ShrS)
    }

    pub fn commutative(self) -> bool {
        matches!(self, Bin::Add | Bin::Mul | Bin::And | Bin::Or | Bin::Xor)
    }
}

/// Comparisons. `a > b` is lowered as `b < a`, so four orderings are enough.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cc {
    Eq,
    Ne,
    LtU,
    LeU,
    LtS,
    LeS,
}

impl Cc {
    pub fn name(self) -> &'static str {
        match self {
            Cc::Eq => "eq",
            Cc::Ne => "ne",
            Cc::LtU => "ltu",
            Cc::LeU => "leu",
            Cc::LtS => "lts",
            Cc::LeS => "les",
        }
    }
}

/// Calls into the runtime library.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Sys {
    /// The buttons held now (a byte of `builtins::BUTTONS` bits).
    Joy,
    /// The buttons that went down this frame.
    JoyPressed,
    Spr,
    Hide,
    Tile,
    GetTile,
    Text,
    Num,
    Cls,
    Rnd,
    Beep,
    Noise,
    Scroll,
    Frames,
    Vsync,
    DebugU8,
    DebugI8,
    DebugU16,
    DebugI16,
    Stop,
}

impl Sys {
    pub fn name(self) -> &'static str {
        match self {
            Sys::Joy => "joy",
            Sys::JoyPressed => "joypressed",
            Sys::Spr => "spr",
            Sys::Hide => "hide",
            Sys::Tile => "tile",
            Sys::GetTile => "gettile",
            Sys::Text => "text",
            Sys::Num => "num",
            Sys::Cls => "cls",
            Sys::Rnd => "rnd",
            Sys::Beep => "beep",
            Sys::Noise => "noise",
            Sys::Scroll => "scroll",
            Sys::Frames => "frames",
            Sys::Vsync => "vsync",
            Sys::DebugU8 => "debug_u8",
            Sys::DebugI8 => "debug_i8",
            Sys::DebugU16 => "debug_u16",
            Sys::DebugI16 => "debug_i16",
            Sys::Stop => "stop",
        }
    }

    /// Widths of the arguments (true = 16 bits) and of the result.
    pub fn signature(self) -> (&'static [bool], Option<bool>) {
        match self {
            Sys::Joy | Sys::JoyPressed => (&[], Some(false)),
            Sys::Spr => (&[false; 4], None),
            Sys::Hide | Sys::Noise | Sys::DebugU8 | Sys::DebugI8 => (&[false], None),
            Sys::Tile => (&[false; 3], None),
            Sys::GetTile => (&[false; 2], Some(false)),
            Sys::Text | Sys::Scroll | Sys::Beep => (&[false; 2], None),
            Sys::Num => (&[false, false, true], None),
            Sys::Cls | Sys::Vsync | Sys::Stop => (&[], None),
            Sys::Rnd => (&[false], Some(false)),
            Sys::Frames => (&[], Some(true)),
            Sys::DebugU16 | Sys::DebugI16 => (&[true], None),
        }
    }

    /// Whether removing an unused call changes nothing.
    pub fn pure(self) -> bool {
        matches!(
            self,
            Sys::Joy | Sys::JoyPressed | Sys::GetTile | Sys::Frames
        )
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Inst {
    Mov(Reg, Val),
    Bin(Bin, Reg, Val, Val),
    Neg(Reg, Val),
    Not(Reg, Val),
    /// `d = a cc b` (0 or 1); `wide` is the width of a and b.
    Cmp(Cc, Reg, Val, Val, bool),
    Zext(Reg, Val),
    Sext(Reg, Val),
    Trunc(Reg, Val),
    LoadG(Reg, usize),
    StoreG(usize, Val),
    LoadA(Reg, usize, Val),
    StoreA(usize, Val, Val),
    Call(Option<Reg>, usize, Vec<Val>),
    /// A runtime call; the string index is for `text()`.
    Sys(Option<Reg>, Sys, Vec<Val>, Option<usize>),
}

impl Inst {
    pub fn def(&self) -> Option<Reg> {
        match self {
            Inst::Mov(d, _)
            | Inst::Bin(_, d, ..)
            | Inst::Neg(d, _)
            | Inst::Not(d, _)
            | Inst::Cmp(_, d, ..)
            | Inst::Zext(d, _)
            | Inst::Sext(d, _)
            | Inst::Trunc(d, _)
            | Inst::LoadG(d, _)
            | Inst::LoadA(d, ..) => Some(*d),
            Inst::Call(d, ..) | Inst::Sys(d, ..) => *d,
            Inst::StoreG(..) | Inst::StoreA(..) => None,
        }
    }

    pub fn uses(&self) -> Vec<Val> {
        match self {
            Inst::Mov(_, a)
            | Inst::Neg(_, a)
            | Inst::Not(_, a)
            | Inst::Zext(_, a)
            | Inst::Sext(_, a)
            | Inst::Trunc(_, a)
            | Inst::StoreG(_, a) => vec![*a],
            Inst::Bin(_, _, a, b) | Inst::Cmp(_, _, a, b, _) | Inst::StoreA(_, a, b) => {
                vec![*a, *b]
            }
            Inst::LoadA(_, _, i) => vec![*i],
            Inst::LoadG(..) => vec![],
            Inst::Call(_, _, args) | Inst::Sys(_, _, args, _) => args.clone(),
        }
    }

    pub fn uses_mut(&mut self) -> Vec<&mut Val> {
        match self {
            Inst::Mov(_, a)
            | Inst::Neg(_, a)
            | Inst::Not(_, a)
            | Inst::Zext(_, a)
            | Inst::Sext(_, a)
            | Inst::Trunc(_, a)
            | Inst::StoreG(_, a) => vec![a],
            Inst::Bin(_, _, a, b) | Inst::Cmp(_, _, a, b, _) | Inst::StoreA(_, a, b) => {
                vec![a, b]
            }
            Inst::LoadA(_, _, i) => vec![i],
            Inst::LoadG(..) => vec![],
            Inst::Call(_, _, args) | Inst::Sys(_, _, args, _) => args.iter_mut().collect(),
        }
    }

    pub fn def_mut(&mut self) -> Option<&mut Reg> {
        match self {
            Inst::Mov(d, _)
            | Inst::Bin(_, d, ..)
            | Inst::Neg(d, _)
            | Inst::Not(d, _)
            | Inst::Cmp(_, d, ..)
            | Inst::Zext(d, _)
            | Inst::Sext(d, _)
            | Inst::Trunc(d, _)
            | Inst::LoadG(d, _)
            | Inst::LoadA(d, ..) => Some(d),
            Inst::Call(d, ..) | Inst::Sys(d, ..) => d.as_mut(),
            Inst::StoreG(..) | Inst::StoreA(..) => None,
        }
    }

    /// Whether the instruction does something besides writing its destination.
    pub fn has_effect(&self) -> bool {
        match self {
            Inst::StoreG(..) | Inst::StoreA(..) | Inst::Call(..) => true,
            Inst::Sys(_, s, ..) => !s.pure(),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ins {
    pub k: Inst,
    /// Source line, for the annotated assembly listing.
    pub line: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Term {
    Jmp(BlockId),
    /// Branch on a bool: to the first block when it is 1.
    Br(Val, BlockId, BlockId),
    Ret(Option<Val>),
}

impl Term {
    pub fn succs(&self) -> Vec<BlockId> {
        match self {
            Term::Jmp(b) => vec![*b],
            Term::Br(_, t, f) => vec![*t, *f],
            Term::Ret(_) => vec![],
        }
    }

    pub fn succs_mut(&mut self) -> Vec<&mut BlockId> {
        match self {
            Term::Jmp(b) => vec![b],
            Term::Br(_, t, f) => vec![t, f],
            Term::Ret(_) => vec![],
        }
    }

    pub fn uses(&self) -> Vec<Val> {
        match self {
            Term::Br(v, ..) | Term::Ret(Some(v)) => vec![*v],
            _ => vec![],
        }
    }

    pub fn uses_mut(&mut self) -> Vec<&mut Val> {
        match self {
            Term::Br(v, ..) | Term::Ret(Some(v)) => vec![v],
            _ => vec![],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub insts: Vec<Ins>,
    pub term: Term,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub struct RegInfo {
    pub wide: bool,
    /// The source variable it holds, or empty for a temporary.
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    pub params: Vec<Reg>,
    /// The return width (true = 16 bits), or `None` for no value.
    pub ret: Option<bool>,
    pub regs: Vec<RegInfo>,
    /// Block 0 is the entry.
    pub blocks: Vec<Block>,
}

impl Func {
    pub fn wide(&self, r: Reg) -> bool {
        self.regs[r.0 as usize].wide
    }

    pub fn new_reg(&mut self, wide: bool, name: &str) -> Reg {
        self.regs.push(RegInfo {
            wide,
            name: name.to_string(),
        });
        Reg(self.regs.len() as u32 - 1)
    }

    pub fn preds(&self) -> Vec<Vec<BlockId>> {
        let mut preds = vec![Vec::new(); self.blocks.len()];
        for (b, block) in self.blocks.iter().enumerate() {
            for s in block.term.succs() {
                if !preds[s].contains(&b) {
                    preds[s].push(b);
                }
            }
        }
        preds
    }

    pub fn size(&self) -> usize {
        self.blocks.iter().map(|b| b.insts.len() + 1).sum()
    }
}

#[derive(Clone, Debug)]
pub struct GlobalVar {
    pub name: String,
    pub wide: bool,
}

#[derive(Clone, Debug)]
pub struct Array {
    pub name: String,
    pub wide: bool,
    pub len: u32,
    /// Contents of a `const` array, stored in ROM.
    pub rom: Option<Vec<u16>>,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub funcs: Vec<Func>,
    pub globals: Vec<GlobalVar>,
    pub arrays: Vec<Array>,
    pub strings: Vec<String>,
    pub tiles: Vec<crate::gfx::Tile>,
    /// The function that sets the globals' initial values.
    pub init_globals: usize,
    pub init: Option<usize>,
    pub update: Option<usize>,
}

impl Module {
    /// Whether `r` in function `f` is 16 bits wide; constants take the width of their context.
    pub fn listing(&self) -> String {
        let mut out = String::new();
        for f in &self.funcs {
            let _ = write!(out, "{}", FuncDisplay(self, f));
        }
        out
    }
}

pub struct FuncDisplay<'a>(pub &'a Module, pub &'a Func);

impl fmt::Display for FuncDisplay<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (m, f) = (self.0, self.1);
        let reg = |r: Reg| {
            let info = &f.regs[r.0 as usize];
            let w = if info.wide { "w" } else { "" };
            if info.name.is_empty() {
                format!("%{}{w}", r.0)
            } else {
                format!("%{}{w}.{}", r.0, info.name)
            }
        };
        let val = |v: Val| match v {
            Val::R(r) => reg(r),
            Val::K(k) => k.to_string(),
        };
        let params: Vec<String> = f.params.iter().map(|&p| reg(p)).collect();
        writeln!(out, "function {}({}):", f.name, params.join(", "))?;
        for (b, block) in f.blocks.iter().enumerate() {
            writeln!(out, "  b{b}:")?;
            for ins in &block.insts {
                let text = match &ins.k {
                    Inst::Mov(d, a) => format!("{} = {}", reg(*d), val(*a)),
                    Inst::Bin(op, d, a, b) => {
                        format!("{} = {} {}, {}", reg(*d), op.name(), val(*a), val(*b))
                    }
                    Inst::Neg(d, a) => format!("{} = neg {}", reg(*d), val(*a)),
                    Inst::Not(d, a) => format!("{} = not {}", reg(*d), val(*a)),
                    Inst::Cmp(cc, d, a, b, _) => {
                        format!("{} = {} {}, {}", reg(*d), cc.name(), val(*a), val(*b))
                    }
                    Inst::Zext(d, a) => format!("{} = zext {}", reg(*d), val(*a)),
                    Inst::Sext(d, a) => format!("{} = sext {}", reg(*d), val(*a)),
                    Inst::Trunc(d, a) => format!("{} = trunc {}", reg(*d), val(*a)),
                    Inst::LoadG(d, g) => format!("{} = {}", reg(*d), m.globals[*g].name),
                    Inst::StoreG(g, a) => format!("{} = {}", m.globals[*g].name, val(*a)),
                    Inst::LoadA(d, a, i) => {
                        format!("{} = {}[{}]", reg(*d), m.arrays[*a].name, val(*i))
                    }
                    Inst::StoreA(a, i, v) => {
                        format!("{}[{}] = {}", m.arrays[*a].name, val(*i), val(*v))
                    }
                    Inst::Call(d, func, args) => {
                        let args: Vec<String> = args.iter().map(|a| val(*a)).collect();
                        let call = format!("call {}({})", m.funcs[*func].name, args.join(", "));
                        match d {
                            Some(d) => format!("{} = {call}", reg(*d)),
                            None => call,
                        }
                    }
                    Inst::Sys(d, s, args, text) => {
                        let mut args: Vec<String> = args.iter().map(|a| val(*a)).collect();
                        if let Some(t) = text {
                            args.push(format!("{:?}", m.strings[*t]));
                        }
                        let call = format!("{}({})", s.name(), args.join(", "));
                        match d {
                            Some(d) => format!("{} = {call}", reg(*d)),
                            None => call,
                        }
                    }
                };
                writeln!(out, "    {text}")?;
            }
            let term = match &block.term {
                Term::Jmp(t) => format!("jmp b{t}"),
                Term::Br(v, t, e) => format!("br {}, b{t}, b{e}", val(*v)),
                Term::Ret(Some(v)) => format!("ret {}", val(*v)),
                Term::Ret(None) => "ret".into(),
            };
            writeln!(out, "    {term}")?;
        }
        Ok(())
    }
}
