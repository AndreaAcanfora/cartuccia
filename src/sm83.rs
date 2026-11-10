//! An assembler for the Game Boy's CPU, the Sharp SM83: instruction encoding, labels, and jump
//! relaxation (a jump becomes the 2-byte `jr` when its target is close enough, `jp` otherwise).

use std::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum R8 {
    B = 0,
    C = 1,
    D = 2,
    E = 3,
    H = 4,
    L = 5,
    /// `(hl)`: the byte HL points to.
    M = 6,
    A = 7,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum R16 {
    BC = 0,
    DE = 1,
    HL = 2,
    SP = 3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pair {
    BC = 0,
    DE = 1,
    HL = 2,
    AF = 3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cond {
    NZ = 0,
    Z = 1,
    NC = 2,
    C = 3,
}

impl Cond {
    pub fn invert(self) -> Cond {
        match self {
            Cond::NZ => Cond::Z,
            Cond::Z => Cond::NZ,
            Cond::NC => Cond::C,
            Cond::C => Cond::NC,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alu {
    Add = 0,
    Adc = 1,
    Sub = 2,
    Sbc = 3,
    And = 4,
    Xor = 5,
    Or = 6,
    Cp = 7,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rot {
    Rlc = 0,
    Rrc = 1,
    Rl = 2,
    Rr = 3,
    Sla = 4,
    Sra = 5,
    Swap = 6,
    Srl = 7,
}

pub type Label = u32;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Ins {
    Ld(R8, R8),
    Ldi(R8, u8),
    Ld16(R16, u16),
    /// `ld rr, label + offset`
    Ld16L(R16, Label, u16),
    LdAMem(u16),
    LdMemA(u16),
    /// `ldh a, ($FF00 + n)`
    LdhA(u8),
    /// `ldh ($FF00 + n), a`
    LdhW(u8),
    LdAInd(R16),
    LdIndA(R16),
    LdAHli,
    LdAHld,
    LdHliA,
    LdHldA,
    Alu(Alu, R8),
    AluI(Alu, u8),
    Inc(R8),
    Dec(R8),
    Inc16(R16),
    Dec16(R16),
    AddHl(R16),
    Push(Pair),
    Pop(Pair),
    /// A jump: assembled as `jr` when in range, `jp` otherwise.
    Jp(Option<Cond>, Label),
    JpHl,
    Call(Option<Cond>, Label),
    /// `call nn` to a fixed address (the DMA routine in HRAM).
    CallAddr(u16),
    Ret(Option<Cond>),
    Reti,
    Cb(Rot, R8),
    Bit(u8, R8),
    Res(u8, R8),
    Set(u8, R8),
    Rlca,
    Rrca,
    Rla,
    Rra,
    Cpl,
    Scf,
    Ccf,
    Nop,
    Halt,
    Di,
    Ei,
    LdSpHl,
    // pseudo-instructions
    Label(Label),
    Db(Vec<u8>),
    /// A table of label addresses.
    Dw(Vec<Label>),
    /// Pads with zeros up to this address.
    Org(u16),
    Comment(String),
}

#[derive(Default, Clone)]
pub struct Asm {
    pub code: Vec<Ins>,
    pub names: Vec<String>,
}

impl Asm {
    pub fn new_label(&mut self, name: impl Into<String>) -> Label {
        self.names.push(name.into());
        self.names.len() as Label - 1
    }

    pub fn place(&mut self, l: Label) {
        self.code.push(Ins::Label(l));
    }

    pub fn emit(&mut self, i: Ins) {
        self.code.push(i);
    }
}

pub struct Assembled {
    pub bytes: Vec<u8>,
    pub label_addr: Vec<u16>,
    /// Address of each instruction in `Asm::code`.
    pub addr: Vec<u16>,
}

fn r16_name(r: R16) -> &'static str {
    ["bc", "de", "hl", "sp"][r as usize]
}

fn r8_name(r: R8) -> &'static str {
    ["b", "c", "d", "e", "h", "l", "(hl)", "a"][r as usize]
}

fn cond_name(c: Cond) -> &'static str {
    ["nz", "z", "nc", "c"][c as usize]
}

/// Size in bytes; `short` says whether a jump is assembled as `jr`.
fn size(i: &Ins, short: bool) -> usize {
    use Ins::*;
    match i {
        Ld(..) | Alu(..) | Inc(_) | Dec(_) | Inc16(_) | Dec16(_) | AddHl(_) | Push(_) | Pop(_)
        | JpHl | Ret(_) | Reti | Rlca | Rrca | Rla | Rra | Cpl | Scf | Ccf | Nop | Halt | Di
        | Ei | LdSpHl | LdAInd(_) | LdIndA(_) | LdAHli | LdAHld | LdHliA | LdHldA => 1,
        Ldi(..) | AluI(..) | LdhA(_) | LdhW(_) | Cb(..) | Bit(..) | Res(..) | Set(..) => 2,
        Ld16(..) | Ld16L(..) | LdAMem(_) | LdMemA(_) | Call(..) | CallAddr(_) => 3,
        Jp(..) => {
            if short {
                2
            } else {
                3
            }
        }
        Label(_) | Comment(_) | Org(_) => 0,
        Db(b) => b.len(),
        Dw(l) => l.len() * 2,
    }
}

pub fn assemble(asm: &Asm) -> Result<Assembled, String> {
    let n = asm.code.len();
    let mut short = vec![true; n];
    let mut label_addr = vec![0u16; asm.names.len()];
    let mut addr = vec![0u16; n];
    // Relaxation: start with every jump short, lengthen the ones out of range, repeat.
    loop {
        let mut pc: usize = 0;
        for (k, i) in asm.code.iter().enumerate() {
            if let Ins::Org(a) = i {
                if pc > *a as usize {
                    return Err(format!("code overlaps address ${a:04X}"));
                }
                pc = *a as usize;
            }
            addr[k] = pc as u16;
            if let Ins::Label(l) = i {
                label_addr[*l as usize] = pc as u16;
            }
            pc += size(i, short[k]);
            if pc > 0x10000 {
                return Err("the program does not fit in 64 KB".into());
            }
        }
        let mut changed = false;
        for (k, i) in asm.code.iter().enumerate() {
            if let Ins::Jp(_, l) = i
                && short[k]
            {
                let from = addr[k] as i32 + 2;
                let to = label_addr[*l as usize] as i32;
                if !(-128..=127).contains(&(to - from)) {
                    short[k] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut bytes = Vec::new();
    for (k, i) in asm.code.iter().enumerate() {
        while bytes.len() < addr[k] as usize {
            bytes.push(0);
        }
        encode(i, short[k], addr[k], &label_addr, &mut bytes);
    }
    Ok(Assembled {
        bytes,
        label_addr,
        addr,
    })
}

fn encode(i: &Ins, short: bool, pc: u16, labels: &[u16], out: &mut Vec<u8>) {
    use Ins::*;
    let w = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    match i {
        Ld(d, s) => out.push(0x40 | (*d as u8) << 3 | *s as u8),
        Ldi(d, n) => out.extend([0x06 | (*d as u8) << 3, *n]),
        Ld16(r, n) => {
            out.push(0x01 | (*r as u8) << 4);
            w(out, *n);
        }
        Ld16L(r, l, off) => {
            out.push(0x01 | (*r as u8) << 4);
            w(out, labels[*l as usize].wrapping_add(*off));
        }
        LdAMem(a) => {
            out.push(0xFA);
            w(out, *a);
        }
        LdMemA(a) => {
            out.push(0xEA);
            w(out, *a);
        }
        LdhA(n) => out.extend([0xF0, *n]),
        LdhW(n) => out.extend([0xE0, *n]),
        LdAInd(r) => out.push(if *r == R16::BC { 0x0A } else { 0x1A }),
        LdIndA(r) => out.push(if *r == R16::BC { 0x02 } else { 0x12 }),
        LdAHli => out.push(0x2A),
        LdAHld => out.push(0x3A),
        LdHliA => out.push(0x22),
        LdHldA => out.push(0x32),
        Alu(op, r) => out.push(0x80 | (*op as u8) << 3 | *r as u8),
        AluI(op, n) => out.extend([0xC6 | (*op as u8) << 3, *n]),
        Inc(r) => out.push(0x04 | (*r as u8) << 3),
        Dec(r) => out.push(0x05 | (*r as u8) << 3),
        Inc16(r) => out.push(0x03 | (*r as u8) << 4),
        Dec16(r) => out.push(0x0B | (*r as u8) << 4),
        AddHl(r) => out.push(0x09 | (*r as u8) << 4),
        Push(p) => out.push(0xC5 | (*p as u8) << 4),
        Pop(p) => out.push(0xC1 | (*p as u8) << 4),
        Jp(c, l) => {
            let to = labels[*l as usize];
            if short {
                let off = (to as i32 - (pc as i32 + 2)) as i8 as u8;
                out.push(match c {
                    None => 0x18,
                    Some(c) => 0x20 | (*c as u8) << 3,
                });
                out.push(off);
            } else {
                out.push(match c {
                    None => 0xC3,
                    Some(c) => 0xC2 | (*c as u8) << 3,
                });
                w(out, to);
            }
        }
        JpHl => out.push(0xE9),
        Call(c, l) => {
            out.push(match c {
                None => 0xCD,
                Some(c) => 0xC4 | (*c as u8) << 3,
            });
            w(out, labels[*l as usize]);
        }
        CallAddr(a) => {
            out.push(0xCD);
            w(out, *a);
        }
        Ret(c) => out.push(match c {
            None => 0xC9,
            Some(c) => 0xC0 | (*c as u8) << 3,
        }),
        Reti => out.push(0xD9),
        Cb(op, r) => out.extend([0xCB, (*op as u8) << 3 | *r as u8]),
        Bit(b, r) => out.extend([0xCB, 0x40 | b << 3 | *r as u8]),
        Res(b, r) => out.extend([0xCB, 0x80 | b << 3 | *r as u8]),
        Set(b, r) => out.extend([0xCB, 0xC0 | b << 3 | *r as u8]),
        Rlca => out.push(0x07),
        Rrca => out.push(0x0F),
        Rla => out.push(0x17),
        Rra => out.push(0x1F),
        Cpl => out.push(0x2F),
        Scf => out.push(0x37),
        Ccf => out.push(0x3F),
        Nop => out.push(0x00),
        Halt => out.push(0x76),
        Di => out.push(0xF3),
        Ei => out.push(0xFB),
        LdSpHl => out.push(0xF9),
        Label(_) | Comment(_) | Org(_) => {}
        Db(b) => out.extend_from_slice(b),
        Dw(ls) => {
            for l in ls {
                w(out, labels[*l as usize]);
            }
        }
    }
}

/// Assembly text for one instruction.
pub fn text(i: &Ins, names: &[String], short: bool) -> String {
    use Ins::*;
    let l = |x: &u32| names[*x as usize].clone();
    match i {
        Ld(d, s) => format!("ld {},{}", r8_name(*d), r8_name(*s)),
        Ldi(d, n) => format!("ld {},${n:02X}", r8_name(*d)),
        Ld16(r, n) => format!("ld {},${n:04X}", r16_name(*r)),
        Ld16L(r, x, 0) => format!("ld {},{}", r16_name(*r), l(x)),
        Ld16L(r, x, off) => format!("ld {},{}+{off}", r16_name(*r), l(x)),
        LdAMem(a) => format!("ld a,(${a:04X})"),
        LdMemA(a) => format!("ld (${a:04X}),a"),
        LdhA(n) => format!("ldh a,(${:04X})", 0xFF00 | *n as u16),
        LdhW(n) => format!("ldh (${:04X}),a", 0xFF00 | *n as u16),
        LdAInd(r) => format!("ld a,({})", r16_name(*r)),
        LdIndA(r) => format!("ld ({}),a", r16_name(*r)),
        LdAHli => "ld a,(hl+)".into(),
        LdAHld => "ld a,(hl-)".into(),
        LdHliA => "ld (hl+),a".into(),
        LdHldA => "ld (hl-),a".into(),
        Alu(op, r) => format!("{} {}", alu_name(*op), r8_name(*r)),
        AluI(op, n) => format!("{} ${n:02X}", alu_name(*op)),
        Inc(r) => format!("inc {}", r8_name(*r)),
        Dec(r) => format!("dec {}", r8_name(*r)),
        Inc16(r) => format!("inc {}", r16_name(*r)),
        Dec16(r) => format!("dec {}", r16_name(*r)),
        AddHl(r) => format!("add hl,{}", r16_name(*r)),
        Push(p) => format!("push {}", ["bc", "de", "hl", "af"][*p as usize]),
        Pop(p) => format!("pop {}", ["bc", "de", "hl", "af"][*p as usize]),
        Jp(c, x) => {
            let op = if short { "jr" } else { "jp" };
            match c {
                Some(c) => format!("{op} {},{}", cond_name(*c), l(x)),
                None => format!("{op} {}", l(x)),
            }
        }
        JpHl => "jp hl".into(),
        Call(c, x) => match c {
            Some(c) => format!("call {},{}", cond_name(*c), l(x)),
            None => format!("call {}", l(x)),
        },
        CallAddr(a) => format!("call ${a:04X}"),
        Ret(c) => match c {
            Some(c) => format!("ret {}", cond_name(*c)),
            None => "ret".into(),
        },
        Reti => "reti".into(),
        Cb(op, r) => format!(
            "{} {}",
            ["rlc", "rrc", "rl", "rr", "sla", "sra", "swap", "srl"][*op as usize],
            r8_name(*r)
        ),
        Bit(b, r) => format!("bit {b},{}", r8_name(*r)),
        Res(b, r) => format!("res {b},{}", r8_name(*r)),
        Set(b, r) => format!("set {b},{}", r8_name(*r)),
        Rlca => "rlca".into(),
        Rrca => "rrca".into(),
        Rla => "rla".into(),
        Rra => "rra".into(),
        Cpl => "cpl".into(),
        Scf => "scf".into(),
        Ccf => "ccf".into(),
        Nop => "nop".into(),
        Halt => "halt".into(),
        Di => "di".into(),
        Ei => "ei".into(),
        LdSpHl => "ld sp,hl".into(),
        Label(x) => format!("{}:", l(x)),
        Db(b) => {
            let shown: Vec<String> = b.iter().take(8).map(|x| format!("${x:02X}")).collect();
            let more = if b.len() > 8 {
                format!(" ... ({} bytes)", b.len())
            } else {
                String::new()
            };
            format!("db {}{more}", shown.join(","))
        }
        Dw(ls) => format!("dw {}", ls.iter().map(l).collect::<Vec<_>>().join(",")),
        Org(a) => format!("org ${a:04X}"),
        Comment(c) => format!("; {c}"),
    }
}

fn alu_name(op: Alu) -> &'static str {
    [
        "add a,", "adc a,", "sub", "sbc a,", "and", "xor", "or", "cp",
    ][op as usize]
}

/// The program as text: address, bytes and instruction, for a range of `asm.code`.
pub fn listing(asm: &Asm, out: &Assembled, range: std::ops::Range<usize>) -> String {
    let mut s = String::new();
    for k in range {
        let i = &asm.code[k];
        match i {
            Ins::Label(_) => {
                let _ = writeln!(s, "{}", text(i, &asm.names, false));
            }
            Ins::Comment(c) => {
                let _ = writeln!(s, "                  ; {c}");
            }
            Ins::Org(_) => {}
            _ => {
                let short = matches!(i, Ins::Jp(..)) && {
                    let a = out.addr[k] as usize;
                    let b = out.bytes.get(a).copied().unwrap_or(0);
                    b == 0x18 || (b & 0xE7) == 0x20
                };
                let a = out.addr[k] as usize;
                let len = size(i, short).min(4);
                let bytes: Vec<String> = out.bytes[a..a + len]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                let more = if size(i, short) > 4 { "+" } else { " " };
                let _ = writeln!(
                    s,
                    "  {:04X}  {:<11}{more} {}",
                    a,
                    bytes.join(" "),
                    text(i, &asm.names, short)
                );
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_relaxes() {
        let mut a = Asm::default();
        let top = a.new_label("top");
        let far = a.new_label("far");
        a.place(top);
        a.emit(Ins::Ldi(R8::A, 5));
        a.emit(Ins::Ld(R8::B, R8::A));
        a.emit(Ins::Jp(Some(Cond::NZ), top));
        a.emit(Ins::Jp(None, far));
        a.emit(Ins::Db(vec![0; 300]));
        a.place(far);
        a.emit(Ins::Cb(Rot::Swap, R8::A));
        let out = assemble(&a).unwrap();
        assert_eq!(&out.bytes[0..5], &[0x3E, 0x05, 0x47, 0x20, 0xFB]);
        assert_eq!(out.bytes[5], 0xC3);
        assert_eq!(out.label_addr[far as usize], 8 + 300);
        assert_eq!(&out.bytes[308..310], &[0xCB, 0x37]);
    }
}
