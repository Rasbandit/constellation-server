//! The front end: source text to a [`World`], with line and column on every value. It reads the
//! syntax and nothing else; every check is in [`crate::build_model`].
//!
//! No front end is built yet: the owner is deciding the authoring syntax (YAML with rule
//! strings, D-131, or a language of its own with its own file extension). This is the one
//! function a front end provides; until one exists it refuses every input.

use crate::Error;
use crate::model::World;

/// Read `src`, the text of the file `file`, into a world.
///
/// # Errors
/// Every syntax error, with its line and column. For now, always one error saying no front
/// end exists.
pub fn read(src: &str, file: &str) -> Result<World, Vec<Error>> {
    let _ = src;
    Err(vec![Error {
        file: file.to_owned(),
        pos: None,
        msg: "no front end reads world files yet: the authoring syntax is not decided".into(),
        help: Some("build a world from its model with `world_build::build_model`".into()),
    }])
}
