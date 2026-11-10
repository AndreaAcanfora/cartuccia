# Changelog

## 0.1.0

- The language: Lua syntax, `_init` / `_update`, `u8 i8 u16 i16 bool` inferred from use, arrays in
  RAM and ROM, constants, pixel art in `gfx [[ ]]` blocks.
- Built-ins for sprites, the background, text, numbers, joypad, sound, random numbers and debug output.
- Type inference with explanations, recursion detection, checks for comparisons that are always
  true or false.
- The optimizer: inlining, sparse conditional constant propagation, strength reduction, copy
  propagation, coalescing, dead code elimination, control-flow simplification.
- SM83 code generation with overlaid static frames, HRAM allocation, a register cache and fused
  comparisons; a runtime library; 32 KB ROMs with a valid header.
- The playground: live compilation, the mattone emulator, annotated assembly, IR, stats, share links.
- Differential fuzzing against an IR interpreter and the emulator.
