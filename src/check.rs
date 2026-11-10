//! Name resolution and type inference.
//!
//! Every variable, parameter, return value and number literal gets a type variable. Using two
//! values together (`a + b`, passing `x` to `spr`) unifies their variables, so types flow from how
//! values are used: a variable passed to `spr()` as a coordinate becomes `u8` without a single
//! annotation. When the types disagree, the error says where each one came from.
//!
//! Variables that nothing pins down get the smallest type that holds their literals: `u8` for
//! 0..255, `u16` up to 65535, `i8`/`i16` when a literal is negative.

use crate::ast::*;
use crate::builtins::{self, Arg, BUTTONS};
use crate::diag::{Error, Result, Span};
use crate::gfx::{self, Tile};
use std::collections::{HashMap, HashSet};

/// The first tile number given to `gfx` graphics; tiles below hold the font.
pub const FIRST_GFX_TILE: u32 = 0x80;

#[derive(Clone, Debug)]
pub enum Global {
    Var(Prim),
    Array {
        elem: Prim,
        len: u32,
        /// Values of a `const` array (stored in ROM); `None` for a `var` array (in RAM).
        values: Option<Vec<i32>>,
    },
    /// A named constant. `Some` when declared with a type, otherwise it adapts like a literal.
    Const(i32, Option<Prim>),
    Gfx(u32),
    Func(usize),
}

#[derive(Clone, Debug)]
pub struct FuncSig {
    pub name: String,
    pub params: Vec<Prim>,
    pub ret: Option<Prim>,
    /// Index of the function in `Program::items`.
    pub item: usize,
}

#[derive(Debug)]
pub struct Checked {
    /// Type of each expression, by `Expr::id`.
    pub expr_ty: Vec<Prim>,
    /// Type of each `local` and `for` variable, by the `Stmt::span.start` of its declaration.
    pub decl_ty: HashMap<u32, Prim>,
    pub funcs: Vec<FuncSig>,
    pub globals: HashMap<String, Global>,
    /// Scalar and array globals in declaration order (the order they are laid out in RAM).
    pub global_order: Vec<String>,
    pub tiles: Vec<Tile>,
}

type V = u32;

#[derive(Clone, Debug)]
enum Bound {
    Any,
    Int { lo: i32, hi: i32 },
    Prim(Prim),
}

#[derive(Clone, Debug)]
struct Class {
    parent: V,
    bound: Bound,
    why: Option<(Span, String)>,
}

/// A value of `ty` can be written as `v`: anything from the signed minimum to the unsigned
/// maximum of its width, so `-1` is a fine `u8` (255) and `0xFF` a fine `i8` (-1).
fn fits(lo: i32, hi: i32, ty: Prim) -> bool {
    match ty {
        Prim::Bool => false,
        p if p.wide() => lo >= -32768 && hi <= 65535,
        _ => lo >= -128 && hi <= 255,
    }
}

fn default_int(lo: i32, hi: i32) -> Prim {
    if lo >= 0 {
        if hi <= 255 { Prim::U8 } else { Prim::U16 }
    } else if lo >= -128 && hi <= 127 {
        Prim::I8
    } else {
        Prim::I16
    }
}

struct Sig {
    params: Vec<V>,
    ret: Option<V>,
}

struct Checker<'a> {
    prog: &'a Program,
    classes: Vec<Class>,
    expr_var: Vec<Option<V>>,
    decl_var: HashMap<u32, V>,
    globals: HashMap<String, Global>,
    global_var: HashMap<String, V>,
    sigs: Vec<Sig>,
    funcs: Vec<FuncSig>,
    calls: Vec<HashSet<usize>>,
    // per-function state
    scopes: Vec<HashMap<String, (V, bool)>>,
    current: Option<usize>,
    loops: u32,
    /// Comparisons with 0 to re-check once types are known: (span, operand var, op, zero on right).
    zero_compares: Vec<(Span, V, BinOp, bool)>,
}

pub fn check(prog: &Program) -> Result<Checked> {
    let c = Checker {
        prog,
        classes: Vec::new(),
        expr_var: vec![None; prog.exprs as usize],
        decl_var: HashMap::new(),
        globals: HashMap::new(),
        global_var: HashMap::new(),
        sigs: Vec::new(),
        funcs: Vec::new(),
        calls: Vec::new(),
        scopes: Vec::new(),
        current: None,
        loops: 0,
        zero_compares: Vec::new(),
    };
    c.run()
}

impl Checker<'_> {
    fn new_var(&mut self, bound: Bound, why: Option<(Span, String)>) -> V {
        let id = self.classes.len() as V;
        self.classes.push(Class {
            parent: id,
            bound,
            why,
        });
        id
    }

    fn prim_var(&mut self, p: Prim, span: Span, why: impl Into<String>) -> V {
        self.new_var(Bound::Prim(p), Some((span, why.into())))
    }

    fn int_var(&mut self, value: i32) -> V {
        self.new_var(
            Bound::Int {
                lo: value,
                hi: value,
            },
            None,
        )
    }

    fn find(&mut self, v: V) -> V {
        let p = self.classes[v as usize].parent;
        if p == v {
            return v;
        }
        let root = self.find(p);
        self.classes[v as usize].parent = root;
        root
    }

    fn bound(&mut self, v: V) -> Bound {
        let r = self.find(v);
        self.classes[r as usize].bound.clone()
    }

    fn describe(&mut self, v: V) -> String {
        match self.bound(v) {
            Bound::Any => "of unknown type".into(),
            Bound::Int { .. } => "a number".into(),
            Bound::Prim(p) => p.name().into(),
        }
    }

    fn why_note(&mut self, v: V, mut err: Error) -> Error {
        let r = self.find(v);
        if let Some((span, text)) = self.classes[r as usize].why.clone() {
            err = err.note(Some(span), text);
        }
        err
    }

    /// Makes `a` and `b` the same type. `what` describes the pair for the error message.
    fn unify(&mut self, a: V, b: V, span: Span, what: &str) -> Result<()> {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return Ok(());
        }
        let (ba, bb) = (self.bound(ra), self.bound(rb));
        let merged = match (&ba, &bb) {
            (Bound::Any, _) => Some((bb.clone(), rb)),
            (_, Bound::Any) => Some((ba.clone(), ra)),
            (Bound::Int { lo: l1, hi: h1 }, Bound::Int { lo: l2, hi: h2 }) => Some((
                Bound::Int {
                    lo: (*l1).min(*l2),
                    hi: (*h1).max(*h2),
                },
                ra,
            )),
            (Bound::Int { lo, hi }, Bound::Prim(p)) | (Bound::Prim(p), Bound::Int { lo, hi }) => {
                if *p == Prim::Bool {
                    None
                } else if !fits(*lo, *hi, *p) {
                    let value = if fits(*lo, *lo, *p) { *hi } else { *lo };
                    let holder = if matches!(ba, Bound::Prim(_)) { ra } else { rb };
                    let err = Error::new(
                        span,
                        format!("{value} does not fit in {} ({what})", p.name()),
                    );
                    return Err(self.why_note(holder, err));
                } else {
                    let holder = if matches!(ba, Bound::Prim(_)) { ra } else { rb };
                    Some((Bound::Prim(*p), holder))
                }
            }
            (Bound::Prim(p), Bound::Prim(q)) if p == q => Some((ba.clone(), ra)),
            _ => None,
        };
        let Some((bound, why_from)) = merged else {
            let (da, db) = (self.describe(ra), self.describe(rb));
            let mut err = Error::new(span, format!("{what}: one is {da}, the other is {db}"));
            err = self.why_note(ra, err);
            err = self.why_note(rb, err);
            if da != "bool" && db != "bool" {
                err = err.note(
                    None,
                    "convert one of them, like u16(x), or declare it with a type, like `local x: u16`",
                );
            }
            return Err(err);
        };
        let why = self.classes[why_from as usize]
            .why
            .clone()
            .or(self.classes[ra as usize].why.clone())
            .or(self.classes[rb as usize].why.clone());
        self.classes[rb as usize].parent = ra;
        self.classes[ra as usize].bound = bound;
        self.classes[ra as usize].why = why;
        Ok(())
    }

    fn require_int(&mut self, v: V, span: Span, what: &str) -> Result<()> {
        if let Bound::Prim(Prim::Bool) = self.bound(v) {
            let err = Error::new(span, format!("{what} must be a number, not true/false"));
            return Err(self.why_note(v, err));
        }
        let any_int = self.int_var(0);
        self.unify(v, any_int, span, what)
    }

    fn require_bool(&mut self, v: V, span: Span, what: &str) -> Result<()> {
        match self.bound(v) {
            Bound::Int { .. } | Bound::Prim(Prim::U8 | Prim::I8 | Prim::U16 | Prim::I16) => {
                let err = Error::new(
                    span,
                    format!("{what} must be true or false, but this is a number"),
                )
                .note(None, "compare it to something, like `x ~= 0`");
                Err(self.why_note(v, err))
            }
            _ => {
                let b = self.prim_var(Prim::Bool, span, "");
                self.unify(v, b, span, what)
            }
        }
    }

    fn resolve(&mut self, v: V) -> Prim {
        match self.bound(v) {
            Bound::Prim(p) => p,
            Bound::Int { lo, hi } => default_int(lo, hi),
            Bound::Any => Prim::U8,
        }
    }

    fn ann_var(&mut self, ann: &TypeAnn, name: &str) -> V {
        self.prim_var(
            ann.prim,
            ann.span,
            format!("`{name}` is declared {}", ann.prim.name()),
        )
    }

    fn reserved(name: &str) -> bool {
        builtins::find(name).is_some()
            || BUTTONS.iter().any(|(b, _)| *b == name)
            || Prim::from_name(name).is_some()
    }

    fn declare_global(&mut self, name: &str, span: Span, g: Global) -> Result<()> {
        if Self::reserved(name) {
            return Err(Error::new(
                span,
                format!("`{name}` is a built-in name: pick another one"),
            ));
        }
        if self.globals.contains_key(name) {
            return Err(Error::new(span, format!("`{name}` is defined twice")));
        }
        self.globals.insert(name.to_string(), g);
        Ok(())
    }

    fn run(mut self) -> Result<Checked> {
        let prog = self.prog;
        let mut tiles = Vec::new();
        let mut global_order = Vec::new();
        // Declarations first, so code can use names defined further down.
        for (index, item) in prog.items.iter().enumerate() {
            match item {
                Item::Gfx { name, art, span } => {
                    let first = FIRST_GFX_TILE + tiles.len() as u32;
                    tiles.extend(gfx::parse_art(art, *span)?);
                    if FIRST_GFX_TILE as usize + tiles.len() > 256 {
                        return Err(Error::new(
                            *span,
                            format!(
                                "too many graphics: {} tiles of 8x8, the most is {}",
                                tiles.len(),
                                256 - FIRST_GFX_TILE
                            ),
                        ));
                    }
                    self.declare_global(name, *span, Global::Gfx(first))?;
                }
                Item::Function(f) => {
                    let fi = self.funcs.len();
                    self.declare_global(&f.name, f.span, Global::Func(fi))?;
                    let mut seen = HashSet::new();
                    let mut params = Vec::new();
                    for p in &f.params {
                        if !seen.insert(&p.name) {
                            return Err(Error::new(
                                p.span,
                                format!("two parameters are called `{}`", p.name),
                            ));
                        }
                        params.push(match &p.ty {
                            Some(t) => self.ann_var(t, &p.name),
                            None => self.new_var(Bound::Any, None),
                        });
                    }
                    let returns_value = f.ret.is_some() || returns_value(&f.body);
                    let ret = if returns_value {
                        Some(match &f.ret {
                            Some(t) => self.prim_var(
                                t.prim,
                                t.span,
                                format!("{}() is declared to return {}", f.name, t.prim.name()),
                            ),
                            None => self.new_var(Bound::Any, None),
                        })
                    } else {
                        None
                    };
                    self.sigs.push(Sig { params, ret });
                    self.funcs.push(FuncSig {
                        name: f.name.clone(),
                        params: Vec::new(),
                        ret: None,
                        item: index,
                    });
                    self.calls.push(HashSet::new());
                }
                Item::Global {
                    name,
                    ty,
                    init,
                    list,
                    constant,
                    span,
                } => {
                    if let Some(TypeAnn {
                        array: Some(len),
                        prim,
                        span: tspan,
                    }) = ty
                    {
                        if *prim == Prim::Bool {
                            return Err(Error::new(
                                *tspan,
                                "arrays hold numbers: use u8 with 0 and 1",
                            ));
                        }
                        let values = if *constant {
                            let Some(list) = list else {
                                return Err(Error::new(
                                    *span,
                                    "a `const` array needs its values: `= {1, 2, 3}`",
                                ));
                            };
                            let mut values = Vec::new();
                            for e in list {
                                let v = self.const_eval(e)?;
                                if !fits(v, v, *prim) {
                                    return Err(Error::new(
                                        e.span,
                                        format!("{v} does not fit in {}", prim.name()),
                                    ));
                                }
                                values.push(v);
                            }
                            if let Some(n) = len
                                && *n as usize != values.len()
                            {
                                return Err(Error::new(
                                    *tspan,
                                    format!(
                                        "the array has {n} elements but {} values",
                                        values.len()
                                    ),
                                ));
                            }
                            Some(values)
                        } else {
                            if init.is_some() {
                                return Err(Error::new(
                                    *span,
                                    "a `var` array starts filled with zeros and has no `=`",
                                ));
                            }
                            None
                        };
                        let len = len.unwrap_or(values.as_ref().map_or(0, |v| v.len() as u32));
                        if len == 0 || len > 4096 {
                            return Err(Error::new(*tspan, "an array has from 1 to 4096 elements"));
                        }
                        self.declare_global(
                            name,
                            *span,
                            Global::Array {
                                elem: *prim,
                                len,
                                values,
                            },
                        )?;
                        global_order.push(name.clone());
                        continue;
                    }
                    if list.is_some() {
                        return Err(Error::new(
                            *span,
                            "a { } list needs an array type, like `const t: u8[] = {1, 2}`",
                        ));
                    }
                    if *constant {
                        let Some(init) = init else {
                            return Err(Error::new(*span, "a `const` needs a value"));
                        };
                        let v = self.const_eval(init)?;
                        let fixed = ty.as_ref().map(|t| t.prim);
                        if let Some(p) = fixed
                            && !(fits(v, v, p) || p == Prim::Bool && (v == 0 || v == 1))
                        {
                            return Err(Error::new(
                                init.span,
                                format!("{v} does not fit in {}", p.name()),
                            ));
                        }
                        if !(-32768..=65535).contains(&v) {
                            return Err(Error::new(
                                init.span,
                                format!("{v} does not fit in 16 bits"),
                            ));
                        }
                        self.declare_global(name, *span, Global::Const(v, fixed))?;
                    } else {
                        let var = match ty {
                            Some(t) => self.ann_var(t, name),
                            None => self.new_var(Bound::Any, None),
                        };
                        self.declare_global(name, *span, Global::Var(Prim::U8))?;
                        self.global_var.insert(name.clone(), var);
                        global_order.push(name.clone());
                    }
                }
            }
        }

        // Global initializers.
        self.calls.push(HashSet::new()); // the initializer pseudo-function
        for item in &prog.items {
            if let Item::Global {
                name,
                init: Some(init),
                constant: false,
                ..
            } = item
            {
                let v = self.expr(init)?;
                let gv = self.global_var[name];
                self.unify(gv, v, init.span, &format!("the value of `{name}`"))?;
            }
        }

        for (fi, sig_item) in self.funcs.clone().iter().enumerate() {
            let Item::Function(f) = &prog.items[sig_item.item] else {
                unreachable!()
            };
            self.current = Some(fi);
            self.scopes = vec![HashMap::new()];
            for (p, &v) in f.params.iter().zip(&self.sigs[fi].params) {
                if Self::reserved(&p.name) {
                    return Err(Error::new(
                        p.span,
                        format!("`{}` is a built-in name: pick another one", p.name),
                    ));
                }
                self.scopes[0].insert(p.name.clone(), (v, false));
            }
            self.block(&f.body)?;
            self.current = None;
        }

        for name in ["_init", "_update"] {
            if let Some(Global::Func(fi)) = self.globals.get(name) {
                let Item::Function(f) = &prog.items[self.funcs[*fi].item] else {
                    unreachable!()
                };
                if !f.params.is_empty() || self.sigs[*fi].ret.is_some() {
                    return Err(Error::new(
                        f.span,
                        format!(
                            "{name}() is called by the Game Boy: it takes no parameters and returns nothing"
                        ),
                    ));
                }
            }
        }
        if !self.globals.contains_key("_init") && !self.globals.contains_key("_update") {
            return Err(Error::new(
                Span {
                    line: 1,
                    col: 1,
                    ..Span::default()
                },
                "nothing to run: define `function _update()` (called every frame) or `function _init()`",
            ));
        }
        self.check_recursion()?;

        // Every type is known now.
        for (span, v, op, zero_right) in std::mem::take(&mut self.zero_compares) {
            let p = self.resolve(v);
            let never = matches!((op, zero_right), (BinOp::Lt, true) | (BinOp::Gt, false));
            let always = matches!((op, zero_right), (BinOp::Ge, true) | (BinOp::Le, false));
            if !p.signed() && (never || always) {
                let err = Error::new(
                    span,
                    format!(
                        "this is always {}: the value is {}, which is never below 0",
                        if never { "false" } else { "true" },
                        p.name()
                    ),
                )
                .note(
                    None,
                    "declare it with a signed type (i8 or i16) to allow negative values",
                );
                return Err(self.why_note(v, err));
            }
        }
        let mut expr_ty = Vec::with_capacity(self.expr_var.len());
        for v in self.expr_var.clone() {
            expr_ty.push(match v {
                Some(v) => self.resolve(v),
                None => Prim::U8,
            });
        }
        let mut decl_ty = HashMap::new();
        for (k, v) in self.decl_var.clone() {
            let p = self.resolve(v);
            decl_ty.insert(k, p);
        }
        for (name, v) in self.global_var.clone() {
            let p = self.resolve(v);
            self.globals.insert(name, Global::Var(p));
        }
        for fi in 0..self.funcs.len() {
            let params: Vec<V> = self.sigs[fi].params.clone();
            self.funcs[fi].params = params.into_iter().map(|v| self.resolve(v)).collect();
            self.funcs[fi].ret = self.sigs[fi].ret.map(|v| self.resolve(v));
        }
        Ok(Checked {
            expr_ty,
            decl_ty,
            funcs: self.funcs,
            globals: self.globals,
            global_order,
            tiles,
        })
    }

    fn check_recursion(&self) -> Result<()> {
        // 0 unvisited, 1 on the stack, 2 done
        fn visit(c: &Checker, f: usize, state: &mut [u8], path: &mut Vec<usize>) -> Result<()> {
            state[f] = 1;
            path.push(f);
            let mut callees: Vec<usize> = c.calls[f].iter().copied().collect();
            callees.sort();
            for g in callees {
                if state[g] == 1 {
                    let start = path.iter().position(|&x| x == g).unwrap();
                    let names: Vec<String> = path[start..]
                        .iter()
                        .chain(std::iter::once(&g))
                        .map(|&i| format!("{}()", c.funcs[i].name))
                        .collect();
                    let Item::Function(func) = &c.prog.items[c.funcs[g].item] else {
                        unreachable!()
                    };
                    return Err(Error::new(
                        func.span,
                        format!("recursion is not supported: {}", names.join(" calls ")),
                    )
                    .note(
                        None,
                        "each function's variables have a fixed place in the Game Boy's 8 KB of RAM, \
                         so a function cannot be running twice at once: use a loop",
                    ));
                }
                if state[g] == 0 {
                    visit(c, g, state, path)?;
                }
            }
            path.pop();
            state[f] = 2;
            Ok(())
        }
        let mut state = vec![0u8; self.funcs.len()];
        for f in 0..self.funcs.len() {
            if state[f] == 0 {
                visit(self, f, &mut state, &mut Vec::new())?;
            }
        }
        Ok(())
    }

    /// Evaluates a constant expression (for `const` and array lists).
    fn const_eval(&self, e: &Expr) -> Result<i32> {
        let not_const = || {
            Error::new(
                e.span,
                "a constant must be computable when compiling: numbers, other constants and operators",
            )
        };
        Ok(match &e.kind {
            ExprKind::Int(n) => *n as i32,
            ExprKind::Bool(b) => *b as i32,
            ExprKind::Name(n) => match (self.globals.get(n), BUTTONS.iter().find(|b| b.0 == n)) {
                (Some(Global::Const(v, _)), _) => *v,
                (Some(Global::Gfx(t)), _) => *t as i32,
                (_, Some((_, mask))) => *mask as i32,
                (None, None) => {
                    return Err(Error::new(
                        e.span,
                        format!(
                            "unknown constant `{n}` (constants must be defined before they are used)"
                        ),
                    ));
                }
                _ => return Err(not_const()),
            },
            ExprKind::Unary(op, x) => {
                let x = self.const_eval(x)?;
                match op {
                    UnOp::Neg => -x,
                    UnOp::BitNot => !x & 0xFFFF,
                    UnOp::Not => (x == 0) as i32,
                }
            }
            ExprKind::Binary(op, a, b) => {
                let (a, b) = (self.const_eval(a)?, self.const_eval(b)?);
                match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a.saturating_mul(b),
                    BinOp::Div | BinOp::Mod if b == 0 => {
                        return Err(Error::new(e.span, "division by zero"));
                    }
                    BinOp::Div => a / b,
                    BinOp::Mod => a % b,
                    BinOp::And => a & b,
                    BinOp::Or => a | b,
                    BinOp::Xor => a ^ b,
                    BinOp::Shl => a.checked_shl(b.clamp(0, 31) as u32).unwrap_or(0),
                    BinOp::Shr => a >> b.clamp(0, 31),
                    BinOp::Eq => (a == b) as i32,
                    BinOp::Ne => (a != b) as i32,
                    BinOp::Lt => (a < b) as i32,
                    BinOp::Le => (a <= b) as i32,
                    BinOp::Gt => (a > b) as i32,
                    BinOp::Ge => (a >= b) as i32,
                    BinOp::LogicAnd => (a != 0 && b != 0) as i32,
                    BinOp::LogicOr => (a != 0 || b != 0) as i32,
                }
            }
            _ => return Err(not_const()),
        })
    }

    fn lookup_local(&self, name: &str) -> Option<(V, bool)> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn unknown_name(&self, name: &str, span: Span) -> Error {
        let mut candidates: Vec<&str> = self
            .scopes
            .iter()
            .flat_map(|s| s.keys().map(String::as_str))
            .collect();
        candidates.extend(self.globals.keys().map(String::as_str));
        candidates.extend(builtins::BUILTINS.iter().map(|b| b.name));
        candidates.extend(BUTTONS.iter().map(|b| b.0));
        let best = candidates
            .into_iter()
            .map(|c| (edit_distance(&name.to_lowercase(), &c.to_lowercase()), c))
            .filter(|(d, c)| *d <= 2 && *d < c.len())
            .min();
        let err = Error::new(span, format!("unknown name `{name}`"));
        match best {
            Some((_, c)) => err.note(None, format!("did you mean `{c}`?")),
            None => err,
        }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Result<()> {
        self.scopes.push(HashMap::new());
        for s in stmts {
            self.stmt(s)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        match &s.kind {
            StmtKind::Local { name, ty, init } => {
                if Self::reserved(name) {
                    return Err(Error::new(
                        s.span,
                        format!("`{name}` is a built-in name: pick another one"),
                    ));
                }
                let var = match ty {
                    Some(t) => self.ann_var(t, name),
                    None => self.new_var(Bound::Any, None),
                };
                if let Some(init) = init {
                    let v = self.expr(init)?;
                    self.unify(var, v, init.span, &format!("the value of `{name}`"))?;
                }
                self.decl_var.insert(s.span.start, var);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(name.clone(), (var, false));
            }
            StmtKind::Assign { target, op, value } => {
                let (tv, tspan, tname) = match target {
                    Target::Name(name) => {
                        let v = if let Some((v, readonly)) = self.lookup_local(name) {
                            if readonly {
                                return Err(Error::new(
                                    s.span,
                                    format!(
                                        "`{name}` is the loop variable: it cannot be changed inside the loop"
                                    ),
                                ));
                            }
                            v
                        } else {
                            match self.globals.get(name) {
                                Some(Global::Var(_)) => self.global_var[name],
                                Some(Global::Const(..)) | Some(Global::Gfx(_)) => {
                                    return Err(Error::new(
                                        s.span,
                                        format!(
                                            "`{name}` is a constant: declare it with `var` to change it"
                                        ),
                                    ));
                                }
                                Some(Global::Array { .. }) => {
                                    return Err(Error::new(
                                        s.span,
                                        format!(
                                            "`{name}` is an array: assign one element, like {name}[i] = 1"
                                        ),
                                    ));
                                }
                                Some(Global::Func(_)) => {
                                    return Err(Error::new(
                                        s.span,
                                        format!("`{name}` is a function"),
                                    ));
                                }
                                None => return Err(self.unknown_name(name, s.span)),
                            }
                        };
                        (v, s.span, name.clone())
                    }
                    Target::Index(name, idx) => {
                        let v = self.index(name, idx, s.span, true)?;
                        (v, s.span, format!("{name}[...]"))
                    }
                };
                let v = self.expr(value)?;
                if let Some(op) = op {
                    self.require_int(tv, tspan, &format!("`{tname}` in `{}=`", op.symbol()))?;
                }
                self.unify(
                    tv,
                    v,
                    value.span,
                    &format!("`{tname}` and the value assigned to it"),
                )?;
            }
            StmtKind::If { arms, otherwise } => {
                for (cond, body) in arms {
                    let v = self.expr(cond)?;
                    self.require_bool(v, cond.span, "the condition of `if`")?;
                    self.block(body)?;
                }
                self.block(otherwise)?;
            }
            StmtKind::While { cond, body } => {
                let v = self.expr(cond)?;
                self.require_bool(v, cond.span, "the condition of `while`")?;
                self.loops += 1;
                self.block(body)?;
                self.loops -= 1;
            }
            StmtKind::For {
                var,
                from,
                to,
                body,
                ..
            } => {
                if Self::reserved(var) {
                    return Err(Error::new(
                        s.span,
                        format!("`{var}` is a built-in name: pick another one"),
                    ));
                }
                let a = self.expr(from)?;
                let b = self.expr(to)?;
                self.require_int(a, from.span, "the start of a for loop")?;
                self.unify(a, b, to.span, "the start and the end of the loop")?;
                self.decl_var.insert(s.span.start, a);
                self.scopes.push(HashMap::from([(var.clone(), (a, true))]));
                self.loops += 1;
                self.block(body)?;
                self.loops -= 1;
                self.scopes.pop();
            }
            StmtKind::Break => {
                if self.loops == 0 {
                    return Err(Error::new(s.span, "`break` outside a loop"));
                }
            }
            StmtKind::Return(value) => {
                let fi = self.current.unwrap();
                let name = self.funcs[fi].name.clone();
                match (value, self.sigs[fi].ret) {
                    (Some(e), Some(rv)) => {
                        let v = self.expr(e)?;
                        self.unify(rv, v, e.span, &format!("the values returned by {name}()"))?;
                    }
                    (None, Some(_)) => {
                        return Err(Error::new(
                            s.span,
                            format!("{name}() returns a value: `return` needs one too"),
                        ));
                    }
                    (Some(e), None) => {
                        return Err(Error::new(e.span, format!("{name}() returns nothing")));
                    }
                    (None, None) => {}
                }
            }
            StmtKind::Call(e) => {
                self.call(e, false)?;
            }
        }
        Ok(())
    }

    fn index(&mut self, name: &str, idx: &Expr, span: Span, write: bool) -> Result<V> {
        let (elem, constant) = match self.globals.get(name) {
            Some(Global::Array { elem, values, .. }) => (*elem, values.is_some()),
            Some(_) => return Err(Error::new(span, format!("`{name}` is not an array"))),
            None if self.lookup_local(name).is_some() => {
                return Err(Error::new(span, format!("`{name}` is not an array")));
            }
            None => return Err(self.unknown_name(name, span)),
        };
        if write && constant {
            return Err(Error::new(
                span,
                format!(
                    "`{name}` is a `const` array, stored in the cartridge's ROM: it cannot change"
                ),
            ));
        }
        let iv = self.expr(idx)?;
        self.require_int(iv, idx.span, "an array index")?;
        Ok(self.prim_var(
            elem,
            span,
            format!("the elements of `{name}` are {}", elem.name()),
        ))
    }

    /// Checks a call; `value` says whether its result is used.
    fn call(&mut self, e: &Expr, value: bool) -> Result<Option<V>> {
        let ExprKind::Call(name, args) = &e.kind else {
            unreachable!()
        };
        if let Some(Global::Func(fi)) = self.globals.get(name).cloned() {
            let nparams = self.sigs[fi].params.len();
            if args.len() != nparams {
                return Err(Error::new(
                    e.span,
                    format!(
                        "{name}() takes {nparams} argument{}, not {}",
                        plural(nparams),
                        args.len()
                    ),
                ));
            }
            for (k, arg) in args.iter().enumerate() {
                let v = self.expr(arg)?;
                let p = self.sigs[fi].params[k];
                self.unify(p, v, arg.span, &format!("argument {} of {name}()", k + 1))?;
            }
            let caller = self.current.unwrap_or(self.funcs.len());
            self.calls[caller].insert(fi);
            let ret = self.sigs[fi].ret;
            if value && ret.is_none() {
                return Err(Error::new(e.span, format!("{name}() returns nothing")));
            }
            return Ok(ret);
        }
        let Some(b) = builtins::find(name) else {
            return Err(match self.globals.get(name) {
                Some(_) => Error::new(e.span, format!("`{name}` is not a function")),
                None => self.unknown_name(name, e.span),
            });
        };
        if args.len() != b.args.len() {
            return Err(Error::new(
                e.span,
                format!(
                    "{name}() takes {} argument{}, not {}{}",
                    b.args.len(),
                    plural(b.args.len()),
                    args.len(),
                    if b.usage.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", b.usage)
                    }
                ),
            ));
        }
        if matches!(name.as_str(), "abs" | "min" | "max") {
            // Generic: the result has the type of the arguments.
            let first = self.expr(&args[0])?;
            self.require_int(first, args[0].span, &format!("the argument of {name}()"))?;
            for arg in &args[1..] {
                let v = self.expr(arg)?;
                self.unify(first, v, arg.span, &format!("the arguments of {name}()"))?;
            }
            return Ok(Some(first));
        }
        for (k, (arg, kind)) in args.iter().zip(b.args).enumerate() {
            match kind {
                Arg::Str => {
                    if !matches!(arg.kind, ExprKind::Str(_)) {
                        return Err(Error::new(
                            arg.span,
                            format!(
                                "{name}() takes a string in quotes here; to show a number use num()"
                            ),
                        ));
                    }
                }
                Arg::Prim(p, label) => {
                    let v = self.expr(arg)?;
                    let want = self.prim_var(
                        *p,
                        arg.span,
                        format!("{name}() takes {} as its {label}", p.name()),
                    );
                    self.unify(
                        want,
                        v,
                        arg.span,
                        &format!("argument {} of {name}()", k + 1),
                    )?;
                }
                Arg::AnyInt => {
                    let v = self.expr(arg)?;
                    self.require_int(v, arg.span, &format!("the argument of {name}()"))?;
                }
                Arg::Any => {
                    self.expr(arg)?;
                }
            }
        }
        match b.ret {
            Some(p) => Ok(Some(self.prim_var(
                p,
                e.span,
                format!("{name}() returns {}", p.name()),
            ))),
            None if value => Err(Error::new(e.span, format!("{name}() returns nothing"))),
            None => Ok(None),
        }
    }

    fn expr(&mut self, e: &Expr) -> Result<V> {
        let v = match &e.kind {
            ExprKind::Int(n) => self.int_var(*n as i32),
            ExprKind::Bool(_) => self.prim_var(Prim::Bool, e.span, ""),
            ExprKind::Str(_) => {
                return Err(Error::new(e.span, "a string can only be passed to text()"));
            }
            ExprKind::Name(name) => {
                if let Some((v, _)) = self.lookup_local(name) {
                    v
                } else if let Some((_, mask)) = BUTTONS.iter().find(|b| b.0 == name) {
                    self.int_var(*mask as i32)
                } else {
                    match self.globals.get(name).cloned() {
                        Some(Global::Var(_)) => self.global_var[name],
                        Some(Global::Const(value, None)) => self.int_var(value),
                        Some(Global::Const(_, Some(p))) => self.prim_var(
                            p,
                            e.span,
                            format!("the constant `{name}` is declared {}", p.name()),
                        ),
                        Some(Global::Gfx(t)) => self.int_var(t as i32),
                        Some(Global::Array { .. }) => {
                            return Err(Error::new(
                                e.span,
                                format!("`{name}` is an array: use one element, like {name}[i]"),
                            ));
                        }
                        Some(Global::Func(_)) => {
                            return Err(Error::new(
                                e.span,
                                format!("`{name}` is a function: call it, like {name}()"),
                            ));
                        }
                        None => return Err(self.unknown_name(name, e.span)),
                    }
                }
            }
            ExprKind::Index(name, idx) => self.index(name, idx, e.span, false)?,
            ExprKind::Call(..) => self.call(e, true)?.unwrap(),
            ExprKind::Unary(op, x) => match op {
                UnOp::Neg => {
                    if let ExprKind::Int(n) = x.kind {
                        self.expr(x)?;
                        self.int_var(-(n as i32))
                    } else {
                        let v = self.expr(x)?;
                        self.require_int(v, x.span, "the operand of `-`")?;
                        v
                    }
                }
                UnOp::BitNot => {
                    let v = self.expr(x)?;
                    self.require_int(v, x.span, "the operand of `~`")?;
                    v
                }
                UnOp::Not => {
                    let v = self.expr(x)?;
                    self.require_bool(v, x.span, "the operand of `not`")?;
                    v
                }
            },
            ExprKind::Binary(op, a, b) => {
                let (va, vb) = (self.expr(a)?, self.expr(b)?);
                let sym = op.symbol();
                match op {
                    BinOp::LogicAnd | BinOp::LogicOr => {
                        self.require_bool(va, a.span, &format!("the left side of `{sym}`"))?;
                        self.require_bool(vb, b.span, &format!("the right side of `{sym}`"))?;
                        va
                    }
                    BinOp::Shl | BinOp::Shr => {
                        self.require_int(va, a.span, &format!("the left side of `{sym}`"))?;
                        self.require_int(vb, b.span, "a shift count")?;
                        va
                    }
                    BinOp::Eq | BinOp::Ne => {
                        self.unify(
                            va,
                            vb,
                            e.span,
                            &format!("the two sides of `{sym}` must have the same type"),
                        )?;
                        self.prim_var(Prim::Bool, e.span, "")
                    }
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        self.require_int(va, a.span, &format!("the left side of `{sym}`"))?;
                        self.unify(
                            va,
                            vb,
                            e.span,
                            &format!("the two sides of `{sym}` must have the same type"),
                        )?;
                        if matches!(b.kind, ExprKind::Int(0)) {
                            self.zero_compares.push((e.span, va, *op, true));
                        } else if matches!(a.kind, ExprKind::Int(0)) {
                            self.zero_compares.push((e.span, vb, *op, false));
                        }
                        self.prim_var(Prim::Bool, e.span, "")
                    }
                    _ => {
                        self.require_int(va, a.span, &format!("the left side of `{sym}`"))?;
                        self.unify(
                            va,
                            vb,
                            e.span,
                            &format!("the two sides of `{sym}` must have the same type"),
                        )?;
                        va
                    }
                }
            }
        };
        self.expr_var[e.id as usize] = Some(v);
        Ok(v)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn returns_value(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Return(Some(_)) => true,
        StmtKind::If { arms, otherwise } => {
            arms.iter().any(|(_, b)| returns_value(b)) || returns_value(otherwise)
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } => returns_value(body),
        _ => false,
    })
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + (ca != *cb) as usize)
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn ok(src: &str) -> Checked {
        let p = parse(src).unwrap();
        check(&p).unwrap_or_else(|e| panic!("{}", e.render(src, "t")))
    }

    fn err(src: &str) -> String {
        let p = parse(src).unwrap();
        let e = check(&p).expect_err("should fail");
        e.render(src, "t")
    }

    #[test]
    fn types_flow_from_builtins() {
        let c = ok(
            "var px = 80\nvar score = 0\nfunction _update()\n spr(0, px, 10, 0)\n num(0, 0, score)\nend",
        );
        assert!(matches!(c.globals["px"], Global::Var(Prim::U8)));
        assert!(matches!(c.globals["score"], Global::Var(Prim::U16)));
    }

    #[test]
    fn defaults_follow_literals() {
        let c = ok("var a = 300\nvar b = -5\nvar c = -1000\nfunction _init() end");
        assert!(matches!(c.globals["a"], Global::Var(Prim::U16)));
        assert!(matches!(c.globals["b"], Global::Var(Prim::I8)));
        assert!(matches!(c.globals["c"], Global::Var(Prim::I16)));
    }

    #[test]
    fn parameters_are_inferred_from_calls() {
        let c =
            ok("function add(a, b) return a + b end\nfunction _update() num(1, 1, add(1, 2)) end");
        assert_eq!(c.funcs[0].params, vec![Prim::U16, Prim::U16]);
        assert_eq!(c.funcs[0].ret, Some(Prim::U16));
    }

    #[test]
    fn mismatch_explains_both_sides() {
        let e = err("var x: u8 = 1\nvar s = 0\nfunction _update()\n num(0, 0, s)\n x = s\nend");
        assert!(e.contains("one is u8, the other is u16"), "{e}");
        assert!(e.contains("`x` is declared u8"), "{e}");
        assert!(e.contains("num() takes u16 as its value"), "{e}");
    }

    #[test]
    fn literal_too_big() {
        let e = err("var x: u8 = 300\nfunction _init() end");
        assert!(e.contains("300 does not fit in u8"), "{e}");
    }

    #[test]
    fn unsigned_below_zero() {
        let e = err("var x: u8 = 1\nfunction _update() if x < 0 then x = 1 end end");
        assert!(e.contains("always false"), "{e}");
    }

    #[test]
    fn numbers_are_not_conditions() {
        let e = err("var x = 1\nfunction _update() if x then x = 2 end end");
        assert!(e.contains("x ~= 0"), "{e}");
    }

    #[test]
    fn recursion_is_reported() {
        let e = err(
            "function f(n: u8) g(n) end\nfunction g(n: u8) f(n) end\nfunction _init() f(1) end",
        );
        assert!(
            e.contains("recursion is not supported: f() calls g() calls f()"),
            "{e}"
        );
    }

    #[test]
    fn suggestions_for_typos() {
        let e = err("function _update() sprr(0, 0, 0, 0) end");
        assert!(e.contains("did you mean `spr`"), "{e}");
    }

    #[test]
    fn loop_variable_is_read_only() {
        let e = err("function _init() for i = 1, 3 do i = 2 end end");
        assert!(e.contains("loop variable"), "{e}");
    }
}
