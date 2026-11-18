"""Play a generated world with Archipelago's own CommonClient: connect to a room the server hosts
and check one location, then check that the item received is the one the bundle placed there.

Usage (`just e2e-full` runs it through tools/ap-gen/check.sh, which sets ROOM_URL and
ROOM_BUNDLE):

    <archipelago venv python> tools/ap-client-check/play_generated.py --ap <checkout> \
        --slot <name> --game <game>

Expected values come from the bundle file the server loaded (its placements), never from the
server's replies. Exits 0 when every check passes.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
from typing import Any

from ap_client import Failure, check, load_archipelago, make_probe


async def run(CommonContext: type, url: str, bundle: dict[str, Any], slot: str, game: str, stay: bool = False) -> None:
    team, slot_id = bundle["connect_names"][slot]
    placed = next(s for s in bundle["slots"] if s["id"] == slot_id)
    check(placed["game"] == game, f"slot {slot} plays {game!r} in the bundle")
    # A location whose item goes to this slot, so the client must receive it.
    own = [p for p in placed["locations"] if p["receiver"] == slot_id]
    check(own != [], f"the bundle places {len(own)} of {len(placed['locations'])} items for {slot} in its own world")
    target = own[0]

    client = make_probe(CommonContext, game)(url, slot)
    await client.connect()
    await client.until("Connected", lambda: any(c == "Connected" for c, _ in client.packets))
    connected = next(a for c, a in client.packets if c == "Connected")
    check(connected["slot"] == slot_id and connected["team"] == team, f"connected as {slot} (slot {slot_id})")
    check(sorted(connected["missing_locations"] + connected["checked_locations"])
          == sorted(p["location"] for p in placed["locations"]),
          "the room's locations are the bundle's")

    await client.send_msgs([{"cmd": "LocationScouts", "locations": [target["location"]], "create_as_hint": 0}])
    await client.until("LocationInfo", lambda: any(c == "LocationInfo" for c, _ in client.packets))
    info = next(a for c, a in client.packets if c == "LocationInfo")["locations"][0]
    # CommonClient decodes it into its NetworkItem tuple.
    check((info.item, info.location, info.player, info.flags)
          == (target["item"], target["location"], slot_id, target["flags"]),
          f"LocationInfo for location {target['location']} matches the bundle (item {target['item']})")

    await client.check_locations([target["location"]])
    await client.until("the item from the checked location",
                       lambda: any(i.location == target["location"] for i in client.items_received))
    got = next(i for i in client.items_received if i.location == target["location"])
    check((got.item, got.player, got.flags) == (target["item"], slot_id, target["flags"]),
          f"checking location {target['location']} gives item {target['item']}, as the bundle placed it")
    check(client.invalid() == [], f"no InvalidPacket ({client.invalid()})")
    if stay:
        print("staying connected; stop with Ctrl-C or SIGTERM", flush=True)
        await asyncio.Event().wait()
    await client.disconnect()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ap", required=True, help="Archipelago checkout (its CommonClient)")
    parser.add_argument("--slot", required=True)
    parser.add_argument("--game", required=True)
    parser.add_argument("--url", default=os.environ.get("ROOM_URL"), help="default: $ROOM_URL")
    parser.add_argument("--bundle", default=os.environ.get("ROOM_BUNDLE"), help="default: $ROOM_BUNDLE")
    parser.add_argument("--stay", action="store_true", help="stay connected after the checks")
    args = parser.parse_args()
    if not args.url or not args.bundle:
        parser.error("--url and --bundle (or ROOM_URL and ROOM_BUNDLE) are required")
    with open(args.bundle) as f:
        bundle = json.load(f)
    CommonContext = load_archipelago(args.ap)
    print(f"connecting to {args.url} as {args.slot}")
    try:
        asyncio.run(run(CommonContext, args.url, bundle, args.slot, args.game, args.stay))
    except Failure as e:
        print(f"FAILED: {e}", file=sys.stderr)
        return 1
    print("generated world played: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
