//! Compile errors: a message, where it happened, and optional notes that say why.

use std::fmt;

/// A position in the source: 1-based line and column, plus the byte range for underlining.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn to(self, other: Span) -> Span {
        Span {
            end: other.end.max(self.end),
            ..self
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub span: Option<Span>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub span: Span,
    pub message: String,
    pub notes: Vec<Note>,
}

impl Error {
    pub fn new(span: Span, message: impl Into<String>) -> Error {
        Error {
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn note(mut self, span: Option<Span>, text: impl Into<String>) -> Error {
        self.notes.push(Note {
            span,
            text: text.into(),
        });
        self
    }

    /// The error with the offending line and a caret under the problem, like rustc does.
    pub fn render(&self, source: &str, file: &str) -> String {
        let mut out = format!(
            "{}:{}:{}: error: {}\n",
            file, self.span.line, self.span.col, self.message
        );
        out += &excerpt(source, self.span);
        for note in &self.notes {
            match note.span {
                Some(span) => {
                    out += &format!("{}:{}:{}: note: {}\n", file, span.line, span.col, note.text);
                    out += &excerpt(source, span);
                }
                None => out += &format!("note: {}\n", note.text),
            }
        }
        out
    }
}

fn excerpt(source: &str, span: Span) -> String {
    let Some(text) = source.lines().nth(span.line.saturating_sub(1) as usize) else {
        return String::new();
    };
    let start = span.col.saturating_sub(1) as usize;
    let width = (span.end.saturating_sub(span.start) as usize)
        .clamp(1, text.chars().count().saturating_sub(start).max(1));
    format!(
        "{:>4} | {}\n     | {}{}\n",
        span.line,
        text,
        " ".repeat(start),
        "^".repeat(width)
    )
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.span.line, self.span.col, self.message)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
