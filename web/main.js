// The playground: the compiler (cartuccia.wasm, Rust) turns the editor's text into a ROM,
// and the emulator (mattone.wasm, C) runs it. Both run in the page; nothing goes to a server.

import { highlight } from "./editor.js";

const EXAMPLES = [
  { file: "hello.cart", name: "Hello, Game Boy" },
  { file: "snake.cart", name: "Snake" },
  { file: "breakout.cart", name: "Breakout" },
  { file: "starfield.cart", name: "Starfield" },
  { file: "music.cart", name: "Music box" },
];
const FRAME_MS = 1000 / 59.7275;
const BUTTONS = { right: 1, left: 2, up: 4, down: 8, a: 16, b: 32, select: 64, start: 128 };
const KEYS = {
  ArrowRight: "right", ArrowLeft: "left", ArrowUp: "up", ArrowDown: "down",
  KeyX: "a", KeyZ: "b", Enter: "start", ShiftLeft: "select", ShiftRight: "select",
};
const SHADES = ["#e0f8d0", "#88c070", "#346856", "#081820"];
const OUT = { rom: 0, error: 1, asm: 2, irBefore: 3, irAfter: 4, stats: 5, runtimeAsm: 6 };

const $ = (id) => document.getElementById(id);
const source = $("source");
const highlightEl = $("highlight");
const gutter = $("gutter");
const statusEl = $("status");
const canvas = $("screen");
const ctx = canvas.getContext("2d");

async function instantiate(url) {
  const bytes = await (await fetch(url)).arrayBuffer();
  return (await WebAssembly.instantiate(bytes, {})).instance.exports;
}

const [compiler, gb] = await Promise.all([instantiate("cartuccia.wasm"), instantiate("mattone.wasm")]);

// ---------- compiling ----------

const encoder = new TextEncoder();
const decoder = new TextDecoder();
let result = null; // the last successful compile: { rom, stats, ... }
let errorLine = 0;

function output(kind) {
  const ptr = compiler.output_ptr(kind);
  const len = compiler.output_len(kind);
  return new Uint8Array(compiler.memory.buffer, ptr, len).slice();
}

function titleOf(src) {
  const first = /^\s*--\s*([^.:\n]+)/.exec(src);
  return (first ? first[1] : "game").trim().toUpperCase().replace(/[^A-Z0-9 ]/g, "").slice(0, 15) || "GAME";
}

function compile(src) {
  const title = titleOf(src);
  const bytes = encoder.encode(src + title);
  const ptr = compiler.input(bytes.length);
  new Uint8Array(compiler.memory.buffer, ptr, bytes.length).set(bytes);
  const t0 = performance.now();
  const ok = compiler.compile($("optimize").checked ? 1 : 0, encoder.encode(title).length);
  const ms = performance.now() - t0;
  if (!ok) {
    return { error: JSON.parse(decoder.decode(output(OUT.error))), ms };
  }
  return {
    title,
    ms,
    rom: output(OUT.rom),
    asm: decoder.decode(output(OUT.asm)),
    runtimeAsm: decoder.decode(output(OUT.runtimeAsm)),
    irBefore: decoder.decode(output(OUT.irBefore)),
    irAfter: decoder.decode(output(OUT.irAfter)),
    stats: JSON.parse(decoder.decode(output(OUT.stats))),
  };
}

function build(run) {
  const r = compile(source.value);
  if (r.error) {
    errorLine = r.error.line;
    statusEl.className = "error";
    statusEl.textContent = r.error.text;
  } else {
    errorLine = 0;
    result = r;
    const s = r.stats;
    statusEl.className = "ok";
    statusEl.textContent = `✓ compiled in ${r.ms.toFixed(1)} ms · ${s.rom_used.toLocaleString("en")} of 32,768 bytes · ` +
      `${s.ir_before} → ${s.ir_after} IR instructions`;
    renderPanels();
    if (run) insert(r.rom);
  }
  render();
}

// ---------- editor ----------

function render() {
  highlightEl.innerHTML = highlight(source.value, errorLine);
  const lines = source.value.split("\n").length;
  let g = "";
  for (let i = 1; i <= lines; i++) g += i === errorLine ? `<span class="err">${i}</span>\n` : `${i}\n`;
  gutter.innerHTML = g;
}

let timer = 0;
source.addEventListener("input", () => {
  render();
  saveDraft();
  clearTimeout(timer);
  timer = setTimeout(() => build($("live").checked), 350);
});

source.addEventListener("keydown", (e) => {
  if (e.key === "Tab") {
    e.preventDefault();
    document.execCommand("insertText", false, "  ");
  } else if (e.key === "Enter" && !e.ctrlKey && !e.metaKey) {
    // keep the indentation, one level more after a line that opens a block
    const before = source.value.slice(0, source.selectionStart);
    const line = before.slice(before.lastIndexOf("\n") + 1);
    let indent = /^\s*/.exec(line)[0];
    if (/^\s*(function|if|elseif|else|while|for)\b/.test(line) && !/\bend\s*$/.test(line)) indent += "  ";
    e.preventDefault();
    document.execCommand("insertText", false, "\n" + indent);
  }
});

addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    build(true);
    canvas.focus();
  }
});

function setSource(text) {
  source.value = text;
  render();
  build(true);
}

function saveDraft() {
  try {
    localStorage.setItem("cartuccia:draft", source.value);
  } catch {
    // storage unavailable: drafts are a convenience
  }
}

// ---------- examples and sharing ----------

const examplesEl = $("examples");
examplesEl.innerHTML = `<option value="">Examples…</option>` +
  EXAMPLES.map((x) => `<option value="${x.file}">${x.name}</option>`).join("");
examplesEl.addEventListener("change", async () => {
  if (!examplesEl.value) return;
  const text = await (await fetch(`examples/${examplesEl.value}`)).text();
  history.replaceState(null, "", location.pathname);
  setSource(text);
  examplesEl.value = "";
  canvas.focus();
});

const b64url = (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const unb64url = (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));

async function pack(text) {
  const stream = new Blob([encoder.encode(text)]).stream().pipeThrough(new CompressionStream("deflate-raw"));
  return b64url(new Uint8Array(await new Response(stream).arrayBuffer()));
}

async function unpack(code) {
  const stream = new Blob([unb64url(code)]).stream().pipeThrough(new DecompressionStream("deflate-raw"));
  return decoder.decode(await new Response(stream).arrayBuffer());
}

$("share").addEventListener("click", async () => {
  const url = `${location.origin}${location.pathname}#code=${await pack(source.value)}`;
  history.replaceState(null, "", url);
  try {
    await navigator.clipboard.writeText(url);
    toast("Link copied: anyone who opens it can play and edit your game");
  } catch {
    toast("The link is in the address bar");
  }
});

$("download").addEventListener("click", () => {
  if (!result) return toast("Fix the errors first");
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([result.rom], { type: "application/octet-stream" }));
  a.download = `${result.title.toLowerCase().replace(/ +/g, "-") || "game"}.gb`;
  a.click();
  URL.revokeObjectURL(a.href);
  toast("Saved: it runs in any Game Boy emulator, or on a real Game Boy with a flash cartridge");
});

$("run").addEventListener("click", () => {
  build(true);
  canvas.focus();
});
$("optimize").addEventListener("change", () => build(true));

// ---------- panels ----------

const tabs = document.querySelectorAll("#tabs button");
let activeTab = "stats";
for (const t of tabs) {
  t.addEventListener("click", () => {
    activeTab = t.dataset.tab;
    for (const u of tabs) u.setAttribute("aria-selected", String(u === t));
    for (const p of document.querySelectorAll(".panel")) p.hidden = p.dataset.panel !== activeTab;
    renderPanels();
  });
}
$("show-runtime").addEventListener("change", renderPanels);
for (const r of document.querySelectorAll("input[name=ir]")) r.addEventListener("change", renderPanels);

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");

function asmHtml(text) {
  return text.split("\n").map((line) => {
    if (/^\s+; /.test(line)) return `<span class="asm-src">${esc(line)}</span>`;
    if (/^\S.*:$/.test(line)) return `<span class="asm-label">${esc(line)}</span>`;
    const m = /^(\s+[0-9A-F]{4})(\s+[0-9A-F ]+\+?)(.*)$/.exec(line);
    return m ? `<span class="asm-addr">${m[1]}</span><span class="asm-bytes">${esc(m[2])}</span>${esc(m[3])}` : esc(line);
  }).join("\n");
}

function renderPanels() {
  if (!result) return;
  const s = result.stats;
  if (activeTab === "stats") {
    const pct = (n) => `${((n / 32768) * 100).toFixed(2)}%`;
    const kb = (n) => n.toLocaleString("en");
    $("stats").innerHTML = `
      <div class="tiles">
        <div class="tile"><b>${kb(s.rom_used)}</b><span>bytes of 32,768 ROM</span></div>
        <div class="tile"><b>${kb(s.code)}</b><span>bytes of your code</span></div>
        <div class="tile"><b>${kb(s.ram)}</b><span>bytes of RAM (of 6,144)</span></div>
        <div class="tile"><b>${s.hram}</b><span>bytes in fast HRAM</span></div>
        <div class="tile"><b>${s.ir_before} → ${s.ir_after}</b><span>IR instructions, optimized</span></div>
      </div>
      <div class="bar" role="img" aria-label="ROM use">
        <span class="rt" style="width:${pct(s.runtime + 0x150)}"></span>
        <span class="code" style="width:${pct(s.code)}"></span>
        <span class="data" style="width:${pct(s.data)}"></span>
      </div>
      <div class="legend">
        <span><i style="background:#5c6378"></i>runtime ${kb(s.runtime + 0x150)}</span>
        <span><i style="background:var(--accent)"></i>your code ${kb(s.code)}</span>
        <span><i style="background:var(--num)"></i>text, tables, graphics ${kb(s.data)}</span>
        <span>free ${kb(32768 - s.rom_used)}</span>
      </div>
      <table>
        <thead><tr><th>function</th><th>bytes</th><th>RAM</th><th>HRAM</th></tr></thead>
        <tbody>${s.funcs.map((f) => `<tr><td>${esc(f.name)}</td><td>${f.bytes}</td><td>${f.ram}</td><td>${f.hram}</td></tr>`).join("")}</tbody>
      </table>
      <p class="note" style="margin-top:12px">Functions that never run at the same time share their RAM, so a function's
      variables cost nothing while it is not running. Small functions and functions called once are inlined.</p>`;
  } else if (activeTab === "asm") {
    const text = $("show-runtime").checked ? result.runtimeAsm + result.asm : result.asm;
    $("asm").innerHTML = asmHtml(text);
  } else if (activeTab === "ir") {
    const after = document.querySelector("input[name=ir]:checked").value === "after";
    $("ir").textContent = after ? result.irAfter : result.irBefore;
  }
}

$("ref").innerHTML = `
  <h3>The frame</h3>
  <p><code>function _init()</code> runs once at the start; <code>function _update()</code> runs every frame, 60 times a second.</p>
  <h3>Drawing</h3>
  <dl>
    <dt>spr(n, x, y, tile)</dt><dd>sprite n (0-39) at pixel x, y; sprites float over the background</dd>
    <dt>hide(n)</dt><dd>takes sprite n off the screen</dd>
    <dt>tile(col, row, t)</dt><dd>background tile at grid cell col, row (20 x 18 visible)</dd>
    <dt>gettile(col, row)</dt><dd>reads a background tile back: handy for walls and collisions</dd>
    <dt>text(col, row, "HI")</dt><dd>text on the background</dd>
    <dt>num(col, row, n)</dt><dd>a number, left-aligned in 5 cells</dd>
    <dt>cls()</dt><dd>clears the background</dd>
    <dt>scroll(x, y)</dt><dd>scrolls the background</dd>
  </dl>
  <h3>Input, sound, time</h3>
  <dl>
    <dt>btn(LEFT)</dt><dd>true while held: LEFT RIGHT UP DOWN A B START SELECT</dd>
    <dt>btnp(A)</dt><dd>true only in the frame it goes down</dd>
    <dt>beep(note, len)</dt><dd>note 0-71 from C3 (12 per octave), len 1-7</dd>
    <dt>noise(len)</dt><dd>a burst of noise</dd>
    <dt>rnd(n)</dt><dd>a random number from 0 to n-1</dd>
    <dt>frames()</dt><dd>frames since the start</dd>
    <dt>vsync()</dt><dd>waits for the next frame, for games written as a loop</dd>
    <dt>debug(x)</dt><dd>prints x to the Link port tab</dd>
  </dl>
  <h3>Graphics</h3>
  <p><code>gfx name = [[ ... ]]</code> draws tiles in the code: <code>.</code> white, <code>1</code> light,
  <code>2</code> dark, <code>3</code> black. 8x8 pixels per tile; bigger drawings become several tiles
  (name, name+1, ...).</p>
  <h3>Types</h3>
  <p><code>u8 i8 u16 i16 bool</code>. You rarely write them: a variable passed to <code>spr()</code> as x becomes
  <code>u8</code>, one passed to <code>num()</code> becomes <code>u16</code>. Numbers wrap around like on the real CPU.
  Convert with <code>u16(x)</code>. Arrays: <code>var grid: u8[64]</code>, <code>const notes: u8[] = {1, 2, 3}</code>.</p>
  <h3>Language</h3>
  <p>Lua's: <code>if … then … elseif … else … end</code>, <code>while … do … end</code>,
  <code>for i = 0, 9 do … end</code> (inclusive, optional step), <code>local</code>, <code>and or not</code>,
  <code>~=</code>, <code>--</code> comments. Plus <code>+= -= *=</code>, <code>&amp; | ^ ~ &lt;&lt; &gt;&gt;</code> and <code>'A'</code> character codes.</p>`;

// ---------- the Game Boy ----------

let romLoaded = false;
let held = 0;
let tapped = 0; // pressed since the last frame: a press shorter than a frame still counts
let serialText = "";
let audio = null;
let muted = localStorage.getItem("cartuccia:muted") === "1";

function insert(rom) {
  new Uint8Array(gb.memory.buffer).set(rom, gb.rom_buffer());
  if (gb.load(rom.length, 1) !== 0) return toast("The emulator refused the ROM");
  romLoaded = true;
  serialText = "";
  $("serial").textContent = "";
  gb.set_sample_rate(audio && !muted ? audio.ctx.sampleRate : 0);
}

const view = new DataView(gb.memory.buffer, gb.palette(), 16);
SHADES.forEach((hex, i) => {
  const n = parseInt(hex.slice(1), 16);
  view.setUint32(i * 4, (0xff << 24) | ((n & 0xff) << 16) | (n & 0xff00) | (n >> 16), true);
});

let image = null;
function draw() {
  if (!image || image.data.buffer !== gb.memory.buffer) {
    image = new ImageData(new Uint8ClampedArray(gb.memory.buffer, gb.screen(), 160 * 144 * 4), 160, 144);
  }
  ctx.putImageData(image, 0, 0);
}

function readSerial() {
  const n = gb.serial_take?.() ?? 0;
  if (!n) return;
  serialText += decoder.decode(new Uint8Array(gb.memory.buffer, gb.serial_buffer(), n));
  if (serialText.length > 20000) serialText = serialText.slice(-10000);
  const el = $("serial");
  el.textContent = serialText;
  el.scrollTop = el.scrollHeight;
}

function playAudio() {
  const frames = gb.audio_take();
  if (!frames || !audio || muted) return;
  const { ctx: actx } = audio;
  const pcm = new Int16Array(gb.memory.buffer, gb.audio_buffer(), frames * 2);
  const buffer = actx.createBuffer(2, frames, actx.sampleRate);
  const left = buffer.getChannelData(0);
  const right = buffer.getChannelData(1);
  for (let i = 0; i < frames; i++) {
    left[i] = pcm[i * 2] / 32768;
    right[i] = pcm[i * 2 + 1] / 32768;
  }
  const now = actx.currentTime;
  if (audio.next < now + 0.01 || audio.next > now + 0.25) audio.next = now + 0.05;
  const src = actx.createBufferSource();
  src.buffer = buffer;
  src.connect(actx.destination);
  src.start(audio.next);
  audio.next += frames / actx.sampleRate;
}

function startAudio() {
  if (audio || muted) return;
  audio = { ctx: new AudioContext({ latencyHint: "interactive" }), next: 0 };
  gb.set_sample_rate(audio.ctx.sampleRate);
}

for (const type of ["pointerdown", "keydown"]) {
  addEventListener(type, () => (audio ? audio.ctx.resume() : startAudio()), { capture: true });
}

$("sound").textContent = muted ? "🔇" : "🔊";
$("sound").addEventListener("click", () => {
  muted = !muted;
  localStorage.setItem("cartuccia:muted", muted ? "1" : "0");
  $("sound").textContent = muted ? "🔇" : "🔊";
  if (!muted) startAudio();
  gb.set_sample_rate(audio && !muted ? audio.ctx.sampleRate : 0);
});

let last = performance.now();
let debt = 0;
function loop(now) {
  const elapsed = Math.min(now - last, 250);
  last = now;
  if (romLoaded && !document.hidden) {
    debt += elapsed;
    let ran = 0;
    while (debt >= FRAME_MS && ran < 4) {
      gb.set_buttons(held | tapped);
      tapped = 0;
      gb.run_frame();
      readSerial();
      debt -= FRAME_MS;
      ran++;
    }
    if (ran) {
      draw();
      playAudio();
    }
  }
  requestAnimationFrame(loop);
}

function press(name, down) {
  const bit = BUTTONS[name];
  held = down ? held | bit : held & ~bit;
  if (down) tapped |= bit;
  document.querySelectorAll(`[data-button="${name}"]`).forEach((b) => b.classList.toggle("held", down));
}

// The keyboard plays only while the screen has focus, so typing never moves the game.
canvas.addEventListener("keydown", (e) => {
  const name = KEYS[e.code];
  if (name) {
    e.preventDefault();
    press(name, true);
  }
});
canvas.addEventListener("keyup", (e) => {
  const name = KEYS[e.code];
  if (name) press(name, false);
});
canvas.addEventListener("blur", () => Object.keys(BUTTONS).forEach((n) => press(n, false)));

const pad = $("pad");
const pointers = new Map();
const buttonAt = (x, y) => document.elementFromPoint(x, y)?.closest?.("[data-button]")?.dataset.button ?? null;
pad.addEventListener("pointerdown", (e) => {
  const name = buttonAt(e.clientX, e.clientY);
  if (!name) return;
  e.preventDefault();
  pad.setPointerCapture(e.pointerId);
  pointers.set(e.pointerId, name);
  press(name, true);
});
pad.addEventListener("pointermove", (e) => {
  if (!pointers.has(e.pointerId)) return;
  const before = pointers.get(e.pointerId);
  const now = buttonAt(e.clientX, e.clientY);
  if (now === before) return;
  if (before) press(before, false);
  if (now) press(now, true);
  pointers.set(e.pointerId, now);
});
for (const type of ["pointerup", "pointercancel"]) {
  pad.addEventListener(type, (e) => {
    const name = pointers.get(e.pointerId);
    pointers.delete(e.pointerId);
    if (name && ![...pointers.values()].includes(name)) press(name, false);
  });
}
pad.addEventListener("contextmenu", (e) => e.preventDefault());

function toast(text) {
  const t = $("toast");
  t.textContent = text;
  t.classList.add("show");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => t.classList.remove("show"), 3000);
}

// ---------- start ----------

async function fromHash() {
  const hash = new URLSearchParams(location.hash.slice(1));
  if (hash.get("code")) {
    try {
      return await unpack(hash.get("code"));
    } catch {
      toast("That link is damaged: loading an example instead");
    }
  } else if (hash.get("example")) {
    const file = EXAMPLES.find((x) => x.file === `${hash.get("example")}.cart`)?.file;
    if (file) return (await fetch(`examples/${file}`)).text();
  }
  return null;
}

addEventListener("hashchange", async () => {
  const text = await fromHash();
  if (text !== null && text !== source.value) setSource(text);
});

let initial = await fromHash();
if (initial === null) {
  try {
    initial = localStorage.getItem("cartuccia:draft");
  } catch {
    initial = null;
  }
}
if (!initial) initial = await (await fetch("examples/hello.cart")).text();
setSource(initial);
requestAnimationFrame(loop);
