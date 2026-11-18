"""Drive our server with Archipelago's own client library (CommonClient) from a local checkout.

Usage (from the repo root; `just ap-client-check` builds the server and runs this):

    <archipelago venv python> tools/ap-client-check/check_common_client.py \
        --ap <Archipelago checkout> --server <constellation-server binary> --bundle <fixture>

It starts our server on a free loopback port with the fixture bundle (in-memory log), connects
two `CommonContext` clients (Alice and Bob), and checks what a standard client does on connect
and with data storage: the `Get` and `SetNotify` every CommonClient sends after `Connected`
(hints, item and location name groups), `Set` with a reply to the writer and to a watcher,
a `Get` round trip, the starting inventory, client status changes, and hints (`!hint`, the
watched `_read_hints_` key, `UpdateHint`). Expected values come
from the fixture bundle and the protocol document. Exits 0 when every check passes.

Local only (CI has no Archipelago checkout). It runs their client library as a test client; it
never runs their server. Side effects are kept out of the user's files: the data package cache
goes to a temporary directory and the client's persistent store is not written.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import subprocess
import sys
import threading
from typing import Any

from ap_client import GUARD, Failure, check, load_archipelago, make_probe

GAME = "Archipela-Go 2: Electric Boogaloo"


def start_server(binary: str, bundle: str) -> tuple[subprocess.Popen, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("CONSTELLATION_DATABASE_URL")}
    proc = subprocess.Popen([binary, "--bundle", bundle, "--port", "0"], env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.PIPE, text=True)
    found: list[str] = []
    ready = threading.Event()

    def drain() -> None:  # keep reading so the pipe never fills
        assert proc.stderr is not None
        for line in proc.stderr:
            if not found and "listening addr=" in line:
                found.append(line.split("listening addr=", 1)[1].split()[0])
                ready.set()
        ready.set()

    threading.Thread(target=drain, daemon=True).start()
    if not ready.wait(GUARD) or not found:
        proc.kill()
        raise Failure("the server never logged its address")
    return proc, f"ws://{found[0]}"


async def run(CommonContext: type, url: str, bundle: dict[str, Any]) -> None:
    Probe = make_probe(CommonContext, GAME)
    game = bundle["games"][GAME]
    alice = Probe(url, "Alice")
    await alice.connect()
    items_key, locations_key = f"_read_item_name_groups_{GAME}", f"_read_location_name_groups_{GAME}"
    # What every CommonClient asks for right after `Connected` (Get, then SetNotify).
    on_connect = ["_read_hints_0_1", items_key, locations_key, "_read_race_mode"]
    await alice.until("the keys CommonClient reads on connect",
                      lambda: all(k in alice.stored_data for k in on_connect))
    check(alice.stored_data[items_key] == game["item_name_groups"], "item name groups match the seed")
    check(alice.stored_data[locations_key] == game["location_name_groups"],
          "location name groups match the seed")
    check(alice.stored_data["_read_hints_0_1"] == [], "hints read as an empty list")
    check(alice.stored_data["_read_race_mode"] == bundle["race_mode"], "race mode is the seed's")

    alice.set_notify("shared", "_read_client_status_0_2")
    await alice.until("Get of the watched keys", lambda: "shared" in alice.stored_data)
    check(alice.stored_data["_read_client_status_0_2"] == 0, "Bob's status is 0 before he connects")

    bob = Probe(url, "Bob")
    await bob.connect()
    await alice.until("Bob's status 5", lambda: alice.stored_data.get("_read_client_status_0_2") == 5)
    check(True, "a watcher hears Bob connect (5)")
    start = bundle["slots"][1]["start_inventory"]
    await bob.until("Bob's starting inventory", lambda: len(bob.items_received) >= len(start))
    got = [(i.item, i.location, i.player) for i in bob.items_received[:len(start)]]
    check(got == [(i, -2, 0) for i in start], "the starting inventory arrives first, from location -2")

    await bob.send_msgs([{"cmd": "Set", "key": "shared", "default": 1, "want_reply": True,
                          "operations": [{"operation": "add", "value": 4},
                                         {"operation": "mul", "value": 3}]}])
    await bob.until("Bob's own SetReply", lambda: bob.stored_data.get("shared") == 15)
    await alice.until("the watcher's SetReply", lambda: alice.stored_data.get("shared") == 15)
    replies = [a for c, a in alice.packets if c == "SetReply" and a["key"] == "shared"]
    check(replies[-1]["original_value"] == 1 and replies[-1]["slot"] == 2,
          "SetReply carries the original value and the writing slot")
    check(sum(1 for c, a in bob.packets if c == "SetReply" and a["key"] == "shared") == 1,
          "the writer gets exactly one SetReply")

    await alice.send_msgs([{"cmd": "Get", "keys": ["shared", "never-set"], "probe": 7}])
    await alice.until("Retrieved", lambda: any(c == "Retrieved" and a.get("probe") == 7
                                               for c, a in alice.packets))
    got = next(a for c, a in alice.packets if c == "Retrieved" and a.get("probe") == 7)
    check(got["keys"] == {"shared": 15, "never-set": None}, "Get reads stored and missing keys")

    # Hints: Alice earns a point with a check, then buys a hint with `!hint`; CommonClient
    # watches its own `_read_hints_` key from connect, so the new hint reaches its store.
    hints_key = "_read_hints_0_1"
    first = bundle["slots"][0]["locations"][0]["location"]
    await alice.send_msgs([{"cmd": "LocationChecks", "locations": [first]}])
    await alice.until("a hint point", lambda: alice.hint_points == bundle["server_options"]["location_check_points"])
    wanted = next(p for p in bundle["slots"][1]["locations"] if p["receiver"] == 1 and not p["flags"] & 4)
    name = next(n for n, i in game["item_name_to_id"].items() if i == wanted["item"])
    await alice.send_msgs([{"cmd": "Say", "text": f"!hint {name}"}])
    await alice.until("the hint in the watched key",
                      lambda: any(h["item"] == wanted["item"] for h in alice.stored_data.get(hints_key, [])))
    hint = next(h for h in alice.stored_data[hints_key] if h["item"] == wanted["item"])
    check(hint["receiving_player"] == 1 and hint["status"] == 30 and hint["class"] == "Hint",
          "!hint stores a priority hint for Alice and the hints key tells her")
    check(any(c == "PrintJSON" and a.get("type") == "Hint" for c, a in alice.packets),
          "a Hint print reaches the receiver")
    await alice.until("hint points spent", lambda: alice.hint_points == 0)
    check(True, "the paid hint cost its point")
    await alice.send_msgs([{"cmd": "UpdateHint", "player": hint["finding_player"],
                            "location": hint["location"], "status": 10}])
    await alice.until("the status change in the watched key",
                      lambda: any(h["item"] == wanted["item"] and h["status"] == 10
                                  for h in alice.stored_data[hints_key]))
    check(True, "UpdateHint by the receiver reaches the hints key")

    await bob.send_msgs([{"cmd": "StatusUpdate", "status": 20}])
    await alice.until("Bob's status 20", lambda: alice.stored_data.get("_read_client_status_0_2") == 20)
    check(True, "a watcher hears Bob's StatusUpdate (20)")
    await bob.disconnect()
    await alice.until("Bob's status 0", lambda: alice.stored_data.get("_read_client_status_0_2") == 0)
    check(True, "a watcher hears Bob's last connection leave (0)")

    for p in (alice, bob):
        check(p.invalid() == [], f"{p.username} got no InvalidPacket ({p.invalid()})")
    await alice.disconnect()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ap", required=True, help="Archipelago checkout (its CommonClient)")
    parser.add_argument("--server", required=True, help="constellation-server binary")
    parser.add_argument("--bundle", required=True, help="seed bundle (the e2e fixture)")
    args = parser.parse_args()
    with open(args.bundle) as f:
        bundle = json.load(f)
    CommonContext = load_archipelago(args.ap)
    proc, url = start_server(args.server, args.bundle)
    try:
        asyncio.run(run(CommonContext, url, bundle))
    except Failure as e:
        print(f"FAILED: {e}", file=sys.stderr)
        return 1
    finally:
        proc.terminate()
        proc.wait(GUARD)
    print("CommonClient check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
