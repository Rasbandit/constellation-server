//! The edge compatible with Archipelago clients (doc 05 6.2 rule 2: the only crate that knows
//! that protocol's types). It speaks the protocol from Archipelago's published protocol
//! document, in our own implementation, and maps it onto `room-core` commands and events
//! through a `runtime::RoomHandle`.
//!
//! [`CompatRoom`] holds one room's fixed data, prepared once; [`Session`] is one connection's
//! state machine; [`serve`] runs a session over a `yawc` WebSocket.

mod limit;
mod session;
mod socket;
pub mod wire;

use config::Limits;
use proto::Cid;
use runtime::RoomHandle;
use seed_bundle::SeedBundle;
use std::collections::BTreeMap;
use std::sync::Arc;

pub use session::{Closed, Session};
pub use socket::{serve, ws_options};

/// The protocol level we implement: the protocol document as of Archipelago 0.6.8 (the
/// checkout we read). Sent in `RoomInfo.version`.
pub const PROTOCOL_VERSION: (u32, u32, u32) = (0, 6, 8);

/// The oldest client version accepted from a client with no game (a tracker): the same
/// server-wide floor as the reference server (`min_client_version` in `MultiServer.py`).
const NO_GAME_MIN_VERSION: (u32, u32, u32) = (0, 5, 0);

/// Tags that mean "no game": such clients may connect with an empty game name and may not
/// send checks (section `Tags`).
const NO_GAME_TAGS: [&str; 3] = ["HintGame", "Tracker", "TextOnly"];

/// One room's data, prepared once and shared by every session.
#[derive(Debug)]
pub struct CompatRoom {
    bundle: Arc<SeedBundle>,
    room: RoomHandle,
    password: Option<String>,
    /// `(slot, location)` to item flags.
    flags: BTreeMap<(u32, i64), u8>,
    /// Game name to its data package as JSON, serialized once (R4: no heavy work per request).
    packages: BTreeMap<String, String>,
    limits: Limits,
}

impl CompatRoom {
    /// Prepare a room. `password` overrides the bundle's (`None` keeps it).
    ///
    /// # Errors
    /// A message when a data package cannot be encoded.
    pub fn new(
        bundle: Arc<SeedBundle>,
        room: RoomHandle,
        password: Option<String>,
        limits: Limits,
    ) -> Result<Self, String> {
        let flags = bundle
            .slots
            .iter()
            .flat_map(|s| s.locations.iter().map(move |p| ((s.id, p.location), p.flags)))
            .collect();
        let packages = bundle
            .games
            .iter()
            .map(|(name, g)| serde_json::to_string(g).map(|json| (name.clone(), json)))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let password = password.or_else(|| bundle.server_options.password.clone());
        Ok(Self { bundle, room, password, flags, packages, limits })
    }

    fn flags(&self, slot: u32, location: i64) -> u8 {
        self.flags.get(&(slot, location)).copied().unwrap_or(0)
    }
}

/// The idempotency key of a check from this edge: the location itself.
///
/// A location is checked once per slot, so a client that re-sends checks after a reconnect (as the protocol asks) is
/// answered from the first check, across sessions and restarts of the client.
#[must_use]
pub const fn check_cid(location: i64) -> Cid {
    Cid(u64::from_le_bytes(location.to_le_bytes()))
}

#[cfg(test)]
mod tests;
