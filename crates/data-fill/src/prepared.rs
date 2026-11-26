//! A world in its ready form: everything a generation needs that depends only on the world
//! version, made once ([`build`], at upload) and stored as one file, so a generation loads it
//! ([`Prepared::open`]) and does only the work of its own seed.
//!
//! The file is the compiled package ([`crate::section::compile`]: the rules section and the
//! key tables) with one more section, `SECTION_SEED_GAME`: the game's package file as every
//! seed of this world names it (the names and ids clients are sent, with their checksum).

use seed_bundle::package::{self, SECTION_SEED_GAME};
use seed_bundle::{GameData, GamePackage};
use serde_json::Value;

use crate::section;
use crate::seed::{game_data, manifest_game};
use crate::world::World;

/// The layout a prepared world is stored and found under: the rules section's version and
/// `config::PREPARED_WORLD_LAYOUT`. A file of another layout is stale.
pub const LAYOUT: u32 = (section::VERSION as u32) << 16 | config::PREPARED_WORLD_LAYOUT as u32;

/// Most bytes the game's names and ids may inflate to: a seed's own cap, since a seed names
/// them.
const GAME_MAX_BYTES: u64 = config::Limits::DESIGN.bundle_max_bytes;

/// A loaded prepared world: the compiled world, and the tables every seed of it shares.
#[derive(Debug)]
pub struct Prepared {
    world: World,
    game: String,
    /// The game's names, ids and checksum, as a seed bundle carries them.
    data: GameData,
    /// The game's package as a seed file is encoded on it (ids to indexes), under its key.
    table: GamePackage,
}

/// The game's package file in a prepared world's `file`: what a seed of that world names by
/// key, for the store that keeps seeds' packages.
///
/// # Errors
/// A file without that section.
pub fn seed_game(file: &[u8]) -> Result<&[u8], String> {
    package::stored(file, SECTION_SEED_GAME)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "prepared world: no seed game section".to_owned())
}

/// Prepare the world `manifest` describes: the file to store, and the world it loads as.
///
/// # Errors
/// A manifest without a `game`, or one the compiler or the loader refuses.
pub fn build(manifest: &Value) -> Result<(Vec<u8>, World), String> {
    let game = manifest_game(manifest)?;
    let (rules, keys) = section::sections(manifest)?;
    let world = World::from_sections(rules.clone(), keys.clone())?;
    let seed_game = GamePackage::file(game, &game_data(&world)?).map_err(|e| e.to_string())?;
    Ok((section::package(rules, keys, &[(SECTION_SEED_GAME, &seed_game)])?, world))
}

impl Prepared {
    /// Load a prepared world's `file`, checked as [`World::from_package`] checks a package.
    ///
    /// # Errors
    /// A file that is not a prepared world of this layout.
    pub fn open(file: &[u8]) -> Result<Self, String> {
        let world = World::from_package(file)?;
        let seed_game = seed_game(file)?;
        let (game, data) =
            package::game_of(seed_game, GAME_MAX_BYTES).map_err(|e| e.to_string())?;
        let table = GamePackage::new(package::key_of(seed_game), &game, &data);
        Ok(Self { world, game, data, table })
    }

    /// The compiled world.
    #[must_use]
    pub const fn world(&self) -> &World {
        &self.world
    }

    /// The game's name.
    #[must_use]
    pub fn game(&self) -> &str {
        &self.game
    }

    /// The game's names, ids and checksum.
    #[must_use]
    pub const fn data(&self) -> &GameData {
        &self.data
    }

    /// The game's package, keyed as seeds of this world name it.
    #[must_use]
    pub const fn table(&self) -> &GamePackage {
        &self.table
    }
}
