//! Differential testing. Random well-typed programs run three ways:
//!
//! 1. the IR interpreter on the unoptimized IR (the reference),
//! 2. the IR interpreter after optimization,
//! 3. a real Game Boy emulator (mattone) running the compiled ROM, at -O0 and at full optimization.
//!
//! Every way must print the same numbers. The emulator runs need `mattone-run`: set MATTONE_RUN
//! to its path, or keep a mattone checkout next to this one.

use cartuccia::interp::{Machine, Stop};
use std::fmt::Write;
use std::path::PathBuf;
use std::process::Command;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

const TYPES: [&str; 4] = ["u8", "i8", "u16", "i16"];

fn literal(r: &mut Rng, ty: &str) -> String {
    let v: i64 = match ty {
        "u8" => r.below(256) as i64,
        "i8" => r.below(256) as i64 - 128,
        "u16" => {
            if r.chance(50) {
                r.below(300) as i64
            } else {
                r.below(65536) as i64
            }
        }
        _ => {
            if r.chance(50) {
                r.below(600) as i64 - 300
            } else {
                r.below(65536) as i64 - 32768
            }
        }
    };
    if v < 0 {
        format!("({v})")
    } else {
        v.to_string()
    }
}

struct Gen {
    r: Rng,
    /// Variables in scope: (name, type, assignable).
    vars: Vec<(String, &'static str, bool)>,
    funcs: Vec<(String, Vec<&'static str>, &'static str)>,
    next: u32,
    depth: u32,
}

impl Gen {
    fn fresh(&mut self) -> String {
        self.next += 1;
        format!("v{}", self.next)
    }

    fn pick_type(&mut self) -> &'static str {
        TYPES[self.r.below(4) as usize]
    }

    fn expr(&mut self, ty: &'static str, depth: u32) -> String {
        let choice = if depth == 0 {
            self.r.below(3)
        } else {
            self.r.below(14)
        };
        match choice {
            0 => literal(&mut self.r, ty),
            1 | 2 => {
                let candidates: Vec<String> = self
                    .vars
                    .iter()
                    .filter(|v| v.1 == ty)
                    .map(|v| v.0.clone())
                    .collect();
                if candidates.is_empty() {
                    literal(&mut self.r, ty)
                } else {
                    candidates[self.r.below(candidates.len() as u64) as usize].clone()
                }
            }
            3 => {
                let from = self.pick_type();
                format!("{ty}({})", self.expr(from, depth - 1))
            }
            4 => {
                let op = ["-", "~"][self.r.below(2) as usize];
                let e = self.expr(ty, depth - 1);
                if e.trim_matches(|c| c == '(' || c == ')')
                    .parse::<i64>()
                    .is_ok()
                {
                    return e; // `-literal` is a literal, with its own range rules
                }
                format!("{op}({e})")
            }
            5 => {
                let op = ["<<", ">>"][self.r.below(2) as usize];
                let count = if self.r.chance(50) {
                    self.r.below(17).to_string()
                } else {
                    format!("({} & 15)", self.expr("u8", depth - 1))
                };
                format!("({} {op} {count})", self.expr(ty, depth - 1))
            }
            6 if ty == "u8" => format!("grid[{} & 15]", self.expr("u8", depth - 1)),
            6 if ty == "i16" => format!("words[{} & 7]", self.expr("u8", depth - 1)),
            7 if ty == "u8" => format!("table[{} & 7]", self.expr("u8", depth - 1)),
            8 if !self.funcs.is_empty() => {
                let matching: Vec<usize> = (0..self.funcs.len())
                    .filter(|&i| self.funcs[i].2 == ty)
                    .collect();
                if matching.is_empty() {
                    return literal(&mut self.r, ty);
                }
                let f = matching[self.r.below(matching.len() as u64) as usize];
                let (name, params, _) = self.funcs[f].clone();
                let args: Vec<String> = params.iter().map(|p| self.expr(p, depth - 1)).collect();
                format!("{name}({})", args.join(", "))
            }
            9 => {
                let f = ["min", "max", "abs"][self.r.below(3) as usize];
                if f == "abs" {
                    format!("abs({})", self.expr(ty, depth - 1))
                } else {
                    format!(
                        "{f}({}, {})",
                        self.expr(ty, depth - 1),
                        self.expr(ty, depth - 1)
                    )
                }
            }
            _ => {
                let op =
                    ["+", "-", "*", "/", "%", "&", "|", "^", "+", "-"][self.r.below(10) as usize];
                format!(
                    "({} {op} {})",
                    self.expr(ty, depth - 1),
                    self.expr(ty, depth - 1)
                )
            }
        }
    }

    fn cond(&mut self, depth: u32) -> String {
        match self.r.below(6) {
            0 if depth > 0 => format!("({} and {})", self.cond(depth - 1), self.cond(depth - 1)),
            1 if depth > 0 => format!("({} or {})", self.cond(depth - 1), self.cond(depth - 1)),
            2 if depth > 0 => format!("not {}", self.cond(depth - 1)),
            _ => {
                let ty = self.pick_type();
                let op = ["<", "<=", ">", ">=", "==", "~="][self.r.below(6) as usize];
                let a = self.expr(ty, 2);
                let mut b = self.expr(ty, 1);
                if b == "0" && matches!(ty, "u8" | "u16") && matches!(op, "<" | ">=") {
                    b = "1".into(); // the checker rejects comparisons that are always false
                }
                if a == "0" && matches!(ty, "u8" | "u16") && matches!(op, ">" | "<=") {
                    return format!("({b} {op} 1)");
                }
                format!("({a} {op} {b})")
            }
        }
    }

    fn block(&mut self, out: &mut String, indent: usize, n: usize) {
        let saved = self.vars.len();
        for _ in 0..n {
            self.stmt(out, indent);
        }
        self.vars.truncate(saved);
    }

    fn stmt(&mut self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent);
        let assignable: Vec<(String, &'static str)> = self
            .vars
            .iter()
            .filter(|v| v.2)
            .map(|v| (v.0.clone(), v.1))
            .collect();
        match self.r.below(10) {
            0 | 1 => {
                let ty = self.pick_type();
                let name = self.fresh();
                let e = self.expr(ty, 3);
                let _ = writeln!(out, "{pad}local {name}: {ty} = {e}");
                self.vars.push((name, ty, true));
            }
            2 | 3 if !assignable.is_empty() => {
                let (name, ty) = assignable[self.r.below(assignable.len() as u64) as usize].clone();
                let op = ["=", "+=", "-=", "*="][self.r.below(4) as usize];
                let e = self.expr(ty, 3);
                let _ = writeln!(out, "{pad}{name} {op} {e}");
            }
            4 if self.depth < 3 => {
                self.depth += 1;
                let c = self.cond(2);
                let _ = writeln!(out, "{pad}if {c} then");
                self.block(out, indent + 1, 2);
                if self.r.chance(50) {
                    let c = self.cond(1);
                    let _ = writeln!(out, "{pad}elseif {c} then");
                    self.block(out, indent + 1, 2);
                }
                if self.r.chance(50) {
                    let _ = writeln!(out, "{pad}else");
                    self.block(out, indent + 1, 2);
                }
                let _ = writeln!(out, "{pad}end");
                self.depth -= 1;
            }
            5 if self.depth < 2 => {
                self.depth += 1;
                let name = self.fresh();
                let ty = ["u8", "i8", "u16", "i16"][self.r.below(4) as usize];
                let (from, to, step) = match self.r.below(3) {
                    0 => (0i32, self.r.below(6) as i32, 1i32),
                    1 => (self.r.below(10) as i32, 0, -(self.r.below(3) as i32 + 1)),
                    _ => (-3, 3, 2),
                };
                let (from, to) = if ty.starts_with('u') && (from < 0 || to < 0) {
                    (0, 4)
                } else {
                    (from, to)
                };
                let lit = |v: i32| {
                    if v < 0 {
                        format!("({v})")
                    } else {
                        v.to_string()
                    }
                };
                let step = if ty.starts_with('u') && step < 0 && from < to {
                    1
                } else {
                    step
                };
                let step_text = if step == 1 {
                    String::new()
                } else {
                    format!(", {step}")
                };
                let _ = writeln!(
                    out,
                    "{pad}for {name} = {ty}({}), {}{step_text} do",
                    lit(from),
                    lit(to)
                );
                self.vars.push((name, ty, false));
                self.block(out, indent + 1, 2);
                self.vars.pop();
                let _ = writeln!(out, "{pad}end");
                self.depth -= 1;
            }
            6 => {
                let i = self.expr("u8", 2);
                let v = self.expr("u8", 2);
                let _ = writeln!(out, "{pad}grid[{i} & 15] = {v}");
            }
            7 => {
                let i = self.expr("u8", 1);
                let v = self.expr("i16", 2);
                let op = ["=", "+="][self.r.below(2) as usize];
                let _ = writeln!(out, "{pad}words[{i} & 7] {op} {v}");
            }
            _ => {
                let ty = self.pick_type();
                let e = self.expr(ty, 3);
                let _ = writeln!(out, "{pad}debug({e})");
            }
        }
    }

    fn program(&mut self) -> String {
        let mut out = String::new();
        out +=
            "var grid: u8[16]\nvar words: i16[8]\nconst table: u8[] = {3, 1, 4, 1, 5, 9, 2, 6}\n";
        for g in 0..3 {
            let ty = self.pick_type();
            let lit = literal(&mut self.r, ty);
            let _ = writeln!(out, "var g{g}: {ty} = {lit}");
            self.vars.push((format!("g{g}"), ty, true));
        }
        for f in 0..self.r.below(4) {
            let params: Vec<&'static str> =
                (0..self.r.below(3) + 1).map(|_| self.pick_type()).collect();
            let ret = self.pick_type();
            let names: Vec<String> = (0..params.len()).map(|i| format!("p{i}")).collect();
            let saved = self.vars.clone();
            for (n, t) in names.iter().zip(&params) {
                self.vars.push((n.clone(), t, true));
            }
            let sig: Vec<String> = names
                .iter()
                .zip(&params)
                .map(|(n, t)| format!("{n}: {t}"))
                .collect();
            let _ = writeln!(out, "function f{f}({}): {ret}", sig.join(", "));
            let n = self.r.below(3) as usize;
            self.block(&mut out, 1, n);
            let e = self.expr(ret, 3);
            let _ = writeln!(out, "  return {e}\nend");
            self.vars = saved;
            self.funcs.push((format!("f{f}"), params, ret));
        }
        out += "function _init()\n";
        self.block(&mut out, 1, 12);
        for g in 0..3 {
            let _ = writeln!(out, "  debug(g{g})");
        }
        out += "  for i = 0, 15 do debug(grid[i]) end\n  for i = 0, 7 do debug(words[i]) end\n  stop()\nend\n";
        out
    }
}

fn mattone() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MATTONE_RUN") {
        return Some(PathBuf::from(p));
    }
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../mattone/build/mattone-run");
    local.exists().then_some(local)
}

fn interpret(m: &cartuccia::ir::Module) -> Result<String, String> {
    let mut machine = Machine::new(m, 2_000_000);
    match machine.run(10) {
        Stop::Stopped => Ok(machine.out),
        Stop::Limit => Err("step limit".into()),
        Stop::Fault(f) => Err(f),
    }
}

fn emulate(run: &PathBuf, rom: &[u8], name: &str) -> String {
    let dir = std::env::temp_dir().join("cartuccia-fuzz");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.gb"));
    std::fs::write(&path, rom).unwrap();
    let out = Command::new(run)
        .args(["--serial", "--until", "#stop", "--frames", "3000"])
        .arg(&path)
        .output()
        .expect("run mattone-run");
    String::from_utf8_lossy(&out.stdout).replace("#stop\n", "")
}

/// Returns false when the program was skipped (it runs too long).
fn check_program(src: &str, seed: u64, emulator: Option<&PathBuf>) -> bool {
    let fail = |what: &str, a: &str, b: &str| -> ! {
        let dir = std::env::temp_dir().join("cartuccia-fuzz");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("fail-{seed}.cart"));
        std::fs::write(&path, src).unwrap();
        let diff: Vec<String> = a
            .lines()
            .zip(b.lines())
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .take(5)
            .map(|(i, (x, y))| format!("line {}: {x} vs {y}", i + 1))
            .collect();
        panic!(
            "seed {seed}: {what} differ ({} vs {} lines) {diff:?}\nprogram saved to {}",
            a.lines().count(),
            b.lines().count(),
            path.display()
        );
    };
    let module = match cartuccia::front(src) {
        Ok(m) => m,
        Err(e) => panic!(
            "seed {seed}: generated program does not compile:\n{}\n{src}",
            e.render(src, "fuzz")
        ),
    };
    let reference = match interpret(&module) {
        Ok(out) => out,
        Err(_) => return false, // runs too long: skip
    };
    let mut optimized = module.clone();
    cartuccia::opt::optimize(&mut optimized);
    let after = interpret(&optimized).unwrap_or_else(|e| format!("error: {e}"));
    if after != reference {
        fail("interpreter before/after optimization", &reference, &after);
    }
    if let Some(run) = emulator {
        for optimize in [true, false] {
            let opts = cartuccia::Options {
                optimize,
                title: "FUZZ".into(),
            };
            let out = cartuccia::compile(src, &opts)
                .unwrap_or_else(|e| panic!("seed {seed}: {}", e.render(src, "fuzz")));
            let got = emulate(run, &out.rom, &format!("{seed}-{optimize}"));
            if got != reference {
                fail(
                    if optimize {
                        "interpreter and Game Boy (optimized)"
                    } else {
                        "interpreter and Game Boy (-O0)"
                    },
                    &reference,
                    &got,
                );
            }
        }
    }
    true
}

fn programs(seed: u64, n: u64) -> impl Iterator<Item = (u64, String)> {
    (0..n).map(move |i| {
        let s = seed + i;
        let mut g = Gen {
            r: Rng(s.wrapping_mul(0x9E3779B97F4A7C15) | 1),
            vars: Vec::new(),
            funcs: Vec::new(),
            next: 0,
            depth: 0,
        };
        (s, g.program())
    })
}

#[test]
fn optimizer_preserves_meaning() {
    let n = std::env::var("FUZZ_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    let checked = programs(1, n)
        .filter(|(seed, src)| check_program(src, *seed, None))
        .count();
    eprintln!("{checked} of {n} programs checked");
    assert!(checked as u64 * 10 >= n * 8, "too many programs skipped");
}

#[test]
fn game_boy_agrees_with_interpreter() {
    let Some(run) = mattone() else {
        eprintln!("mattone-run not found: skipping the emulator runs");
        return;
    };
    let n = std::env::var("FUZZ_EMU_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let checked = programs(100_000, n)
        .filter(|(seed, src)| check_program(src, *seed, Some(&run)))
        .count();
    eprintln!("{checked} of {n} programs checked on the Game Boy");
    assert!(checked as u64 * 10 >= n * 8, "too many programs skipped");
}

#[test]
fn example_programs_agree() {
    let Some(run) = mattone() else {
        return;
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/programs");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "cart") {
            let src = std::fs::read_to_string(&path).unwrap();
            check_program(&src, 0, Some(&run));
        }
    }
}
