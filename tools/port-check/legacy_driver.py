"""Legacy side of `port-check`: serves one legacy Python world over JSON lines on stdio.

The world-specific part is an adapter file you write (see docs/port-check.md). This driver loads
it, reads one request per line on stdin and writes one response per line on stdout. Anything
the legacy code prints goes to stderr, so it cannot corrupt the protocol.

Usage: python -I legacy_driver.py <adapter.py> [adapter args...]

The adapter file defines `make(args: list[str]) -> adapter`, where the adapter has:
  hello() -> dict
  instance(seed: int) -> dict
  solve(seed: int, inventory: list[str]) -> dict
  generate(seed: int) -> dict
  spheres(seed: int, placements: dict[str, str]) -> dict

A room of several players adds (see docs/port-check.md, "Several players"):
  hello() also reports "players"; instance and solve take a `player` (1 based);
  room_generate(seed: int) -> dict
  room_spheres(seed: int, placements: dict[str, dict[str, dict]]) -> dict
"""

from __future__ import annotations

import contextlib
import importlib.util
import json
import sys
import traceback

OPS = ("hello", "instance", "solve", "generate", "spheres", "room_generate", "room_spheres")


def load(path: str, args: list[str]):
    spec = importlib.util.spec_from_file_location("legacy_adapter", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load adapter {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.make(args)


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    out = sys.stdout
    with contextlib.redirect_stdout(sys.stderr):
        adapter = load(argv[1], argv[2:])
        for line in sys.stdin:
            if not line.strip():
                continue
            try:
                req = json.loads(line)
                op = req.get("op")
                if op not in OPS:
                    raise ValueError(f"unknown op {op!r}")
                kwargs = {k: v for k, v in req.items() if k != "op"}
                resp = {"ok": True, "result": getattr(adapter, op)(**kwargs)}
            except Exception as e:  # reported to the caller, which stops with this message
                traceback.print_exc(file=sys.stderr)
                resp = {"ok": False, "error": f"{type(e).__name__}: {e}"}
            out.write(json.dumps(resp, sort_keys=True) + "\n")
            out.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
