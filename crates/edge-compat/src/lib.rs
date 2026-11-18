//! The edge compatible with Archipelago clients (doc 05 6.2 rule 2: the only crate that knows
//! that protocol's types). It speaks the protocol from Archipelago's published protocol
//! document, in our own implementation, and maps it onto `room-core` commands and events
//! through a `runtime::RoomHandle`.
//!
//! [`CompatRoom`] holds one room's fixed data, prepared once; [`Session`] is one connection's
//! state machine; [`serve`] runs a session over a `yawc` WebSocket.

mod hints;
mod limit;
mod presence;
mod session;
mod socket;
mod storage;
pub mod wire;

use config::Limits;
use limit::RateTable;
use proto::Cid;
use room_core::HintRules;
use runtime::RoomHandle;
use seed_bundle::{Placed, SeedBundle};
use std::collections::BTreeMap;
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

/// One room's data, prepared once and shared by every session.
pub struct CompatRoom {
    bundle: Arc<SeedBundle>,
    room: RoomHandle,
    password: Option<String>,
    /// `(slot, location)` to what is placed there.
    placed: BTreeMap<(u32, i64), Placed>,
    /// `(receiving slot, item)` to every `(slot, location)` holding it, for `!hint`.
    by_item: BTreeMap<(u32, i64), Vec<(u32, i64)>>,
    /// The hint economy, from the seed (the room holds the same rules).
    hint_rules: HintRules,
    /// Game name to its data package as JSON, serialized once (R4: no heavy work per request).
    packages: BTreeMap<String, String>,
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
    /// Packets that write hints (scouts creating hints, `UpdateHint`), per slot.
    hint_writes: RateTable<u32>,
    /// Who is connected to each slot, and the client statuses they set.
    presence: presence::Presence,
}

impl std::fmt::Debug for CompatRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatRoom")
            .field("seed", &self.bundle.seed_name)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
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
        let placed: BTreeMap<(u32, i64), Placed> = bundle
            .slots
            .iter()
            .flat_map(|s| s.locations.iter().map(move |p| ((s.id, p.location), *p)))
            .collect();
        let mut by_item: BTreeMap<(u32, i64), Vec<(u32, i64)>> = BTreeMap::new();
        for (&at, p) in &placed {
            by_item.entry((p.receiver, p.item)).or_default().push(at);
        }
        let o = &bundle.server_options;
        let hint_rules = HintRules { cost_pct: o.hint_cost, check_points: o.location_check_points };
        // Name groups are not part of the package clients get; they read them through `Get`
        // (reference server: the reduced package, `MultiServer.py:624-641`).
        let packages = bundle
            .games
            .iter()
            .map(|(name, g)| {
                let reduced = serde_json::json!({
                    "item_name_to_id": g.item_name_to_id,
                    "location_name_to_id": g.location_name_to_id,
                    "checksum": g.checksum,
                });
                serde_json::to_string(&reduced).map(|json| (name.clone(), json))
            })
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let password = password.or_else(|| bundle.server_options.password.clone());
        let l = limits;
        let keys = l.rate_table_max_keys;
        Ok(Self {
            bundle,
            room,
            password,
            placed,
            by_item,
            hint_rules,
            packages,
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
            hint_writes: RateTable::new(l.hint_burst, l.hint_refill_milli_per_sec, keys),
            presence: presence::Presence::default(),
        })
    }

    fn flags(&self, slot: u32, location: i64) -> u8 {
        self.placed.get(&(slot, location)).map_or(0, |p| p.flags)
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
