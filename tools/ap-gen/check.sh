#!/usr/bin/env bash
# Real end-to-end generation through the server's admin API (`just ap-gen-check`): start the
# server with a throwaway --data-dir, upload one apworld and one player yaml, generate in the
# sandbox with a local Archipelago checkout, and wait for the bundle. Local only.
#
#   check.sh <apworld> <yaml> <archipelago checkout> <python with its dependencies> [command...]
#
# The server also runs with --rooms (holding the e2e fixture, so there are two rooms and a
# client must name its room key). With a command (`just e2e-full`): wait until the server hosts
# the generated world as a room, then run the command with ROOM_URL (ws://<key>.localhost:<port>)
# and ROOM_BUNDLE (the generated bundle as v1 JSON, from GET /api/worlds/<id>/bundle: the
# server decodes the seed it keeps in its store) set; the script exits with its status.
set -euo pipefail
apworld=$1 yaml=$2 ap=$3 python=$4
shift 4
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
pid=
trap '[ -n "$pid" ] && kill "$pid" 2>/dev/null; rm -rf "$work"' EXIT

token=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
# The header goes to curl from a file, so the token never shows in `ps`.
printf 'Authorization: Bearer %s\n' "$token" >"$work/auth"
mkdir "$work/rooms"
cp "$root/crates/e2e/fixtures/two-slot.json" "$work/rooms/fixture.json"
CONSTELLATION_ADMIN_TOKEN=$token CONSTELLATION_AP_DIR=$ap CONSTELLATION_AP_PYTHON=$python \
    CONSTELLATION_AP_GEN_SCRIPT=$root/tools/ap-gen/ap_gen.py CONSTELLATION_ROOMS_SCAN_MS=200 \
    "${CARGO_TARGET_DIR:-$root/target}/debug/constellation-server" --rooms "$work/rooms" \
    --port 0 --data-dir "$work/data" 2>"$work/log" &
pid=$!
for _ in $(seq 100); do
    # The admin API has its own listener (loopback); its line comes before the room's.
    grep -q 'listening addr=' "$work/log" && break
    sleep 0.1
done
addr=$(grep -o 'admin api addr=[^ ]*' "$work/log" | head -1 | cut -d= -f2)
[ -n "${addr:-}" ] || { cat "$work/log"; exit 1; }

api() { curl -sS --fail-with-body -H @"$work/auth" "$@"; }
urlname() { python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1]))' "$(basename "$1")"; }
w=$(api -X PUT --data-binary @"$apworld" "http://$addr/api/apworlds?name=$(urlname "$apworld")")
y=$(api -X PUT --data-binary @"$yaml" "http://$addr/api/yamls?name=$(urlname "$yaml")")
echo "apworld: $w"
echo "yaml: $y"
idof() { python3 -c 'import json, sys; print(json.load(sys.stdin)["id"])'; }
body=$(printf '{"apworlds":["%s"],"yamls":["%s"]}' "$(idof <<<"$w")" "$(idof <<<"$y")")
job=$(api -X POST --data-binary "$body" "http://$addr/api/worlds" | idof)
start=$(date +%s)
while :; do
    status=$(api "http://$addr/api/worlds/$job")
    case $status in
    *'"status":"done"'*) break ;;
    *'"status":"failed"'*) echo "$status"; exit 1 ;;
    esac
    sleep 1
done
echo "done in $(($(date +%s) - start)) s (queue to done, as seen by polling)"
python3 -c '
import json, sys
s = json.loads(sys.argv[1])
g = s["generation"]
print("seed", s["seed_name"], "-", s["slots"], "slots,", g["locations"], "locations, sidecar",
      g["seconds"], "s; load_bundle accepted it (status done)")
' "$status"
[ $# -gt 0 ] || exit 0

# Every rooms scan is 200 ms; the room is up within a few.
for _ in $(seq 100); do
    status=$(api "http://$addr/api/worlds/$job")
    case $status in *'"room_key"'*) break ;; esac
    sleep 0.1
done
address=$(python3 -c 'import json, sys; print(json.loads(sys.argv[1]).get("address", ""))' "$status")
[ -n "$address" ] || { echo "the world never became a room: $status"; tail -20 "$work/log"; exit 1; }
echo "room address: $address"
rc=0
api "http://$addr/api/worlds/$job/bundle" >"$work/$job.json"
ROOM_URL="ws://$address" ROOM_BUNDLE="$work/$job.json" "$@" || rc=$?
[ "$rc" -eq 0 ] || { echo "client check failed ($rc); server log tail:"; tail -20 "$work/log"; }
exit "$rc"
