"""What the CommonClient checks share: Archipelago's own client library, loaded from a local
checkout without touching the user's files, and a probe client that records every packet.

Local only (CI has no Archipelago checkout). It runs their client library as a test client; it
never runs their server.
"""

from __future__ import annotations

import asyncio
import os
import sys
import tempfile
from typing import Callable

GUARD = 20.0  # seconds any single wait may take before the check fails instead of hanging


class Failure(Exception):
    pass


def check(cond: bool, what: str) -> None:
    if not cond:
        raise Failure(what)
    print(f"ok: {what}")


def load_archipelago(ap: str) -> type:
    """Import CommonContext from the checkout `ap`. The data package cache goes to a temporary
    directory and the client's persistent store is never written."""
    # Set before Archipelago is imported.
    os.environ["XDG_CACHE_HOME"] = tempfile.mkdtemp(prefix="ap-client-check-")
    sys.path.insert(0, os.path.abspath(ap))
    import ModuleUpdate  # Archipelago's dependency check; skip it, the venv is already set up

    ModuleUpdate.update_ran = True
    import Utils

    Utils.persistent_store = lambda *a, **k: None  # never write into the checkout or home
    from CommonClient import CommonContext

    return CommonContext


def make_probe(base: type, game: str) -> type:
    class Probe(base):  # type: ignore[misc, valid-type]
        items_handling = 0b111  # every item, own world too, and the starting inventory

        def __init__(self, address: str, name: str) -> None:
            super().__init__(address, None)
            self.game = game
            self.username = name
            self.packets: list[tuple[str, dict]] = []
            self.changed = asyncio.Event()

        async def server_auth(self, password_requested: bool = False) -> None:
            await super().server_auth(password_requested)
            await self.get_username()
            await self.send_connect()

        def on_package(self, cmd: str, args: dict) -> None:
            self.packets.append((cmd, args))
            self.changed.set()

        async def until(self, what: str, done: Callable[[], bool]) -> None:
            async def wait() -> None:
                while not done():
                    self.changed.clear()
                    await self.changed.wait()

            try:
                await asyncio.wait_for(wait(), GUARD)
            except asyncio.TimeoutError:
                raise Failure(f"{self.username}: timed out waiting for {what}") from None

        def invalid(self) -> list[dict]:
            return [a for c, a in self.packets if c == "InvalidPacket"]

    return Probe
