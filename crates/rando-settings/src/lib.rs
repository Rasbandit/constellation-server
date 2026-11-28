//! The settings language (design doc 20): player and preset files (`.randoset`) parsed into a
//! tree with spans ([`ast`]), checked against a world's settings schema ([`schema`],
//! [`check`]), layered with presets into canonical JSON and the settings hash ([`canon`]),
//! formatted ([`fmt`]) and edited by byte span ([`edit`]).
//!
//! `no_std` with `alloc`, no IO and no unsafe code (doc 20 section 8.1, doc 05 section 6), so
//! the client SDK can embed it and it builds for WebAssembly. Files, worlds and presets come in
//! as text from the caller.

#![no_std]

extern crate alloc;

use alloc::string::String;

use rando_syntax::{Diagnostic, Pos, Severity};

pub mod ast;
pub mod canon;
pub mod check;
// pub mod edit;
pub mod fmt;
pub mod schema;
#[cfg(test)]
mod testdata;

/// An error with its doc 20 section 7.1 key.
pub(crate) fn diag(
    file: &str,
    key: &str,
    pos: Option<Pos>,
    msg: String,
    help: Option<String>,
) -> Diagnostic {
    Diagnostic {
        file: file.into(),
        pos,
        msg,
        help,
        key: Some(key.into()),
        severity: Severity::Error,
    }
}
