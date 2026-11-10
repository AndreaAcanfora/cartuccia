# The cartuccia language

A program is a list of declarations: graphics, constants, variables and functions, in any order.
The Game Boy calls two of the functions:

```lua
function _init()     -- once, at the start (the screen is off: drawing is instant)
end

function _update()   -- every frame, 60 times a second
end
```

At least one of them must exist. A game can also be one long loop in `_init` that calls `vsync()`
to wait for each frame.

## Values and types

| type | values | |
|---|---|---|
| `u8` | 0 to 255 | the native size: fastest |
| `i8` | -128 to 127 | |
| `u16` | 0 to 65535 | scores, timers |
| `i16` | -32768 to 32767 | |
| `bool` | `true`, `false` | conditions |

Types are inferred: you only write them when you want to. Each variable, parameter and return
value gets the type that its uses demand. Passing a variable to `spr()` as a coordinate makes it
`u8`; passing it to `num()` makes it `u16`; adding it to a `u16` makes it `u16`. A variable that
nothing pins down gets the smallest type that holds its literals: `u8` for 0..255, `u16` above,
`i8`/`i16` when a literal is negative. Write the type to be sure: `var score: u16 = 0`,
`local dx: i8 = -1`, `function add(a: u8, b: u8): u8`.

Arithmetic wraps around, as on the CPU: a `u8` at 255 plus 1 is 0, and `-1` stored in a `u8` is 255.
That makes `x += dx` with `dx = -1` move left even when both are `u8`.

The two sides of an operator must have the same type. Convert with `u8(x)`, `i8(x)`, `u16(x)`,
`i16(x)`: widening a signed value keeps its sign, narrowing keeps the low byte, and a `bool`
converts to 0 or 1.

Numbers: `42`, `0xFF`, `0b1010_0001`, and `'A'` (the character code, 65). Text is ASCII only.

## Declarations

```lua
const SPEED = 2               -- a constant: replaced by its value, adapts to any type
const MAX: u16 = 1000         -- a constant with a fixed type
var lives = 3                 -- a variable (in RAM, starts at its value)
var score: u16                -- starts at 0
var grid: u8[64]              -- an array in RAM, starts filled with zeros
const level: u8[] = {1, 0, 1} -- an array in ROM, read-only

gfx ship = [[
...33...
..3223..
.322223.
32222223
33....33
3......3
........
........
]]
```

A `gfx` block draws one or more 8x8 tiles: `.` (or `0`) is white, `1` light grey, `2` dark grey,
`3` (or `#`) black. In sprites, white is transparent. A 16x8 drawing is two tiles, `name` and
`name + 1`, left to right then top to bottom. The name's value is the tile number, to pass to
`spr()` and `tile()`. There is room for 128 tiles of graphics.

## Statements

```lua
local x = 10                  -- a local variable, visible to the end of its block
x = x + 1                     -- also x += 1, x -= 1, x *= 2
grid[i] = 5                   -- also grid[i] += 1

if x > 10 then
  ...
elseif x == 10 then
  ...
else
  ...
end

while x > 0 do
  x -= 1
  if done then break end
end

for i = 0, 9 do ... end       -- 0 to 9 inclusive
for i = 10, 0, -2 do ... end  -- a constant step, up to 255 either way

return x                      -- from a function
```

A `for` loop never overflows: `for i = 0, 255 do` with a `u8` runs 256 times and stops. The loop
variable cannot be assigned inside the loop.

Conditions must be `bool`: write `if x ~= 0 then`, not `if x then`.

## Operators

From the loosest to the tightest binding:

| | |
|---|---|
| `or` | logical, short-circuit |
| `and` | logical, short-circuit |
| `==` `~=` (or `!=`) `<` `<=` `>` `>=` | comparison: signed or unsigned by type |
| `\|` | bitwise or |
| `^` (or `~`) | bitwise exclusive or |
| `&` | bitwise and |
| `<<` `>>` | shifts: `>>` keeps the sign of signed values |
| `+` `-` | |
| `*` `/` `%` | division truncates toward zero; `x / 0` is all ones and `x % 0` is `x` |
| `-` `~` `not` | negation, bitwise not, logical not |

## Functions

```lua
function distance(a, b)
  if a > b then return a - b end
  return b - a
end
```

Parameters and return types are inferred from the calls. Functions cannot be recursive, directly
or through others: each function's variables live at fixed addresses (the Game Boy has 8 KB of RAM,
and a stack frame per call would waste most of it). Functions that are never running at the same
time share their memory.

## Built-in functions

### Screen

The background is a grid of 32x32 tiles, of which 20x18 are visible. Sprites float above it: 40
of them, at most 10 on one line.

| | |
|---|---|
| `spr(n, x, y, tile)` | puts sprite `n` (0-39) at pixel `x, y` (0, 0 is the top left corner) |
| `hide(n)` | takes sprite `n` off the screen |
| `tile(col, row, t)` | sets a background tile |
| `gettile(col, row)` | reads a background tile back: for walls, collisions, free cells |
| `text(col, row, "TEXT")` | writes text; each character is the tile with its ASCII code |
| `num(col, row, n)` | writes a number (0-65535), left-aligned in 5 cells |
| `cls()` | clears the background (sprites stay) |
| `scroll(x, y)` | scrolls the background |

The screen can only be changed between frames, so `tile()` writes are queued and copied in the
next VBlank, about 48 of them per frame. Writing more is fine: the game waits a frame for the queue
to empty. In `_init` (and right after `cls()`) the screen is off and writes are immediate.

### Input

`btn(b)` is true while button `b` is held, `btnp(b)` only in the frame it is pressed. Buttons:
`LEFT RIGHT UP DOWN A B START SELECT`. In the playground: arrows, X (A), Z (B), Enter (Start),
Shift (Select).

### Sound

| | |
|---|---|
| `beep(note, length)` | a square wave: `note` 0-71 counts semitones from C3 (12 is C4, 24 is C5); `length` 1 (short) to 7 (long), 0 stops it |
| `noise(length)` | a burst of noise, for explosions and hits |

### Other

| | |
|---|---|
| `rnd(n)` | a random number from 0 to n-1; `rnd(0)` is 0 to 255 |
| `frames()` | frames since the start (`u16`) |
| `vsync()` | waits for the next frame |
| `min(a, b)`, `max(a, b)`, `abs(x)` | of any number type |
| `debug(x)` | sends `x` and a newline through the link port: the playground shows it in its Link port tab, `mattone-run --serial` prints it |
| `stop()` | halts the game |

## Memory

| | |
|---|---|
| ROM | 32 KB, about 2.3 KB of it the runtime |
| RAM | 6 KB for variables, arrays and function frames |
| HRAM | 91 bytes, used automatically for the most used variables |
