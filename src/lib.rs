#![allow(clippy::needless_range_loop)]
//! cartuccia: a small Lua-flavoured language and an optimizing compiler that turns it into
//! Game Boy cartridges.
//!
//! ```text
//! source -> lexer -> parser -> type inference -> IR -> optimizer -> SM83 code -> ROM
//! ```

pub mod ast;
pub mod builtins;
pub mod check;
pub mod codegen;
pub mod diag;
pub mod font;
pub mod gfx;
pub mod interp;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod opt;
pub mod parser;
pub mod rom;
pub mod runtime;
pub mod sm83;

#[cfg(target_arch = "wasm32")]
mod wasm;

pub use diag::Error;

#[derive(Clone, Debug)]
pub struct Options {
    pub optimize: bool,
    /// The cartridge title in the header (15 characters at most).
    pub title: String,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            optimize: true,
            title: "CARTUCCIA".into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FuncStats {
    pub name: String,
    pub bytes: usize,
    pub ram: u16,
    pub hram: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Bytes of the ROM in use (the rest is padding).
    pub rom_used: usize,
    pub runtime_bytes: usize,
    pub code_bytes: usize,
    pub data_bytes: usize,
    pub ram_used: u16,
    pub hram_used: u16,
    /// IR instructions before and after optimization.
    pub ir_before: usize,
    pub ir_after: usize,
    pub funcs: Vec<FuncStats>,
}

pub struct Output {
    pub rom: Vec<u8>,
    /// The game's code as annotated assembly.
    pub asm: String,
    /// The runtime library as assembly.
    pub runtime_asm: String,
    pub ir_before: String,
    pub ir_after: String,
    pub stats: Stats,
}

/// Parses, checks and lowers to IR, without optimizing.
pub fn front(source: &str) -> Result<ir::Module, Error> {
    let program = parser::parse(source)?;
    let checked = check::check(&program)?;
    Ok(lower::lower(&program, &checked))
}

fn ir_size(m: &ir::Module) -> usize {
    let used = opt::used_funcs(m);
    m.funcs
        .iter()
        .zip(used)
        .filter(|(_, u)| *u)
        .map(|(f, _)| f.blocks.iter().map(|b| b.insts.len()).sum::<usize>())
        .sum()
}

fn listing_of(m: &ir::Module) -> String {
    let used = opt::used_funcs(m);
    let mut out = String::new();
    for (f, u) in m.funcs.iter().zip(used) {
        if u {
            out += &ir::FuncDisplay(m, f).to_string();
            out.push('\n');
        }
    }
    out
}

pub fn compile(source: &str, opts: &Options) -> Result<Output, Error> {
    let mut module = front(source)?;
    let ir_before = listing_of(&module);
    let before = ir_size(&module);
    if opts.optimize {
        opt::optimize(&mut module);
    } else {
        for f in &mut module.funcs {
            opt::simplify_cfg(f);
        }
    }
    let ir_after = listing_of(&module);
    let after = ir_size(&module);
    let generated = codegen::generate(&module, source)?;
    let assembled = sm83::assemble(&generated.asm).map_err(|msg| {
        Error::new(
            diag::Span {
                line: 1,
                col: 1,
                ..Default::default()
            },
            msg,
        )
    })?;
    let rom_used = assembled.bytes.len();
    let rom = rom::build(assembled.bytes.clone(), &opts.title)?;

    let code = &generated.asm.code;
    let user_start_addr = assembled.addr[generated.user_start] as usize;
    let data_start = code
        .iter()
        .position(|i| matches!(i, sm83::Ins::Comment(c) if c == "data"))
        .map_or(rom_used, |k| assembled.addr[k] as usize);
    let runtime_bytes = user_start_addr - 0x150;
    let mut funcs = Vec::new();
    for (k, f) in generated.funcs.iter().enumerate() {
        let start = assembled.label_addr[f.label as usize] as usize;
        let end = generated.funcs.get(k + 1).map_or(data_start, |g| {
            assembled.label_addr[g.label as usize] as usize
        });
        funcs.push(FuncStats {
            name: f.name.clone(),
            bytes: end.saturating_sub(start),
            ram: f.wram,
            hram: f.hram,
        });
    }
    let asm = sm83::listing(&generated.asm, &assembled, generated.user_start..code.len());
    let runtime_asm = sm83::listing(&generated.asm, &assembled, 0..generated.user_start);
    Ok(Output {
        rom,
        asm,
        runtime_asm,
        ir_before,
        ir_after,
        stats: Stats {
            rom_used,
            runtime_bytes,
            code_bytes: data_start - user_start_addr,
            data_bytes: rom_used - data_start,
            ram_used: generated.ram_used,
            hram_used: generated.hram_used,
            ir_before: before,
            ir_after: after,
            funcs,
        },
    })
}
