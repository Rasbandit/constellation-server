//! The edge compatible with Archipelago clients (doc 05 6.2 rule 2: the only crate that knows
//! that protocol's types). It speaks the protocol from Archipelago's published protocol
//! document, in our own implementation, and maps it onto `room-core` commands and events
//! through a `runtime::RoomHandle`.
//!
//! [`CompatRoom`] holds one room's fixed data, prepared once; [`Session`] is one connection's
//! state machine; [`serve`] runs a session over a `yawc` WebSocket.

mod limit;
mod presence;
mod session;
mod socket;
mod storage;
pub mod wire;

use config::Limits;
use limit::RateTable;
use proto::Cid;
use runtime::RoomHandle;
use seed_bundle::Seed;
use std::net::IpAddr;
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

/// One room's data, shared by every session. The seed is read in place and its game packages
/// (with their JSON, serialized once: R4, no heavy work per request) are shared by every room
/// of the same games.
pub struct CompatRoom {
    bundle: Arc<Seed>,
    room: RoomHandle,
    password: Option<String>,
    limits: Limits,
    /// Chat and bounce, per slot (they share the fan-out path).
    fanout: RateTable<u32>,
    /// Data package requests, per client address.
    datapackage: RateTable<IpAddr>,
    /// `Connect` packets, per client address.
    connects: RateTable<IpAddr>,
    /// `Set` packets, per slot (their own bucket, so shared-value writes never use up chat).
    kv_sets: RateTable<u32>,
    /// Bytes `Set` packets logged, per slot (every write is logged in full).
    kv_bytes: RateTable<u32>,
    /// Who is connected to each slot, and the client statuses they set.
    presence: presence::Presence,
}

impl std::fmt::Debug for CompatRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatRoom")
            .field("seed", &self.bundle.seed_name())
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl CompatRoom {
    /// Prepare a room. `password` overrides the bundle's (`None` keeps it).
    ///
    /// # Errors
    /// None today; kept so the host treats every room setup step alike.
    pub fn new(
        bundle: Arc<Seed>,
        room: RoomHandle,
        password: Option<String>,
        limits: Limits,
    ) -> Result<Self, String> {
        let password = password.or_else(|| bundle.password().map(str::to_owned));
        let l = limits;
        let keys = l.rate_table_max_keys;
        Ok(Self {
            bundle,
            room,
            password,
            limits,
            fanout: RateTable::new(l.fanout_burst, l.fanout_refill_milli_per_sec, keys),
            datapackage: RateTable::new(
                l.datapackage_burst,
                l.datapackage_refill_milli_per_sec,
                keys,
            ),
            connects: RateTable::new(l.connect_burst, l.connect_refill_milli_per_sec, keys),
            kv_sets: RateTable::new(l.kv_set_burst, l.kv_set_refill_milli_per_sec, keys),
            kv_bytes: RateTable::new(
                l.kv_write_bytes_burst,
                l.kv_write_bytes_refill_milli_per_sec,
                keys,
            ),
            presence: presence::Presence::default(),
        })
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
