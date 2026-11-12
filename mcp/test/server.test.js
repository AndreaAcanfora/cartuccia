// Drives the server through a real MCP client over stdio.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const examples = new URL("../../examples/", import.meta.url);
const breakout = new URL("breakout.cart", examples).pathname;
let client;

before(async () => {
  client = new Client({ name: "test", version: "0" });
  await client.connect(new StdioClientTransport({ command: process.execPath, args: [new URL("../src/index.js", import.meta.url).pathname] }));
});
after(() => client.close());

const call = (name, args = {}) => client.callTool({ name, arguments: args });
const texts = (r) => r.content.filter((c) => c.type === "text").map((c) => c.text).join("\n");
const images = (r) => r.content.filter((c) => c.type === "image");

test("lists the tools", async () => {
  const { tools } = await client.listTools();
  assert.deepEqual(tools.map((t) => t.name).sort(), ["compile", "export", "guide", "play", "play_start", "run"]);
});

test("guide returns llms.txt", async () => {
  assert.match(texts(await call("guide")), /Rules that trip people up/);
});

test("compile: stats for a good program, the error for a bad one", async () => {
  const ok = await call("compile", { file: breakout });
  assert.ok(!ok.isError);
  assert.match(texts(ok), /breakout\.cart compiled: "BREAKOUT"/);
  const bad = await call("compile", { code: "function _update()\n  x = 1\nend\n" });
  assert.ok(bad.isError);
  assert.match(texts(bad), /game\.cart:2:3: error: unknown name `x`/);
  const asm = await call("compile", { file: breakout, show: "asm" });
  assert.match(texts(asm), /call rt_spr/);
});

test("run: screenshots change when the game reacts to buttons", async () => {
  const r = await call("run", {
    file: breakout,
    steps: [
      { frames: 30, screenshot: true },
      { buttons: ["start"], frames: 2 },
      { frames: 60, screenshot: true },
    ],
  });
  assert.ok(!r.isError, texts(r));
  const [a, b] = images(r);
  assert.equal(images(r).length, 2);
  assert.ok(Buffer.from(a.data, "base64").subarray(1, 4).toString() === "PNG");
  assert.notEqual(a.data, b.data, "the ball should have moved after START");
});

test("run: debug() output comes back", async () => {
  const code = "function _update()\n  if frames() == 3 then debug(42) end\nend\n";
  const r = await call("run", { code, steps: [{ frames: 10 }], ascii: true });
  assert.match(texts(r), /debug\(\) output:\n42/);
});

test("play: step by step", async () => {
  assert.ok((await call("play", {})).isError, "play before play_start fails");
  const start = await call("play_start", { file: breakout, frames: 30 });
  const first = images(start)[0].data;
  const moved = await call("play", { buttons: ["left"], frames: 20 });
  assert.match(texts(moved), /frame 51/);
  assert.notEqual(images(moved)[0].data, first, "the bat should have moved");
});

test("export: .gb and .html", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cartuccia-"));
  const gb = path.join(dir, "b.gb");
  const html = path.join(dir, "b.html");
  await call("export", { file: breakout, out: gb });
  await call("export", { file: breakout, out: html });
  assert.equal(fs.statSync(gb).size, 32768);
  assert.match(fs.readFileSync(html, "utf8"), /<title>Breakout<\/title>/);
  assert.ok((await call("export", { file: breakout, out: path.join(dir, "x.zip") })).isError);
});
