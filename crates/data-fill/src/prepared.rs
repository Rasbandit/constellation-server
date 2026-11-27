//! A world in its ready form: everything a generation needs that depends only on the world
//! version, made once ([`build`], at upload) and stored as one file, so a generation loads it
//! ([`Prepared::open`]) and does only the work of its own seed.
//!
//! The file is the compiled package ([`crate::section::compile`]: the rules section and the
//! key tables) with one more section, `SECTION_SEED_GAME`: the game's package file as every
//! seed of this world names it (the names and ids clients are sent, with their checksum). Its
//! content is also what a seed's hash reads of the game, so that is not written per seed.

use seed_bundle::package::{self, SECTION_GAME, SECTION_SEED_GAME, SECTION_SHUFFLES};
use seed_bundle::{GameData, GamePackage};
use serde_json::Value;

use crate::section;
use crate::seed::{game_data, manifest_game};
use crate::world::World;

/// The layout of a prepared world's file, as text: the versions of everything in it (the rules
/// section, the package file format it is and embeds, and `config::PREPARED_WORLD_LAYOUT` for
/// which sections it holds). Part of what a stored world is found under, with the build that
/// made it: the file does not carry it.
#[must_use]
pub fn layout() -> String {
    format!("{}.{}.{}", section::VERSION, package::VERSION, config::PREPARED_WORLD_LAYOUT)
}

/// The design cap of a seed file, where no configured one is at hand: the default of a fill
/// request, and the most the game's names and ids may inflate to (a seed names them).
pub const SEED_MAX_BYTES: u64 = config::Limits::DESIGN.bundle_max_bytes;

/// A loaded prepared world: the compiled world, and the tables every seed of it shares.
#[derive(Debug)]
pub struct Prepared {
    world: World,
    game: String,
    /// The game's names, ids and checksum, as a seed bundle carries them.
    data: GameData,
    /// The game's package as a seed file is encoded on it (ids to indexes), under its key.
    table: GamePackage,
    /// The canonical JSON of a seed bundle's `games` (this one game's `data`), as its hash
    /// reads it.
    games_json: Vec<u8>,
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
    let (rules, keys, shuffles) = section::compiled(manifest)?;
    let world = World::from_parts(rules.clone(), keys.clone(), shuffles.as_deref())?;
    let seed_game = GamePackage::file(game, &game_data(&world)?).map_err(|e| e.to_string())?;
    let mut more: Vec<(u16, &[u8])> = vec![(SECTION_SEED_GAME, &seed_game)];
    more.extend(shuffles.iter().map(|b| (SECTION_SHUFFLES, &b[..])));
    Ok((section::package(rules, keys, &more)?, world))
}

impl Prepared {
    /// Load a prepared world's `file`, checked as [`World::from_package`] checks a package.
    ///
    /// # Errors
    /// A file whose sections are missing, damaged or of another version than this engine reads.
    pub fn open(file: &[u8]) -> Result<Self, String> {
        let world = World::from_package(file)?;
        let seed_game = seed_game(file)?;
        let content = package::section_of(seed_game, SECTION_GAME, SEED_MAX_BYTES)
            .map_err(|e| e.to_string())?
            .ok_or("prepared world: no game section in the seed game")?;
        let (game, data) = GamePackage::parse(&content).map_err(|e| e.to_string())?;
        // The package's content already holds the data as a bundle's hash reads it.
        let data_json = package::data_json(&content, &game)
            .ok_or("prepared world: the seed game's content is not canonical")?;
        let name = Value::from(game.as_str()).to_string();
        let games_json = [b"{", name.as_bytes(), b":", data_json, b"}"].concat();
        let table = GamePackage::new(package::key_of(seed_game), &game, &data);
        Ok(Self { world, game, data, table, games_json })
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

    /// Bytes this prepared world holds in memory: the loaded world, the game's name, both
    /// tables and the games' JSON. What a cache of prepared worlds counts against its cap;
    /// checked against a counting allocator in `tests/memory.rs` (CI runs it).
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        self.world.held_bytes()
            + self.game.capacity()
            + self.data.held_bytes()
            + self.table.held_bytes()
            + self.games_json.capacity()
    }

    /// The game's names, ids and checksum.
    #[must_use]
    pub const fn data(&self) -> &GameData {
        &self.data
    }

    /// The canonical JSON of the `games` of every seed bundle of this world
    /// (`SeedBundle::hash_with_games`).
    #[must_use]
    pub fn games_json(&self) -> &[u8] {
        &self.games_json
    }

    /// The game's package, keyed as seeds of this world name it.
    #[must_use]
    pub const fn table(&self) -> &GamePackage {
        &self.table
    }
}
