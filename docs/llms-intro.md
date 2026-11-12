# cartuccia: notes for an AI writing a Game Boy game

cartuccia is a small Lua-flavoured language that compiles to real Game Boy ROMs. Programs are
written and played at https://andreaacanfora.github.io/cartuccia/ (paste the code in the editor:
it compiles and restarts as you type). This file is everything needed to write one: the rules
that trip people up, the full language reference, and two complete games.

## How to answer

- Reply with the whole program in a single ```lua code block, never a fragment or a diff.
- The first line is a comment with the title and the controls, like
  `-- Space Dodge. Arrows move, A jumps, START restarts.`: the title becomes the cartridge name.
- Draw the graphics yourself with `gfx` blocks (8x8 pixels, `.` `1` `2` `3`).
- Keep it playable: show the score and how to start, let START restart after game over.
- After the code, say in a sentence or two how to play.

## Rules that trip people up

- It is Lua's syntax, but not Lua: no tables, no strings in variables, no closures, no `..`,
  no `print`. Data lives in fixed-size arrays (`var xs: u8[16]`) and `const` arrays in ROM.
- No recursion, direct or indirect. Use loops.
- Conditions must be `bool`: `if n ~= 0 then`, not `if n then`.
- Both sides of an operator need the same type. A `u8` plus a `u16` is an error: convert with
  `u16(x)`. Scores above 255 must be `u16`: declare them `var score: u16 = 0`.
- Numbers wrap like on the CPU: a `u8` at 0 minus 1 is 255. `if x < 0` on an unsigned value is
  rejected; check before subtracting (`if x > 0 then x -= 1 end`). Coordinates are `u8`.
- The screen is 160x144 pixels, or 20x18 background tiles of 8x8. Sprites are 0-39, at most 10
  on one horizontal line; `hide(n)` removes one. Sprite coordinates are the top left corner.
- `text()` and `num()` write background tiles, at tile columns and rows (not pixels). Text is
  ASCII; use capitals. `num()` takes 5 cells.
- Tile writes outside `_init` are queued for the next frame (about 48 per frame): redraw only what
  changes, not the whole screen every frame. `cls()` turns the screen off and back on: use it for
  a new level or game over, not every frame.
- `btnp()` is true only in the frame a button is pressed: use it for jumping, firing, menus.
  `btn()` is true while held: use it for moving. Players press START (Enter on a keyboard) or A
  (X) to begin, so accept both.
- There are only `u8` `i8` `u16` `i16` `bool`; no fractions. For slow movement, move every other
  frame (`if frames() % 2 == 0 then ... end`) or keep positions times 16 in a `u16`.
- RAM is 6 KB and ROM 32 KB: plenty for a small game, but arrays of more than a few hundred
  entries add up.

