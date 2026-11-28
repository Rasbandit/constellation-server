#!/bin/sh
# Plain-sh test of docker/db-roles.sh against a stub psql: it works with secret files of mode
# 600, and an unreadable or empty password file stops it before Postgres is touched.
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
check() { [ "$2" = "$3" ] || { echo "FAIL $1: got $2, want $3"; fail=1; }; }

mkdir "$tmp/bin" "$tmp/s"
cat > "$tmp/bin/psql" <<'S'
#!/bin/sh
echo called >> "$STUB_LOG"
cat >/dev/null
S
chmod +x "$tmp/bin/psql"
for f in postgres_password app_password migrate_password; do echo "pw-$f" > "$tmp/s/$f"; done
chmod 600 "$tmp/s"/*

run() { : > "$tmp/log"; PATH="$tmp/bin:$PATH" STUB_LOG="$tmp/log" SECRETS_DIR="$tmp/s" sh "$here/db-roles.sh" >/dev/null 2>&1; }
calls() { wc -l < "$tmp/log" | tr -d ' '; }

run; check "mode 600 files" "$?" 0; check "psql ran once" "$(calls)" 1
: > "$tmp/s/app_password"
run; check "empty app password" "$?" 1; check "no psql after empty" "$(calls)" 0
echo x > "$tmp/s/app_password"; rm "$tmp/s/migrate_password"
run; check "missing migrate password" "$?" 1; check "no psql after missing" "$(calls)" 0
exit $fail
