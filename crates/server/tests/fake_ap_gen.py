"""A stand-in for tools/ap-gen/ap_gen.py for crates/server/tests/worlds.rs: same command line and
output shape, no Archipelago. It runs inside the real sandbox, so it also reports what the
sandbox let it do."""

import json
import os
import shutil
import socket
import sys
import time
from pathlib import Path


def probe_network() -> str:
    try:
        socket.create_connection(("1.1.1.1", 53), timeout=1).close()
        return "open"
    except OSError:
        return "blocked"


def probe_write(path: str) -> str:
    try:
        Path(path).write_text("x")
        return "allowed"
    except OSError:
        return "refused"


def main(argv):
    cmd = argv[0]
    if cmd == "inspect-apworld":
        # The caps come from the server's config (Limits::DESIGN).
        if argv[2:] != ["--max-unpacked", str(256 << 20), "--max-entries", "20000"]:
            print(f"bad caps {argv[2:]}", file=sys.stderr)
            return 2
        data = Path(argv[1]).read_bytes()
        if data.startswith(b"BAD"):
            print("refused: not a zip file", file=sys.stderr)
            return 1
        print(json.dumps({"module": data.decode().strip() or "fake", "game": "Fake Game",
                          "world_version": "1.0.0"}))
        return 0
    if cmd == "inspect-yaml":
        name = Path(argv[1]).read_text().strip()
        print(json.dumps([{"name": name, "game": "Fake Game"}]))
        return 0
    if cmd == "generate":
        a = dict(zip(argv[1::2], argv[2::2]))
        out = Path(a["--out"])
        if any(p.read_text() == "SLOW" for p in Path(a["--yamls"]).iterdir()):
            time.sleep(60)  # the server's generation timeout must kill this
        shutil.copy(Path(__file__).parent / "bundle.json", out / "bundle.json")
        print(json.dumps({
            "custom_worlds": sorted(os.listdir(a["--apworlds"])),
            "yamls": len(os.listdir(a["--yamls"])),
            "network": probe_network(),
            "write_tools": probe_write("/tools/x"),
            "write_checkout": probe_write("/ap/written-by-job"),
        }))
        return 0
    print(f"unknown command {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
