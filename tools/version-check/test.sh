#!/bin/sh
# Tests check.sh against small fixture trees: a match passes; each kind of drift fails.
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
tree() { # name cargo-version web-version crate-line
  mkdir -p "$tmp/$1/web" "$tmp/$1/crates/a"
  printf '[workspace.package]\nversion = "%s"\n' "$2" > "$tmp/$1/Cargo.toml"
  printf '{\n  "version": "%s"\n}\n' "$3" > "$tmp/$1/web/package.json"
  printf '[package]\n%s\n' "$4" > "$tmp/$1/crates/a/Cargo.toml"
}
want() { # name expected-status [tag]
  ROOT="$tmp/$1" sh "$here/check.sh" "$3" >/dev/null 2>&1
  [ "$?" -eq "$2" ] || { echo "FAIL $1 $3: wanted status $2"; fail=1; }
}
tree ok 1.2.3 1.2.3 'version.workspace = true'
tree web 1.2.3 1.2.4 'version.workspace = true'
tree crate 1.2.3 1.2.3 'version = "0.0.0"'
want ok 0
want ok 0 v1.2.3
want ok 1 v1.2.4
want ok 1 1.2.3
want web 1
want crate 1
[ "$fail" -eq 0 ] && echo "version-check tests ok"
exit "$fail"
