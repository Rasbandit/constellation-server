//! Client status presence (the archived protocol plan section 5): a slot reads as connected (5) while
//! any connection is on it and as unknown (0) once the last one leaves; `StatusUpdate` sets it
//! in between. A reached goal (30) is durable in the room and always wins. Presence itself is
//! not durable: a restart disconnects everyone, so 0 is right afterwards (reference
//! `MultiServer.py:1043-1046, 1071-1074, 2315-2328`).
//!
//! Every change is published to the `_read_client_status_<team>_<slot>` watchers while the lock
//! is held, so watchers see the changes in the order they happened.
#![allow(
    clippy::redundant_pub_crate,
    reason = "rustc's unreachable_pub (denied) needs crate visibility in this private module"
)]

use crate::storage::status_key;
use crate::wire::{CLIENT_CONNECTED, CLIENT_UNKNOWN};
use std::collections::BTreeMap;
use tokio::sync::Mutex;

#[derive(Debug, Default, Clone, Copy)]
struct Slot {
    connections: usize,
    status: i64,
    /// The status is final (30): set under the lock when the goal is reached, so a change that
    /// read "no goal" before it can never publish after it.
    done: bool,
}

/// Per-slot connection counts and statuses for one room.
#[derive(Debug, Default)]
pub(crate) struct Presence {
    slots: Mutex<BTreeMap<u32, Slot>>,
}

/// What a presence change asks of the caller: tell the watchers of `key` its new `value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Changed {
    pub(crate) key: String,
    pub(crate) value: i64,
}

/// Who a change is about.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Who {
    pub(crate) team: u32,
    pub(crate) slot: u32,
    /// The slot's status is final as far as the caller knows (`storage::done`): it stays 30
    /// and nothing is published.
    pub(crate) done: bool,
}

impl Presence {
    /// How many connections are on each slot that has any.
    pub(crate) async fn connections(&self) -> BTreeMap<u32, usize> {
        let slots = self.slots.lock().await;
        slots.iter().filter(|(_, s)| s.connections > 0).map(|(k, s)| (*k, s.connections)).collect()
    }

    /// The status a slot's connections set (0 when none did).
    pub(crate) async fn status(&self, slot: u32) -> i64 {
        self.slots.lock().await.get(&slot).map_or(CLIENT_UNKNOWN, |s| s.status)
    }

    /// Apply `change` to the slot and, when it says so, publish the result, under the lock.
    async fn update(
        &self,
        who: Who,
        change: impl FnOnce(&mut Slot) -> bool,
        publish: impl AsyncFnOnce(Changed),
    ) {
        let mut slots = self.slots.lock().await;
        let s = slots.entry(who.slot).or_default();
        s.done |= who.done;
        if change(s) && !s.done {
            publish(Changed { key: status_key(who.team, who.slot), value: s.status }).await;
        }
        drop(slots);
    }

    /// Reach the slot's goal with `act` (the durable goal command) while holding the lock, then
    /// mark the status final. A status change waiting for the lock sees it and publishes
    /// nothing, so 30 is the last value watchers hear.
    pub(crate) async fn reach_goal<E>(
        &self,
        slot: u32,
        act: impl AsyncFnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        let mut slots = self.slots.lock().await;
        act().await?;
        slots.entry(slot).or_default().done = true;
        drop(slots);
        Ok(())
    }

    /// A connection joined: unknown becomes connected.
    pub(crate) async fn join(&self, who: Who, publish: impl AsyncFnOnce(Changed)) {
        let change = |s: &mut Slot| {
            s.connections = s.connections.saturating_add(1);
            let fresh = s.status == CLIENT_UNKNOWN;
            if fresh {
                s.status = CLIENT_CONNECTED;
            }
            fresh
        };
        self.update(who, change, publish).await;
    }

    /// A connection left: the last one out makes the slot unknown.
    pub(crate) async fn part(&self, who: Who, publish: impl AsyncFnOnce(Changed)) {
        let change = |s: &mut Slot| {
            s.connections = s.connections.saturating_sub(1);
            let last = s.connections == 0;
            if last {
                s.status = CLIENT_UNKNOWN;
            }
            last
        };
        self.update(who, change, publish).await;
    }

    /// A `StatusUpdate` other than the goal: stored and published even when unchanged, as the
    /// reference does.
    pub(crate) async fn set(&self, who: Who, status: i64, publish: impl AsyncFnOnce(Changed)) {
        let change = |s: &mut Slot| {
            s.status = status;
            true
        };
        self.update(who, change, publish).await;
    }
}
