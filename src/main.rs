//! The command line: `cartuccia game.cart` writes game.gb.

use std::process::ExitCode;

const USAGE: &str = "usage: cartuccia [options] GAME.cart

  -o FILE     write the ROM to FILE (default: GAME.gb)
  --asm       print the game's assembly
  --ir        print the IR before and after optimization
  --stats     print sizes
  --run       interpret the program and print what debug() writes (no ROM)
  -O0         turn the optimizer off
  --title T   cartridge title (default: the file name)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input = None;
    let mut output = None;
    let (mut asm, mut ir, mut stats, mut run) = (false, false, false, false);
    let mut opts = cartuccia::Options::default();
    let mut title = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" if i + 1 < args.len() => {
                i += 1;
                output = Some(args[i].clone());
            }
            "--title" if i + 1 < args.len() => {
                i += 1;
                title = Some(args[i].clone());
            }
            "--asm" => asm = true,
            "--ir" => ir = true,
            "--stats" => stats = true,
            "--run" => run = true,
            "-O0" => opts.optimize = false,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            a if a.starts_with('-') || input.is_some() => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            a => input = Some(a.to_string()),
        }
        i += 1;
    }
    let Some(input) = input else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let source = match std::fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stem = std::path::Path::new(&input)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("game")
        .to_string();
    opts.title = title.unwrap_or_else(|| stem.clone());

    if run {
        let mut module = match cartuccia::front(&source) {
            Ok(m) => m,
            Err(e) => {
                eprint!("{}", e.render(&source, &input));
                return ExitCode::FAILURE;
            }
        };
        if opts.optimize {
            cartuccia::opt::optimize(&mut module);
        }
        let mut machine = cartuccia::interp::Machine::new(&module, 50_000_000);
        let stop = machine.run(600);
        print!("{}", machine.out);
        if let cartuccia::interp::Stop::Fault(msg) = stop {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    let out = match cartuccia::compile(&source, &opts) {
        Ok(out) => out,
        Err(e) => {
            eprint!("{}", e.render(&source, &input));
            return ExitCode::FAILURE;
        }
    };
    if ir {
        println!("; ---- IR before optimization\n{}", out.ir_before);
        println!("; ---- IR after optimization\n{}", out.ir_after);
    }
    if asm {
        print!("{}", out.asm);
    }
    if stats {
        let s = &out.stats;
        println!(
            "ROM {} of 32768 bytes (runtime {}, game code {}, data {}); RAM {} bytes, HRAM {}; IR {} -> {} instructions",
            s.rom_used,
            s.runtime_bytes,
            s.code_bytes,
            s.data_bytes,
            s.ram_used,
            s.hram_used,
            s.ir_before,
            s.ir_after
        );
        for f in &s.funcs {
            println!(
                "  {:<20} {:>5} bytes  {:>4} RAM  {:>3} HRAM",
                f.name, f.bytes, f.ram, f.hram
            );
        }
    }
    let path = output.unwrap_or_else(|| {
        std::path::Path::new(&input)
            .with_extension("gb")
            .to_string_lossy()
            .into_owned()
    });
    if let Err(e) = std::fs::write(&path, &out.rom) {
        eprintln!("{path}: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
