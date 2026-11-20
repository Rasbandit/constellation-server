# Running the room server in a container

The image holds the room server only: no web UI, no admin or generation API. Flags and environment variables are in the README; the image runs `constellation-server --host 0.0.0.0 --port 38281 --rooms /data/rooms`.

```sh
cp .env.example .env
mkdir -p secrets
openssl rand -hex 24 > secrets/postgres_password
printf 'postgres://constellation:%s@postgres/constellation' "$(cat secrets/postgres_password)" > secrets/database_url
chmod 400 secrets/database_url && sudo chown 10001 secrets/database_url
docker compose up -d
```

- Secrets are files mounted as Docker secrets; the server reads the database URL from `CONSTELLATION_DATABASE_URL_FILE`. The server runs as uid 10001 and the mount keeps the host file's owner and mode, so `database_url` must be readable by that uid: the `chmod 400` and `chown 10001` above keep it private to it (the Postgres entrypoint reads its password file as root, so `postgres_password` can stay `600`).
- Seed bundles (`*.json`) go in the `rooms` volume at `/data/rooms`, for example `docker compose cp seed.json server:/data/rooms/` (the server lists the folder, no restart needed for a new room) or a bind mount in place of the `rooms` volume. Each room logs `room ready key=<key>`.
- Postgres 19: the volume is mounted at `/var/lib/postgresql` with `PGDATA=/var/lib/postgresql/19/docker`, the layout the official image has used since 18.
- `restart: on-failure`, never `always`. Exit code 3 means a newer owner fenced this process, and restarting it makes two instances fight over one database. The image entrypoint (`docker/entrypoint.sh`) runs the server as a child, forwards SIGTERM and SIGINT to it, and turns exit 3 into 0 (logging `server exited 3 (fenced ...)`), so `on-failure` leaves it stopped. Every other exit code passes through and restarts.
- The image runs as a non-root user. It listens on plain HTTP and WebSocket; TLS and a reverse proxy are outside this file.

## DNS

A client reaches a room at `<key>.<host>:<port>` (the first label of the `Host` header is the room key). A deployment needs a wildcard DNS record `*.<host>` pointing at the server (and a wildcard TLS certificate if TLS ends in front of it). A proxy must pass `Host` through unchanged. For a trial on one machine, `<key>.127.0.0.1.sslip.io` resolves without DNS setup.

## Versions and releases
One version covers the server, the web UI and the image: `version` under `[workspace.package]` in `Cargo.toml` (every crate inherits it). `web/package.json` must carry the same one; `tools/version-check/check.sh` checks that and CI runs it, so there is no per-PR bump. The server answers `GET /version` with `{"version":"<version>"}`; a build made with `CONSTELLATION_BUILD_SHA` set (the image's `BUILD_SHA` build arg, used for every non-release build) reads `<version>+<short sha>`. The web UI shows `server <v> / web <v>` in a corner badge and flags a difference.

To release: bump the version in `Cargo.toml` and `web/package.json` (and `Cargo.lock`, by building once), merge, then tag `v<version>`, for example `git tag v0.2.0 && git push origin v0.2.0`. The release workflow refuses a tag that is not `v<Cargo version>`, then builds and pushes the image tagged `0.2.0` and `0.2` (and `latest`, unless the tag has a suffix such as `-rc1`) with a bare version at `/version`.

Pushing a tag `v*` runs `.github/workflows/release.yml`, which pushes `linux/amd64` to `ghcr.io/<owner>/constellation-server`. Pre-release tags (such as `v0.2.0-rc1`) do not move `latest`.
