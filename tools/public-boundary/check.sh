#!/bin/sh
# The public crates (`[package.metadata.rando] public = true`) move to their own repo, so they
# may not reach a server crate: no path dependency (normal, dev or build) on a crate that is not
# public, and no `"../..."` path in their sources or manifests that lands inside a server
# crate's folder (fixtures, `include_str!`). A crate with no mark is a server crate.
# Usage: check.sh    ROOT=<dir> METADATA=<file> checks another tree (the tests do).
root="${ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
meta="${METADATA:-}"
if [ -z "$meta" ]; then
  meta=$(mktemp) || exit 1
  trap 'rm -f "$meta"' EXIT
  (cd "$root" && cargo metadata --format-version 1 --no-deps) > "$meta" || exit 1
fi
fail=0
public=$(jq -r '.packages[] | select(.metadata.rando.public == true) | .name' "$meta")
[ -n "$public" ] || { echo "no crate is marked public"; exit 1; }
# Dependencies: every path dependency of a public crate must be public.
bad=$(jq -r '
  [.packages[] | select(.metadata.rando.public == true) | .name] as $pub
  | .packages[] | select(.metadata.rando.public == true) | . as $p
  | .dependencies[] | select(.path != null and .name != $p.name)
  | select(.name as $n | $pub | index($n) | not)
  | "\($p.name) -> \(.name) (\(.kind // "normal"))"' "$meta")
[ -z "$bad" ] || { echo "$bad" | sed 's/^/public crate depends on a server crate: /'; fail=1; }
# Folders of the server crates, then every relative path literal in a public crate.
server_dirs=$(jq -r '.packages[] | select(.metadata.rando.public != true) | .manifest_path' "$meta" \
  | xargs -r -n1 dirname)
for dir in $(jq -r '.packages[] | select(.metadata.rando.public == true) | .manifest_path' "$meta" \
  | xargs -r -n1 dirname); do
  grep -rnoE --include='*.rs' --include='*.toml' '"[^"]*\.\./[^"]*"' "$dir" 2>/dev/null \
    | while IFS= read -r hit; do
      file=${hit%%:*}
      lit=$(printf '%s' "${hit#*:*:}" | tr -d '"')
      rel=${lit#/}
      # A literal is read from its own file (`include_str!`) or the crate folder (`env!`).
      for base in "$(dirname "$file")" "$dir"; do
        target=$(realpath -m "$base/$rel")
        for s in $server_dirs; do
          case "$target/" in "$s"/*) echo "public crate reads a server crate's files: ${hit%%:\"*} $lit" ;; esac
        done
      done
    done
done > "${meta}.paths"
if [ -s "${meta}.paths" ]; then sort -u "${meta}.paths"; fail=1; fi
rm -f "${meta}.paths"
[ "$fail" -eq 0 ] && echo "public boundary ok: $(echo "$public" | wc -l) public crates"
exit "$fail"
