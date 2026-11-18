"""A stand-in for tools/ap-gen/ap_gen.py for crates/server/tests/worlds.rs: same command line and
output shape, no Archipelago. It runs inside the real sandbox and reports what the sandbox let
it do. A yaml's text picks a behaviour: a player name, or SLOW, WAIT, FLOOD_ERR, FLOOD_OUT,
BAD_RESULT, MEMFD."""

import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent


def can_write(path: str, size: int = 1) -> str:
    try:
        with open(path, "wb") as f:
            f.write(b"x" * size)
        return "allowed"
    except OSError:
        return "refused"


def forks(n: int) -> int:
    """How many of n children could be started (they wait, then all end with the sandbox)."""
    started = 0
    for _ in range(n):
        try:
            pid = os.fork()
        except OSError:
            break
        if pid == 0:
            time.sleep(30)
            os._exit(0)
        started += 1
    return started


def memfds(n: int) -> int:
    """How many of n memfds could be open at once."""
    fds = []
    try:
        for _ in range(n):
            fds.append(os.memfd_create("probe"))
    except OSError:
        pass
    count = len(fds)
    for fd in fds:
        os.close(fd)
    return count


def probes() -> dict:
    canary = json.loads((HERE / "probe.json").read_text())
    try:
        userns = subprocess.run(["unshare", "-Ur", "true"], capture_output=True).returncode
    except OSError:
        userns = -1
    return {
        # Only loopback: a fresh network namespace (an offline host has its own interfaces).
        "interfaces": [name for _, name in socket.if_nameindex()],
        "env_has_canary": any(canary["value"] in v for v in os.environ.values()),
        "pid1_env_has_canary": canary["value"].encode() in Path("/proc/1/environ").read_bytes(),
        "visible": [p for p in canary["hidden_paths"] if os.path.exists(p)],
        "write_root": can_write("/x"),
        "write_dev": can_write("/dev/x"),
        "write_ap": can_write("/ap/x"),
        "write_tools": can_write(str(HERE / "x")),
        "write_tmp_over_cap": can_write("/tmp/big", 300 * 1024 * 1024),
        "core_worlds": sorted(os.listdir("/ap/worlds")),
        "nested_userns_exit": userns,
        "forks_of_100": forks(100),
        "memfds_of_300": memfds(300),
    }


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
        # A sidecar must not be able to set the server's own fields.
        print(json.dumps({"module": data.decode().strip() or "fake", "game": "Fake Game",
                          "world_version": "1.0.0", "id": "../../etc", "name": "spoofed"}))
        return 0
    if cmd == "inspect-yaml":
        name = Path(argv[1]).read_text().strip()
        print(json.dumps([{"name": name, "game": "Fake Game"}]))
        return 0
    if cmd == "generate":
        a = dict(zip(argv[1::2], argv[2::2]))
        texts = {p.read_text() for p in Path(a["--yamls"]).iterdir()}
        if "SLOW" in texts:
            time.sleep(60)  # the server's generation timeout must kill this
        if "WAIT" in texts:
            while not (HERE / "release").exists():  # the test creates it on the host
                time.sleep(0.05)
        if "FLOOD_ERR" in texts:
            chunk = b"e" * (1 << 20)
            for _ in range(64):
                os.write(2, chunk)
            os.write(2, b"THE-END")
            return 1
        if "FLOOD_OUT" in texts:
            chunk = b" " * (1 << 20)
            for _ in range(80):
                os.write(1, chunk)
            return 0
        if "BAD_RESULT" in texts:
            print("[]")
            return 0
        if "MEMFD" in texts:
            # Memory no address space cap sees: only the sandbox's cgroup can stop this.
            chunk = b"m" * (1 << 20)
            for _ in range(3):
                fd = os.memfd_create("pin")
                for _ in range(100):
                    os.write(fd, chunk)
            # A valid result: without the cgroup cap this job would end `done`.
            bundle = json.loads((HERE / "bundle.json").read_text())
            print(json.dumps({"report": {}, "bundle": bundle}))
            return 0
        out = Path(a["--out"])
        # Anything left in the scratch folder must stay in the sandbox.
        os.symlink("/etc/hostname", out / "bundle.json")
        os.mkfifo(out / "fifo")
        report = {
            "custom_worlds": sorted(os.listdir(a["--apworlds"])),
            "yamls": len(os.listdir(a["--yamls"])),
            **probes(),
        }
        bundle = json.loads((HERE / "bundle.json").read_text())
        print(json.dumps({"report": report, "bundle": bundle}))
        return 0
    print(f"unknown command {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
