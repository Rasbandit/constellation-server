#!/bin/sh
# Tests check.sh on small fixture trees: a clean tree passes; each kind of crossing fails.
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
tree() { # name deps-of-pub (JSON list) source-line-of-pub
  for c in pub lib srv; do mkdir -p "$tmp/$1/crates/$c/src" "$tmp/$1/crates/$c/fixtures"; done
  printf '%s\n' "$3" > "$tmp/$1/crates/pub/src/lib.rs"
  m() { printf '{"name":"%s","manifest_path":"%s","metadata":%s,"dependencies":%s}' \
    "$1" "$tmp/$2/crates/$1/Cargo.toml" "$3" "$4"; }
  printf '{"packages":[%s,%s,%s]}\n' \
    "$(m pub "$1" '{"rando":{"public":true}}' "$2")" \
    "$(m lib "$1" '{"rando":{"public":true}}' '[]')" \
    "$(m srv "$1" 'null' '[]')" > "$tmp/$1.json"
}
dep() { printf '{"name":"%s","path":"%s","kind":%s}' "$1" "$2" "$3"; }
want() { # name expected-status
  ROOT="$tmp/$1" METADATA="$tmp/$1.json" sh "$here/check.sh" >/dev/null 2>&1
  [ "$?" -eq "$2" ] || { echo "FAIL $1: wanted status $2"; fail=1; }
}
ok_deps="[$(dep lib /x/lib null),$(dep pub /x/pub '"dev"'),{\"name\":\"serde\",\"kind\":null}]"
tree ok "$ok_deps" 'const A: &str = include_str!("../fixtures/a.json"); // own and public files'
tree dep "[$(dep srv /x/srv null)]" ''
tree dev "[$(dep srv /x/srv '"dev"')]" ''
tree include '[]' 'const A: &str = include_str!("../../srv/fixtures/a.json");'
tree manifest '[]' 'const P: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../srv/fixtures/a.json");'
tree public_files '[]' 'const P: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib/fixtures/a.json");'
want ok 0
want dep 1
want dev 1
want include 1
want manifest 1
want public_files 0
[ "$fail" -eq 0 ] && echo "public-boundary tests ok"
exit "$fail"
