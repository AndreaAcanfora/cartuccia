//! The syntax tree the parser builds and the type checker annotates.

use crate::diag::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prim {
    U8,
    I8,
    U16,
    I16,
    Bool,
}

impl Prim {
    pub fn name(self) -> &'static str {
        match self {
            Prim::U8 => "u8",
            Prim::I8 => "i8",
            Prim::U16 => "u16",
            Prim::I16 => "i16",
            Prim::Bool => "bool",
        }
    }

    pub fn from_name(name: &str) -> Option<Prim> {
        Some(match name {
            "u8" => Prim::U8,
            "i8" => Prim::I8,
            "u16" => Prim::U16,
            "i16" => Prim::I16,
            "bool" => Prim::Bool,
            _ => return None,
        })
    }

    pub fn is_int(self) -> bool {
        self != Prim::Bool
    }

    pub fn signed(self) -> bool {
        matches!(self, Prim::I8 | Prim::I16)
    }

    pub fn wide(self) -> bool {
        matches!(self, Prim::U16 | Prim::I16)
    }

    /// The range of values the type holds.
    pub fn range(self) -> (i32, i32) {
        match self {
            Prim::U8 => (0, 255),
            Prim::I8 => (-128, 127),
            Prim::U16 => (0, 65535),
            Prim::I16 => (-32768, 32767),
            Prim::Bool => (0, 1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypeAnn {
    pub prim: Prim,
    /// `u8[10]`: an array of 10. `u8[]` in a `const` takes its length from the initializer.
    pub array: Option<Option<u32>>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LogicAnd,
    LogicOr,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::And => "&",
            BinOp::Or => "|",
            BinOp::Xor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "~=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::LogicAnd => "and",
            BinOp::LogicOr => "or",
        }
    }

    pub fn is_compare(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    BitNot,
    Not,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(u32),
    Bool(bool),
    Str(String),
    Name(String),
    Index(String, Box<Expr>),
    Call(String, Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    /// Index into the type checker's table of expression types.
    pub id: u32,
}

#[derive(Clone, Debug)]
pub enum Target {
    Name(String),
    Index(String, Expr),
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Local {
        name: String,
        ty: Option<TypeAnn>,
        init: Option<Expr>,
    },
    Assign {
        target: Target,
        op: Option<BinOp>,
        value: Expr,
    },
    If {
        arms: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Vec<Stmt>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        from: Expr,
        to: Expr,
        /// A constant step: 1 when omitted, may be negative.
        step: i32,
        body: Vec<Stmt>,
    },
    Break,
    Return(Option<Expr>),
    Call(Expr),
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeAnn>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<TypeAnn>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Item {
    /// `var x = 1`, `var grid: u8[64]`, `const SPEED = 2`, `const level: u8[] = {1, 2, 3}`.
    Global {
        name: String,
        ty: Option<TypeAnn>,
        init: Option<Expr>,
        /// `{ ... }` initializer of a constant array.
        list: Option<Vec<Expr>>,
        constant: bool,
        span: Span,
    },
    Function(Function),
    Gfx {
        name: String,
        art: String,
        span: Span,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub items: Vec<Item>,
    /// Number of expression ids handed out.
    pub exprs: u32,
}
