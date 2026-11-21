#!/bin/sh
# Makes the two Postgres roles the server uses, so it never logs in as a superuser (docs/container.md):
#   constellation_migrate  owns the database and its tables; runs the migrations
#   constellation_app      the server's runtime role; the server grants it SELECT, INSERT, UPDATE
#                          and DELETE on the tables (and USAGE on sequences) after each migration
# The names are constants in crates/config (DB_MIGRATE_ROLE, DB_APP_ROLE); a test keeps them equal.
# Run once as a one-shot container (the `db-roles` compose service, or `docker run` by hand), as
# root, so secret files of mode 600 are readable. Safe to run again: it only (re)sets passwords
# and ownership. Both password files are read and checked BEFORE Postgres is touched: an
# unreadable or empty one stops it with a non-zero exit and no role is made or changed.
#
# Environment: PGHOST (default postgres), PGUSER and PGDATABASE (default constellation), and
# SECRETS_DIR (default /run/secrets) holding postgres_password (the bootstrap superuser's),
# app_password and migrate_password.
set -eu
dir=${SECRETS_DIR:-/run/secrets}
secret() {
    [ -r "$dir/$1" ] || { echo "db-roles: cannot read $dir/$1" >&2; exit 1; }
    v=$(head -n1 "$dir/$1")
    [ -n "$v" ] || { echo "db-roles: $dir/$1 is empty" >&2; exit 1; }
    printf '%s' "$v"
}
PGPASSWORD=$(secret postgres_password)
APP_PW=$(secret app_password)
MIGRATE_PW=$(secret migrate_password)
export PGPASSWORD APP_PW MIGRATE_PW
PGHOST=${PGHOST:-postgres} PGUSER=${PGUSER:-constellation} PGDATABASE=${PGDATABASE:-constellation}
export PGHOST PGUSER PGDATABASE
# The passwords go to psql through the environment, never an argument, so `ps` does not show them.
psql -1 -v ON_ERROR_STOP=1 -v db="$PGDATABASE" <<'SQL'
\set migrate_pw `printenv MIGRATE_PW`
\set app_pw `printenv APP_PW`
SELECT 'CREATE ROLE constellation_migrate LOGIN'
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'constellation_migrate') \gexec
SELECT 'CREATE ROLE constellation_app LOGIN'
 WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'constellation_app') \gexec
ALTER ROLE constellation_migrate PASSWORD :'migrate_pw';
ALTER ROLE constellation_app PASSWORD :'app_pw';
ALTER DATABASE :"db" OWNER TO constellation_migrate;
REVOKE ALL ON DATABASE :"db" FROM PUBLIC;
GRANT CONNECT ON DATABASE :"db" TO constellation_migrate, constellation_app;
ALTER SCHEMA public OWNER TO constellation_migrate;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
SQL
echo "db-roles: roles ready"
