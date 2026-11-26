"""Generation sidecar: inspects uploads and generates a seed bundle with an Archipelago checkout.

The server runs every call inside its sandbox (crates/server/src/sandbox.rs). It prints one JSON
value to stdout; on failure it prints the reason to stderr and exits non-zero. `generate`
prints `{"report": ..., "bundle": ...}`: the server writes the bundle file itself, so nothing the
sandbox writes ever reaches the host.

    ap_gen.py inspect-apworld <path> --max-unpacked <bytes> --max-entries <n>
    ap_gen.py inspect-yaml <path>
    ap_gen.py generate --ap <checkout> --apworlds <dir> --yamls <dir> --out <dir>
    ap_gen.py scan-apworld <path> --max-unpacked <bytes> --max-entries <n>
    ap_gen.py check --ap <checkout> --apworlds <dir> --module <name> --seed <n> --out <dir>

`inspect-*` and `scan-apworld` read data only and never import or run a world's code.
`generate` and `check` run uploaded world code, which is why the server sandboxes them.

`scan-apworld` is the catalogue's code scan (issue 189): it reads each Python file of an
apworld as a syntax tree and lists code a person should look at (network, other programs,
changing files, code built or decoded at run time, native code). It catches common patterns
only; the sandbox is what contains a world. `check` generates one test seed of a catalogue
version: it writes its own player file (the world's game, default options) and prints only
the report.

How generation follows Archipelago 0.6.8 (files in its checkout, read, not copied):
- `worlds/__init__.py` loads `.apworld` files from `user_path("custom_worlds")` when
  `Utils.user_path` is the checkout itself, which it picks only for a writable checkout. The
  sandbox mounts the checkout read-only, so this sets that choice (`user_path.cached_path`)
  before anything reads it. It also seeds default settings in memory, so no settings file is
  written. The sandbox mounts the job's apworlds at `custom_worlds/`, so `--apworlds` must be
  that folder. A world loads as `worlds.<file stem>`, so each file is
  named after the top-level folder inside it.
- `Generate.mystery_argparse` and `Generate.main` roll the player settings from the yamls in
  `--player_files_path`, then `Main.main` generates and writes `AP_<seed>.zip` (the
  `.archipelago` multidata plus any patch files) to `--outputpath`; `WebHostLib/generate.py`
  (`gen_game`) runs the same two steps. Spoilers are off (`--spoiler 0`). Patch files cannot
  be skipped: `--skip_output` also skips the multidata (`Main.py`), so they are written and
  ignored; only the multidata reaches `ap_import`.
"""

from __future__ import annotations

import argparse
import ast
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

# The unpacked size and entry caps come from the server (crates/config, `Limits`).
MAX_MANIFEST_BYTES = 64 * 1024
# A world's module name: what `worlds.<name>` imports, so a Python identifier.
MODULE_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")


class Refused(Exception):
    """Input we will not accept; the message is shown to the uploader."""


def inspect_apworld(path: Path, max_unpacked: int, max_entries: int) -> dict[str, Any]:
    """Validate an apworld zip and read its manifest without running any of its code."""
    try:
        zf = zipfile.ZipFile(path)
    except zipfile.BadZipFile as e:
        raise Refused(f"not a zip file: {e}") from e
    with zf:
        infos = zf.infolist()
        if not infos:
            raise Refused("empty zip")
        if len(infos) > max_entries:
            raise Refused(f"more than {max_entries} entries")
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
            if total > max_unpacked:
                raise Refused(f"larger than {max_unpacked} bytes unpacked")
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


def use_default_settings() -> None:
    """Give Archipelago its default settings from memory. Left alone, `get_settings` writes a
    new host.yaml into the checkout, which the sandbox mounts read-only (and nothing here
    should persist). Call after the checkout is on sys.path."""
    import settings

    settings.skip_autosave = True
    settings.get_settings._cache = settings.Settings(None)


def _generate_zip(
    ap: Path,
    apworlds: Path,
    yamls: Path,
    out: Path,
    seed: int | None = None,
    before: Any = None,
) -> Path:
    """Generate from the yamls into `out` and return the one `AP_*.zip`. `before`, when given,
    runs once the worlds can be imported and before the yamls are read."""
    if not (ap / "custom_worlds").exists() or not os.path.samefile(apworlds, ap / "custom_worlds"):
        raise Refused("--apworlds must be the checkout's custom_worlds folder (the sandbox mounts it)")
    out.mkdir(parents=True, exist_ok=True)
    os.chdir(ap)
    sys.path.insert(0, str(ap))
    # Archipelago logs and prints to stdout; ours carries only the JSON result (main() writes
    # it to the original stdout, which _quiet_stdout keeps aside).
    with contextlib.redirect_stdout(sys.stderr):
        import logging

        import Utils

        Utils.user_path.cached_path = Utils.local_path()
        use_default_settings()

        logging.basicConfig(stream=sys.stderr, level=logging.INFO)
        import ModuleUpdate

        # Archipelago's dependency check pip-installs what is missing; the sandbox has no
        # network and the venv is prepared beforehand (as in tools/ap-client-check).
        ModuleUpdate.update_ran = True
        if before is not None:
            before()
        import Generate
        import Main

        argv = ["--player_files_path", str(yamls), "--outputpath", str(out), "--spoiler", "0"]
        if seed is not None:
            argv += ["--seed", str(seed)]
        args = Generate.mystery_argparse(argv)
        erargs, rolled = Generate.main(args)
        Main.main(erargs, rolled)
    zips = sorted(out.glob("AP_*.zip"))
    if len(zips) != 1:
        raise Refused(f"expected one AP_*.zip in the output, found {len(zips)}")
    return zips[0]


def _converted(zip_path: Path, start: float) -> tuple[dict[str, Any], dict[str, Any]]:
    """The bundle of a generated zip and its report."""
    # The worlds are loaded (generation imported them), so their hint blacklists are at hand.
    bundle = ap_import.convert(ap_import.load_multidata(zip_path), ap_import.load_hint_blacklists())
    report = {
        "seed_name": bundle["seed_name"],
        "slots": len(bundle["slots"]),
        "locations": sum(len(s["locations"]) for s in bundle["slots"]),
        "seconds": round(time.monotonic() - start, 1),
    }
    return report, bundle


def generate(ap: Path, apworlds: Path, yamls: Path, out: Path) -> dict[str, Any]:
    """Generate a multiworld from the yamls; `out` is scratch space for Archipelago's output."""
    start = time.monotonic()
    report, bundle = _converted(_generate_zip(ap, apworlds, yamls, out), start)
    return {"report": report, "bundle": bundle}


def game_of_module(module: str) -> str:
    """The one game the world in `worlds.<module>` registers (the worlds must be importable)."""
    import worlds  # noqa: F401  (loads custom_worlds)
    from worlds.AutoWorld import AutoWorldRegister

    games = sorted({
        cls.game for cls in AutoWorldRegister.world_types.values()
        if cls.__module__ == f"worlds.{module}" or cls.__module__.startswith(f"worlds.{module}.")
    })
    if not games:
        raise Refused(f"the apworld {module!r} registers no world")
    if len(games) > 1:
        raise Refused(f"the apworld {module!r} registers more than one game: {games}")
    return games[0]


def test_player(game: str) -> dict[str, Any]:
    """A player file for `game` with its default options (an empty options mapping)."""
    return {"name": "Tester", "game": game, game: {}}


def check(ap: Path, apworlds: Path, module: str, seed: int, out: Path) -> dict[str, Any]:
    """Generate one test seed of the world `module` with a default player; the report only."""
    start = time.monotonic()
    players = out / "players"

    def write_player() -> None:
        players.mkdir(parents=True, exist_ok=True)
        # JSON is YAML, and it quotes any game name safely.
        text = json.dumps(test_player(game_of_module(module)))
        (players / "Tester.yaml").write_text(text, encoding="utf-8")

    report, _ = _converted(_generate_zip(ap, apworlds, players, out / "gen", seed, write_player), start)
    return {"report": report}


# The code scan's reasons, in the words shown to the reviewer.
NETWORK = "connects to the network"
PROGRAMS = "runs other programs"
FILES = "deletes, moves or changes files, or uses a path outside the world"
HIDDEN = "runs code that is built or decoded while it runs"
NATIVE = "loads compiled native code"
BLOB = "holds a long encoded text"
UNREADABLE = "could not be read as Python"
# Most flags one scan returns, so a hostile world cannot make the result huge.
MAX_FLAGS = 200
# A string constant this long made only of base64 or hex characters is flagged.
BLOB_CHARS = 2000
BLOB_RE = re.compile(r"[A-Za-z0-9+/=\s]+\Z")

# Modules whose import is flagged, by their top-level name.
FLAGGED_IMPORTS = {
    **dict.fromkeys(["socket", "ssl", "http", "urllib", "urllib3", "requests", "httpx",
                     "aiohttp", "websocket", "websockets", "ftplib", "smtplib", "telnetlib",
                     "xmlrpc", "socketserver"], NETWORK),
    **dict.fromkeys(["subprocess", "pty", "multiprocessing"], PROGRAMS),
    **dict.fromkeys(["ctypes", "cffi"], NATIVE),
    **dict.fromkeys(["marshal"], HIDDEN),
}
# Calls flagged whatever their arguments, by dotted name after import aliases.
FLAGGED_CALLS = {
    **dict.fromkeys(["os.system", "os.popen", "os.fork", "os.forkpty", "os.posix_spawn",
                     "os.posix_spawnp"], PROGRAMS),
    **dict.fromkeys(["os.remove", "os.unlink", "os.rmdir", "os.removedirs", "os.rename",
                     "os.replace", "os.chmod", "os.chown", "shutil.rmtree", "shutil.move",
                     "os.path.expanduser", "pathlib.Path.home"], FILES),
}
# Calls flagged when what they run is not a constant string.
DYNAMIC_CALLS = {"exec", "eval", "compile", "__import__", "importlib.import_module"}
# Files that are not Python source: compiled code we cannot read.
NATIVE_SUFFIXES = (".so", ".pyd", ".dll", ".dylib", ".exe")


def _dotted(node: ast.AST, aliases: dict[str, str]) -> str | None:
    """`a.b.c` for a name or attribute chain, its first part replaced by what it imports."""
    parts = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if not isinstance(node, ast.Name):
        return None
    parts.append(aliases.get(node.id, node.id))
    return ".".join(reversed(parts))


def _scan_source(name: str, source: str) -> list[dict[str, Any]]:
    """The flags of one Python file."""
    try:
        tree = ast.parse(source, filename=name)
    except (SyntaxError, ValueError, RecursionError, MemoryError) as e:
        return [{"file": name, "line": 0, "reason": f"{UNREADABLE}: {type(e).__name__}"}]
    flags: list[dict[str, Any]] = []
    aliases: dict[str, str] = {}

    def flag(node: ast.AST, reason: str) -> None:
        flags.append({"file": name, "line": getattr(node, "lineno", 0), "reason": reason})

    try:
        nodes = list(ast.walk(tree))
    except RecursionError:
        return [{"file": name, "line": 0, "reason": f"{UNREADABLE}: RecursionError"}]
    # Imports first, so a call made before its module's import line still resolves.
    for node in nodes:
        if isinstance(node, ast.Import):
            for a in node.names:
                # `import a.b as c` binds c to a.b; `import a.b` binds a to a.
                if a.asname:
                    aliases[a.asname] = a.name
                else:
                    top = a.name.split(".")[0]
                    aliases[top] = top
                if (why := FLAGGED_IMPORTS.get(a.name.split(".")[0])) is not None:
                    flag(node, why)
        elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
            for a in node.names:
                aliases[a.asname or a.name] = f"{node.module}.{a.name}"
            if (why := FLAGGED_IMPORTS.get(node.module.split(".")[0])) is not None:
                flag(node, why)
    for node in nodes:
        if isinstance(node, ast.Call):
            called = _dotted(node.func, aliases)
            if called is None:
                continue
            if (why := FLAGGED_CALLS.get(called)) is not None:
                flag(node, why)
            elif called.startswith(("os.exec", "os.spawn")):
                flag(node, PROGRAMS)
            elif called in DYNAMIC_CALLS:
                first = node.args[0] if node.args else None
                if not (isinstance(first, ast.Constant) and isinstance(first.value, str)):
                    flag(node, HIDDEN)
            elif called == "open" and node.args:
                path = node.args[0]
                if isinstance(path, ast.Constant) and isinstance(path.value, str):
                    p = path.value
                    if p.startswith(("/", "~")) or ".." in PurePosixPath(p).parts:
                        flag(node, FILES)
        elif isinstance(node, ast.Constant) and isinstance(node.value, (str, bytes)):
            text = node.value if isinstance(node.value, str) else node.value.decode("latin-1")
            if len(text) >= BLOB_CHARS and BLOB_RE.match(text):
                flag(node, BLOB)
    return flags


def scan_apworld(path: Path, max_unpacked: int, max_entries: int) -> list[dict[str, Any]]:
    """The code scan of an apworld (checked as `inspect-apworld` checks it): flags ordered by
    file and line, at most MAX_FLAGS. Nothing in it is imported or run."""
    inspect_apworld(path, max_unpacked, max_entries)
    flags: list[dict[str, Any]] = []
    with zipfile.ZipFile(path) as zf:
        for info in sorted(zf.infolist(), key=lambda i: i.filename):
            name = info.filename
            if info.is_dir():
                continue
            lower = name.lower()
            if lower.endswith(NATIVE_SUFFIXES):
                flags.append({"file": name, "line": 0, "reason": NATIVE})
            elif lower.endswith(".pyc"):
                flags.append({"file": name, "line": 0, "reason": f"{UNREADABLE}: compiled Python"})
            elif lower.endswith(".py"):
                source = zf.read(info).decode("utf-8", errors="replace")
                flags.extend(_scan_source(name, source))
    flags.sort(key=lambda f: (f["file"], f["line"], f["reason"]))
    return flags[:MAX_FLAGS]


def _quiet_stdout() -> Any:
    """Point fd 1 at stderr, so code that writes to it directly (world code can) cannot mix
    with the result; return a file on the original stdout for the result."""
    sys.stdout.flush()
    result = os.fdopen(os.dup(1), "w")
    os.dup2(2, 1)
    return result


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="ap_gen.py")
    sub = p.add_subparsers(dest="cmd", required=True)
    ia = sub.add_parser("inspect-apworld")
    ia.add_argument("path", type=Path)
    ia.add_argument("--max-unpacked", type=int, required=True)
    ia.add_argument("--max-entries", type=int, required=True)
    sub.add_parser("inspect-yaml").add_argument("path", type=Path)
    g = sub.add_parser("generate")
    for flag in ("--ap", "--apworlds", "--yamls", "--out"):
        g.add_argument(flag, type=Path, required=True)
    sc = sub.add_parser("scan-apworld")
    sc.add_argument("path", type=Path)
    sc.add_argument("--max-unpacked", type=int, required=True)
    sc.add_argument("--max-entries", type=int, required=True)
    c = sub.add_parser("check")
    for flag in ("--ap", "--apworlds", "--out"):
        c.add_argument(flag, type=Path, required=True)
    c.add_argument("--module", required=True)
    c.add_argument("--seed", type=int, required=True)
    a = p.parse_args(argv)
    runs_code = a.cmd in ("generate", "check")
    result_out = _quiet_stdout() if runs_code else sys.stdout
    try:
        if a.cmd == "inspect-apworld":
            result: Any = inspect_apworld(a.path, a.max_unpacked, a.max_entries)
        elif a.cmd == "inspect-yaml":
            result = inspect_yaml(a.path)
        elif a.cmd == "scan-apworld":
            result = scan_apworld(a.path, a.max_unpacked, a.max_entries)
        elif a.cmd == "check":
            if not MODULE_NAME.match(a.module):
                raise Refused(f"{a.module!r} is not a Python module name")
            result = check(a.ap, a.apworlds, a.module, a.seed, a.out)
        else:
            result = generate(a.ap, a.apworlds, a.yamls, a.out)
    except (Refused, OSError) as e:
        print(f"refused: {e}", file=sys.stderr)
        return 1
    print(json.dumps(result), file=result_out, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
