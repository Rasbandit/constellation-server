//! Synthetic seed bundles sized like real ones, for the memory harness and format tests.
//!
//! Shaped after a real one-slot world (1516 placed locations of a 2641-location game with 237
//! items, a 3.6 KB `slot_data`, one location group listing every location): the sizes, not the
//! content. Deterministic from `seed`.

use serde_json::{Map, Value, json};

/// The shape of a synthetic world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// Slots (players), numbered from 1.
    pub slots: u32,
    /// Distinct games; slot `n` plays game `n % games`.
    pub games: u32,
    /// Locations each game defines.
    pub game_locations: u32,
    /// Items each game defines.
    pub game_items: u32,
    /// Locations placed in each slot's world (at most `game_locations`).
    pub placed: u32,
    /// The rng seed (printed by the harness).
    pub seed: u64,
}

impl Shape {
    /// Like the real one-slot world.
    pub const ONE_SLOT: Self =
        Self { slots: 1, games: 1, game_locations: 2641, game_items: 237, placed: 1516, seed: 1 };
    /// Eight slots, 8000 placed locations, four games.
    pub const EIGHT_SLOT: Self =
        Self { slots: 8, games: 4, game_locations: 1500, game_items: 200, placed: 1000, seed: 8 };
}

/// SplitMix64: small, seeded, good enough to spread placements.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.next() % u64::from(n.max(1))).unwrap_or(0)
    }
}

fn game_name(g: u32) -> String {
    format!("Synthetic Game {g}")
}

/// Ids like real worlds: a per-game base plus a running number.
fn id(g: u32, n: u32) -> i64 {
    i64::from(g + 1) * 100_000 + i64::from(n) + 1
}

fn game(g: u32, s: &Shape) -> Value {
    let locations: Vec<String> = (0..s.game_locations)
        .map(|n| format!("Region {:02} Location Number {n:05}", n % 40))
        .collect();
    let items: Vec<String> = (0..s.game_items).map(|n| format!("Item Name {n:04}")).collect();
    let to_id = |names: &[String]| -> Map<String, Value> {
        names.iter().enumerate().map(|(n, k)| (k.clone(), json!(id(g, n as u32)))).collect()
    };
    // Archipelago adds an everything-group; the regions make the rest.
    let mut location_groups = Map::new();
    location_groups.insert("Everywhere".into(), json!(locations));
    for r in 0..10 {
        let members: Vec<&String> = locations.iter().skip(r).step_by(40).collect();
        location_groups.insert(format!("Region {r:02}"), json!(members));
    }
    let mut item_groups = Map::new();
    item_groups.insert("Everything".into(), json!(items));
    for r in 0..7 {
        let members: Vec<&String> = items.iter().skip(r).step_by(7).take(16).collect();
        item_groups.insert(format!("Group {r}"), json!(members));
    }
    json!({
        "checksum": format!("{:040x}", u64::from(g) + s.seed),
        "item_name_groups": item_groups,
        "item_name_to_id": to_id(&items),
        "location_name_groups": location_groups,
        "location_name_to_id": to_id(&locations),
    })
}

fn slot_data(rng: &mut Rng) -> Value {
    let mut m = Map::new();
    for k in 0..100 {
        m.insert(format!("option_number_{k:03}"), json!(rng.below(400)));
    }
    m.insert("names".into(), json!(["one", "two", "three"]));
    Value::Object(m)
}

/// A v1 seed bundle (the schema of `seed_bundle::SeedBundle`) of this shape, as JSON text.
#[must_use]
pub fn bundle(s: &Shape) -> String {
    let mut rng = Rng(s.seed);
    let mut games = Map::new();
    for g in 0..s.games {
        games.insert(game_name(g), game(g, s));
    }
    games.insert(
        "Archipelago".into(),
        json!({
            "checksum": "ac9141e9ad0318df2fa27da5f20c50a842afeecb",
            "item_name_groups": {"Everything": ["Nothing"]},
            "item_name_to_id": {"Nothing": -1},
            "location_name_groups": {"Everywhere": ["Cheat Console", "Server"]},
            "location_name_to_id": {"Cheat Console": -1, "Server": -2},
        }),
    );
    let slots: Vec<Value> = (1..=s.slots)
        .map(|n| {
            let g = n % s.games;
            // Every `step`th location of the game is placed, so the presence set has gaps.
            let step = (s.game_locations / s.placed.max(1)).max(1);
            let locations: Vec<Value> = (0..s.placed.min(s.game_locations))
                .map(|k| {
                    let receiver = 1 + rng.below(s.slots);
                    let rg = receiver % s.games;
                    let flags = [0, 0, 0, 0, 1, 1, 2, 4][rng.below(8) as usize];
                    json!({
                        "location": id(g, (k * step).min(s.game_locations - 1)),
                        "item": id(rg, rng.below(s.game_items)),
                        "receiver": receiver,
                        "flags": flags,
                    })
                })
                .collect();
            json!({
                "id": n, "team": 0, "name": format!("Player{n}"), "game": game_name(g),
                "kind": "player",
                "minimum_client_version": {"major": 0, "minor": 5, "build": 0},
                "group_members": [], "locations": locations,
                "slot_data": slot_data(&mut rng), "start_inventory": [],
            })
        })
        .collect();
    let connect: Map<String, Value> =
        (1..=s.slots).map(|n| (format!("Player{n}"), json!([0, n]))).collect();
    json!({
        "format": 1,
        "seed_name": format!("synthetic-{}-{}", s.slots, s.seed),
        "generator_version": {"major": 0, "minor": 6, "build": 8},
        "server_options": {
            "password": null, "hint_cost": 10, "location_check_points": 1,
            "release_mode": "auto", "collect_mode": "auto", "remaining_mode": "goal",
        },
        "slots": slots,
        "connect_names": connect,
        "games": games,
        "race_mode": 0,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_shapes_are_valid_bundles_of_the_promised_size() {
        for s in [Shape::ONE_SLOT, Shape::EIGHT_SLOT] {
            let b = seed_bundle::SeedBundle::from_json(&bundle(&s)).unwrap();
            let placed: usize = b.slots.iter().map(|x| x.locations.len()).sum();
            assert_eq!(placed, (s.slots * s.placed) as usize);
            assert_eq!(b.games.len() as u32, s.games + 1);
        }
        let one = bundle(&Shape::ONE_SLOT);
        assert_eq!(one, bundle(&Shape::ONE_SLOT), "deterministic");
        // About the real world's 290 KB of compact JSON.
        assert!((200_000..400_000).contains(&one.len()), "{}", one.len());
    }
}
