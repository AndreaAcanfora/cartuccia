//! The functions every game can call, and the button names.

use crate::ast::Prim;

#[derive(Clone, Copy, Debug)]
pub enum Arg {
    /// A number of this type; the label names it in error messages ("x", "tile").
    Prim(Prim, &'static str),
    /// A string literal.
    Str,
    /// A number of any type.
    AnyInt,
    /// A number or a bool.
    Any,
}

pub struct Builtin {
    pub name: &'static str,
    pub args: &'static [Arg],
    pub ret: Option<Prim>,
    /// Shown when the call has the wrong number of arguments.
    pub usage: &'static str,
}

use Arg::Prim as P;
use Prim::*;

pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "btn",
        args: &[P(U8, "button")],
        ret: Some(Bool),
        usage: "btn(LEFT) is true while LEFT is held",
    },
    Builtin {
        name: "btnp",
        args: &[P(U8, "button")],
        ret: Some(Bool),
        usage: "btnp(A) is true in the frame A goes down",
    },
    Builtin {
        name: "spr",
        args: &[
            P(U8, "sprite number"),
            P(U8, "x"),
            P(U8, "y"),
            P(U8, "tile"),
        ],
        ret: None,
        usage: "spr(n, x, y, tile) puts sprite n (0-39) at x, y",
    },
    Builtin {
        name: "hide",
        args: &[P(U8, "sprite number")],
        ret: None,
        usage: "hide(n) removes sprite n from the screen",
    },
    Builtin {
        name: "tile",
        args: &[P(U8, "column"), P(U8, "row"), P(U8, "tile")],
        ret: None,
        usage: "tile(col, row, t) draws tile t on the background grid",
    },
    Builtin {
        name: "gettile",
        args: &[P(U8, "column"), P(U8, "row")],
        ret: Some(U8),
        usage: "gettile(col, row) reads back a background tile",
    },
    Builtin {
        name: "text",
        args: &[P(U8, "column"), P(U8, "row"), Arg::Str],
        ret: None,
        usage: "text(col, row, \"HELLO\")",
    },
    Builtin {
        name: "num",
        args: &[P(U8, "column"), P(U8, "row"), P(U16, "value")],
        ret: None,
        usage: "num(col, row, value) writes a number",
    },
    Builtin {
        name: "cls",
        args: &[],
        ret: None,
        usage: "cls() clears the background",
    },
    Builtin {
        name: "rnd",
        args: &[P(U8, "limit")],
        ret: Some(U8),
        usage: "rnd(n) is a random number from 0 to n-1 (rnd(0): 0 to 255)",
    },
    Builtin {
        name: "beep",
        args: &[P(U8, "note"), P(U8, "length")],
        ret: None,
        usage: "beep(note, length): note 0-71 from C3, length 1-7 (0 stops)",
    },
    Builtin {
        name: "noise",
        args: &[P(U8, "length")],
        ret: None,
        usage: "noise(length): length 1-7 (0 stops)",
    },
    Builtin {
        name: "scroll",
        args: &[P(U8, "x"), P(U8, "y")],
        ret: None,
        usage: "scroll(x, y) moves the background",
    },
    Builtin {
        name: "frames",
        args: &[],
        ret: Some(U16),
        usage: "frames() counts the frames since the start",
    },
    Builtin {
        name: "vsync",
        args: &[],
        ret: None,
        usage: "vsync() waits for the next frame",
    },
    Builtin {
        name: "debug",
        args: &[Arg::Any],
        ret: None,
        usage: "debug(x) prints x to the link port",
    },
    Builtin {
        name: "stop",
        args: &[],
        ret: None,
        usage: "stop() halts the game",
    },
    Builtin {
        name: "u8",
        args: &[Arg::Any],
        ret: Some(U8),
        usage: "u8(x) converts x",
    },
    Builtin {
        name: "i8",
        args: &[Arg::Any],
        ret: Some(I8),
        usage: "i8(x) converts x",
    },
    Builtin {
        name: "u16",
        args: &[Arg::Any],
        ret: Some(U16),
        usage: "u16(x) converts x",
    },
    Builtin {
        name: "i16",
        args: &[Arg::Any],
        ret: Some(I16),
        usage: "i16(x) converts x",
    },
    Builtin {
        name: "abs",
        args: &[Arg::AnyInt],
        ret: None,
        usage: "abs(x)",
    },
    Builtin {
        name: "min",
        args: &[Arg::AnyInt, Arg::AnyInt],
        ret: None,
        usage: "min(a, b)",
    },
    Builtin {
        name: "max",
        args: &[Arg::AnyInt, Arg::AnyInt],
        ret: None,
        usage: "max(a, b)",
    },
];

pub fn find(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Bits of the joypad byte the runtime keeps, and the names games use for them.
pub const BUTTONS: &[(&str, u8)] = &[
    ("A", 0x01),
    ("B", 0x02),
    ("SELECT", 0x04),
    ("START", 0x08),
    ("RIGHT", 0x10),
    ("LEFT", 0x20),
    ("UP", 0x40),
    ("DOWN", 0x80),
];
