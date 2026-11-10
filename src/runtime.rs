//! The runtime library every ROM carries, written with the assembler: start-up, the frame loop,
//! joypad, sprites, the background tile queue, text, numbers, sound, random numbers and the
//! arithmetic the SM83 lacks (multiply, divide, shifts by a variable count).
//!
//! Memory map:
//!
//! | address       | contents                                              |
//! |---------------|-------------------------------------------------------|
//! | `C000-C09F`   | sprite table, copied to the PPU by DMA every frame    |
//! | `C100-C18F`   | background writes waiting for the next VBlank         |
//! | `C200-C5FF`   | copy of the 32x32 background map, for `gettile()`     |
//! | `C600-DDFF`   | the game's variables, arrays and function frames      |
//! | `DE00-DFFF`   | stack (return addresses only: frames are static)      |
//! | `FF80-FF89`   | the DMA routine (the CPU can only run from HRAM during DMA) |
//! | `FF8A-FFA3`   | runtime variables                                     |
//! | `FFA4-FFFE`   | the game's hottest variables (`ldh` is faster)        |

use crate::sm83::{Alu, Asm, Cond, Ins::*, Label, R8::*, R16, Rot};
use std::collections::HashMap;

pub const OAM_SHADOW: u16 = 0xC000;
pub const QUEUE: u16 = 0xC100;
/// Background writes per frame: what fits in VBlank together with the sprite DMA.
pub const QUEUE_MAX: u8 = 48;
pub const MAP: u16 = 0xC200;
pub const USER_RAM: u16 = 0xC600;
pub const USER_RAM_END: u16 = 0xDE00;
pub const STACK_TOP: u16 = 0xE000;

pub const H_DMA: u8 = 0x80;
pub const H_VBL: u8 = 0x8A;
pub const H_LCDOFF: u8 = 0x8B;
pub const H_JOY: u8 = 0x8C;
pub const H_JOYNEW: u8 = 0x8D;
pub const H_FRAME: u8 = 0x8E;
pub const H_SCX: u8 = 0x90;
pub const H_SCY: u8 = 0x91;
pub const H_RNG: u8 = 0x92;
pub const H_QLEN: u8 = 0x94;
pub const H_CNT: u8 = 0x95;
pub const H_X0: u8 = 0x96;
pub const H_X: u8 = 0x97;
pub const H_Y: u8 = 0x98;
pub const H_NUM: u8 = 0x9B; // 5 digits
pub const H_SIGN: u8 = 0xA0;
pub const H_DSIGN: u8 = 0xA1;
pub const HRAM_USER: u8 = 0xA4;
pub const HRAM_USER_END: u8 = 0xFF; // exclusive: FFFF is the interrupt enable register

const LCDC: u8 = 0x40;
const LY: u8 = 0x44;
const LCDC_ON: u8 = 0x93; // LCD on, tiles at $8000, sprites on, background on

/// Runtime entry points the code generator calls, by name.
pub struct Runtime {
    pub labels: HashMap<&'static str, Label>,
}

impl Runtime {
    pub fn get(&self, name: &str) -> Label {
        *self
            .labels
            .get(name)
            .unwrap_or_else(|| panic!("no runtime routine {name}"))
    }
}

/// The frequency register value of 72 notes from C3: 2048 - 131072 / f.
fn note_table() -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..72 {
        let f = 130.8128 * 2f64.powf(n as f64 / 12.0);
        let x = (2048.0 - 131072.0 / f).round() as u16;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Labels the code generator must define: the game's entry points and data.
pub struct Hooks {
    pub init_globals: Label,
    pub init: Option<Label>,
    pub update: Option<Label>,
    pub gfx: Label,
    pub gfx_tiles: u16,
}

/// Emits the vectors, the start-up code and every routine. Code starts at `$0150`.
pub fn emit(a: &mut Asm, hooks: &Hooks) -> Runtime {
    let mut labels = HashMap::new();
    let mut l = |a: &mut Asm, name: &'static str| -> Label {
        let label = a.new_label(name);
        labels.insert(name, label);
        label
    };
    let names = [
        "start",
        "vblank",
        "rt_lcd_off",
        "rt_vsync",
        "rt_joy",
        "rt_flush",
        "rt_wait_flush",
        "rt_tile",
        "rt_gettile",
        "rt_cls",
        "rt_text",
        "rt_num",
        "num_digit",
        "rt_spr",
        "rt_hide",
        "rt_rnd",
        "rt_beep",
        "rt_noise",
        "rt_scroll",
        "rt_debug_u8",
        "rt_debug_i8",
        "rt_debug_u16",
        "rt_debug_i16",
        "serial_send",
        "rt_stop",
        "rt_mul16",
        "rt_divu16",
        "rt_divs16",
        "rt_modu16",
        "rt_mods16",
        "neg_hl",
        "neg_de",
        "rt_shl8",
        "rt_shr8",
        "rt_sar8",
        "rt_shl16",
        "rt_shr16",
        "rt_sar16",
        "rt_memcpy",
        "rt_fill",
        "font",
        "notes",
        "map_offset",
    ];
    for name in names {
        l(a, name);
    }
    let rt = Runtime { labels };
    let g = |name: &str| rt.get(name);
    let local = |a: &mut Asm, name: &str| a.new_label(name);

    // Restart and interrupt vectors.
    a.emit(Org(0x40));
    a.place(g("vblank"));
    a.emit(Push(crate::sm83::Pair::AF));
    a.emit(Ldi(A, 1));
    a.emit(LdhW(H_VBL));
    a.emit(Pop(crate::sm83::Pair::AF));
    a.emit(Reti);
    for vector in [0x48, 0x50, 0x58, 0x60] {
        a.emit(Org(vector));
        a.emit(Reti);
    }
    a.emit(Org(0x100));
    a.emit(Nop);
    a.emit(Jp(None, g("start")));
    a.emit(Org(0x150));

    // ---- start-up
    a.emit(Comment(
        "start-up: clear memory, load the font and graphics, call the game".into(),
    ));
    a.place(g("start"));
    a.emit(Di);
    a.emit(Ld16(R16::SP, STACK_TOP));
    a.emit(Call(None, g("rt_lcd_off")));
    // clear WRAM, up to the stack (which holds this call's return address)
    a.emit(Ld16(R16::HL, 0xC000));
    a.emit(Ldi(B, ((USER_RAM_END - 0xC000) >> 8) as u8));
    a.emit(Alu(Alu::Xor, A));
    a.emit(Call(None, g("rt_fill")));
    // clear HRAM
    a.emit(Ld16(R16::HL, 0xFF80));
    a.emit(Ldi(B, 0x7F));
    let hclear = local(a, "start.hclear");
    a.place(hclear);
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdHliA);
    a.emit(Dec(B));
    a.emit(Jp(Some(Cond::NZ), hclear));
    // the DMA routine: ld a,$C0; ldh ($46),a; ld a,40; .w: dec a; jr nz,.w; ret
    let dma = [
        0x3E,
        (OAM_SHADOW >> 8) as u8,
        0xE0,
        0x46,
        0x3E,
        0x28,
        0x3D,
        0x20,
        0xFD,
        0xC9,
    ];
    for (k, byte) in dma.iter().enumerate() {
        a.emit(Ldi(A, *byte));
        a.emit(LdhW(H_DMA + k as u8));
    }
    // clear the tile data (the maps are filled below)
    a.emit(Ld16(R16::HL, 0x8000));
    a.emit(Ldi(B, 0x18));
    a.emit(Alu(Alu::Xor, A));
    a.emit(Call(None, g("rt_fill")));
    // font into tiles $20-$7F, each row twice: color 3 on color 0
    a.emit(Ld16(R16::HL, 0x8200));
    a.emit(Ld16L(R16::DE, g("font"), 0));
    a.emit(Ld16(R16::BC, 96 * 8));
    let font = local(a, "start.font");
    a.place(font);
    a.emit(LdAInd(R16::DE));
    a.emit(Inc16(R16::DE));
    a.emit(LdHliA);
    a.emit(LdHliA);
    a.emit(Dec16(R16::BC));
    a.emit(Ld(A, B));
    a.emit(Alu(Alu::Or, C));
    a.emit(Jp(Some(Cond::NZ), font));
    // graphics into tiles $80 and up
    if hooks.gfx_tiles > 0 {
        a.emit(Ld16(R16::HL, 0x8800));
        a.emit(Ld16L(R16::DE, hooks.gfx, 0));
        a.emit(Ld16(R16::BC, hooks.gfx_tiles * 16));
        a.emit(Call(None, g("rt_memcpy")));
    }
    // background and its copy: spaces
    for base in [0x9800u16, MAP] {
        a.emit(Ld16(R16::HL, base));
        a.emit(Ldi(B, 4));
        a.emit(Ldi(A, 0x20));
        a.emit(Call(None, g("rt_fill")));
    }
    a.emit(CallAddr(0xFF00 | H_DMA as u16)); // the sprite table is all zeros: clears OAM
    // palettes, sound, scroll
    a.emit(Ldi(A, 0xE4));
    for reg in [0x47, 0x48, 0x49] {
        a.emit(LdhW(reg));
    }
    a.emit(Ldi(A, 0x80));
    a.emit(LdhW(0x26)); // NR52: sound on
    a.emit(Ldi(A, 0xFF));
    a.emit(LdhW(0x25)); // NR51: every channel on both sides
    a.emit(Ldi(A, 0x77));
    a.emit(LdhW(0x24)); // NR50: full volume
    a.emit(Ldi(A, 0xE1));
    a.emit(LdhW(H_RNG));
    a.emit(Ldi(A, 0xAC));
    a.emit(LdhW(H_RNG + 1));
    a.emit(Ldi(A, 1));
    a.emit(LdhW(H_LCDOFF));
    a.emit(LdhW(0xFF)); // IE: VBlank
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdhW(0x0F)); // IF
    a.emit(Ei);
    a.emit(Call(None, hooks.init_globals));
    if let Some(init) = hooks.init {
        a.emit(Call(None, init));
    }
    let frame = local(a, "main.frame");
    a.place(frame);
    a.emit(Call(None, g("rt_vsync")));
    if let Some(update) = hooks.update {
        a.emit(Call(None, update));
    }
    a.emit(Jp(None, frame));

    // ---- LCD off, waiting for VBlank first (turning it off mid-frame can damage a real LCD)
    a.place(g("rt_lcd_off"));
    a.emit(LdhA(LCDC));
    a.emit(Bit(7, A));
    a.emit(Ret(Some(Cond::Z)));
    let wait = local(a, "lcd_off.wait");
    a.place(wait);
    a.emit(LdhA(LY));
    a.emit(AluI(Alu::Cp, 144));
    a.emit(Jp(Some(Cond::C), wait));
    a.emit(LdhA(LCDC));
    a.emit(Res(7, A));
    a.emit(LdhW(LCDC));
    a.emit(Ldi(A, 1));
    a.emit(LdhW(H_LCDOFF));
    a.emit(Ret(None));

    // ---- vsync: wait for VBlank, then sprites, background writes, scroll, joypad, frame count
    a.emit(Comment(
        "vsync: everything that must happen in VBlank".into(),
    ));
    a.place(g("rt_vsync"));
    a.emit(LdhA(H_LCDOFF));
    a.emit(Alu(Alu::Or, A));
    let on = local(a, "vsync.on");
    a.emit(Jp(Some(Cond::Z), on));
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdhW(H_LCDOFF));
    a.emit(Ldi(A, LCDC_ON));
    a.emit(LdhW(LCDC));
    a.place(on);
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdhW(H_VBL));
    let vwait = local(a, "vsync.wait");
    a.place(vwait);
    a.emit(Halt);
    a.emit(LdhA(H_VBL));
    a.emit(Alu(Alu::Or, A));
    a.emit(Jp(Some(Cond::Z), vwait));
    a.emit(CallAddr(0xFF00 | H_DMA as u16));
    a.emit(Call(None, g("rt_flush")));
    a.emit(LdhA(H_SCX));
    a.emit(LdhW(0x43));
    a.emit(LdhA(H_SCY));
    a.emit(LdhW(0x42));
    a.emit(Call(None, g("rt_joy")));
    a.emit(Ld16(R16::HL, 0xFF00 | H_FRAME as u16));
    a.emit(Inc(M));
    a.emit(Ret(Some(Cond::NZ)));
    a.emit(Inc16(R16::HL));
    a.emit(Inc(M));
    a.emit(Ret(None));

    // ---- joypad: held buttons in H_JOY, newly pressed in H_JOYNEW
    a.place(g("rt_joy"));
    a.emit(Ldi(A, 0x20));
    a.emit(LdhW(0x00));
    a.emit(LdhA(0x00));
    a.emit(LdhA(0x00));
    a.emit(Cpl);
    a.emit(AluI(Alu::And, 0x0F));
    a.emit(Cb(Rot::Swap, A));
    a.emit(Ld(B, A));
    a.emit(Ldi(A, 0x10));
    a.emit(LdhW(0x00));
    for _ in 0..6 {
        a.emit(LdhA(0x00));
    }
    a.emit(Cpl);
    a.emit(AluI(Alu::And, 0x0F));
    a.emit(Alu(Alu::Or, B));
    a.emit(Ld(B, A));
    a.emit(Ldi(A, 0x30));
    a.emit(LdhW(0x00));
    a.emit(LdhA(H_JOY));
    a.emit(Cpl);
    a.emit(Alu(Alu::And, B));
    a.emit(LdhW(H_JOYNEW));
    a.emit(Ld(A, B));
    a.emit(LdhW(H_JOY));
    a.emit(Ret(None));

    // ---- background queue: (address, tile) triples written in VBlank
    a.place(g("rt_flush"));
    a.emit(LdhA(H_QLEN));
    a.emit(Alu(Alu::Or, A));
    a.emit(Ret(Some(Cond::Z)));
    a.emit(Ld(B, A));
    a.emit(Ld16(R16::HL, QUEUE));
    let fl = local(a, "flush.loop");
    a.place(fl);
    a.emit(LdAHli);
    a.emit(Ld(E, A));
    a.emit(LdAHli);
    a.emit(Ld(D, A));
    a.emit(LdAHli);
    a.emit(LdIndA(R16::DE));
    a.emit(Dec(B));
    a.emit(Jp(Some(Cond::NZ), fl));
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdhW(H_QLEN));
    a.emit(Ret(None));

    a.place(g("rt_wait_flush"));
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdhW(H_VBL));
    let wf = local(a, "wait_flush.wait");
    a.place(wf);
    a.emit(Halt);
    a.emit(LdhA(H_VBL));
    a.emit(Alu(Alu::Or, A));
    a.emit(Jp(Some(Cond::Z), wf));
    a.emit(Jp(None, g("rt_flush")));

    // map_offset: B = column, C = row -> HL = (row & 31) * 32 + (column & 31), D = H
    a.place(g("map_offset"));
    a.emit(Ld(A, C));
    a.emit(AluI(Alu::And, 31));
    a.emit(Ld(L, A));
    a.emit(Ldi(H, 0));
    for _ in 0..5 {
        a.emit(AddHl(R16::HL));
    }
    a.emit(Ld(A, B));
    a.emit(AluI(Alu::And, 31));
    a.emit(Alu(Alu::Or, L));
    a.emit(Ld(L, A));
    a.emit(Ld(D, H));
    a.emit(Ret(None));

    // tile: B = column, C = row, A = tile
    a.emit(Comment(
        "tile(column, row, t): write the copy, then VRAM now or in the next VBlank".into(),
    ));
    a.place(g("rt_tile"));
    a.emit(Ld(E, A));
    a.emit(Call(None, g("map_offset")));
    a.emit(Ld(A, D));
    a.emit(AluI(Alu::Add, (MAP >> 8) as u8));
    a.emit(Ld(H, A));
    a.emit(Ld(A, M));
    a.emit(Alu(Alu::Cp, E));
    a.emit(Ret(Some(Cond::Z)));
    a.emit(Ld(M, E));
    a.emit(Ld(A, D));
    a.emit(AluI(Alu::Add, 0x98));
    a.emit(Ld(H, A));
    a.emit(LdhA(H_LCDOFF));
    a.emit(Alu(Alu::Or, A));
    let queue = local(a, "tile.queue");
    a.emit(Jp(Some(Cond::Z), queue));
    a.emit(Ld(M, E));
    a.emit(Ret(None));
    a.place(queue);
    a.emit(LdhA(H_QLEN));
    a.emit(AluI(Alu::Cp, QUEUE_MAX));
    let room = local(a, "tile.room");
    a.emit(Jp(Some(Cond::C), room));
    a.emit(Push(crate::sm83::Pair::HL));
    a.emit(Push(crate::sm83::Pair::DE));
    a.emit(Call(None, g("rt_wait_flush")));
    a.emit(Pop(crate::sm83::Pair::DE));
    a.emit(Pop(crate::sm83::Pair::HL));
    a.emit(Alu(Alu::Xor, A));
    a.place(room);
    a.emit(Ld(C, A));
    a.emit(Inc(A));
    a.emit(LdhW(H_QLEN));
    a.emit(Ld(A, C));
    a.emit(Alu(Alu::Add, A));
    a.emit(Alu(Alu::Add, C));
    a.emit(Ld(C, A));
    a.emit(Ldi(B, (QUEUE >> 8) as u8));
    a.emit(Ld(A, L));
    a.emit(LdIndA(R16::BC));
    a.emit(Inc(C));
    a.emit(Ld(A, H));
    a.emit(LdIndA(R16::BC));
    a.emit(Inc(C));
    a.emit(Ld(A, E));
    a.emit(LdIndA(R16::BC));
    a.emit(Ret(None));

    // gettile: B = column, C = row -> A
    a.place(g("rt_gettile"));
    a.emit(Call(None, g("map_offset")));
    a.emit(Ld(A, H));
    a.emit(AluI(Alu::Add, (MAP >> 8) as u8));
    a.emit(Ld(H, A));
    a.emit(Ld(A, M));
    a.emit(Ret(None));

    // cls: LCD off (vsync turns it back on), clear the background and its copy
    a.place(g("rt_cls"));
    a.emit(Call(None, g("rt_lcd_off")));
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdhW(H_QLEN));
    for base in [0x9800u16, MAP] {
        a.emit(Ld16(R16::HL, base));
        a.emit(Ldi(B, 4));
        a.emit(Ldi(A, 0x20));
        a.emit(Call(None, g("rt_fill")));
    }
    a.emit(Ret(None));

    // text: B = column, C = row, DE = string (ends with 0)
    a.place(g("rt_text"));
    let tl = local(a, "text.loop");
    a.place(tl);
    a.emit(LdAInd(R16::DE));
    a.emit(Alu(Alu::Or, A));
    a.emit(Ret(Some(Cond::Z)));
    a.emit(Push(crate::sm83::Pair::DE));
    a.emit(Push(crate::sm83::Pair::BC));
    a.emit(Call(None, g("rt_tile")));
    a.emit(Pop(crate::sm83::Pair::BC));
    a.emit(Pop(crate::sm83::Pair::DE));
    a.emit(Inc16(R16::DE));
    a.emit(Inc(B));
    a.emit(Jp(None, tl));

    // num: B = column, C = row, HL = value: left-aligned in 5 columns
    a.place(g("rt_num"));
    a.emit(Ld(A, B));
    a.emit(LdhW(H_X0));
    a.emit(LdhW(H_X));
    a.emit(Ld(A, C));
    a.emit(LdhW(H_Y));
    emit_digits(a, &rt);
    let draw = local(a, "num.draw");
    let pad = local(a, "num.pad");
    a.place(draw);
    a.emit(Ld(A, L));
    a.emit(AluI(Alu::Cp, H_NUM + 5));
    a.emit(Jp(Some(Cond::Z), pad));
    a.emit(LdAHli);
    a.emit(Push(crate::sm83::Pair::HL));
    a.emit(Ld(D, A));
    a.emit(LdhA(H_X));
    a.emit(Ld(B, A));
    a.emit(Inc(A));
    a.emit(LdhW(H_X));
    a.emit(LdhA(H_Y));
    a.emit(Ld(C, A));
    a.emit(Ld(A, D));
    a.emit(Call(None, g("rt_tile")));
    a.emit(Pop(crate::sm83::Pair::HL));
    a.emit(Jp(None, draw));
    a.place(pad);
    a.emit(LdhA(H_X0));
    a.emit(AluI(Alu::Add, 5));
    a.emit(Ld(B, A));
    a.emit(LdhA(H_X));
    a.emit(Alu(Alu::Cp, B));
    a.emit(Ret(Some(Cond::Z)));
    a.emit(Ld(B, A));
    a.emit(Inc(A));
    a.emit(LdhW(H_X));
    a.emit(LdhA(H_Y));
    a.emit(Ld(C, A));
    a.emit(Ldi(A, b' '));
    a.emit(Call(None, g("rt_tile")));
    a.emit(Jp(None, pad));

    // num_digit: HL = value, BC = -divisor, DE = output -> one ASCII digit, HL = remainder
    a.place(g("num_digit"));
    a.emit(Ldi(A, b'0' - 1));
    let dl = local(a, "digit.loop");
    a.place(dl);
    a.emit(Inc(A));
    a.emit(AddHl(R16::BC));
    a.emit(Jp(Some(Cond::C), dl));
    a.emit(Push(crate::sm83::Pair::AF));
    a.emit(Ld(A, L));
    a.emit(Alu(Alu::Sub, C));
    a.emit(Ld(L, A));
    a.emit(Ld(A, H));
    a.emit(Alu(Alu::Sbc, B));
    a.emit(Ld(H, A));
    a.emit(Pop(crate::sm83::Pair::AF));
    a.emit(LdIndA(R16::DE));
    a.emit(Inc16(R16::DE));
    a.emit(Ret(None));

    // spr: A = sprite, B = x, C = y, D = tile
    a.place(g("rt_spr"));
    a.emit(AluI(Alu::Cp, 40));
    a.emit(Ret(Some(Cond::NC)));
    a.emit(Alu(Alu::Add, A));
    a.emit(Alu(Alu::Add, A));
    a.emit(Ld(L, A));
    a.emit(Ldi(H, (OAM_SHADOW >> 8) as u8));
    a.emit(Ld(A, C));
    a.emit(AluI(Alu::Add, 16));
    a.emit(LdHliA);
    a.emit(Ld(A, B));
    a.emit(AluI(Alu::Add, 8));
    a.emit(LdHliA);
    a.emit(Ld(M, D));
    a.emit(Ret(None));

    // hide: A = sprite
    a.place(g("rt_hide"));
    a.emit(AluI(Alu::Cp, 40));
    a.emit(Ret(Some(Cond::NC)));
    a.emit(Alu(Alu::Add, A));
    a.emit(Alu(Alu::Add, A));
    a.emit(Ld(L, A));
    a.emit(Ldi(H, (OAM_SHADOW >> 8) as u8));
    a.emit(Ldi(M, 0));
    a.emit(Ret(None));

    // rnd: A = limit -> A. A 16-bit xorshift (John Metcalf's 7,9,8), stirred with the DIV
    // register so every game plays differently, scaled as (r * limit) >> 8.
    a.place(g("rt_rnd"));
    a.emit(Ld(E, A));
    a.emit(LdhA(H_RNG));
    a.emit(Ld(L, A));
    a.emit(LdhA(0x04));
    a.emit(Alu(Alu::Xor, L));
    a.emit(Ld(L, A));
    a.emit(LdhA(H_RNG + 1));
    a.emit(Ld(H, A));
    a.emit(Rra);
    a.emit(Ld(A, L));
    a.emit(Rra);
    a.emit(Alu(Alu::Xor, H));
    a.emit(Ld(H, A));
    a.emit(Ld(A, L));
    a.emit(Rra);
    a.emit(Ld(A, H));
    a.emit(Rra);
    a.emit(Alu(Alu::Xor, L));
    a.emit(Ld(L, A));
    a.emit(Alu(Alu::Xor, H));
    a.emit(Ld(H, A));
    a.emit(LdhW(H_RNG + 1));
    a.emit(Ld(A, L));
    a.emit(LdhW(H_RNG));
    a.emit(Inc(E));
    a.emit(Dec(E));
    a.emit(Ret(Some(Cond::Z)));
    a.emit(Ld(L, A));
    a.emit(Ldi(H, 0));
    a.emit(Ldi(D, 0));
    a.emit(Call(None, g("rt_mul16")));
    a.emit(Ld(A, H));
    a.emit(Ret(None));

    // beep: A = note, B = length (envelope pace; 0 = silence), on channel 2
    a.place(g("rt_beep"));
    a.emit(Ld(C, A));
    a.emit(Ld(A, B));
    a.emit(AluI(Alu::And, 7));
    let bon = local(a, "beep.on");
    a.emit(Jp(Some(Cond::NZ), bon));
    a.emit(LdhW(0x17));
    a.emit(Ret(None));
    a.place(bon);
    a.emit(AluI(Alu::Or, 0xF0));
    a.emit(LdhW(0x17));
    a.emit(Ldi(A, 0x80));
    a.emit(LdhW(0x16));
    a.emit(Ld(A, C));
    a.emit(AluI(Alu::Cp, 72));
    let bok = local(a, "beep.ok");
    a.emit(Jp(Some(Cond::C), bok));
    a.emit(Ldi(A, 71));
    a.place(bok);
    a.emit(Ld(L, A));
    a.emit(Ldi(H, 0));
    a.emit(AddHl(R16::HL));
    a.emit(Ld16L(R16::DE, g("notes"), 0));
    a.emit(AddHl(R16::DE));
    a.emit(LdAHli);
    a.emit(LdhW(0x18));
    a.emit(Ld(A, M));
    a.emit(AluI(Alu::Or, 0x80));
    a.emit(LdhW(0x19));
    a.emit(Ret(None));

    // noise: A = length (0 = silence), on channel 4
    a.place(g("rt_noise"));
    a.emit(AluI(Alu::And, 7));
    let non = local(a, "noise.on");
    a.emit(Jp(Some(Cond::NZ), non));
    a.emit(LdhW(0x21));
    a.emit(Ret(None));
    a.place(non);
    a.emit(AluI(Alu::Or, 0xF0));
    a.emit(LdhW(0x21));
    a.emit(Alu(Alu::Xor, A));
    a.emit(LdhW(0x20));
    a.emit(Ldi(A, 0x52));
    a.emit(LdhW(0x22));
    a.emit(Ldi(A, 0x80));
    a.emit(LdhW(0x23));
    a.emit(Ret(None));

    // scroll: B = x, C = y (applied in the next VBlank)
    a.place(g("rt_scroll"));
    a.emit(Ld(A, B));
    a.emit(LdhW(H_SCX));
    a.emit(Ld(A, C));
    a.emit(LdhW(H_SCY));
    a.emit(Ret(None));

    // debug: print a number and a newline to the link port
    a.place(g("rt_debug_u8"));
    a.emit(Ld(L, A));
    a.emit(Ldi(H, 0));
    a.emit(Jp(None, g("rt_debug_u16")));
    a.place(g("rt_debug_i8"));
    a.emit(Ld(L, A));
    a.emit(Alu(Alu::Add, A));
    a.emit(Alu(Alu::Sbc, A));
    a.emit(Ld(H, A));
    a.place(g("rt_debug_i16"));
    a.emit(Bit(7, H));
    a.emit(Jp(Some(Cond::Z), g("rt_debug_u16")));
    a.emit(Ldi(A, b'-'));
    a.emit(Call(None, g("serial_send")));
    a.emit(Call(None, g("neg_hl")));
    a.place(g("rt_debug_u16"));
    emit_digits(a, &rt);
    let sl = local(a, "debug.loop");
    a.place(sl);
    a.emit(LdAHli);
    a.emit(Call(None, g("serial_send")));
    a.emit(Ld(A, L));
    a.emit(AluI(Alu::Cp, H_NUM + 5));
    a.emit(Jp(Some(Cond::NZ), sl));
    a.emit(Ldi(A, b'\n'));
    // fall through
    a.place(g("serial_send"));
    a.emit(LdhW(0x01));
    a.emit(Ldi(A, 0x81));
    a.emit(LdhW(0x02));
    let sw = local(a, "serial.wait");
    a.place(sw);
    a.emit(LdhA(0x02));
    a.emit(Bit(7, A));
    a.emit(Jp(Some(Cond::NZ), sw));
    a.emit(Ret(None));

    // stop: say so on the link port, mark it for emulators (ld b,b), halt for good
    a.place(g("rt_stop"));
    for ch in b"#stop\n" {
        a.emit(Ldi(A, *ch));
        a.emit(Call(None, g("serial_send")));
    }
    a.emit(Ld(B, B));
    a.emit(Di);
    let halt = local(a, "stop.halt");
    a.place(halt);
    a.emit(Halt);
    a.emit(Nop); // with interrupts off, HALT can run the next byte twice
    a.emit(Jp(None, halt));

    // mul16: HL * DE -> HL (low 16 bits)
    a.emit(Comment("arithmetic the SM83 does not have".into()));
    a.place(g("rt_mul16"));
    a.emit(Ld(B, H));
    a.emit(Ld(C, L));
    a.emit(Ld16(R16::HL, 0));
    a.emit(Ldi(A, 16));
    let ml = local(a, "mul.loop");
    let ms = local(a, "mul.skip");
    a.place(ml);
    a.emit(AddHl(R16::HL));
    a.emit(Cb(Rot::Sla, E));
    a.emit(Cb(Rot::Rl, D));
    a.emit(Jp(Some(Cond::NC), ms));
    a.emit(AddHl(R16::BC));
    a.place(ms);
    a.emit(Dec(A));
    a.emit(Jp(Some(Cond::NZ), ml));
    a.emit(Ret(None));

    // divu16: HL / DE -> HL quotient, BC remainder. x / 0 is $FFFF and x % 0 is x.
    a.place(g("rt_divu16"));
    a.emit(Ld16(R16::BC, 0));
    a.emit(Ldi(A, 16));
    a.emit(LdhW(H_CNT));
    let dloop = local(a, "div.loop");
    let force = local(a, "div.force");
    let next = local(a, "div.next");
    let fits = local(a, "div.fits");
    a.place(dloop);
    a.emit(AddHl(R16::HL));
    a.emit(Cb(Rot::Rl, C));
    a.emit(Cb(Rot::Rl, B));
    a.emit(Jp(Some(Cond::C), force));
    a.emit(Ld(A, C));
    a.emit(Alu(Alu::Sub, E));
    a.emit(Ld(C, A));
    a.emit(Ld(A, B));
    a.emit(Alu(Alu::Sbc, D));
    a.emit(Ld(B, A));
    a.emit(Jp(Some(Cond::NC), fits));
    a.emit(Ld(A, C));
    a.emit(Alu(Alu::Add, E));
    a.emit(Ld(C, A));
    a.emit(Ld(A, B));
    a.emit(Alu(Alu::Adc, D));
    a.emit(Ld(B, A));
    a.emit(Jp(None, next));
    a.place(force);
    a.emit(Ld(A, C));
    a.emit(Alu(Alu::Sub, E));
    a.emit(Ld(C, A));
    a.emit(Ld(A, B));
    a.emit(Alu(Alu::Sbc, D));
    a.emit(Ld(B, A));
    a.place(fits);
    a.emit(Inc(L));
    a.place(next);
    a.emit(LdhA(H_CNT));
    a.emit(Dec(A));
    a.emit(LdhW(H_CNT));
    a.emit(Jp(Some(Cond::NZ), dloop));
    a.emit(Ret(None));

    // divs16: signed, truncating toward zero; the remainder has the sign of the dividend
    a.place(g("rt_divs16"));
    a.emit(Ld(A, H));
    a.emit(LdhW(H_DSIGN));
    a.emit(Alu(Alu::Xor, D));
    a.emit(LdhW(H_SIGN));
    a.emit(Bit(7, H));
    a.emit(Call(Some(Cond::NZ), g("neg_hl")));
    a.emit(Bit(7, D));
    a.emit(Call(Some(Cond::NZ), g("neg_de")));
    a.emit(Call(None, g("rt_divu16")));
    a.emit(LdhA(H_SIGN));
    a.emit(Bit(7, A));
    a.emit(Call(Some(Cond::NZ), g("neg_hl")));
    a.emit(LdhA(H_DSIGN));
    a.emit(Bit(7, A));
    a.emit(Ret(Some(Cond::Z)));
    a.emit(Alu(Alu::Xor, A));
    a.emit(Alu(Alu::Sub, C));
    a.emit(Ld(C, A));
    a.emit(Ldi(A, 0));
    a.emit(Alu(Alu::Sbc, B));
    a.emit(Ld(B, A));
    a.emit(Ret(None));

    a.place(g("rt_modu16"));
    a.emit(Call(None, g("rt_divu16")));
    a.emit(Ld(H, B));
    a.emit(Ld(L, C));
    a.emit(Ret(None));
    a.place(g("rt_mods16"));
    a.emit(Call(None, g("rt_divs16")));
    a.emit(Ld(H, B));
    a.emit(Ld(L, C));
    a.emit(Ret(None));

    a.place(g("neg_hl"));
    a.emit(Alu(Alu::Xor, A));
    a.emit(Alu(Alu::Sub, L));
    a.emit(Ld(L, A));
    a.emit(Ldi(A, 0));
    a.emit(Alu(Alu::Sbc, H));
    a.emit(Ld(H, A));
    a.emit(Ret(None));
    a.place(g("neg_de"));
    a.emit(Alu(Alu::Xor, A));
    a.emit(Alu(Alu::Sub, E));
    a.emit(Ld(E, A));
    a.emit(Ldi(A, 0));
    a.emit(Alu(Alu::Sbc, D));
    a.emit(Ld(D, A));
    a.emit(Ret(None));

    // shifts by a count in B: A (8 bits) or HL (16 bits)
    let shifts: [(&str, &[crate::sm83::Ins]); 6] = [
        ("rt_shl8", &[Alu(Alu::Add, A)]),
        ("rt_shr8", &[Cb(Rot::Srl, A)]),
        ("rt_sar8", &[Cb(Rot::Sra, A)]),
        ("rt_shl16", &[AddHl(R16::HL)]),
        ("rt_shr16", &[Cb(Rot::Srl, H), Cb(Rot::Rr, L)]),
        ("rt_sar16", &[Cb(Rot::Sra, H), Cb(Rot::Rr, L)]),
    ];
    for (name, step) in shifts {
        a.place(g(name));
        let lp = a.new_label(format!("{name}.loop"));
        let test = a.new_label(format!("{name}.test"));
        a.emit(Inc(B));
        a.emit(Jp(None, test));
        a.place(lp);
        for i in step {
            a.emit(i.clone());
        }
        a.place(test);
        a.emit(Dec(B));
        a.emit(Jp(Some(Cond::NZ), lp));
        a.emit(Ret(None));
    }

    // fill: HL = destination, B = 256-byte pages, A = value (4 bytes per loop: 3 cycles a byte)
    a.place(g("rt_fill"));
    let fo = local(a, "fill.page");
    let fi = local(a, "fill.loop");
    a.place(fo);
    a.emit(Ldi(C, 64));
    a.place(fi);
    for _ in 0..4 {
        a.emit(LdHliA);
    }
    a.emit(Dec(C));
    a.emit(Jp(Some(Cond::NZ), fi));
    a.emit(Dec(B));
    a.emit(Jp(Some(Cond::NZ), fo));
    a.emit(Ret(None));

    // memcpy: HL = destination, DE = source, BC = count (> 0)
    a.place(g("rt_memcpy"));
    let cl = local(a, "memcpy.loop");
    a.place(cl);
    a.emit(LdAInd(R16::DE));
    a.emit(LdHliA);
    a.emit(Inc16(R16::DE));
    a.emit(Dec16(R16::BC));
    a.emit(Ld(A, B));
    a.emit(Alu(Alu::Or, C));
    a.emit(Jp(Some(Cond::NZ), cl));
    a.emit(Ret(None));

    // data
    a.emit(Comment("font: ASCII 32-127".into()));
    a.place(g("font"));
    a.emit(Db(crate::font::FONT.iter().flatten().copied().collect()));
    a.emit(Comment("note frequencies".into()));
    a.place(g("notes"));
    a.emit(Db(note_table()));
    rt
}

/// HL = value -> five ASCII digits at H_NUM; HL points at the first significant one.
fn emit_digits(a: &mut Asm, rt: &Runtime) {
    a.emit(Ld16(R16::DE, 0xFF00 | H_NUM as u16));
    for d in [10000u16, 1000, 100, 10] {
        a.emit(Ld16(R16::BC, d.wrapping_neg()));
        a.emit(Call(None, rt.get("num_digit")));
    }
    a.emit(Ld(A, L));
    a.emit(AluI(Alu::Add, b'0'));
    a.emit(LdIndA(R16::DE));
    a.emit(Ld16(R16::HL, 0xFF00 | H_NUM as u16));
    a.emit(Ldi(B, 4));
    let skip = a.new_label("digits.skip");
    let done = a.new_label("digits.done");
    a.place(skip);
    a.emit(Ld(A, M));
    a.emit(AluI(Alu::Cp, b'0'));
    a.emit(Jp(Some(Cond::NZ), done));
    a.emit(Inc16(R16::HL));
    a.emit(Dec(B));
    a.emit(Jp(Some(Cond::NZ), skip));
    a.place(done);
}
