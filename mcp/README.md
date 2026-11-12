# cartuccia-mcp

**Let Claude write, play and fix Game Boy games.**

An [MCP](https://modelcontextprotocol.io) server for [cartuccia](https://github.com/AndreaAcanfora/cartuccia),
a small Lua-flavoured language that compiles to real Game Boy ROMs. It gives an AI agent the
compiler and a Game Boy: it writes the game, runs it, presses the buttons, looks at the screen,
fixes what is wrong, and hands you a `.gb` cartridge or a page you can play offline.

It runs the same two WebAssembly modules as the [playground](https://andreaacanfora.github.io/cartuccia/):
the compiler (Rust) and [mattone](https://github.com/AndreaAcanfora/mattone), a Game Boy emulator
in C that passes the mooneye and Blargg test suites. Nothing to build, nothing native: Node 20 or newer.

## Set it up

**Claude Code**

```sh
claude mcp add cartuccia -- npx -y cartuccia-mcp
```

**Claude Desktop, Cursor, and other clients**: add it to the client's MCP configuration
(`claude_desktop_config.json`, `.cursor/mcp.json`, ...):

```json
{
  "mcpServers": {
    "cartuccia": { "command": "npx", "args": ["-y", "cartuccia-mcp"] }
  }
}
```

Then ask for a game: *“Make a Game Boy game where a frog crosses a road full of cars. Test it
by playing it, then export frog.html.”*

## What the agent gets

| tool | |
|---|---|
| `guide` | the language, the rules that trip models up, and two complete games ([llms.txt](https://andreaacanfora.github.io/cartuccia/llms.txt)) |
| `compile` | the ROM and function sizes, or the error with the line, the reason and a fix; optionally the assembly or the IR |
| `run` | runs the game from power-on with a script of button presses, returns screenshots and what the game printed with `debug()` |
| `play_start`, `play` | plays the game one move at a time: hold these buttons for N frames, see the screen |
| `export` | writes a `.gb` cartridge, or a standalone `.html` page (about 90 KB, works offline, touch controls on phones) |

Games are `.cart` files in your project, so the agent edits them with its usual tools and you can
open them in the playground too. The guide is also exposed as the resource `cartuccia://guide`.

## An example

[examples/frog.cart](../examples/frog.cart) was written by Claude Code with this server and one
prompt, no edits by hand. While playtesting it found and fixed, among other things, a HUD where
the level number overwrote the “L” of LIVES, trucks driving backwards in one lane, and a frog that
jumped back to the start the instant it reached the other side. It is in the playground's
examples as “Frog Road”.

## Development

```sh
scripts/build-web.sh        # from the repository root: builds the two .wasm modules and llms.txt
cd mcp
npm install
npm run assets              # copies them into assets/
npm test                    # drives the server through a real MCP client
```
