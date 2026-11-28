//! The syntax shared by the world language (`.rando`, design doc 18) and the settings language
//! (`.randoset`, design doc 20 section 8.1): the lexer ([`lex`]), places in the source
//! ([`Pos`]), the diagnostics type ([`Diagnostic`]), "did you mean" ([`suggest`]) and the
//! canonical JSON writer ([`canon`]). `no_std` with `alloc`, so the WebAssembly build carries
//! the lexer and not the world compiler.

#![no_std]

extern crate alloc;

use alloc::string::String;
use core::fmt;

pub mod canon;
pub mod lex;
pub mod suggest;

/// `bytes` in lowercase hex, as SHA-256 sums are written (generated-file headers, package
/// sums, preset pins and the settings hash).
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A place in the source: the file (an index into the caller's list of files), line and
/// column, both from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    /// The file, by index.
    pub file: u32,
    /// Line, from 1.
    pub line: usize,
    /// Column in characters, from 1.
    pub col: usize,
}

impl Pos {
    /// A place.
    #[must_use]
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { file: 0, line, col }
    }

    /// A place in file `file`.
    #[must_use]
    pub fn in_file(file: u32, line: usize, col: usize) -> Pos {
        Pos { file, line, col }
    }

    /// `n` characters further along the same line.
    #[must_use]
    pub fn right(self, n: usize) -> Pos {
        Pos { file: self.file, line: self.line, col: self.col + n }
    }
}

/// A comparison, in the engine's operand order (`rule_ops::CMP_OPS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl Cmp {
    /// The engine's operand index.
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// How bad a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// It stops the build or the check.
    #[default]
    Error,
    /// Worth a look; it stops nothing.
    Warning,
}

/// One error or warning: where, what, and what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The file it is in, as the caller named it.
    pub file: String,
    /// Where in the file, when it is about one place.
    pub pos: Option<Pos>,
    /// What is wrong.
    pub msg: String,
    /// What to do about it, when there is something to say.
    pub help: Option<String>,
    /// Its stable message key (`settings.unknown_option`, design doc 20 section 7.1), when it
    /// has one.
    pub key: Option<String>,
    /// An error or a warning.
    pub severity: Severity,
}

impl Diagnostic {
    /// An error with no key.
    #[must_use]
    pub fn new(file: String, pos: Option<Pos>, msg: String, help: Option<String>) -> Self {
        Diagnostic { file, pos, msg, help, key: None, severity: Severity::Error }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pos {
            Some(p) => write!(f, "{}:{}:{}: {}", self.file, p.line, p.col, self.msg)?,
            None => write!(f, "{}: {}", self.file, self.msg)?,
        }
        match &self.help {
            Some(h) => write!(f, "\n  help: {h}"),
            None => Ok(()),
        }
    }
}
