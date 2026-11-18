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

/// Where a view that last saw `after` starts in a room whose log ends at `head`, keeping at most
/// `cap` events of history, and whether it must reset (drop what it showed). A cursor past the
/// head means the room restarted from an empty log (an in-memory store); one more than `cap`
/// behind means the viewer would miss events. A new view (`after` 0) gets the newest `cap`
/// events without a reset.
#[must_use]
pub fn view_start(after: Seq, head: Seq, cap: u64) -> (Seq, bool) {
    let floor = Seq(head.0.saturating_sub(cap));
    if after > head {
        (floor, true)
    } else if after < floor {
        (floor, after.0 > 0)
    } else {
        (after, false)
    }
}

impl CompatRoom {
    /// Names come from the slot's game package, shared by every room of that game.
    fn item_name(&self, slot: u32, item: i64) -> Option<&str> {
        self.bundle.slot(slot)?.package().item_name(item)
    }

    fn player(&self, slot: Option<u32>) -> Option<&str> {
        Some(self.bundle.slot(slot?)?.name())
    }

    fn location_name(&self, slot: u32, location: i64) -> Option<&str> {
        self.bundle.slot(slot)?.package().location_name(location)
    }

    /// The flags of the item at `slot`'s `location` (0 when it places nothing there).
    pub(crate) fn flags(&self, slot: u32, location: i64) -> u8 {
        let s = self.bundle.slot(slot);
        s.and_then(|s| s.flags(s.find(location)?)).unwrap_or(0)
    }

    /// The room now: its seed, and per slot its name, game, whether a session is on it, checked
    /// locations of its total, items received and goal.
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn summary(&self) -> Result<Value, RoomError> {
        let ids: Vec<u32> = self.bundle.slots().map(|s| s.id()).collect();
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
            .slots()
            .map(|s| {
                let id = s.id();
                let (checked, items, goal) = progress.get(&id).copied().unwrap_or_default();
                json!({
                    "slot": id,
                    "team": s.team(),
                    "name": s.name(),
                    "game": s.game(),
                    "connected": online.contains_key(&id),
                    "clients": online.get(&id).copied().unwrap_or(0),
                    "checked": checked,
                    "locations": s.placed(),
                    "items_received": items,
                    "goal": goal,
                })
            })
            .collect();
        let o = self.bundle.server_options();
        let options = json!({
            // Whether one is set, never the password itself.
            "password": self.password.is_some(),
            "hint_cost": o.hint_cost,
            "release_mode": o.release_mode,
            "collect_mode": o.collect_mode,
        });
        Ok(json!({"seed": self.bundle.seed_name(), "options": options, "slots": slots}))
    }

    /// A host's view of the room's stream: the durable events from where [`view_start`] puts
    /// `after` (at most `cap` of them), then everything the room publishes live. Also returns
    /// that start and whether the viewer must drop what it showed (`reset`).
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn watch(
        &self,
        after: Seq,
        cap: u64,
    ) -> Result<(Subscription, Seq, bool), RoomError> {
        let head = self.room.read(|s| s.durable()).await?;
        let (start, reset) = view_start(after, head, cap);
        Ok((self.room.subscribe(start).await?, start, reset))
    }

    /// What a host sees of one durable event; `None` for events with nothing to show (data
    /// storage writes and hints).
    #[must_use]
    pub fn describe_event(&self, e: &Event) -> Option<Value> {
        let slot = e.slot.map(|s| s.0);
        Some(match &e.kind {
            EventKind::Checked { location, .. } => json!({
                "kind": "checked",
                "seq": e.seq.0,
                "slot": slot,
                "name": self.player(slot),
                "location": location,
                "location_name": slot.and_then(|s| self.location_name(s, *location)),
            }),
            EventKind::Item { from, location, item, .. } => json!({
                "kind": "item",
                "seq": e.seq.0,
                "from": from.0,
                "from_name": self.player(Some(from.0)),
                "to": slot,
                "to_name": self.player(slot),
                "item": item,
                "item_name": slot.and_then(|s| self.item_name(s, *item)),
                "location": location,
                "location_name": self.location_name(from.0, *location),
                "flags": slot.map(|_| self.flags(from.0, *location)),
            }),
            EventKind::Goal => {
                json!({"kind": "goal", "seq": e.seq.0, "slot": slot, "name": self.player(slot)})
            }
            EventKind::KvSet { .. }
            | EventKind::HintAdded { .. }
            | EventKind::HintStatus { .. } => return None,
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
                    json!({"kind": kind, "slot": print.slot, "name": self.player(print.slot), "team": print.team, "text": print.message}),
                ]
            }
        }
    }
}
