// The playground's two WebAssembly modules, end to end: compile every example with
// web/cartuccia.wasm, run it for two seconds on web/mattone.wasm, check the screen shows something.
import { readFileSync, readdirSync } from "node:fs";

const load = (path) => new WebAssembly.Instance(new WebAssembly.Module(readFileSync(path)), {}).exports;
const compiler = load("web/cartuccia.wasm");
const gb = load("web/mattone.wasm");
const enc = new TextEncoder();

let failed = 0;
for (const file of readdirSync("examples").filter((f) => f.endsWith(".cart"))) {
  const bytes = enc.encode(readFileSync(`examples/${file}`, "utf8"));
  const ptr = compiler.input(bytes.length);
  new Uint8Array(compiler.memory.buffer, ptr, bytes.length).set(bytes);
  const t0 = performance.now();
  if (!compiler.compile(1, 0)) {
    const err = new Uint8Array(compiler.memory.buffer, compiler.output_ptr(1), compiler.output_len(1));
    console.log(`${file}: compile error ${new TextDecoder().decode(err)}`);
    failed++;
    continue;
  }
  const ms = performance.now() - t0;
  const rom = new Uint8Array(compiler.memory.buffer, compiler.output_ptr(0), compiler.output_len(0)).slice();
  new Uint8Array(gb.memory.buffer).set(rom, gb.rom_buffer());
  if (gb.load(rom.length, 1) !== 0) {
    console.log(`${file}: the emulator refused the ROM`);
    failed++;
    continue;
  }
  for (let f = 0; f < 120; f++) gb.run_frame();
  const screen = new Uint32Array(gb.memory.buffer, gb.screen(), 160 * 144);
  const shades = new Set(screen).size;
  console.log(`${file}: ${rom.length} bytes in ${ms.toFixed(1)} ms, ${shades} shades on screen`);
  if (shades < 2) failed++;
}
process.exit(failed ? 1 : 0);
