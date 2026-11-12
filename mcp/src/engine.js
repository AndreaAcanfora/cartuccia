// The compiler (cartuccia.wasm, Rust) and the Game Boy (mattone.wasm, C), the same modules the
// playground runs, driven from Node: compile a program, run it with scripted buttons, take
// screenshots as PNG.

import fs from "node:fs";
import zlib from "node:zlib";

const here = (name) => new URL(`../assets/${name}`, import.meta.url);
const encoder = new TextEncoder();
const decoder = new TextDecoder();

const OUT = { rom: 0, error: 1, asm: 2, irBefore: 3, irAfter: 4, stats: 5 };
export const BUTTONS = { right: 1, left: 2, up: 4, down: 8, a: 16, b: 32, select: 64, start: 128 };
const SHADES = [0xe0f8d0, 0x88c070, 0x346856, 0x081820];

const instantiate = async (bytes) => (await WebAssembly.instantiate(bytes, {})).instance.exports;
const compilerModule = instantiate(fs.readFileSync(here("cartuccia.wasm")));
export const mattoneWasm = fs.readFileSync(here("mattone.wasm"));

export function titleOf(src) {
  const first = /^\s*--\s*([^.:\n]+)/.exec(src);
  return (first ? first[1] : "game").trim().toUpperCase().replace(/[^A-Z0-9 ]/g, "").slice(0, 15) || "GAME";
}

/** Compiles a program: { ok: true, rom, title, stats, asm, ir } or { ok: false, error }. */
export async function compile(src, { optimize = true } = {}) {
  const c = await compilerModule;
  const title = titleOf(src);
  const bytes = encoder.encode(src + title);
  const ptr = c.input(bytes.length);
  new Uint8Array(c.memory.buffer, ptr, bytes.length).set(bytes);
  const ok = c.compile(optimize ? 1 : 0, encoder.encode(title).length);
  const out = (kind) => new Uint8Array(c.memory.buffer, c.output_ptr(kind), c.output_len(kind)).slice();
  const text = (kind) => decoder.decode(out(kind));
  if (!ok) return { ok: false, error: JSON.parse(text(OUT.error)) };
  return {
    ok: true,
    title,
    rom: out(OUT.rom),
    stats: JSON.parse(text(OUT.stats)),
    asm: () => text(OUT.asm),
    ir: () => text(OUT.irAfter),
  };
}

/** A running Game Boy with a cartridge in it. */
export class GameBoy {
  static async create(rom) {
    const gb = new GameBoy(await instantiate(mattoneWasm));
    gb.insert(rom);
    return gb;
  }

  constructor(exports) {
    this.gb = exports;
    const view = new DataView(exports.memory.buffer, exports.palette(), 16);
    SHADES.forEach((n, i) => view.setUint32(i * 4, (0xff << 24) | ((n & 0xff) << 16) | (n & 0xff00) | (n >> 16), true));
    this.frame = 0;
    this.serial = "";
  }

  insert(rom) {
    const { gb } = this;
    new Uint8Array(gb.memory.buffer).set(rom, gb.rom_buffer());
    if (gb.load(rom.length, 1) !== 0) throw new Error("the emulator refused the ROM");
    gb.set_sample_rate(0);
    this.frame = 0;
    this.serial = "";
  }

  /** Runs `frames` frames with `buttons` held (a press shorter than a frame is not needed here). */
  run(frames, buttons = []) {
    const { gb } = this;
    let mask = 0;
    for (const b of buttons) {
      const bit = BUTTONS[String(b).toLowerCase()];
      if (bit === undefined) throw new Error(`unknown button "${b}": use ${Object.keys(BUTTONS).join(", ")}`);
      mask |= bit;
    }
    for (let i = 0; i < frames; i++) {
      gb.set_buttons(mask);
      gb.run_frame();
      this.frame++;
      const n = gb.serial_take();
      if (n) this.serial += decoder.decode(new Uint8Array(gb.memory.buffer, gb.serial_buffer(), n));
    }
    if (this.serial.length > 20000) this.serial = this.serial.slice(-10000);
  }

  /** Takes what debug() printed since the last call. */
  takeSerial() {
    const s = this.serial;
    this.serial = "";
    return s;
  }

  /** The screen as a PNG, each Game Boy pixel `scale` x `scale`. */
  png(scale = 3) {
    const { gb } = this;
    const rgba = new Uint8Array(gb.memory.buffer, gb.screen(), 160 * 144 * 4);
    return encodePng(rgba, 160, 144, scale);
  }

  /** The screen as text, one character per 2x2 pixel block, for a quick look without an image. */
  text() {
    const { gb } = this;
    const rgba = new Uint8Array(gb.memory.buffer, gb.screen(), 160 * 144 * 4);
    const shade = (x, y) => {
      const i = (y * 160 + x) * 4;
      const v = rgba[i] + rgba[i + 1] + rgba[i + 2];
      return v > 600 ? 0 : v > 400 ? 1 : v > 200 ? 2 : 3;
    };
    const chars = " .o#";
    const rows = [];
    for (let y = 0; y < 144; y += 2) {
      let row = "";
      for (let x = 0; x < 160; x += 2) {
        row += chars[Math.max(shade(x, y), shade(x + 1, y), shade(x, y + 1), shade(x + 1, y + 1))];
      }
      rows.push(row.trimEnd());
    }
    return rows.join("\n");
  }
}

// ---------- PNG ----------

const CRC = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(bytes) {
  let c = 0xffffffff;
  for (const b of bytes) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  Buffer.from(data).copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

export function encodePng(rgba, width, height, scale = 1) {
  const w = width * scale;
  const h = height * scale;
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) {
    const row = y * (w * 3 + 1);
    raw[row] = 0;
    const sy = Math.floor(y / scale);
    for (let x = 0; x < w; x++) {
      const s = (sy * width + Math.floor(x / scale)) * 4;
      raw.set([rgba[s], rgba[s + 1], rgba[s + 2]], row + 1 + x * 3);
    }
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(w, 0);
  header.writeUInt32BE(h, 4);
  header[8] = 8; // bits per channel
  header[9] = 2; // RGB
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", zlib.deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
