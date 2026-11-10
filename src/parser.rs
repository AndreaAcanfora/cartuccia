//! Recursive-descent parser. Operator precedence, lowest first:
//! `or`, `and`, comparisons, `|`, `^`, `&`, shifts, `+ -`, `* / %`, unary `- ~ not`.

use crate::ast::*;
use crate::diag::{Error, Result, Span};
use crate::lexer::{Tok, Token, lex};

pub fn parse(src: &str) -> Result<Program> {
    let mut p = Parser {
        toks: lex(src)?,
        pos: 0,
        next_id: 0,
    };
    let mut items = Vec::new();
    while p.peek() != &Tok::Eof {
        items.push(p.item()?);
    }
    Ok(Program {
        items,
        exprs: p.next_id,
    })
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    next_id: u32,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if t.tok != Tok::Eof {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == tok {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: Tok, context: &str) -> Result<Span> {
        if self.peek() == &tok {
            Ok(self.bump().span)
        } else {
            Err(Error::new(
                self.span(),
                format!(
                    "expected {} {}, found {}",
                    tok.describe(),
                    context,
                    self.peek().describe()
                ),
            ))
        }
    }

    fn ident(&mut self, what: &str) -> Result<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                let span = self.bump().span;
                Ok((name, span))
            }
            other => Err(Error::new(
                self.span(),
                format!("expected {what}, found {}", other.describe()),
            )),
        }
    }

    fn expr_node(&mut self, kind: ExprKind, span: Span) -> Expr {
        self.next_id += 1;
        Expr {
            kind,
            span,
            id: self.next_id - 1,
        }
    }

    fn item(&mut self) -> Result<Item> {
        let start = self.span();
        match self.peek() {
            Tok::Function => {
                self.bump();
                Ok(Item::Function(self.function(start)?))
            }
            Tok::Var | Tok::Const => {
                let constant = self.bump().tok == Tok::Const;
                let (name, name_span) = self.ident("a name")?;
                let ty = if self.eat(&Tok::Colon) {
                    Some(self.type_ann(constant)?)
                } else {
                    None
                };
                let (mut init, mut list) = (None, None);
                if self.eat(&Tok::Assign) {
                    if self.peek() == &Tok::LBrace {
                        let open = self.bump().span;
                        let mut values = Vec::new();
                        while self.peek() != &Tok::RBrace {
                            values.push(self.expr()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        self.expect(Tok::RBrace, "to close the list")?;
                        if !constant {
                            return Err(Error::new(
                                open,
                                "only a `const` array can have a { } list: \
                                 a `var` array starts filled with zeros",
                            ));
                        }
                        list = Some(values);
                    } else {
                        init = Some(self.expr()?);
                    }
                }
                Ok(Item::Global {
                    name,
                    ty,
                    init,
                    list,
                    constant,
                    span: name_span,
                })
            }
            Tok::Gfx => {
                self.bump();
                let (name, span) = self.ident("a name for the graphics")?;
                self.expect(Tok::Assign, "after the name")?;
                match self.bump().tok {
                    Tok::Long(art) => Ok(Item::Gfx { name, art, span }),
                    _ => Err(Error::new(
                        self.prev_span(),
                        "expected the pixels between [[ and ]]",
                    )),
                }
            }
            Tok::Local => Err(Error::new(
                start,
                "`local` is for variables inside functions: use `var` at the top level",
            )),
            other => Err(Error::new(
                start,
                format!(
                    "expected `function`, `var`, `const` or `gfx` at the top level, found {}",
                    other.describe()
                ),
            )),
        }
    }

    fn type_ann(&mut self, allow_open: bool) -> Result<TypeAnn> {
        let (name, span) = self.ident("a type (u8, i8, u16, i16 or bool)")?;
        let prim = Prim::from_name(&name).ok_or_else(|| {
            Error::new(
                span,
                format!("unknown type `{name}`: the types are u8, i8, u16, i16 and bool"),
            )
        })?;
        let array = if self.eat(&Tok::LBracket) {
            let len = match self.peek().clone() {
                Tok::Int(n) => {
                    self.bump();
                    Some(n)
                }
                Tok::RBracket if allow_open => None,
                other => {
                    return Err(Error::new(
                        self.span(),
                        format!("expected the array length, found {}", other.describe()),
                    ));
                }
            };
            self.expect(Tok::RBracket, "after the array length")?;
            Some(len)
        } else {
            None
        };
        Ok(TypeAnn {
            prim,
            array,
            span: span.to(self.prev_span()),
        })
    }

    fn function(&mut self, start: Span) -> Result<Function> {
        let (name, span) = self.ident("the function name")?;
        self.expect(Tok::LParen, "after the function name")?;
        let mut params = Vec::new();
        while self.peek() != &Tok::RParen {
            let (pname, pspan) = self.ident("a parameter name")?;
            let ty = if self.eat(&Tok::Colon) {
                let ty = self.type_ann(false)?;
                if ty.array.is_some() {
                    return Err(Error::new(ty.span, "a parameter cannot be an array"));
                }
                Some(ty)
            } else {
                None
            };
            params.push(Param {
                name: pname,
                ty,
                span: pspan,
            });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "to close the parameters")?;
        let ret = if self.eat(&Tok::Colon) {
            let ty = self.type_ann(false)?;
            if ty.array.is_some() {
                return Err(Error::new(ty.span, "a function cannot return an array"));
            }
            Some(ty)
        } else {
            None
        };
        let body = self.block(start, "function")?;
        self.expect(Tok::End, "to close the function")?;
        Ok(Function {
            name,
            params,
            ret,
            body,
            span,
        })
    }

    /// Statements up to (not including) `end`, `else` or `elseif`.
    fn block(&mut self, opened: Span, what: &str) -> Result<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Tok::End | Tok::Else | Tok::Elseif => return Ok(out),
                Tok::Eof => {
                    return Err(Error::new(
                        opened,
                        format!("this {what} is never closed: add `end`"),
                    ));
                }
                _ => out.push(self.stmt()?),
            }
        }
    }

    fn stmt(&mut self) -> Result<Stmt> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Local => {
                self.bump();
                let (name, _) = self.ident("a variable name")?;
                let ty = if self.eat(&Tok::Colon) {
                    let ty = self.type_ann(false)?;
                    if ty.array.is_some() {
                        return Err(Error::new(
                            ty.span,
                            "arrays live at the top level: declare it with `var`",
                        ));
                    }
                    Some(ty)
                } else {
                    None
                };
                let init = if self.eat(&Tok::Assign) {
                    Some(self.expr()?)
                } else {
                    None
                };
                StmtKind::Local { name, ty, init }
            }
            Tok::If => {
                self.bump();
                let mut arms = Vec::new();
                let mut otherwise = Vec::new();
                loop {
                    let cond = self.expr()?;
                    self.expect(Tok::Then, "after the condition")?;
                    let body = self.block(start, "if")?;
                    arms.push((cond, body));
                    match self.bump().tok {
                        Tok::Elseif => continue,
                        Tok::Else => {
                            otherwise = self.block(start, "if")?;
                            self.expect(Tok::End, "to close the if")?;
                            break;
                        }
                        _ => break,
                    }
                }
                StmtKind::If { arms, otherwise }
            }
            Tok::While => {
                self.bump();
                let cond = self.expr()?;
                self.expect(Tok::Do, "after the loop condition")?;
                let body = self.block(start, "while loop")?;
                self.expect(Tok::End, "to close the loop")?;
                StmtKind::While { cond, body }
            }
            Tok::For => {
                self.bump();
                let (var, _) = self.ident("the loop variable")?;
                self.expect(Tok::Assign, "after the loop variable")?;
                let from = self.expr()?;
                self.expect(Tok::Comma, "between the start and the end of the loop")?;
                let to = self.expr()?;
                let step = if self.eat(&Tok::Comma) {
                    let negative = self.eat(&Tok::Minus);
                    let span = self.span();
                    match self.bump().tok {
                        Tok::Int(n) if n > 0 && n < 256 => {
                            if negative {
                                -(n as i32)
                            } else {
                                n as i32
                            }
                        }
                        _ => {
                            return Err(Error::new(
                                span,
                                "the step of a for loop is a number from 1 to 255 (or -1 to -255)",
                            ));
                        }
                    }
                } else {
                    1
                };
                self.expect(Tok::Do, "after the loop range")?;
                let body = self.block(start, "for loop")?;
                self.expect(Tok::End, "to close the loop")?;
                StmtKind::For {
                    var,
                    from,
                    to,
                    step,
                    body,
                }
            }
            Tok::Break => {
                self.bump();
                StmtKind::Break
            }
            Tok::Return => {
                self.bump();
                let value = match self.peek() {
                    Tok::End | Tok::Else | Tok::Elseif | Tok::Eof => None,
                    _ => Some(self.expr()?),
                };
                StmtKind::Return(value)
            }
            Tok::Ident(_) => {
                let e = self.postfix()?;
                let op = match self.peek() {
                    Tok::Assign => Some(None),
                    Tok::PlusAssign => Some(Some(BinOp::Add)),
                    Tok::MinusAssign => Some(Some(BinOp::Sub)),
                    Tok::StarAssign => Some(Some(BinOp::Mul)),
                    _ => None,
                };
                match (op, e.kind) {
                    (Some(op), kind) => {
                        let op_span = self.bump().span;
                        let target = match kind {
                            ExprKind::Name(name) => Target::Name(name),
                            ExprKind::Index(name, idx) => Target::Index(name, *idx),
                            _ => {
                                return Err(Error::new(
                                    op_span,
                                    "only a variable or an array element can be assigned",
                                ));
                            }
                        };
                        let value = self.expr()?;
                        StmtKind::Assign { target, op, value }
                    }
                    (None, kind @ ExprKind::Call(..)) => StmtKind::Call(Expr { kind, ..e }),
                    (None, _) => {
                        return Err(Error::new(
                            e.span,
                            "this does nothing: a statement is a call, an assignment, \
                             or starts with a keyword",
                        ));
                    }
                }
            }
            other => {
                return Err(Error::new(
                    start,
                    format!("expected a statement, found {}", other.describe()),
                ));
            }
        };
        Ok(Stmt {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    pub fn expr(&mut self) -> Result<Expr> {
        self.binary(0)
    }

    fn binary(&mut self, level: usize) -> Result<Expr> {
        const LEVELS: &[&[(Tok, BinOp)]] = &[
            &[(Tok::Or, BinOp::LogicOr)],
            &[(Tok::And, BinOp::LogicAnd)],
            &[
                (Tok::Eq, BinOp::Eq),
                (Tok::Ne, BinOp::Ne),
                (Tok::Lt, BinOp::Lt),
                (Tok::Le, BinOp::Le),
                (Tok::Gt, BinOp::Gt),
                (Tok::Ge, BinOp::Ge),
            ],
            &[(Tok::Pipe, BinOp::Or)],
            &[(Tok::Caret, BinOp::Xor), (Tok::Tilde, BinOp::Xor)],
            &[(Tok::Amp, BinOp::And)],
            &[(Tok::Shl, BinOp::Shl), (Tok::Shr, BinOp::Shr)],
            &[(Tok::Plus, BinOp::Add), (Tok::Minus, BinOp::Sub)],
            &[
                (Tok::Star, BinOp::Mul),
                (Tok::Slash, BinOp::Div),
                (Tok::Percent, BinOp::Mod),
            ],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        loop {
            let Some(&(_, op)) = LEVELS[level].iter().find(|(t, _)| t == self.peek()) else {
                return Ok(left);
            };
            self.bump();
            let right = self.binary(level + 1)?;
            if op.is_compare()
                && let ExprKind::Binary(prev, ..) = &left.kind
                && prev.is_compare()
            {
                return Err(Error::new(
                    left.span.to(right.span),
                    "comparisons do not chain: write `a < b and b < c`",
                ));
            }
            let span = left.span.to(right.span);
            left = self.expr_node(ExprKind::Binary(op, Box::new(left), Box::new(right)), span);
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        let start = self.span();
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Tilde => UnOp::BitNot,
            Tok::Not => UnOp::Not,
            _ => return self.postfix(),
        };
        self.bump();
        let operand = self.unary()?;
        let span = start.to(operand.span);
        Ok(self.expr_node(ExprKind::Unary(op, Box::new(operand)), span))
    }

    fn postfix(&mut self) -> Result<Expr> {
        let start = self.span();
        match self.bump().tok {
            Tok::Int(n) => Ok(self.expr_node(ExprKind::Int(n), start)),
            Tok::True => Ok(self.expr_node(ExprKind::Bool(true), start)),
            Tok::False => Ok(self.expr_node(ExprKind::Bool(false), start)),
            Tok::Str(s) => Ok(self.expr_node(ExprKind::Str(s), start)),
            Tok::LParen => {
                let mut e = self.expr()?;
                let close = self.expect(Tok::RParen, "to close the parenthesis")?;
                e.span = start.to(close);
                Ok(e)
            }
            Tok::Ident(name) => match self.peek() {
                Tok::LParen => {
                    self.bump();
                    let mut args = Vec::new();
                    while self.peek() != &Tok::RParen {
                        args.push(self.expr()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    let close = self.expect(Tok::RParen, "to close the call")?;
                    Ok(self.expr_node(ExprKind::Call(name, args), start.to(close)))
                }
                Tok::LBracket => {
                    self.bump();
                    let idx = self.expr()?;
                    let close = self.expect(Tok::RBracket, "to close the index")?;
                    Ok(self.expr_node(ExprKind::Index(name, Box::new(idx)), start.to(close)))
                }
                _ => Ok(self.expr_node(ExprKind::Name(name), start)),
            },
            other => Err(Error::new(
                start,
                format!("expected a value, found {}", other.describe()),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_small_game() {
        let p = parse(
            "var x = 10\nconst t: u8[] = {1, 2}\ngfx dot = [[\n........\n]]\n\
             function _update()\n  if btn(LEFT) then x -= 1 elseif x > 3 and not b then x = 0 end\n\
             for i = 0, 7, 2 do spr(i, x * 2 + 1, 3, dot) end\nend",
        )
        .unwrap();
        assert_eq!(p.items.len(), 4);
    }

    #[test]
    fn precedence() {
        let p = parse("var x = 1 + 2 * 3 == 7").unwrap();
        let Item::Global { init: Some(e), .. } = &p.items[0] else {
            panic!()
        };
        let ExprKind::Binary(BinOp::Eq, l, _) = &e.kind else {
            panic!("{e:?}")
        };
        assert!(matches!(l.kind, ExprKind::Binary(BinOp::Add, ..)));
    }

    #[test]
    fn friendly_errors() {
        let e = parse("function f()\n x = 1\n").unwrap_err();
        assert!(e.message.contains("never closed"), "{e}");
        let e = parse("function f() if a < b < c then end end").unwrap_err();
        assert!(e.message.contains("chain"), "{e}");
        let e = parse("function f() x end").unwrap_err();
        assert!(e.message.contains("does nothing"), "{e}");
    }
}
