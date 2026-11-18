"""Tests for setup_tables on a throwaway world package written to a temp dir.

Run: python -m unittest discover -s tools/port-check
"""

import json
import os
import sys
import tempfile
import textwrap
import unittest

from setup_tables import main, to_json

WORLD = {
    "__init__.py": """
        from . import opts

        class Game:
            options_dataclass = opts.Options
    """,
    "opts.py": """
        class Toggle:
            options = {"false": 0, "true": 1}
            default = 0

        class Speed:
            options = {"slow": 0, "fast": 1}
            aliases = {"quick": 1}
            default = 1

        class Count:
            range_start = 1
            range_end = 30
            special_range_names = {"all": 30}
            default = True

        class Odd:
            default = "x"

        class Options:
            type_hints = {"hard": Toggle, "speed": Speed, "count": Count, "odd": Odd}
    """,
    "tables.py": """
        import enum
        from typing import NamedTuple

        class Loc(enum.Enum):
            NORTH_GATE = "North Gate"
            SOUTH_GATE = "South Gate"

        class Row(NamedTuple):
            scene: str
            id: int

        rows = {Loc.SOUTH_GATE: Row("s", 2), Loc.NORTH_GATE: Row("n", 1)}
        never = {"b", "a"}
        order = [Loc.SOUTH_GATE, Loc.NORTH_GATE]
        nested = {"inner": {"deep": (1, 2)}}
    """,
}


class SetupTables(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = self.tmp.name
        os.makedirs(os.path.join(root, "game_pkg"))
        for name, body in WORLD.items():
            with open(os.path.join(root, "game_pkg", name), "w") as f:
                f.write(textwrap.dedent(body))
        self.root = root
        self.spec = os.path.join(root, "spec.json")
        self.src = os.path.join(root, "in.json")
        self.dst = os.path.join(root, "out.json")
        with open(self.src, "w") as f:
            json.dump({"items": [], "x.other": 1}, f)

    def tearDown(self):
        for k in [k for k in sys.modules if k.split(".")[0] in ("game_pkg", "setup_prelude")]:
            del sys.modules[k]
        self.tmp.cleanup()

    def run_tool(self, spec, *extra):
        with open(self.spec, "w") as f:
            json.dump(spec, f)
        args = ["--path", self.root, "--module", "game_pkg", "--spec", self.spec,
                "--manifest", self.src, "--out", self.dst, *extra]
        self.assertEqual(main(args), 0)
        with open(self.dst) as f:
            return json.load(f)

    def test_tables_resolve_relative_to_the_world_module_and_convert(self):
        out = self.run_tool({"tables": {
            "rows": "tables.rows", "never": "tables.never", "order": "tables.order",
            "deep": "tables.nested.inner.deep"}})
        self.assertEqual(out["x.other"], 1, "the rest of the manifest is kept")
        self.assertEqual(out["x.setup"], {
            "rows": {"north_gate": ["n", 1], "south_gate": ["s", 2]},
            "never": ["a", "b"],
            "order": ["south_gate", "north_gate"],
            "deep": [1, 2],
        })

    def test_options_are_read_from_the_class_that_declares_them(self):
        out = self.run_tool({"options": "Game"})
        self.assertEqual(out["x.setup"]["options"], [
            {"key": "hard", "kind": "toggle", "default": 0},
            {"key": "speed", "kind": "choice", "default": 1,
             "names": {"slow": 0, "fast": 1, "quick": 1}},
            {"key": "count", "kind": "range", "default": 1, "range": [1, 30],
             "names": {"all": 30}},
            {"key": "odd", "kind": "other", "default": None},
        ])

    def test_a_prelude_runs_first_and_can_add_computed_tables(self):
        prelude = os.path.join(self.root, "prelude.py")
        with open(prelude, "w") as f:
            f.write(textwrap.dedent("""
                import sys
                sys.ran_prelude = True

                def extra(world):
                    return {"gates": len(world.tables.Loc)}
            """))
        out = self.run_tool({"tables": {"order": "tables.order"}}, "--prelude", prelude,
                            "--section", "x.tables")
        self.assertTrue(getattr(sys, "ran_prelude", False))
        self.assertEqual(out["x.tables"], {"order": ["south_gate", "north_gate"], "gates": 2})

    def test_a_missing_attribute_names_its_path(self):
        with self.assertRaisesRegex(SystemExit, "tables.nope"):
            self.run_tool({"tables": {"x": "tables.nope"}})

    def test_values_json_cannot_hold_are_refused(self):
        with self.assertRaisesRegex(TypeError, "at t: object"):
            to_json(object(), "t")


if __name__ == "__main__":
    unittest.main()
