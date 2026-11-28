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
#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;

use rando_syntax::{Diagnostic, Pos, Severity};

pub mod ast;
pub mod canon;
pub mod check;
pub mod edit;
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

/// `errs` cut to [`rando_limits::MAX_ERRORS`], with one more line saying how many were dropped,
/// so a broken file cannot return errors without end.
pub(crate) fn capped(mut errs: Vec<Diagnostic>, file: &str) -> Vec<Diagnostic> {
    let max = rando_limits::MAX_ERRORS;
    if errs.len() > max {
        let msg = rando_syntax::more_not_shown(errs.len() - max, "errors");
        errs.truncate(max);
        errs.push(diag(
            file,
            "settings.limit",
            None,
            msg,
            Some("fix these and check again".into()),
        ));
    }
    errs
}
