"""Tests for ap_import.convert on a hand-built multidata mapping (no Archipelago needed).

Run: python -m unittest discover -s tools/ap-import
"""

import enum
import unittest
from typing import NamedTuple

import zlib

from ap_import import FORMAT, convert, inflate


class Kind(enum.IntEnum):
    PLAYER = 1
    GROUP = 2


class Slot(NamedTuple):
    name: str
    game: str
    type: int
    group_members: tuple = ()


def multidata():
    return {
        "seed_name": "123",
        "version": (0, 6, 8),
        "minimum_versions": {"server": (0, 5, 0), "clients": {1: (0, 1, 6)}},
        "slot_info": {2: Slot("Bob", "G", Kind.PLAYER), 1: Slot("Al", "G", Kind.PLAYER)},
        "connect_names": {"Al": (0, 1), "Bob": (0, 2)},
        "locations": {1: {11: (7, 2, 1), 10: (8, 1, 0)}, 2: {10: (7, 1, 4)}},
        "slot_data": {1: {"goals": ({"id": "x"},), "tags": {"b", "a"}, "kind": Kind.GROUP}},
        "datapackage": {
            "G": {"item_name_to_id": {"I7": 7, "I8": 8}, "location_name_to_id": {"L10": 10, "L11": 11},
                  "checksum": "c", "item_name_groups": {}},
            "Unplayed": {"item_name_to_id": {}, "location_name_to_id": {}, "checksum": "u"},
        },
        "server_options": {"password": None, "hint_cost": 5, "release_mode": "goal", "collect_mode": None},
    }


class Hint(NamedTuple):
    # The reference's Hint field order (NetUtils.py:393-401).
    receiving_player: int
    finding_player: int
    location: int
    item: int
    found: bool
    entrance: str = ""
    item_flags: int = 0
    status: int = 0


class ConvertTests(unittest.TestCase):
    def test_slots_come_out_in_slot_order_with_sorted_locations(self):
        b = convert(multidata())
        self.assertEqual(b["format"], FORMAT)
        self.assertEqual([s["id"] for s in b["slots"]], [1, 2])
        self.assertEqual(b["slots"][0]["locations"], [
            {"location": 10, "item": 8, "receiver": 1, "flags": 0},
            {"location": 11, "item": 7, "receiver": 2, "flags": 1},
        ])
        self.assertEqual(b["slots"][1]["locations"], [{"location": 10, "item": 7, "receiver": 1, "flags": 4}])

    def test_slot_data_sets_and_enums_become_json_values(self):
        data = convert(multidata())["slots"][0]["slot_data"]
        self.assertEqual(data, {"goals": [{"id": "x"}], "tags": ["a", "b"], "kind": 2})

    def test_a_slot_without_slot_data_or_version_gets_empty_defaults(self):
        bob = convert(multidata())["slots"][1]
        self.assertEqual(bob["slot_data"], {})
        self.assertEqual(bob["minimum_client_version"], {"major": 0, "minor": 0, "build": 0})
        self.assertEqual((bob["team"], bob["kind"]), (0, "player"))

    def test_only_played_games_are_kept_and_extra_package_keys_dropped(self):
        md = multidata()
        md["datapackage"]["G"]["extra"] = 1
        games = convert(md)["games"]
        self.assertEqual(list(games), ["G"])
        self.assertEqual(set(games["G"]), {"item_name_to_id", "location_name_to_id", "checksum",
                                           "item_name_groups", "location_name_groups", "hint_blacklist"})

    def test_name_groups_are_kept_and_missing_ones_are_empty(self):
        md = multidata()
        md["datapackage"]["G"]["item_name_groups"] = {"Both": ["I7", "I8"], "Set": {"I8"}}
        g = convert(md)["games"]["G"]
        self.assertEqual(g["item_name_groups"], {"Both": ["I7", "I8"], "Set": ["I8"]})
        self.assertEqual(g["location_name_groups"], {})

    def test_the_generic_archipelago_package_is_kept_although_no_slot_plays_it(self):
        md = multidata()
        md["datapackage"]["Archipelago"] = {"item_name_to_id": {"Nothing": -1},
                                            "location_name_to_id": {"Server": -2}, "checksum": "a"}
        self.assertEqual(sorted(convert(md)["games"]), ["Archipelago", "G"])

    def test_options_missing_or_null_fall_back_to_defaults(self):
        o = convert(multidata())["server_options"]
        self.assertEqual(o, {"password": None, "hint_cost": 5, "location_check_points": 1,
                             "release_mode": "goal", "collect_mode": "auto", "remaining_mode": "goal"})

    def test_a_played_game_without_a_data_package_is_an_error(self):
        md = multidata()
        del md["datapackage"]["G"]
        with self.assertRaisesRegex(ValueError, "no embedded data package"):
            convert(md)


    def test_starting_inventory_is_kept_per_slot_in_order(self):
        md = multidata()
        md["precollected_items"] = {2: [8, 7, 8]}
        slots = convert(md)["slots"]
        self.assertEqual([s["start_inventory"] for s in slots], [[], [8, 7, 8]])

    def test_group_slots_are_refused_until_supported(self):
        md = multidata()
        md["slot_info"][3] = Slot("Grp", "G", Kind.GROUP, (1, 2))
        with self.assertRaisesRegex(ValueError, "group slot"):
            convert(md)

    def test_start_hints_are_kept_once_per_location(self):
        md = multidata()
        # The generator stores each hint under the finder and the receiver (Main.py:281-291).
        h = Hint(2, 1, 11, 7, False, "", 1, 30)
        md["precollected_hints"] = {1: {h}, 2: {h, Hint(1, 2, 10, 7, False, "", 4, 0)}}
        self.assertEqual(convert(md)["start_hints"], [
            {"finding": 1, "location": 11, "status": 30},
            {"finding": 2, "location": 10, "status": 0},
        ])
        self.assertEqual(convert(multidata())["start_hints"], [])

    def test_entrances_and_spheres_are_kept_per_location(self):
        md = multidata()
        md["er_hint_data"] = {1: {11: "Cave"}}
        md["spheres"] = [{1: {10}}, {1: {11}, 2: {10}}]
        b = convert(md)
        self.assertEqual(b["slots"][0]["entrances"], {"11": "Cave"})
        self.assertEqual(b["slots"][1]["entrances"], {})
        self.assertEqual([p["sphere"] for p in b["slots"][0]["locations"]], [0, 1])
        self.assertEqual(b["slots"][1]["locations"][0]["sphere"], 1)
        self.assertNotIn("sphere", convert(multidata())["slots"][0]["locations"][0])

    def test_hint_blacklists_are_kept_per_played_game(self):
        b = convert(multidata(), {"G": frozenset({"I8", "I7"}), "Unplayed": {"x"}})
        self.assertEqual(b["games"]["G"]["hint_blacklist"], ["I7", "I8"])
        self.assertNotIn("Unplayed", b["games"])
        self.assertEqual(convert(multidata())["games"]["G"]["hint_blacklist"], [])

    def test_race_mode_is_carried(self):
        self.assertEqual(convert(multidata())["race_mode"], 0)
        md = multidata()
        md["race_mode"] = 1
        self.assertEqual(convert(md)["race_mode"], 1)


class InflateTests(unittest.TestCase):
    def test_a_stream_within_the_cap_inflates(self):
        raw = bytes([3]) + zlib.compress(b"x" * 1000)
        self.assertEqual(inflate(raw, 1000), b"x" * 1000)

    def test_a_stream_over_the_cap_is_refused_without_inflating_it_all(self):
        raw = bytes([3]) + zlib.compress(b"x" * 1001)
        with self.assertRaisesRegex(ValueError, "larger than 1000 bytes"):
            inflate(raw, 1000)

    def test_a_newer_container_or_garbage_is_refused(self):
        with self.assertRaisesRegex(ValueError, "newer"):
            inflate(bytes([4]) + zlib.compress(b"x"), 10)
        with self.assertRaises((ValueError, zlib.error)):
            inflate(b"", 10)
        with self.assertRaises(zlib.error):
            inflate(bytes([3]) + b"not zlib", 10)


if __name__ == "__main__":
    unittest.main()
