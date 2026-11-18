"""Export a legacy Python world's static seed-setup tables into a manifest section, once per world
version, so a port can run seed setup from data with no Python per seed.

Imports the world module (nothing is generated) and writes the tables a spec file names. See
docs/port-check.md, "Exporting seed-setup tables".

Usage: python -I setup_tables.py --module <dotted world module> --spec <spec.json>
         --manifest <in.json> --out <out.json> [--path <dir>]... [--prelude <file.py>]
         [--section <key>]
"""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import enum
import importlib
import importlib.util
import json
import sys


def to_json(v, at: str = "value"):
    """A value as JSON: enum members become their lowercased member name, sets sorted lists,
    tuples and named tuples lists, dataclasses objects. Anything else is refused."""
    if isinstance(v, enum.Enum):
        return v.name.lower()
    if v is None or isinstance(v, (bool, int, float, str)):
        return v
    if isinstance(v, dict):
        return {str(to_json(k, at)): to_json(x, f"{at}.{k}") for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [to_json(x, f"{at}[{i}]") for i, x in enumerate(v)]
    if isinstance(v, (set, frozenset)):
        return sorted((to_json(x, at) for x in v), key=lambda x: json.dumps(x, sort_keys=True))
    if dataclasses.is_dataclass(v) and not isinstance(v, type):
        return to_json({f.name: getattr(v, f.name) for f in dataclasses.fields(v)}, at)
    raise TypeError(f"at {at}: {type(v).__name__} has no JSON form")


def resolve(world, path: str):
    """`a.b.c` from the world module: submodules are imported on demand, dict keys work too."""
    obj = world
    for seg in path.split("."):
        if isinstance(obj, dict) and seg in obj:
            obj = obj[seg]
        elif hasattr(obj, seg):
            obj = getattr(obj, seg)
        elif hasattr(obj, "__path__"):
            try:
                obj = importlib.import_module(f"{obj.__name__}.{seg}")
            except ModuleNotFoundError as e:
                raise SystemExit(f"{path}: {e}") from e
        else:
            raise SystemExit(f"{path}: no {seg!r}")
    return obj


def options(cls) -> list[dict]:
    """The option declarations of a world class (`options_dataclass.type_hints`), by shape:
    a range has `range_start`/`range_end`, a toggle's names are false/true, a choice has names."""
    out = []
    for key, o in cls.options_dataclass.type_hints.items():
        default = getattr(o, "default", None)
        row = {"key": key, "default": int(default) if isinstance(default, (bool, int)) else None}
        names = dict(getattr(o, "options", None) or {})
        if hasattr(o, "range_start") and hasattr(o, "range_end"):
            row["kind"] = "range"
            row["range"] = [o.range_start, o.range_end]
            row["names"] = dict(getattr(o, "special_range_names", None) or {})
        elif names == {"false": 0, "true": 1}:
            row["kind"] = "toggle"
        elif names:
            row["kind"] = "choice"
            names.update(getattr(o, "aliases", None) or {})
            row["names"] = names
        else:
            row["kind"] = "other"
        out.append(row)
    return out


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--module", required=True, help="dotted name of the world module")
    p.add_argument("--spec", required=True, help='{"tables": {key: path}, "options": path}')
    p.add_argument("--manifest", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--path", action="append", default=[], help="put on sys.path first")
    p.add_argument("--prelude", help="run before the import; may define extra(world) -> dict")
    p.add_argument("--section", default="x.setup")
    a = p.parse_args(argv)
    with open(a.spec) as f:
        spec = json.load(f)
    with open(a.manifest) as f:
        manifest = json.load(f)
    sys.path[:0] = a.path
    extra = None
    # Legacy code may print on import; keep stdout for the summary line only.
    with contextlib.redirect_stdout(sys.stderr):
        if a.prelude:
            ps = importlib.util.spec_from_file_location("setup_prelude", a.prelude)
            if ps is None or ps.loader is None:
                raise SystemExit(f"cannot load prelude {a.prelude}")
            pm = importlib.util.module_from_spec(ps)
            ps.loader.exec_module(pm)
            extra = getattr(pm, "extra", None)
        world = importlib.import_module(a.module)
        section = {k: to_json(resolve(world, path), k) for k, path in spec.get("tables", {}).items()}
        if "options" in spec:
            section["options"] = options(resolve(world, spec["options"]))
        if extra is not None:
            section.update(to_json(extra(world), "extra"))
    manifest[a.section] = section
    with open(a.out, "w") as f:
        json.dump(manifest, f, sort_keys=True)
    print(json.dumps({k: len(v) if hasattr(v, "__len__") else v for k, v in section.items()}))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
