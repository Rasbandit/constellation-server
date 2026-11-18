"""Convert a seed made by Archipelago's generator into our JSON seed bundle.

Usage (with Archipelago importable, for example its checkout on PYTHONPATH):

    python ap_import.py <seed.archipelago | seed.zip> <bundle.json>

Reading the `.archipelago` file needs Archipelago's own loader, because it is a compressed
pickle; `load_multidata` calls it at runtime. Everything else here is plain data shaping, so
`convert` takes the decoded mapping and is tested without Archipelago (test_ap_import.py).

The bundle schema is owned by the Rust crate `seed-bundle` (crates/seed-bundle/src/lib.rs);
keep the two in step, and bump FORMAT on any incompatible change.
"""

from __future__ import annotations

import enum
import json
import sys
import zipfile
import zlib
from pathlib import Path
from typing import Any

FORMAT = 1

# Archipelago's slot type values (docs/network protocol.md, SlotType). Groups (item links) are
# refused until the server supports them.
SLOT_KINDS = {0: "spectator", 1: "player"}

# Largest decompressed multidata we will unpickle: a guard against a compression bomb.
MAX_DECOMPRESSED = 512 * 1024 * 1024

# Options the Rust side reads, with the value used when the seed does not record one.
OPTION_DEFAULTS = {
    "password": None,
    "hint_cost": 10,
    "location_check_points": 1,
    "release_mode": "auto",
    "collect_mode": "auto",
    "remaining_mode": "goal",
}


def _version(v: Any) -> dict[str, int]:
    major, minor, build = (int(x) for x in tuple(v)[:3])
    return {"major": major, "minor": minor, "build": build}


def _plain(value: Any) -> Any:
    """Turn sets, enums and other non-JSON values from slot_data into JSON values."""
    if isinstance(value, enum.Enum):
        return _plain(value.value)
    if isinstance(value, dict):
        return {str(k): _plain(v) for k, v in value.items()}
    if isinstance(value, (set, frozenset)):
        return sorted((_plain(v) for v in value), key=repr)
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    return value


def convert(multidata: dict[str, Any]) -> dict[str, Any]:
    """Shape a decoded multidata mapping into a seed bundle (a JSON-ready dict)."""
    connect_names = {str(n): [int(t), int(s)] for n, (t, s) in multidata["connect_names"].items()}
    team_of = {slot: team for team, slot in connect_names.values()}
    client_min = multidata.get("minimum_versions", {}).get("clients", {})
    slot_data = multidata.get("slot_data", {})
    locations = multidata["locations"]
    # Item ids only, in the generator's order (Main.py, the `precollected_items` entry).
    start = multidata.get("precollected_items", {})

    slots = []
    for slot_id in sorted(multidata["slot_info"]):
        info = multidata["slot_info"][slot_id]
        if int(info.type) not in SLOT_KINDS:
            raise ValueError(f"slot {slot_id} is a group slot (item link), which is not supported yet")
        placed = [
            {"location": int(loc), "item": int(item), "receiver": int(receiver), "flags": int(flags)}
            for loc, (item, receiver, flags) in sorted(locations.get(slot_id, {}).items())
        ]
        slots.append({
            "id": int(slot_id),
            "team": team_of.get(int(slot_id), 0),
            "name": str(info.name),
            "game": str(info.game),
            "kind": SLOT_KINDS[int(info.type)],
            "minimum_client_version": _version(client_min.get(slot_id, (0, 0, 0))),
            "group_members": [int(m) for m in info.group_members],
            "locations": placed,
            "slot_data": _plain(slot_data.get(slot_id, {})),
            "start_inventory": [int(i) for i in start.get(slot_id, [])],
        })

    played = {s["game"] for s in slots}
    games = {}
    for name, package in multidata.get("datapackage", {}).items():
        # The generic `Archipelago` package names location -2 and player 0 of start items; the
        # generator embeds it in every seed (Main.py, the embedded data package).
        if name not in played and name != "Archipelago":
            continue
        games[name] = {
            "item_name_to_id": {str(k): int(v) for k, v in package["item_name_to_id"].items()},
            "location_name_to_id": {str(k): int(v) for k, v in package["location_name_to_id"].items()},
            "checksum": str(package["checksum"]),
            "item_name_groups": _plain(package.get("item_name_groups", {})),
            "location_name_groups": _plain(package.get("location_name_groups", {})),
        }
    missing = played - games.keys()
    if missing:
        raise ValueError(f"no embedded data package for {sorted(missing)}")

    seed_options = multidata.get("server_options", {})
    options = {k: d if seed_options.get(k) is None else seed_options[k]
               for k, d in OPTION_DEFAULTS.items()}

    return {
        "format": FORMAT,
        "seed_name": str(multidata["seed_name"]),
        "generator_version": _version(multidata["version"]),
        "server_options": options,
        "slots": slots,
        "connect_names": connect_names,
        "games": games,
        "race_mode": int(multidata.get("race_mode", 0)),
    }


def inflate(raw: bytes, limit: int = MAX_DECOMPRESSED) -> bytes:
    """Unwrap the container: one format byte (3 or older), then a zlib stream, at most `limit`
    bytes once inflated. Format as read by the reference server's loader (MultiServer.py)."""
    if not raw:
        raise ValueError("empty multidata file")
    if raw[0] > 3:
        raise ValueError(f"multidata format {raw[0]} is newer than this importer knows")
    d = zlib.decompressobj()
    out = d.decompress(raw[1:], limit)
    if d.unconsumed_tail or len(d.decompress(d.unconsumed_tail, 1)) > 0:
        raise ValueError(f"multidata is larger than {limit} bytes once decompressed")
    return out


def load_multidata(path: Path) -> dict[str, Any]:
    """Read a `.archipelago` file (or the generator's zip) with Archipelago's own loader."""
    from Utils import restricted_loads  # Archipelago, at runtime only

    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as zf:
            name = next(n for n in zf.namelist() if n.endswith(".archipelago"))
            raw = zf.read(name)
    else:
        raw = path.read_bytes()
    return restricted_loads(inflate(raw))


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    bundle = convert(load_multidata(Path(argv[1])))
    Path(argv[2]).write_text(json.dumps(bundle, sort_keys=True, indent=1) + "\n")
    print(f"wrote {argv[2]}: {len(bundle['slots'])} slots, seed {bundle['seed_name']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
