"""Generation sidecar: inspects uploads and generates a seed bundle with an Archipelago checkout.

The server runs every call inside its sandbox (crates/server/src/sandbox.rs). It prints one JSON
value to stdout; on failure it prints the reason to stderr and exits non-zero.

    ap_gen.py inspect-apworld <path>
    ap_gen.py inspect-yaml <path>
    ap_gen.py generate --ap <checkout> --apworlds <dir> --yamls <dir> --out <dir>

`inspect-*` read data only and never import or run a world's code. `generate` runs uploaded
world code, which is why the server sandboxes it.

How generation follows Archipelago 0.6.8 (files in its checkout, read, not copied):
- `worlds/__init__.py` loads `.apworld` files from `user_path("custom_worlds")`, which is the
  checkout's own `custom_worlds/` when the checkout is writable (`Utils.user_path`). The
  sandbox shows a writable throwaway overlay of the checkout with the job's apworlds mounted
  there, so `--apworlds` must be that folder. A world loads as `worlds.<file stem>`, so each
  file is named after the top-level folder inside it.
- `Generate.mystery_argparse` and `Generate.main` roll the player settings from the yamls in
  `--player_files_path`, then `Main.main` generates and writes `AP_<seed>.zip` (the
  `.archipelago` multidata plus any patch files) to `--outputpath`; `WebHostLib/generate.py`
  (`gen_game`) runs the same two steps. Spoilers are off (`--spoiler 0`). Patch files cannot
  be skipped: `--skip_output` also skips the multidata (`Main.py`), so they are written and
  ignored; only the multidata reaches `ap_import`.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import stat
import sys
import time
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "ap-import"))
import ap_import  # noqa: E402  (needs the sys.path entry above; the sidecar runs with -I)

# Design values, not measurements: big enough for large real worlds (a 2 MB world unpacks to
# about 2 MB; the largest bundled worlds are tens of MB), small enough to refuse a zip bomb.
MAX_APWORLD_UNPACKED = 256 * 1024 * 1024
MAX_APWORLD_ENTRIES = 20_000
MAX_MANIFEST_BYTES = 64 * 1024
# A world's module name: what `worlds.<name>` imports, so a Python identifier.
MODULE_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")


class Refused(Exception):
    """Input we will not accept; the message is shown to the uploader."""


def inspect_apworld(path: Path) -> dict[str, Any]:
    """Validate an apworld zip and read its manifest without running any of its code."""
    try:
        zf = zipfile.ZipFile(path)
    except zipfile.BadZipFile as e:
        raise Refused(f"not a zip file: {e}") from e
    with zf:
        infos = zf.infolist()
        if not infos:
            raise Refused("empty zip")
        if len(infos) > MAX_APWORLD_ENTRIES:
            raise Refused(f"more than {MAX_APWORLD_ENTRIES} entries")
        total = 0
        tops = set()
        manifest_info = None
        for info in infos:
            name = info.filename
            parts = PurePosixPath(name).parts
            if name.startswith("/") or "\\" in name or ".." in parts or re.match(r"[A-Za-z]:", name):
                raise Refused(f"unsafe path in zip: {name!r}")
            if stat.S_ISLNK(info.external_attr >> 16):
                raise Refused(f"symlink in zip: {name!r}")
            total += info.file_size
            if total > MAX_APWORLD_UNPACKED:
                raise Refused(f"larger than {MAX_APWORLD_UNPACKED} bytes unpacked")
            if len(parts) > 1 or info.is_dir():
                tops.add(parts[0])
            # The manifest Archipelago reads (worlds/Files.py, APContainer.read_contents).
            if parts[-1] == "archipelago.json" and len(parts) <= 2 and manifest_info is None:
                manifest_info = info
        if len(tops) != 1:
            raise Refused(f"want exactly one top-level folder, found {sorted(tops)}")
        module = tops.pop()
        if not MODULE_NAME.match(module):
            raise Refused(f"top-level folder {module!r} is not a Python module name")
        manifest: dict[str, Any] = {}
        if manifest_info is not None:
            if manifest_info.file_size > MAX_MANIFEST_BYTES:
                raise Refused("archipelago.json is too large")
            try:
                manifest = json.loads(zf.read(manifest_info))
            except ValueError as e:
                raise Refused(f"archipelago.json is not JSON: {e}") from e
            if not isinstance(manifest, dict):
                raise Refused("archipelago.json is not an object")
    game = manifest.get("game")
    return {
        "module": module,
        "game": game if isinstance(game, str) else None,
        "world_version": _str_or_none(manifest.get("world_version")),
        "minimum_ap_version": _str_or_none(manifest.get("minimum_ap_version")),
        "maximum_ap_version": _str_or_none(manifest.get("maximum_ap_version")),
        "authors": [a for a in manifest.get("authors", []) if isinstance(a, str)]
        if isinstance(manifest.get("authors"), list) else [],
    }


def _str_or_none(v: Any) -> str | None:
    return v if isinstance(v, str) else None


def inspect_yaml(path: Path) -> list[dict[str, str]]:
    """Name and game of each player document, as `Generate.main` would read them."""
    import yaml

    try:
        docs = list(yaml.safe_load_all(path.read_text(encoding="utf-8-sig")))
    except (yaml.YAMLError, UnicodeDecodeError) as e:
        raise Refused(f"not valid yaml: {e}") from e
    players = []
    for i, doc in enumerate(docs, 1):
        if doc is None:  # Generate.main skips empty documents too
            continue
        if not isinstance(doc, dict):
            raise Refused(f"document {i} is not a mapping")
        name, game = doc.get("name"), doc.get("game")
        if not isinstance(name, str) or not name.strip():
            raise Refused(f"document {i} has no name")
        # `game` may be a weighted mapping of several games; Archipelago rolls one.
        if isinstance(game, dict) and game and all(isinstance(g, str) for g in game):
            game = ", ".join(sorted(game))
        if not isinstance(game, str) or not game.strip():
            raise Refused(f"document {i} has no game")
        players.append({"name": name, "game": game})
    if not players:
        raise Refused("no player documents")
    return players


def generate(ap: Path, apworlds: Path, yamls: Path, out: Path) -> dict[str, Any]:
    """Generate a multiworld from the yamls and write `<out>/bundle.json`."""
    if not (ap / "custom_worlds").exists() or not os.path.samefile(apworlds, ap / "custom_worlds"):
        raise Refused("--apworlds must be the checkout's custom_worlds folder (the sandbox mounts it)")
    out.mkdir(parents=True, exist_ok=True)
    start = time.monotonic()
    os.chdir(ap)
    sys.path.insert(0, str(ap))
    # Archipelago logs and prints to stdout; ours carries only the JSON result.
    with contextlib.redirect_stdout(sys.stderr):
        import logging

        logging.basicConfig(stream=sys.stderr, level=logging.INFO)
        import ModuleUpdate

        # Archipelago's dependency check pip-installs what is missing; the sandbox has no
        # network and the venv is prepared beforehand (as in tools/ap-client-check).
        ModuleUpdate.update_ran = True
        import Generate
        import Main

        args = Generate.mystery_argparse([
            "--player_files_path", str(yamls),
            "--outputpath", str(out),
            "--spoiler", "0",
        ])
        erargs, seed = Generate.main(args)
        Main.main(erargs, seed)
    zips = sorted(out.glob("AP_*.zip"))
    if len(zips) != 1:
        raise Refused(f"expected one AP_*.zip in the output, found {len(zips)}")
    bundle = ap_import.convert(ap_import.load_multidata(zips[0]))
    (out / "bundle.json").write_text(json.dumps(bundle, sort_keys=True, indent=1) + "\n")
    return {
        "bundle": "bundle.json",
        "seed_name": bundle["seed_name"],
        "slots": len(bundle["slots"]),
        "locations": sum(len(s["locations"]) for s in bundle["slots"]),
        "seconds": round(time.monotonic() - start, 1),
    }


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="ap_gen.py")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("inspect-apworld").add_argument("path", type=Path)
    sub.add_parser("inspect-yaml").add_argument("path", type=Path)
    g = sub.add_parser("generate")
    for flag in ("--ap", "--apworlds", "--yamls", "--out"):
        g.add_argument(flag, type=Path, required=True)
    a = p.parse_args(argv)
    try:
        if a.cmd == "inspect-apworld":
            result: Any = inspect_apworld(a.path)
        elif a.cmd == "inspect-yaml":
            result = inspect_yaml(a.path)
        else:
            result = generate(a.ap, a.apworlds, a.yamls, a.out)
    except (Refused, OSError) as e:
        print(f"refused: {e}", file=sys.stderr)
        return 1
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
