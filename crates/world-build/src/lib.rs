//! The world build tool (design doc 16 of the docs repo, server issue 27): a world as an
//! author wrote it in; the engine's manifest JSON out, in canonical form (sorted keys,
//! integers only, every list in one stated order), plus the per-seed instance the engine fills
//! from for a given set of option values.
//!
//! The parts do not depend on the file syntax: the [`model`] a front end produces, the rule
//! language ([`rule`]), the checks and the manifest ([`build_model`]), the canonical writer
//! ([`canon`]). The front end ([`front`]) is one function, not built until the syntax is
//! decided. What is checked and the choices made where the design doc is silent are in
//! `docs/world-build.md`.

use std::collections::HashMap;
use std::fmt;

use data_fill::Instance;
use serde_json::Value;

pub mod canon;
pub mod front;
mod instance;
mod lower;
pub mod model;
pub mod rule;
mod world;

pub use model::Pos;

/// One build error or warning: where, what, and what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The file it is in, as the caller named it.
    pub file: String,
    /// Where in the file, when it is about one place.
    pub pos: Option<Pos>,
    /// What is wrong.
    pub msg: String,
    /// What to do about it, when there is something to say.
    pub help: Option<String>,
}

impl fmt::Display for Error {
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

/// A built world.
#[derive(Debug, Clone)]
pub struct Built {
    /// The manifest the engine loads.
    pub manifest: Value,
    /// Things worth a look that do not stop the build.
    pub warnings: Vec<Error>,
    text: String,
    model: world::Model,
}

impl Built {
    /// The manifest as canonical JSON text: keys sorted by their bytes, no whitespace, one
    /// newline at the end. Building the same file twice gives the same bytes.
    #[must_use]
    pub fn json(&self) -> &str {
        &self.text
    }

    /// The per-seed instance of this world for the option values in `options` (an option left
    /// out takes its default): which checks exist, the item pool and classes, start items,
    /// fixed placements and placement sets, as the engine's fill reads them.
    ///
    /// # Errors
    /// An unknown option, a value outside its option's range, or more items than checks.
    pub fn instance(&self, options: &HashMap<String, i64>) -> Result<Instance, String> {
        instance::of(&self.model, options)
    }

    /// The instance at every option's default.
    ///
    /// # Errors
    /// As [`Built::instance`].
    pub fn default_instance(&self) -> Result<Instance, String> {
        self.instance(&HashMap::new())
    }
}

/// Build the world in `src`, the text of the file called `file` (used in messages only): the
/// front end, then [`build_model`].
///
/// # Errors
/// The front end's syntax errors, or every error [`build_model`] finds.
pub fn build(src: &str, file: &str) -> Result<Built, Vec<Error>> {
    build_model(&front::read(src, file)?, file)
}

/// Check `w`, a world read from `file`, and build its manifest.
///
/// # Errors
/// Every error found, in file order, up to [`config::WORLD_BUILD_MAX_ERRORS`].
pub fn build_model(w: &model::World, file: &str) -> Result<Built, Vec<Error>> {
    world::build(w, file)
}
