#!/bin/sh
# Runs once, when Postgres first makes its data folder (docker-entrypoint-initdb.d), as the
# bootstrap superuser. Makes the two roles the server uses, so it never logs in as a superuser:
#   constellation_migrate  owns the database and its tables; runs the migrations
#   constellation_app      the server's runtime role; the server grants it SELECT, INSERT, UPDATE
#                          and DELETE on the tables (and USAGE on sequences) after each migration
# The names are constants in crates/config (DB_MIGRATE_ROLE, DB_APP_ROLE); a test keeps them equal.
# Passwords come from the Docker secrets, read by psql itself so they never show in `ps`.
set -eu
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" -v db="$POSTGRES_DB" <<'SQL'
\set migrate_pw `head -n1 /run/secrets/migrate_password`
\set app_pw `head -n1 /run/secrets/app_password`
CREATE ROLE constellation_migrate LOGIN PASSWORD :'migrate_pw';
CREATE ROLE constellation_app LOGIN PASSWORD :'app_pw';
ALTER DATABASE :"db" OWNER TO constellation_migrate;
REVOKE ALL ON DATABASE :"db" FROM PUBLIC;
GRANT CONNECT ON DATABASE :"db" TO constellation_migrate, constellation_app;
ALTER SCHEMA public OWNER TO constellation_migrate;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
SQL
