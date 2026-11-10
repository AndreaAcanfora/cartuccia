# How the compiler works

```text
source ─ lexer ─ parser ─ type inference ─ IR ─ optimizer ─ SM83 code ─ assembler ─ ROM
```

All of it is in this crate, with no dependencies, and compiles both to a command-line tool and to
a WebAssembly module with no imports that the playground calls.

## Front end

[lexer.rs](../src/lexer.rs) and [parser.rs](../src/parser.rs) are hand written: a recursive-descent
parser with one function per precedence level. Errors point at the token and say what was
expected (`this if is never closed: add end`, `comparisons do not chain`).

## Type inference

[check.rs](../src/check.rs) gives every variable, parameter, return value and number literal a
type variable, and unifies variables that must agree, with union-find. A class of variables is
either unknown, "some integer" (with the range of the literals seen), or a concrete type. Each
class remembers *why* it became concrete (`spr() takes u8 as its x`, `x is declared u8`), so a
clash can show both reasons.

After inference, classes still undecided get the smallest type that holds their literals, and the
checker looks for mistakes that need the final types: comparisons of unsigned values with 0 that
are always true or false. It also builds the call graph and rejects recursion.

## The IR

[ir.rs](../src/ir.rs): each function is a control-flow graph of basic blocks. Instructions are
three-address code on virtual registers that are 8 or 16 bits wide:

```text
function _update():
  b0:
    %1 = joy()
    %2 = and %1, 32
    %3 = ne %2, 0
    br %3, b2, b1
```

[lower.rs](../src/lower.rs) produces it. Conditions are lowered straight to branches, so
`a and b` in an `if` never materializes a bool. `for` loops count the distance to the end rather
than comparing past it, so `for i = 0, 255` in a `u8` terminates.

[interp.rs](../src/interp.rs) executes the IR. It is the definition of what a program means:
division by zero, shift counts past the width, wrap-around, signed division.

## The optimizer

[opt.rs](../src/opt.rs) runs these passes until nothing changes:

- **Inlining** of functions up to 12 instructions, and of any function called once.
- **Sparse conditional constant propagation**: constants flow through the graph, and branches
  on known conditions mark the other side unreachable, so constants flow through loops too.
- **Algebraic simplification and strength reduction**: `x + 0`, `x * 1`, `x & 0xFF` on a byte
  disappear; `x * 8` becomes `x << 3`; `x * 10` becomes `(x << 3) + (x << 1)`; `x / 4` and
  `x % 4` on unsigned values become shifts and masks.
- **Coalescing**: `t = a + b; x = t` writes `x` directly.
- **Copy propagation** within blocks.
- **Dead code elimination** by liveness analysis.
- **Control-flow simplification**: constant branches become jumps, empty blocks are skipped,
  blocks with one predecessor are merged into it, unreachable blocks go.

Functions never called after inlining are left out of the ROM.

## Code generation

[codegen.rs](../src/codegen.rs) turns the IR into SM83 instructions. The SM83 is the Game Boy's
CPU: 8-bit, an accumulator A, six more 8-bit registers paired as BC, DE, HL, no multiply, no divide,
and only HL can address memory freely.

**Memory layout.** Without recursion, every register can have a fixed address. Functions that can
be active at the same time are found on the call graph; the others share their memory: each
function's frame starts after the frames of all functions that can call it. Within a frame, the
registers are sorted by use count, weighted by 4 per level of loop nesting, and the hottest 8-bit
ones go in HRAM (`$FF80`-`$FFFE`), where `ldh a,(n)` is 2 bytes and 3 cycles against
`ld a,(nn)`'s 3 bytes and 4 cycles.

**Temporaries that never touch memory.** A value used only by the next instruction stays in A
(8 bits) or HL (16 bits). A comparison feeding a branch is never a value at all: it becomes `cp`
and a conditional jump. A global read once, later in the same block with no store or call in
between, is read straight from its address by the instruction that uses it.

**The register cache.** The generator knows what every register holds: a constant, or the byte
at some address. It updates this after every instruction it emits (a store to an address forgets
other copies of it; a call forgets everything), merges it at block entries from the jumps that
lead there, and skips loads of values already in a register. Loading two variables at `$C600` and
`$C601` becomes `ld hl,$C600 / ld b,(hl) / ld l,$01 / ld c,(hl)`.

**Instruction selection** knows the SM83's shortcuts: `inc a` for `+ 1`, `xor a` for 0,
`add a,a` for `<< 1`, `swap a / and $F0` for `<< 4`, `add hl,hl` for 16-bit shifts,
`x > 8` as `cp 9`, `sbc a,a` to turn the carry into a mask, and `or a` skipped when the last
operation already set the zero flag from A.

Multiplication, division and shifts by a variable call the runtime.

## The runtime and the ROM

[runtime.rs](../src/runtime.rs) is the code in every ROM, written with the assembler: start-up
(clearing memory, loading the font and the graphics into video memory), the VBlank interrupt,
the frame loop (sprite DMA, the queue of background writes, scroll registers, joypad), and the
routines for sprites, text, numbers, sound, random numbers, multiplication and division.

[sm83.rs](../src/sm83.rs) encodes instructions and resolves labels. Jumps start as the 2-byte
`jr` and become 3-byte `jp` only when their target is out of range, repeating until nothing
changes. [rom.rs](../src/rom.rs) pads the result to 32 KB and writes the header: the Nintendo logo
the boot ROM checks, the title, the cartridge type and both checksums.

## Testing

[tests/fuzz.rs](../tests/fuzz.rs) generates random programs that use every type, operator,
conversion, array kind, loop shape and function call, and runs each one:

1. in the IR interpreter, without optimization: the reference;
2. in the IR interpreter, optimized;
3. as a ROM on [mattone](https://github.com/AndreaAcanfora/mattone), compiled with and without
   optimization.

Every `debug()` line must match. A failure saves the program to a file to reproduce it. The CI
runs 5,000 programs through the optimizer and 400 on the emulator.
