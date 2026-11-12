#!/usr/bin/env node
// cartuccia-mcp: an MCP server for writing Game Boy games in cartuccia. An agent reads the guide,
// writes a .cart file, compiles it, runs it with scripted button presses and looks at the
// screen, then plays it step by step, and exports a .gb or a standalone .html.

import fs from "node:fs";
import path from "node:path";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { compile, GameBoy, mattoneWasm, BUTTONS } from "./engine.js";

const pkg = JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8"));
const guide = fs.readFileSync(new URL("../assets/llms.txt", import.meta.url), "utf8");

const MAX_FRAMES = 60 * 60 * 5; // five minutes of game time per call
const MAX_SHOTS = 12;

const server = new McpServer(
  { name: "cartuccia", version: pkg.version },
  {
    instructions:
      "cartuccia is a small Lua-flavoured language that compiles to real Game Boy ROMs. " +
      "Call `guide` once before writing code: the language is not Lua and has rules of its own. " +
      "Write the game to a .cart file, then `compile` it, `run` it with a script of button presses " +
      "and look at the screenshots, and fix what looks wrong. `play_start` and `play` let you play " +
      "it one move at a time. `export` writes a .gb cartridge or a standalone .html page. " +
      "The playground at https://andreaacanfora.github.io/cartuccia/ runs the same code in a browser.",
  },
);

// ---------- helpers ----------

const buttonList = z
  .array(z.enum(Object.keys(BUTTONS)))
  .default([])
  .describe("buttons held during these frames: up, down, left, right, a, b, start, select");

const source = {
  file: z.string().optional().describe("path of a .cart file (relative to the working directory)"),
  code: z.string().optional().describe("the program itself, instead of a file"),
};

function readSource({ file, code }) {
  if (code !== undefined) return { src: code, name: "game.cart" };
  if (!file) throw new Error("give either `file` (a .cart path) or `code`");
  const full = path.resolve(file);
  return { src: fs.readFileSync(full, "utf8"), name: path.basename(full) };
}

const text = (t) => ({ type: "text", text: t });
const image = (gb, scale) => ({ type: "image", data: gb.png(scale).toString("base64"), mimeType: "image/png" });
const fail = (t) => ({ content: [text(t)], isError: true });

// The compiler reports positions as game.cart: show the real file name instead.
const errorText = (error, name) => error.text.replaceAll("game.cart:", `${name}:`);

async function build(args, optimize = true) {
  const { src, name } = readSource(args);
  const r = await compile(src, { optimize });
  return { ...r, name };
}

function statsText(r) {
  const s = r.stats;
  const lines = [
    `✓ ${r.name} compiled: "${r.title}", ${s.rom_used} of 32768 bytes of ROM ` +
      `(runtime ${s.runtime}, your code ${s.code}, data ${s.data}), ${s.ram} of 6144 bytes of RAM, ${s.hram} in HRAM.`,
    `IR instructions: ${s.ir_before} before optimization, ${s.ir_after} after.`,
    "",
    "function            bytes   RAM  HRAM",
  ];
  for (const f of s.funcs) {
    lines.push(`${f.name.padEnd(18)} ${String(f.bytes).padStart(6)} ${String(f.ram).padStart(5)} ${String(f.hram).padStart(5)}`);
  }
  return lines.join("\n");
}

// ---------- tools ----------

server.registerTool(
  "guide",
  {
    title: "cartuccia guide",
    description:
      "The cartuccia language: how to write a game, the rules that trip people up, the full reference " +
      "and two complete games. Read it before writing or changing a game.",
    inputSchema: {},
    annotations: { readOnlyHint: true },
  },
  async () => ({ content: [text(guide)] }),
);

server.registerTool(
  "compile",
  {
    title: "Compile a game",
    description:
      "Compiles a cartuccia program. Returns the size of the ROM and of each function, or the error " +
      "with the line, the reason and a suggested fix. Optionally shows the generated assembly or IR.",
    inputSchema: {
      ...source,
      show: z.enum(["stats", "asm", "ir"]).default("stats").describe("what to return besides the result"),
      optimize: z.boolean().default(true),
    },
    annotations: { readOnlyHint: true },
  },
  async ({ show, optimize, ...args }) => {
    const r = await build(args, optimize);
    if (!r.ok) return fail(errorText(r.error, r.name));
    let out = statsText(r);
    if (show === "asm") out += "\n\n" + r.asm();
    if (show === "ir") out += "\n\n" + r.ir();
    return { content: [text(out)] };
  },
);

const steps = z
  .array(
    z.object({
      buttons: buttonList,
      frames: z.number().int().min(1).max(MAX_FRAMES).describe("how many frames (60 per second)"),
      screenshot: z.boolean().default(false).describe("take a screenshot at the end of this step"),
    }),
  )
  .describe(
    "what to do, in order. Example: wait a second, press START, hold RIGHT for half a second: " +
      '[{"frames":60,"screenshot":true},{"buttons":["start"],"frames":2},{"buttons":["right"],"frames":30,"screenshot":true}]. ' +
      "A button must be released (a step without it) before it counts as pressed again for btnp().",
  );

server.registerTool(
  "run",
  {
    title: "Run a game with scripted input",
    description:
      "Compiles a game and runs it on an emulated Game Boy from power-on, following a script of " +
      "button presses, and returns screenshots and everything the game printed with debug(). " +
      "Use it to check that the game starts, reacts to the buttons and draws what you expect.",
    inputSchema: {
      ...source,
      steps: steps.default([{ buttons: [], frames: 90, screenshot: true }]),
      scale: z.number().int().min(1).max(4).default(2).describe("screenshot zoom: 160x144 pixels times this"),
      ascii: z.boolean().default(false).describe("also return each screenshot as text art (for clients without images)"),
    },
    annotations: { readOnlyHint: true },
  },
  async ({ steps, scale, ascii, ...args }) => {
    const r = await build(args);
    if (!r.ok) return fail(errorText(r.error, r.name));
    const total = steps.reduce((n, s) => n + s.frames, 0);
    if (total > MAX_FRAMES) return fail(`at most ${MAX_FRAMES} frames per call (got ${total})`);
    if (steps.filter((s) => s.screenshot).length > MAX_SHOTS) return fail(`at most ${MAX_SHOTS} screenshots per call`);
    const gb = await GameBoy.create(r.rom);
    const content = [];
    for (const step of steps) {
      gb.run(step.frames, step.buttons);
      if (step.screenshot) {
        const held = step.buttons.length ? `holding ${step.buttons.join("+")}` : "no buttons";
        content.push(text(`frame ${gb.frame} (${(gb.frame / 60).toFixed(1)} s, ${held}):`));
        content.push(image(gb, scale));
        if (ascii) content.push(text(gb.text()));
      }
    }
    const serial = gb.takeSerial();
    content.push(text(serial ? `debug() output:\n${serial}` : "debug() printed nothing."));
    return { content };
  },
);

// One game at a time for step-by-step play.
let session = null;

server.registerTool(
  "play_start",
  {
    title: "Start playing a game",
    description:
      "Compiles a game, turns the Game Boy on and returns the screen after `frames` frames. " +
      "Then call `play` to press buttons and see what happens, one move at a time.",
    inputSchema: {
      ...source,
      frames: z.number().int().min(1).max(MAX_FRAMES).default(60),
      scale: z.number().int().min(1).max(4).default(2),
    },
  },
  async ({ frames, scale, ...args }) => {
    const r = await build(args);
    if (!r.ok) return fail(errorText(r.error, r.name));
    session = { gb: await GameBoy.create(r.rom), scale, name: r.name };
    session.gb.run(frames);
    return {
      content: [text(`${r.name} is running. Frame ${session.gb.frame}:`), image(session.gb, scale), ...serialOf(session.gb)],
    };
  },
);

function serialOf(gb) {
  const s = gb.takeSerial();
  return s ? [text(`debug() output:\n${s}`)] : [];
}

server.registerTool(
  "play",
  {
    title: "Play: press buttons",
    description:
      "Continues the game started with `play_start`: holds `buttons` for `frames` frames, then " +
      "releases them for `release` frames, and returns the screen.",
    inputSchema: {
      buttons: buttonList,
      frames: z.number().int().min(1).max(MAX_FRAMES).default(10),
      release: z.number().int().min(0).max(MAX_FRAMES).default(1).describe("frames with no buttons afterwards"),
    },
  },
  async ({ buttons, frames, release }) => {
    if (!session) return fail("no game running: call play_start first");
    const { gb, scale } = session;
    gb.run(frames, buttons);
    if (release) gb.run(release);
    return { content: [text(`frame ${gb.frame}:`), image(gb, scale), ...serialOf(gb)] };
  },
);

server.registerTool(
  "export",
  {
    title: "Export a game",
    description:
      "Writes the game as a .gb cartridge image (for any Game Boy emulator, or a real Game Boy with a " +
      "flash cartridge) or as a standalone .html page with the emulator inside, which plays offline " +
      "on a computer or a phone.",
    inputSchema: {
      ...source,
      out: z.string().describe("where to write it: a path ending in .gb or .html"),
    },
    annotations: { destructiveHint: false },
  },
  async ({ out, ...args }) => {
    const r = await build(args);
    if (!r.ok) return fail(errorText(r.error, r.name));
    const target = path.resolve(out);
    if (target.endsWith(".gb")) {
      fs.writeFileSync(target, r.rom);
    } else if (target.endsWith(".html")) {
      const { standalone } = await import(new URL("../assets/export.js", import.meta.url));
      fs.writeFileSync(target, standalone(r.title, r.rom, new Uint8Array(mattoneWasm)));
    } else {
      return fail("`out` must end in .gb or .html");
    }
    return { content: [text(`wrote ${target} (${fs.statSync(target).size} bytes)`)] };
  },
);

server.registerResource(
  "guide",
  "cartuccia://guide",
  { title: "cartuccia guide", description: "The language, its pitfalls and two complete games", mimeType: "text/markdown" },
  async (uri) => ({ contents: [{ uri: uri.href, text: guide, mimeType: "text/markdown" }] }),
);

await server.connect(new StdioServerTransport());
