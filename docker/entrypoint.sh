#!/bin/sh
# Runs the server as a child so exit 3 (fenced by a newer owner) can be mapped to 0:
# compose `restart: on-failure` would otherwise restart it and two instances would fence each other.
bin="${SERVER_BIN:-constellation-server}"
"$bin" "$@" &
pid=$!
trap 'kill -TERM "$pid" 2>/dev/null' TERM INT
wait "$pid"
rc=$?
# A trapped signal ends the first wait early; wait again for the child's real status.
while kill -0 "$pid" 2>/dev/null; do wait "$pid"; rc=$?; done
if [ "$rc" -eq 3 ]; then
  echo "entrypoint: server exited 3 (fenced by a newer owner); exiting 0 so the container is not restarted" >&2
  rc=0
fi
exit "$rc"
