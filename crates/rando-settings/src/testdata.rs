//! Test worlds: an invented "Lanternfall" schema with every option kind of doc 20 section 5.1,
//! and the build tool's golden `cinderfall` schema (PR 231).

/// The golden schema the build tool writes for its `cinderfall` fixture.
pub const CINDERFALL: &str =
    include_str!("../../world-build/tests/golden/cinderfall.settings.json");

/// Every kind, groups, doors, a shuffle, an offered common key and a preset.
pub const LANTERNFALL: &str = r#"{
  "format": "schema/1", "world": "lanternfall", "version": "0.1.3",
  "options": [
    {"key": "goal", "kind": "choice", "values": ["find_the_crown", "light_every_lamp"], "default": "find_the_crown", "scope": "seed", "label": "opt.goal.label"},
    {"key": "traps", "kind": "choice", "values": ["none", "a_few", "lots"], "default": "none", "scope": "seed", "label": "opt.traps.label"},
    {"key": "logic_level", "kind": "choice", "values": ["normal", "expert"], "default": "normal", "scope": "seed"},
    {"key": "key_shuffle", "kind": "choice", "values": ["original", "own_area", "own_game", "anywhere"], "default": "original", "scope": "seed"},
    {"key": "door_shuffle", "kind": "choice", "values": ["off", "some", "all"], "default": "off", "scope": "seed"},
    {"key": "item_pool", "kind": "choice", "values": ["scarce", "standard", "plentiful"], "default": "standard", "scope": "seed"},
    {"key": "lights", "kind": "choice", "values": ["on", "off"], "default": "on", "scope": "seed"},
    {"key": "shuffle_mine", "kind": "toggle", "default": false, "scope": "seed", "label": "opt.shuffle_mine.label"},
    {"key": "lamp_color", "kind": "choice", "values": ["teal", "amber"], "default": "amber", "scope": "cosmetic"},
    {"key": "music", "kind": "choice", "values": ["calm", "epic"], "default": "calm", "scope": "cosmetic"},
    {"key": "bombs", "kind": "range", "min": 0, "max": 99, "named": {"normal": 50, "plenty": 99, "unlimited": -1}, "default": 50, "scope": "seed"},
    {"key": "hint_count", "kind": "range", "min": 0, "max": 20, "default": 5, "scope": "knowledge"},
    {"key": "shops", "kind": "set", "of": ["bakery", "tailor", "smithy"], "count": [1, 3], "default": ["bakery"], "scope": "seed", "label": "opt.shops.label"},
    {"key": "trap_mix", "kind": "counts", "of": ["slow", "ice"], "min": 0, "max": 100, "default": {"slow": 50, "ice": 50}, "scope": "seed"},
    {"key": "extra_tools", "kind": "set", "of": "items", "count": [0, 2], "default": [], "scope": "seed", "label": "opt.extra_tools.label"},
    {"key": "greeting", "kind": "text", "max_len": 20, "default": "", "scope": "cosmetic"},
    {"key": "title", "kind": "choice", "values": ["hero", "rogue"], "or_text": true, "max_len": 16, "default": "hero", "scope": "seed"}
  ],
  "items": [
    {"name": "arrow", "copies": 20}, {"name": "bomb_bag", "copies": 1}, {"name": "boots", "copies": 1},
    {"name": "coin", "filler": true}, {"name": "crown_piece", "copies": 3}, {"name": "lamp_oil", "copies": 2},
    {"name": "lantern", "copies": 1}, {"name": "pickaxe", "copies": 1}, {"name": "potion", "copies": 3},
    {"name": "rope", "copies": 1}
  ],
  "places": [
    {"name": "cave_chest"}, {"name": "mine_cart"}, {"name": "p1"}, {"name": "p2"}, {"name": "p3"},
    {"name": "p4"}, {"name": "starter_chest"}, {"name": "tower_chest"}, {"name": "vault_altar"},
    {"name": "well_bucket"}
  ],
  "item_groups": [{"name": "tools", "members": ["boots", "pickaxe", "rope"]}],
  "place_groups": [{"name": "cave_places", "members": ["cave_chest", "mine_cart"]}],
  "doors": [{"name": "cave_mouth"}, {"name": "mill_door"}, {"name": "well_door"}],
  "shuffles": [{"name": "bosses", "things": ["ash_wyrm", "stone_golem"], "slots": ["mill_arena", "tower_arena"]}],
  "offers": ["shared_deaths"],
  "presets": [{"name": "relaxed"}, {"name": "standard"}],
  "strings": {
    "opt.goal.label": {"en": "Goal"}, "opt.traps.label": {"en": "Traps"},
    "opt.shuffle_mine.label": {"en": "Shuffle the mine"}, "opt.shops.label": {"en": "Shops"},
    "opt.extra_tools.label": {"en": "Extra tools"}
  }
}"#;
