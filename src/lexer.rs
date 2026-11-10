//! Turns source text into tokens. The syntax is Lua's, with a few additions:
//! type annotations (`: u8`), compound assignment (`+=`), `!=` next to `~=`, character
//! literals (`'#'`) and C-style bit operators.

use crate::diag::{Error, Result, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Ident(String),
    Int(u32),
    Str(String),
    /// `[[ ... ]]`: a long string, used for pixel art.
    Long(String),
    // keywords
    Function,
    End,
    If,
    Then,
    Elseif,
    Else,
    While,
    Do,
    For,
    Local,
    Var,
    Const,
    Gfx,
    Return,
    Break,
    And,
    Or,
    Not,
    True,
    False,
    // punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(name) => format!("`{name}`"),
            Tok::Int(n) => format!("number {n}"),
            Tok::Str(_) => "a string".into(),
            Tok::Long(_) => "a [[ ]] block".into(),
            Tok::Eof => "the end of the file".into(),
            other => format!("`{}`", other.text()),
        }
    }

    fn text(&self) -> &'static str {
        match self {
            Tok::Function => "function",
            Tok::End => "end",
            Tok::If => "if",
            Tok::Then => "then",
            Tok::Elseif => "elseif",
            Tok::Else => "else",
            Tok::While => "while",
            Tok::Do => "do",
            Tok::For => "for",
            Tok::Local => "local",
            Tok::Var => "var",
            Tok::Const => "const",
            Tok::Gfx => "gfx",
            Tok::Return => "return",
            Tok::Break => "break",
            Tok::And => "and",
            Tok::Or => "or",
            Tok::Not => "not",
            Tok::True => "true",
            Tok::False => "false",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::Comma => ",",
            Tok::Colon => ":",
            Tok::Assign => "=",
            Tok::PlusAssign => "+=",
            Tok::MinusAssign => "-=",
            Tok::StarAssign => "*=",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Amp => "&",
            Tok::Pipe => "|",
            Tok::Caret => "^",
            Tok::Tilde => "~",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::Eq => "==",
            Tok::Ne => "~=",
            Tok::Lt => "<",
            Tok::Le => "<=",
            Tok::Gt => ">",
            Tok::Ge => ">=",
            _ => "?",
        }
    }
}

fn keyword(word: &str) -> Option<Tok> {
    Some(match word {
        "function" => Tok::Function,
        "end" => Tok::End,
        "if" => Tok::If,
        "then" => Tok::Then,
        "elseif" => Tok::Elseif,
        "else" => Tok::Else,
        "while" => Tok::While,
        "do" => Tok::Do,
        "for" => Tok::For,
        "local" => Tok::Local,
        "var" => Tok::Var,
        "const" => Tok::Const,
        "gfx" => Tok::Gfx,
        "return" => Tok::Return,
        "break" => Tok::Break,
        "and" => Tok::And,
        "or" => Tok::Or,
        "not" => Tok::Not,
        "true" => Tok::True,
        "false" => Tok::False,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub fn lex(src: &str) -> Result<Vec<Token>> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut line, mut line_start) = (0usize, 1u32, 0usize);
    let span = |start: usize, end: usize, line: u32, line_start: usize| Span {
        line,
        col: (src[line_start..start].chars().count() + 1) as u32,
        start: start as u32,
        end: end as u32,
    };
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            i += 1;
            line += 1;
            line_start = i;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if src[i..].starts_with("--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        let (start_line, start_line_start) = (line, line_start);
        let tok = if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &src[start..i];
            keyword(word).unwrap_or_else(|| Tok::Ident(word.to_string()))
        } else if c.is_ascii_digit() {
            let (digits, radix) = if src[i..].starts_with("0x") || src[i..].starts_with("0X") {
                i += 2;
                (i, 16)
            } else if src[i..].starts_with("0b") || src[i..].starts_with("0B") {
                i += 2;
                (i, 2)
            } else {
                (i, 10)
            };
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let text: String = src[digits..i].chars().filter(|&c| c != '_').collect();
            match u32::from_str_radix(&text, radix) {
                Ok(n) if n <= 0xFFFF => Tok::Int(n),
                Ok(_) => {
                    return Err(Error::new(
                        span(start, i, line, line_start),
                        "number too big: the largest is 65535",
                    ));
                }
                Err(_) => {
                    return Err(Error::new(
                        span(start, i, line, line_start),
                        format!("`{}` is not a number", &src[start..i]),
                    ));
                }
            }
        } else if c == b'"' {
            i += 1;
            let mut text = String::new();
            loop {
                match bytes.get(i) {
                    None | Some(b'\n') => {
                        return Err(Error::new(
                            span(start, i, line, line_start),
                            "this string has no closing `\"`",
                        ));
                    }
                    Some(b'"') => {
                        i += 1;
                        break;
                    }
                    Some(_) => {
                        let ch = src[i..].chars().next().unwrap();
                        if !(' '..='~').contains(&ch) {
                            return Err(Error::new(
                                span(i, i + ch.len_utf8(), line, line_start),
                                format!("`{ch}` is not in the Game Boy font: use ASCII characters"),
                            ));
                        }
                        text.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            Tok::Str(text)
        } else if c == b'\'' {
            let ch = src[i + 1..].chars().next();
            match ch {
                Some(ch) if (' '..='~').contains(&ch) && bytes.get(i + 2) == Some(&b'\'') => {
                    i += 3;
                    Tok::Int(ch as u32)
                }
                _ => {
                    return Err(Error::new(
                        span(start, start + 1, line, line_start),
                        "a character literal is one ASCII character between quotes, like '#'",
                    ));
                }
            }
        } else if src[i..].starts_with("[[") {
            i += 2;
            let Some(len) = src[i..].find("]]") else {
                return Err(Error::new(
                    span(start, start + 2, line, line_start),
                    "this [[ block has no closing ]]",
                ));
            };
            let text = src[i..i + len].to_string();
            for (k, b) in text.bytes().enumerate() {
                if b == b'\n' {
                    line += 1;
                    line_start = i + k + 1;
                }
            }
            i += len + 2;
            Tok::Long(text)
        } else {
            let two = src.get(i..i + 2).unwrap_or("");
            let (tok, len) = match two {
                "+=" => (Tok::PlusAssign, 2),
                "-=" => (Tok::MinusAssign, 2),
                "*=" => (Tok::StarAssign, 2),
                "<<" => (Tok::Shl, 2),
                ">>" => (Tok::Shr, 2),
                "==" => (Tok::Eq, 2),
                "~=" | "!=" => (Tok::Ne, 2),
                "<=" => (Tok::Le, 2),
                ">=" => (Tok::Ge, 2),
                _ => (
                    match c {
                        b'(' => Tok::LParen,
                        b')' => Tok::RParen,
                        b'[' => Tok::LBracket,
                        b']' => Tok::RBracket,
                        b'{' => Tok::LBrace,
                        b'}' => Tok::RBrace,
                        b',' => Tok::Comma,
                        b':' => Tok::Colon,
                        b'=' => Tok::Assign,
                        b'+' => Tok::Plus,
                        b'-' => Tok::Minus,
                        b'*' => Tok::Star,
                        b'/' => Tok::Slash,
                        b'%' => Tok::Percent,
                        b'&' => Tok::Amp,
                        b'|' => Tok::Pipe,
                        b'^' => Tok::Caret,
                        b'~' => Tok::Tilde,
                        b'<' => Tok::Lt,
                        b'>' => Tok::Gt,
                        _ => {
                            let ch = src[i..].chars().next().unwrap();
                            return Err(Error::new(
                                span(i, i + ch.len_utf8(), line, line_start),
                                format!("unexpected character `{ch}`"),
                            ));
                        }
                    },
                    1,
                ),
            };
            i += len;
            tok
        };
        out.push(Token {
            tok,
            span: span(start, i, start_line, start_line_start),
        });
    }
    out.push(Token {
        tok: Tok::Eof,
        span: span(i, i, line, line_start),
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn numbers_strings_and_operators() {
        assert_eq!(
            toks("x += 0x1F -- comment\nif a ~= 'A' then"),
            vec![
                Tok::Ident("x".into()),
                Tok::PlusAssign,
                Tok::Int(31),
                Tok::If,
                Tok::Ident("a".into()),
                Tok::Ne,
                Tok::Int(65),
                Tok::Then,
                Tok::Eof
            ]
        );
        assert_eq!(toks("0b1010_0001"), vec![Tok::Int(0xA1), Tok::Eof]);
    }

    #[test]
    fn long_strings_keep_line_numbers() {
        let tokens = lex("gfx a = [[\n..\n..\n]]\nx").unwrap();
        assert_eq!(tokens[3].tok, Tok::Long("\n..\n..\n".into()));
        assert_eq!(tokens[4].span.line, 5);
    }

    #[test]
    fn errors() {
        assert!(lex("70000").unwrap_err().message.contains("too big"));
        assert!(lex("\"abc").unwrap_err().message.contains("closing"));
        assert!(lex("x = $").unwrap_err().message.contains("unexpected"));
    }
}
