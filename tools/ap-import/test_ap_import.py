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
        games = convert(multidata())["games"]
        self.assertEqual(list(games), ["G"])
        self.assertEqual(set(games["G"]), {"item_name_to_id", "location_name_to_id", "checksum"})

    def test_options_missing_or_null_fall_back_to_defaults(self):
        o = convert(multidata())["server_options"]
        self.assertEqual(o, {"password": None, "hint_cost": 5, "location_check_points": 1,
                             "release_mode": "goal", "collect_mode": "auto", "remaining_mode": "goal"})

    def test_a_played_game_without_a_data_package_is_an_error(self):
        md = multidata()
        del md["datapackage"]["G"]
        with self.assertRaisesRegex(ValueError, "no embedded data package"):
            convert(md)


    def test_starting_inventory_is_refused_until_supported(self):
        md = multidata()
        md["precollected_items"] = {1: [], 2: [8]}
        with self.assertRaisesRegex(ValueError, "starting inventory"):
            convert(md)
        md["precollected_items"] = {1: [], 2: []}
        convert(md)  # empty lists are fine

    def test_group_slots_are_refused_until_supported(self):
        md = multidata()
        md["slot_info"][3] = Slot("Grp", "G", Kind.GROUP, (1, 2))
        with self.assertRaisesRegex(ValueError, "group slot"):
            convert(md)

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
