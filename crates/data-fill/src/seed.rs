//! The fill's output as a seed: [`generate`] compiles a manifest, fills every player's world
//! together and assembles the [`SeedBundle`] a room runs on, with each item's class, the
//! spheres (when a location is first reachable) and slot details. [`generate_on`] does the same
//! on a world prepared earlier ([`crate::prepared`]), and [`encode_on`] writes its bundle as a
//! v2 seed (`docs/seed-bundle-v2.md`); `seed_bundle::import_v1` does that for a bundle's JSON.

use std::collections::BTreeMap;

use seed_bundle::{GameData, Mode, SeedBundle, ServerOptions, Slot, SlotKind, Version};
use serde_json::{Value, json};
use spec_rng::SpecRng;

use crate::fill::Instance;
use crate::prepared::Prepared;
use crate::room::{self, Seat};
use crate::spheres::RoomSpheres;
use crate::world::World;

/// One player: the slot name and the resolved instance of their world.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Player {
    /// The slot's name, which is also its connect name.
    pub name: String,
    /// Options, locations, pool and start items for this player.
    pub instance: Instance,
}

/// A game or version name from a manifest, trimmed: not empty, at most
/// `config::MANIFEST_NAME_MAX_BYTES`, no control characters (it is kept in listing columns, an
/// index and file names).
///
/// # Errors
/// A message naming what is wrong with it.
pub fn checked_name<'a>(what: &str, name: &'a str) -> Result<&'a str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("the manifest needs a {what:?}"));
    }
    if name.len() > config::MANIFEST_NAME_MAX_BYTES {
        return Err(format!(
            "the manifest's {what:?} is longer than {} bytes",
            config::MANIFEST_NAME_MAX_BYTES
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(format!("the manifest's {what:?} holds a control character"));
    }
    Ok(name)
}

/// The manifest's game name, checked: the one reading of it (the upload record, the folder and
/// the seed all use this).
///
/// # Errors
/// When the manifest has no game, or one [`checked_name`] refuses.
pub fn manifest_game(manifest: &Value) -> Result<&str, String> {
    checked_name("game", manifest["game"].as_str().unwrap_or(""))
}

/// Fill a room of `players`, all playing the world `manifest` describes (its `game` key names
/// it), and assemble the seed named `seed_name`. The same `rng_seed` gives the same bundle
/// whatever `workers` is.
///
/// # Errors
/// A message for a manifest the engine refuses or without a `game`, a player without a name
/// or with a repeated one, an instance the world lacks keys for, a fill that is stuck, or a
/// world a player cannot finish.
pub fn generate(
    manifest: &Value,
    seed_name: &str,
    players: &[Player],
    rng_seed: u64,
    workers: usize,
) -> Result<SeedBundle, String> {
    generate_by(manifest, seed_name, players, rng_seed, workers, room::Algorithm::Exact)
}

/// [`generate`] with the mode that places the progression items ([`room::Algorithm`]). The
/// bundle has no field that names the mode: the same bundle shape whichever made it.
///
/// # Errors
/// As [`generate`].
pub fn generate_by(
    manifest: &Value,
    seed_name: &str,
    players: &[Player],
    rng_seed: u64,
    workers: usize,
    algorithm: room::Algorithm,
) -> Result<SeedBundle, String> {
    let game = manifest_game(manifest)?;
    named(players)?;
    let w = World::load(manifest)?;
    let data = game_data(&w)?;
    fill_on((&w, game, data), seed_name, players, rng_seed, workers, algorithm)
}

/// [`generate_by`] on a prepared world: nothing is compiled and the game's tables are not
/// built again, the fill starts from the stored form. The same bundle as the manifest gives.
///
/// # Errors
/// As [`generate`], less what only a manifest can get wrong.
pub fn generate_on(
    prepared: &Prepared,
    seed_name: &str,
    players: &[Player],
    rng_seed: u64,
    workers: usize,
    algorithm: room::Algorithm,
) -> Result<SeedBundle, String> {
    named(players)?;
    let on = (prepared.world(), prepared.game(), prepared.data().clone());
    fill_on(on, seed_name, players, rng_seed, workers, algorithm)
}

/// The bundle of a room filled on a prepared world: `placed` (one row per seat) and the
/// `spheres` that reached them. The step of [`generate_on`] after the fill, for callers that
/// time the steps apart.
///
/// # Errors
/// An instance naming an item the world lacks, or more slots than a seed takes.
pub fn assemble_on(
    prepared: &Prepared,
    seed_name: &str,
    players: &[Player],
    seats: &[Seat<'_>],
    placed: &[Vec<Option<room::Given>>],
    spheres: &RoomSpheres,
) -> Result<SeedBundle, String> {
    let on = (prepared.world(), prepared.game(), prepared.data().clone());
    assemble(on, seed_name, players, seats, placed, spheres)
}

/// Every player has a name, and no two share one.
fn named(players: &[Player]) -> Result<(), String> {
    for (k, p) in players.iter().enumerate() {
        if p.name.trim().is_empty() {
            return Err(format!("player {} has no name", k + 1));
        }
        if players[..k].iter().any(|q| q.name == p.name) {
            return Err(format!("player name {:?} repeats", p.name));
        }
    }
    Ok(())
}

/// The seed file (v2, `docs/seed-bundle-v2.md`) of a `bundle` assembled on a prepared world,
/// at most `max_bytes`: checked as an imported bundle is, hashed as its JSON text would be
/// ([`seed_bundle::bundle_hash`]) and encoded on the world's package, so nothing is written as
/// JSON and parsed back and the package is not made again.
///
/// # Errors
/// A bundle that breaks a rule a room trusts, or is too large for a seed file.
pub fn encode_on(
    prepared: &Prepared,
    bundle: &SeedBundle,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    bundle.validate().map_err(|e| e.to_string())?;
    let tables = std::slice::from_ref(prepared.table());
    seed_bundle::v2::encode_on(bundle, &bundle.hash()?, max_bytes, tables)
        .map_err(|e| e.to_string())
}

/// A loaded world, its game's name and the game's tables ([`game_data`], which depend only on
/// the world).
type On<'a> = (&'a World, &'a str, GameData);

/// Fill a room of `players` on a loaded world and assemble its seed.
fn fill_on(
    on: On<'_>,
    seed_name: &str,
    players: &[Player],
    rng_seed: u64,
    workers: usize,
    algorithm: room::Algorithm,
) -> Result<SeedBundle, String> {
    let insts: Vec<&Instance> = players.iter().map(|p| &p.instance).collect();
    // The room owns its world: a copy of the loaded bytes, small beside a fill.
    let room = room::Compiled::shared(on.0.clone(), &insts, workers)?;
    let seats = room.seats();
    let (filled, spheres) = room::fill_room_by(
        &seats,
        &mut SpecRng::from_u64(rng_seed),
        config::FILL_MAX_ATTEMPTS,
        room::Path::of(seats.len(), workers),
        algorithm,
    )
    .map_err(|e| format!("{e:?}"))?;
    if let Some(p) = filled.beatable.iter().position(|b| !b) {
        return Err(format!("player {} cannot reach their goal", p + 1));
    }
    assemble(on, seed_name, players, &seats, &filled.placed, &spheres)
}

/// The bundle for filled `placed` (one row per seat) and the `spheres` that reached them.
fn assemble(
    (w, game, data): On<'_>,
    seed_name: &str,
    players: &[Player],
    seats: &[Seat<'_>],
    placed: &[Vec<Option<room::Given>>],
    spheres: &RoomSpheres,
) -> Result<SeedBundle, String> {
    let flags: Vec<Vec<u8>> = players.iter().map(|p| p.instance.flags(w)).collect();
    let rows = room::placements_flagged(seats, placed, Some(spheres), &flags)
        .map_err(|e| format!("{e:?}"))?;
    let mut slots = Vec::with_capacity(players.len());
    for (k, (p, locations)) in players.iter().zip(rows).enumerate() {
        let start_inventory = p
            .instance
            .start_inventory
            .iter()
            .map(|key| w.item(key).map(|i| w.item_id(i)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("player {}: {e}", k + 1))?;
        slots.push(Slot {
            id: u32::try_from(k + 1).map_err(|e| e.to_string())?,
            team: 0,
            name: p.name.clone(),
            game: game.to_owned(),
            kind: SlotKind::Player,
            minimum_client_version: Version { major: 0, minor: 0, build: 0 },
            group_members: Vec::new(),
            locations,
            slot_data: json!({}),
            start_inventory,
            entrances: BTreeMap::new(),
        });
    }
    let connect_names = slots.iter().map(|s| (s.name.clone(), (s.team, s.id))).collect();
    Ok(SeedBundle {
        format: seed_bundle::FORMAT,
        seed_name: seed_name.to_owned(),
        generator_version: Version {
            major: config::SEED_GENERATOR_VERSION.0,
            minor: config::SEED_GENERATOR_VERSION.1,
            build: config::SEED_GENERATOR_VERSION.2,
        },
        server_options: ServerOptions {
            password: None,
            hint_cost: config::SEED_HINT_COST_PCT,
            location_check_points: config::SEED_CHECK_POINTS,
            release_mode: Mode::Auto,
            collect_mode: Mode::Goal,
            remaining_mode: Mode::Goal,
        },
        slots,
        connect_names,
        games: BTreeMap::from([(game.to_owned(), data)]),
        race_mode: 0,
        start_hints: Vec::new(),
    })
}

/// The world's names and ids as the game's data package; the checksum is the hash of both
/// tables, so it changes with them.
pub(crate) fn game_data(w: &World) -> Result<GameData, String> {
    let items: BTreeMap<String, i64> = (0..u16::try_from(w.items()).map_err(|e| e.to_string())?)
        .map(|i| (w.item_key(i).to_owned(), w.item_id(i)))
        .collect();
    let locations: BTreeMap<String, i64> = (0..u32::try_from(w.locations())
        .map_err(|e| e.to_string())?)
        .map(|l| (w.loc_key(l).to_owned(), w.loc_id(l)))
        .collect();
    let tables = serde_json::to_string(&json!({"items": items, "locations": locations}))
        .map_err(|e| e.to_string())?;
    let checksum = seed_bundle::bundle_hash(&tables)?.iter().map(|b| format!("{b:02x}")).collect();
    Ok(GameData {
        item_name_to_id: items,
        location_name_to_id: locations,
        checksum,
        ..GameData::default()
    })
}
