// A game as one HTML file: the ROM and the emulator (mattone.wasm) inlined as base64, with a
// small player around them. It needs no server and no network: double-click it and play.

const b64 = (bytes) => {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
};

const escape = (s) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

const CSS = `
:root { color-scheme: dark; }
html, body { margin: 0; height: 100%; background: #14161c; color: #9aa1b1; font: 14px system-ui, sans-serif; }
body {
  display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px;
  height: 100dvh; overflow: hidden; touch-action: none; user-select: none; -webkit-user-select: none;
}
#screen {
  display: block; width: min(96vw, calc((100dvh - 70px) * 10 / 9)); height: auto;
  image-rendering: pixelated; background: #9bbc0f; outline: none;
}
#bar { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; justify-content: center; }
#bar button { font: inherit; color: #e6e6e6; background: #222631; border: 1px solid #2e3340; border-radius: 7px; padding: 3px 9px; }
a { color: #9bbc0f; }
kbd { font: 12px ui-monospace, monospace; border: 1px solid #2e3340; border-radius: 4px; padding: 0 4px; }
#pad { display: none; }
@media (pointer: coarse) {
  #keys { display: none; }
  #screen { width: min(96vw, calc((100dvh - 290px) * 10 / 9)); }
  #pad { display: grid; grid-template-columns: 1fr 1fr; grid-template-areas: "dpad ab" "ss ss"; gap: 12px 24px; width: min(400px, 96vw); }
  #dpad { grid-area: dpad; position: relative; width: 128px; height: 128px; }
  #dpad button { position: absolute; width: 44px; height: 44px; border-radius: 4px; background: #2b2e37; border: none; }
  #dpad .up { left: 42px; top: 0; } #dpad .down { left: 42px; bottom: 0; }
  #dpad .left { left: 0; top: 42px; } #dpad .right { right: 0; top: 42px; }
  #ab { grid-area: ab; display: flex; gap: 14px; align-items: center; justify-content: flex-end; transform: rotate(-20deg); }
  .round { width: 56px; height: 56px; border-radius: 50%; background: #9c2a4a; color: #fff; border: none; font-weight: 700; }
  #startselect { grid-area: ss; display: flex; justify-content: center; gap: 20px; }
  .pill { font-size: 11px; padding: 6px 14px; border-radius: 20px; background: #2b2e37; color: #ccc; border: none; }
  #pad .held { filter: brightness(1.5); }
}
@media (pointer: coarse) and (orientation: landscape) {
  body { display: grid; grid-template-columns: 1fr auto 1fr; grid-template-areas: "dpad screen ab" "ss screen bar"; align-items: center; justify-items: center; }
  #pad { display: contents; }
  #screen { grid-area: screen; width: auto; height: min(calc(100dvh - 24px), calc((100vw - 340px) * 9 / 10)); }
  #bar { grid-area: bar; }
}`;

const PLAYER = `
(async () => {
  const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
  const gb = (await WebAssembly.instantiate(unb64(WASM), {})).instance.exports;
  const rom = unb64(ROM);
  const canvas = document.getElementById("screen");
  const ctx = canvas.getContext("2d");
  const FRAME_MS = 1000 / 59.7275;
  const BUTTONS = { right: 1, left: 2, up: 4, down: 8, a: 16, b: 32, select: 64, start: 128 };
  const KEYS = {
    ArrowRight: "right", ArrowLeft: "left", ArrowUp: "up", ArrowDown: "down",
    KeyX: "a", KeyZ: "b", Enter: "start", ShiftLeft: "select", ShiftRight: "select",
  };

  const view = new DataView(gb.memory.buffer, gb.palette(), 16);
  [0xe0f8d0, 0x88c070, 0x346856, 0x081820].forEach((n, i) =>
    view.setUint32(i * 4, (0xff << 24) | ((n & 0xff) << 16) | (n & 0xff00) | (n >> 16), true));
  new Uint8Array(gb.memory.buffer).set(rom, gb.rom_buffer());
  gb.load(rom.length, 1);
  gb.set_sample_rate(0);

  let held = 0, tapped = 0, audio = null, muted = false, image = null;
  function press(name, down) {
    const bit = BUTTONS[name];
    held = down ? held | bit : held & ~bit;
    if (down) tapped |= bit;
    document.querySelectorAll('[data-button="' + name + '"]').forEach((b) => b.classList.toggle("held", down));
  }
  function full() {
    if (document.fullscreenElement) document.exitFullscreen();
    else document.documentElement.requestFullscreen?.().catch(() => {});
  }
  addEventListener("keydown", (e) => {
    if (e.code === "KeyF") return full();
    const name = KEYS[e.code];
    if (name) { e.preventDefault(); press(name, true); }
  });
  addEventListener("keyup", (e) => { const name = KEYS[e.code]; if (name) press(name, false); });
  addEventListener("blur", () => Object.keys(BUTTONS).forEach((n) => press(n, false)));

  const pad = document.getElementById("pad");
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
    const before = pointers.get(e.pointerId), now = buttonAt(e.clientX, e.clientY);
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
  document.getElementById("full").addEventListener("click", full);
  const sound = document.getElementById("sound");
  sound.addEventListener("click", () => {
    muted = !muted;
    sound.textContent = muted ? "🔇" : "🔊";
    gb.set_sample_rate(audio && !muted ? audio.ctx.sampleRate : 0);
  });

  for (const type of ["pointerdown", "keydown"]) {
    addEventListener(type, () => {
      if (audio) return audio.ctx.resume();
      audio = { ctx: new AudioContext({ latencyHint: "interactive" }), next: 0 };
      if (!muted) gb.set_sample_rate(audio.ctx.sampleRate);
    }, { capture: true });
  }
  function playAudio() {
    const frames = gb.audio_take();
    if (!frames || !audio || muted) return;
    const actx = audio.ctx;
    const pcm = new Int16Array(gb.memory.buffer, gb.audio_buffer(), frames * 2);
    const buffer = actx.createBuffer(2, frames, actx.sampleRate);
    const left = buffer.getChannelData(0), right = buffer.getChannelData(1);
    for (let i = 0; i < frames; i++) { left[i] = pcm[i * 2] / 32768; right[i] = pcm[i * 2 + 1] / 32768; }
    const now = actx.currentTime;
    if (audio.next < now + 0.01 || audio.next > now + 0.25) audio.next = now + 0.05;
    const src = actx.createBufferSource();
    src.buffer = buffer;
    src.connect(actx.destination);
    src.start(audio.next);
    audio.next += frames / actx.sampleRate;
  }

  let last = performance.now(), debt = 0;
  function loop(now) {
    debt += Math.min(now - last, 250);
    last = now;
    let ran = 0;
    while (!document.hidden && debt >= FRAME_MS && ran < 4) {
      gb.set_buttons(held | tapped);
      tapped = 0;
      gb.run_frame();
      debt -= FRAME_MS;
      ran++;
    }
    if (document.hidden) debt = 0;
    if (ran) {
      if (!image || image.data.buffer !== gb.memory.buffer) {
        image = new ImageData(new Uint8ClampedArray(gb.memory.buffer, gb.screen(), 160 * 144 * 4), 160, 144);
      }
      ctx.putImageData(image, 0, 0);
      playAudio();
    }
    requestAnimationFrame(loop);
  }
  requestAnimationFrame(loop);
  canvas.focus();
})();`;

export function standalone(title, rom, wasm) {
  const name = escape(title.charAt(0) + title.slice(1).toLowerCase());
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="theme-color" content="#14161c">
<title>${name}</title>
<style>${CSS}</style>
</head>
<body>
<canvas id="screen" width="160" height="144" tabindex="0" aria-label="${name}"></canvas>
<div id="pad" aria-label="Game Boy buttons">
  <div id="dpad">
    <button data-button="up" class="up" aria-label="Up"></button>
    <button data-button="left" class="left" aria-label="Left"></button>
    <button data-button="right" class="right" aria-label="Right"></button>
    <button data-button="down" class="down" aria-label="Down"></button>
  </div>
  <div id="ab">
    <button data-button="b" class="round" aria-label="B">B</button>
    <button data-button="a" class="round" aria-label="A">A</button>
  </div>
  <div id="startselect">
    <button data-button="select" class="pill" aria-label="Select">SELECT</button>
    <button data-button="start" class="pill" aria-label="Start">START</button>
  </div>
</div>
<div id="bar">
  <span id="keys">Arrows · <kbd>X</kbd> A · <kbd>Z</kbd> B · <kbd>Enter</kbd> Start · <kbd>Shift</kbd> Select</span>
  <button id="full" title="Full screen (F)">⛶</button>
  <button id="sound" title="Sound">🔊</button>
  <span>made with <a href="https://andreaacanfora.github.io/cartuccia/">cartuccia</a></span>
</div>
<script>
const ROM = "${b64(rom)}";
const WASM = "${b64(wasm)}";
${PLAYER}
</script>
</body>
</html>
`;
}
