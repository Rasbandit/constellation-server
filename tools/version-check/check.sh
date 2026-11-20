#!/bin/sh
# One version for the server, the web UI and the image: the workspace version in Cargo.toml.
# Fails unless web/package.json equals it, no crate sets its own, and (given a tag) the tag is v<version>.
# Usage: check.sh [vX.Y.Z]    ROOT=<dir> checks another tree (the tests do).
root="${ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
cargo=$(awk '/^\[/ { in_pkg = ($0 == "[workspace.package]") } in_pkg && /^version *=/ { gsub(/.*= *"|".*/, ""); print; exit }' "$root/Cargo.toml")
web=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' "$root/web/package.json" | head -n1)
fail=0
[ -n "$cargo" ] || { echo "no [workspace.package] version in Cargo.toml"; exit 1; }
[ "$cargo" = "$web" ] || { echo "version mismatch: Cargo.toml $cargo, web/package.json ${web:-none}"; fail=1; }
for f in "$root"/crates/*/Cargo.toml; do
  case "$f" in */e2e-ap/*) continue ;; esac # its own workspace
  grep -q '^version.workspace = true' "$f" || { echo "$f does not inherit the workspace version"; fail=1; }
done
if [ -n "$1" ] && [ "$1" != "v$cargo" ]; then echo "tag $1 is not v$cargo"; fail=1; fi
[ "$fail" -eq 0 ] && echo "version $cargo ok"
exit "$fail"
