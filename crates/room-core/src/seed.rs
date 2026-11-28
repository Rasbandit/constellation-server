//! A room on a v2 seed file ([`seed_bundle::Seed`]). Here, not in the seed crate, so the seed
//! format does not depend on the room: the room reads the seed through [`World`].

use crate::{CoreError, HintRules, RoomState, World};
use alloc::sync::Arc;
use proto::SlotNo;
use seed_bundle::Seed;

impl World for Seed {
    fn slots(&self) -> &[SlotNo] {
        self.slot_ids()
    }

    fn placed(&self, pos: usize) -> usize {
        self.slot_at(pos).map_or(0, |s| s.placed())
    }

    fn find(&self, pos: usize, location: i64) -> Option<usize> {
        self.slot_at(pos)?.find(location)
    }

    fn target(&self, pos: usize, index: usize) -> Option<(usize, i64)> {
        self.slot_at(pos)?.target(index)
    }

    fn location(&self, pos: usize, index: usize) -> Option<i64> {
        self.slot_at(pos)?.location(index)
    }
}

impl HintRules {
    /// The room's hint economy: the one place it is read from a seed (the room and the edge
    /// both use it).
    #[must_use]
    pub fn of(seed: &Seed) -> Self {
        Self { cost_pct: seed.hint_cost(), check_points: seed.location_check_points() }
    }
}

/// A room's state before any event, on its seed: the placement, the hint economy and the
/// start hints. The one place a room is built from a seed.
///
/// # Errors
/// What the room refuses (`Seed::open` rules most of it out).
pub fn room_state(seed: &Arc<Seed>) -> Result<RoomState, CoreError> {
    let world: Arc<dyn World> = Arc::clone(seed) as _;
    RoomState::from_world(world)?.with_hints(HintRules::of(seed), seed.start_hints())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::Seq;

    /// Two slots of game G; B's location 101 holds A's item 7, and A asked for it at
    /// generation (a start hint).
    const BUNDLE: &str = r#"{
        "format": 1, "seed_name": "s1",
        "generator_version": {"major": 0, "minor": 6, "build": 8},
        "server_options": {
            "password": null, "hint_cost": 10, "location_check_points": 1,
            "release_mode": "auto", "collect_mode": "auto-enabled", "remaining_mode": "goal"
        },
        "slots": [
            {"id": 1, "team": 0, "name": "A", "game": "G", "kind": "player",
             "minimum_client_version": {"major": 0, "minor": 1, "build": 6},
             "locations": [{"location": 100, "item": 7, "receiver": 2, "flags": 1}],
             "slot_data": {"k": [1, 2]}},
            {"id": 2, "team": 0, "name": "B", "game": "G", "kind": "player",
             "minimum_client_version": {"major": 0, "minor": 1, "build": 6},
             "locations": [{"location": 100, "item": 8, "receiver": 2, "flags": 0},
                           {"location": 101, "item": 7, "receiver": 1, "flags": 4}],
             "slot_data": {}}
        ],
        "connect_names": {"A": [0, 1], "B": [0, 2]},
        "games": {"G": {
            "item_name_to_id": {"Seven": 7, "Eight": 8},
            "location_name_to_id": {"L100": 100, "L101": 101},
            "checksum": "abc"
        }},
        "start_hints": [{"finding": 2, "location": 101, "status": 30}]
    }"#;

    #[test]
    fn start_hints_and_the_hint_economy_reach_the_fresh_room() {
        let (seed, _) = seed_bundle::import_v1(BUNDLE.as_bytes(), 1 << 26).unwrap();
        let s = room_state(&Arc::new(seed)).unwrap();
        let hints = s.hints_of(SlotNo(1));
        assert_eq!(hints.len(), 1);
        assert_eq!((hints[0].finding, hints[0].location, hints[0].status), (SlotNo(2), 101, 30));
        assert_eq!(s.hint_rules(), HintRules { cost_pct: 10, check_points: 1 });
        assert_eq!(s.head(), Seq(0), "start hints are not log events");
    }
}
