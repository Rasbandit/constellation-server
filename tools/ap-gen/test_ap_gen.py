"""Tests for the inspect half of ap_gen on zips and yamls built here (no Archipelago needed).

Run: python -m pytest tools/ap-gen/test_ap_gen.py
"""

import json
import stat
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

import ap_gen
from ap_gen import Refused, inspect_yaml

UNPACKED, ENTRIES = 1 << 20, 100


def inspect_apworld(path, max_unpacked=UNPACKED, max_entries=ENTRIES):
    return ap_gen.inspect_apworld(path, max_unpacked, max_entries)


def build_zip(path: Path, entries: dict[str, bytes], symlinks: tuple[str, ...] = ()) -> Path:
    with zipfile.ZipFile(path, "w") as zf:
        for name, data in entries.items():
            zf.writestr(name, data)
        for name in symlinks:
            info = zipfile.ZipInfo(name)
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            zf.writestr(info, "/etc/passwd")
    return path


class InspectApworld(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())

    def zip(self, entries, symlinks=()):
        return build_zip(self.dir / "w.apworld", entries, symlinks)

    def test_a_good_apworld_reports_its_manifest_and_module(self):
        manifest = {"game": "Clique", "world_version": "1.2.3", "minimum_ap_version": "0.6.4",
                    "authors": ["a", 7], "compatible_version": 7, "version": 7}
        got = inspect_apworld(self.zip({
            "clique/__init__.py": b"raise SystemExit('inspect must never run world code')",
            "clique/archipelago.json": json.dumps(manifest).encode(),
        }))
        self.assertEqual(got, {"module": "clique", "game": "Clique", "world_version": "1.2.3",
                               "minimum_ap_version": "0.6.4", "maximum_ap_version": None,
                               "authors": ["a"]})

    def test_without_a_manifest_the_module_is_the_top_level_folder(self):
        got = inspect_apworld(self.zip({"old_world/__init__.py": b""}))
        self.assertEqual((got["module"], got["game"], got["world_version"]), ("old_world", None, None))

    def test_path_traversal_and_absolute_paths_are_refused(self):
        for bad in ("w/../../etc/x", "/etc/passwd", "w/..\\..\\x", "C:/x"):
            with self.subTest(bad=bad):
                with self.assertRaisesRegex(Refused, "unsafe path"):
                    inspect_apworld(self.zip({"w/__init__.py": b"", bad: b"x"}))

    def test_a_symlink_entry_is_refused(self):
        with self.assertRaisesRegex(Refused, "symlink"):
            inspect_apworld(self.zip({"w/__init__.py": b""}, symlinks=("w/link",)))

    def test_over_the_unpacked_size_cap_is_refused_and_at_it_is_accepted(self):
        inspect_apworld(self.zip({"w/__init__.py": b"x" * 100}), max_unpacked=100)
        with self.assertRaisesRegex(Refused, "larger than 100"):
            inspect_apworld(self.zip({"w/__init__.py": b"x" * 101}), max_unpacked=100)

    def test_too_many_entries_is_refused(self):
        inspect_apworld(self.zip({f"w/{i}.py": b"" for i in range(2)}), max_entries=2)
        with self.assertRaisesRegex(Refused, "more than 2 entries"):
            inspect_apworld(self.zip({f"w/{i}.py": b"" for i in range(3)}), max_entries=2)

    def test_empty_two_top_folders_bad_module_and_not_a_zip_are_refused(self):
        with self.assertRaisesRegex(Refused, "empty zip"):
            inspect_apworld(self.zip({}))
        with self.assertRaisesRegex(Refused, "exactly one top-level"):
            inspect_apworld(self.zip({"a/__init__.py": b"", "b/__init__.py": b""}))
        with self.assertRaisesRegex(Refused, "exactly one top-level"):
            inspect_apworld(self.zip({"loose.py": b""}))
        with self.assertRaisesRegex(Refused, "not a Python module"):
            inspect_apworld(self.zip({"my-world/__init__.py": b""}))
        junk = self.dir / "junk.apworld"
        junk.write_bytes(b"not a zip")
        with self.assertRaisesRegex(Refused, "not a zip"):
            inspect_apworld(junk)

    def test_a_manifest_that_is_not_a_json_object_is_refused(self):
        for text in (b"{nope", b"[1]"):
            with self.subTest(text=text):
                with self.assertRaises(Refused):
                    inspect_apworld(self.zip({"w/__init__.py": b"", "w/archipelago.json": text}))


class InspectYaml(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())

    def yaml(self, text: str) -> Path:
        p = self.dir / "p.yaml"
        p.write_text(text)
        return p

    def test_a_good_yaml_names_its_player_and_game(self):
        got = inspect_yaml(self.yaml("name: Al\ngame: Clique\nClique:\n  hard_mode: true\n"))
        self.assertEqual(got, [{"name": "Al", "game": "Clique"}])

    def test_every_document_of_a_multi_document_yaml_is_a_player(self):
        got = inspect_yaml(self.yaml("name: Al\ngame: A\n---\n---\nname: Bo\ngame: {B: 1, C: 2}\n"))
        self.assertEqual(got, [{"name": "Al", "game": "A"}, {"name": "Bo", "game": "B, C"}])

    def test_a_document_without_a_name_or_game_is_refused(self):
        for text, why in (("game: A\n", "no name"), ("name: Al\n", "no game"),
                          ("name: ''\ngame: A\n", "no name"), ("- 1\n", "not a mapping"),
                          ("", "no player documents"), ("name: [\n", "not valid yaml"),
                          ("name: Al\ngame: A\n---\nname: Bo\n", "document 2 has no game")):
            with self.subTest(text=text):
                with self.assertRaisesRegex(Refused, why):
                    inspect_yaml(self.yaml(text))

    def test_python_tags_are_refused_not_run(self):
        with self.assertRaisesRegex(Refused, "not valid yaml"):
            inspect_yaml(self.yaml("name: !!python/object/apply:os.system ['true']\ngame: A\n"))


class Cli(unittest.TestCase):
    def test_a_refusal_exits_1(self):
        self.assertEqual(ap_gen.main(["inspect-apworld", "/nonexistent/x.apworld",
                                      "--max-unpacked", "1", "--max-entries", "1"]), 1)


class DefaultSettings(unittest.TestCase):
    def test_settings_come_from_memory_and_no_host_yaml_is_written(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "settings.py").write_text(
                "skip_autosave = False\n"
                "class Settings:\n"
                "    def __init__(self, location): self.location = location\n"
                "    def save(self, *a): raise OSError(30, 'Read-only file system')\n"
                "def get_settings():\n"
                "    c = getattr(get_settings, '_cache', None)\n"
                "    if c is None: Settings(None).save('host.yaml')\n"
                "    return c\n")
            sys.path.insert(0, d)
            try:
                import settings
                ap_gen.use_default_settings()
                self.assertIsNone(settings.get_settings().location)
                self.assertTrue(settings.skip_autosave)
            finally:
                sys.path.remove(d)
                sys.modules.pop("settings", None)


if __name__ == "__main__":
    unittest.main()
