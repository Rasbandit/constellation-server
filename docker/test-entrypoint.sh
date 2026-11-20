#!/bin/sh
# Plain-sh test of docker/entrypoint.sh against stub children: exit 3 -> 0, other codes pass through,
# SIGTERM reaches the child.
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
check() { [ "$2" = "$3" ] || { echo "FAIL $1: got $2, want $3"; fail=1; }; }

cat > "$tmp/stub" <<'S'
#!/bin/sh
exit "$STUB_RC"
S
chmod +x "$tmp/stub"
for pair in 3:0 0:0 1:1 7:7; do
  SERVER_BIN="$tmp/stub" STUB_RC="${pair%:*}" sh "$here/entrypoint.sh" >/dev/null 2>&1
  check "exit ${pair%:*}" "$?" "${pair#*:}"
done

# The stub exits 42 only if it receives SIGTERM.
cat > "$tmp/loop" <<'S'
#!/bin/sh
trap "exit 42" TERM
while :; do sleep 0.1; done
S
chmod +x "$tmp/loop"
SERVER_BIN="$tmp/loop" sh "$here/entrypoint.sh" >/dev/null 2>&1 &
wp=$!
sleep 1
kill -TERM "$wp"
wait "$wp"
check "SIGTERM forwarded" "$?" "42"

[ "$fail" -eq 0 ] && echo "entrypoint tests ok"
exit "$fail"
