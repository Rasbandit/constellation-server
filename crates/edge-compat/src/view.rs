//! A read-only view of a running room for its host (issue #40): who is connected, progress per
//! slot, and the room's events as they happen. Everything comes from the room itself: the state
//! on the room task ([`RoomHandle::read`]), the live sessions ([`crate::presence`]) and the
//! runtime's published stream ([`Published`]). Nothing here is stored a second time.

use crate::CompatRoom;
use crate::session::Fanout;
use proto::{Event, EventKind, Seq, SlotNo};
use runtime::{Published, RoomError, Subscription};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Item and location names of one game, by id.
#[derive(Debug, Default)]
pub(crate) struct Names {
    items: BTreeMap<i64, String>,
    locations: BTreeMap<i64, String>,
}

impl Names {
    pub(crate) fn of(g: &seed_bundle::GameData) -> Self {
        let flip = |m: &BTreeMap<String, i64>| m.iter().map(|(n, id)| (*id, n.clone())).collect();
        Self { items: flip(&g.item_name_to_id), locations: flip(&g.location_name_to_id) }
    }
}

impl CompatRoom {
    fn names(&self, slot: u32) -> Option<&Names> {
        self.names.get(&self.bundle.slot(slot)?.game)
    }

    fn item_name(&self, slot: u32, item: i64) -> Option<&str> {
        self.names(slot)?.items.get(&item).map(String::as_str)
    }

    fn location_name(&self, slot: u32, location: i64) -> Option<&str> {
        self.names(slot)?.locations.get(&location).map(String::as_str)
    }

    /// The room now: its seed, and per slot its name, game, whether a session is on it, checked
    /// locations of its total, items received and goal.
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn summary(&self) -> Result<Value, RoomError> {
        let ids: Vec<u32> = self.bundle.slots.iter().map(|s| s.id).collect();
        let progress = self
            .room
            .read(move |s| {
                ids.iter()
                    .map(|&id| {
                        let slot = SlotNo(id);
                        let items = s.cursor(slot).map_or(0, |c| c.item_head.0);
                        (id, (s.checked_count(slot), items, s.has_goal(slot)))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .await?;
        let online = self.presence.connections().await;
        let slots: Vec<Value> = self
            .bundle
            .slots
            .iter()
            .map(|s| {
                let (checked, items, goal) = progress.get(&s.id).copied().unwrap_or_default();
                json!({
                    "slot": s.id,
                    "team": s.team,
                    "name": s.name,
                    "game": s.game,
                    "connected": online.contains_key(&s.id),
                    "clients": online.get(&s.id).copied().unwrap_or(0),
                    "checked": checked,
                    "locations": s.locations.len(),
                    "items_received": items,
                    "goal": goal,
                })
            })
            .collect();
        Ok(json!({"seed": self.bundle.seed_name, "slots": slots}))
    }

    /// The room's durable events after `after`, then everything it publishes live.
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn watch(&self, after: Seq) -> Result<Subscription, RoomError> {
        self.room.subscribe(after).await
    }

    /// What a host sees of one durable event; `None` for events with nothing to show (data
    /// storage writes).
    #[must_use]
    pub fn describe_event(&self, e: &Event) -> Option<Value> {
        let slot = e.slot.map(|s| s.0);
        Some(match &e.kind {
            EventKind::Checked { location, .. } => json!({
                "kind": "checked",
                "seq": e.seq.0,
                "slot": slot,
                "location": location,
                "location_name": slot.and_then(|s| self.location_name(s, *location)),
            }),
            EventKind::Item { from, location, item, .. } => json!({
                "kind": "item",
                "seq": e.seq.0,
                "from": from.0,
                "to": slot,
                "item": item,
                "item_name": slot.and_then(|s| self.item_name(s, *item)),
                "location": location,
                "location_name": self.location_name(from.0, *location),
                "flags": slot.map(|_| self.flags(from.0, *location)),
            }),
            EventKind::Goal => json!({"kind": "goal", "seq": e.seq.0, "slot": slot}),
            EventKind::KvSet { .. } => return None,
        })
    }

    /// What a host sees of one published entry: its durable events, or a session's join, leave
    /// or chat.
    #[must_use]
    pub fn describe(&self, p: &Published) -> Vec<Value> {
        match p {
            Published::Events { events, .. } => {
                events.iter().filter_map(|e| self.describe_event(e)).collect()
            }
            Published::Ext { body, .. } => {
                let Ok(Fanout::Print { print }) =
                    serde_json::from_value::<Fanout>((**body).clone())
                else {
                    return Vec::new();
                };
                let kind = match print.kind.as_str() {
                    "Join" => "join",
                    "Part" => "leave",
                    "Chat" => "chat",
                    _ => return Vec::new(),
                };
                vec![
                    json!({"kind": kind, "slot": print.slot, "team": print.team, "text": print.message}),
                ]
            }
        }
    }
}
