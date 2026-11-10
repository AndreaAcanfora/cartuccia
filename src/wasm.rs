//! The browser API: the page copies the source into `input(len)`, calls `compile`, then reads the
//! results straight out of WebAssembly memory with `output_ptr(kind)` and `output_len(kind)`.

use std::cell::RefCell;

/// What `output_ptr` / `output_len` return.
const ROM: u32 = 0;
const ERROR: u32 = 1;
const ASM: u32 = 2;
const IR_BEFORE: u32 = 3;
const IR_AFTER: u32 = 4;
const STATS: u32 = 5;
const RUNTIME_ASM: u32 = 6;
const KINDS: usize = 7;

thread_local! {
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OUTPUTS: RefCell<Vec<Vec<u8>>> = RefCell::new(vec![Vec::new(); KINDS]);
}

/// Makes room for `len` bytes of source and returns where to write them.
#[unsafe(no_mangle)]
pub extern "C" fn input(len: u32) -> *mut u8 {
    INPUT.with(|i| {
        let mut i = i.borrow_mut();
        i.clear();
        i.resize(len as usize, 0);
        i.as_mut_ptr()
    })
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            '\n' => out += "\\n",
            c if (c as u32) < 0x20 => out += &format!("\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Compiles the source in the input buffer. Returns 1 on success, 0 on a compile error (the
/// ERROR output then holds it as JSON: line, column, length, message and the full text).
#[unsafe(no_mangle)]
pub extern "C" fn compile(optimize: u32, title_len: u32) -> u32 {
    let source = INPUT.with(|i| String::from_utf8_lossy(&i.borrow()).into_owned());
    // The title, if any, follows the source in the input buffer.
    let split = source.len().saturating_sub(title_len as usize);
    let (source, title) = source.split_at(split);
    let opts = crate::Options {
        optimize: optimize != 0,
        title: if title.is_empty() {
            "CARTUCCIA".into()
        } else {
            title.into()
        },
    };
    let result = crate::compile(source, &opts);
    OUTPUTS.with(|o| {
        let mut o = o.borrow_mut();
        for v in o.iter_mut() {
            v.clear();
        }
        match result {
            Ok(out) => {
                let s = &out.stats;
                let funcs: Vec<String> = s
                    .funcs
                    .iter()
                    .map(|f| {
                        format!(
                            "{{\"name\":{},\"bytes\":{},\"ram\":{},\"hram\":{}}}",
                            json_str(&f.name),
                            f.bytes,
                            f.ram,
                            f.hram
                        )
                    })
                    .collect();
                let stats = format!(
                    "{{\"rom_used\":{},\"runtime\":{},\"code\":{},\"data\":{},\"ram\":{},\"hram\":{},\"ir_before\":{},\"ir_after\":{},\"funcs\":[{}]}}",
                    s.rom_used,
                    s.runtime_bytes,
                    s.code_bytes,
                    s.data_bytes,
                    s.ram_used,
                    s.hram_used,
                    s.ir_before,
                    s.ir_after,
                    funcs.join(",")
                );
                o[ROM as usize] = out.rom;
                o[ASM as usize] = out.asm.into_bytes();
                o[IR_BEFORE as usize] = out.ir_before.into_bytes();
                o[IR_AFTER as usize] = out.ir_after.into_bytes();
                o[STATS as usize] = stats.into_bytes();
                o[RUNTIME_ASM as usize] = out.runtime_asm.into_bytes();
                1
            }
            Err(e) => {
                let len = e.span.end.saturating_sub(e.span.start).max(1);
                let json = format!(
                    "{{\"line\":{},\"col\":{},\"len\":{},\"message\":{},\"text\":{}}}",
                    e.span.line,
                    e.span.col,
                    len,
                    json_str(&e.message),
                    json_str(&e.render(source, "game.cart"))
                );
                o[ERROR as usize] = json.into_bytes();
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn output_ptr(kind: u32) -> *const u8 {
    OUTPUTS.with(|o| {
        o.borrow()
            .get(kind as usize)
            .map_or(std::ptr::null(), |v| v.as_ptr())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn output_len(kind: u32) -> u32 {
    OUTPUTS.with(|o| o.borrow().get(kind as usize).map_or(0, |v| v.len() as u32))
}
