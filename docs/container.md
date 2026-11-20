# Running the room server in a container

The image holds the room server only: no web UI, no admin or generation API. Flags and environment variables are in the README; the image runs `constellation-server --host 0.0.0.0 --port 38281 --rooms /data/rooms`.

```sh
cp .env.example .env
mkdir -p secrets
openssl rand -hex 24 > secrets/postgres_password
printf 'postgres://constellation:%s@postgres/constellation' "$(cat secrets/postgres_password)" > secrets/database_url
docker compose up -d
```

- Secrets are files mounted as Docker secrets; the server reads the database URL from `CONSTELLATION_DATABASE_URL_FILE`.
- Seed bundles (`*.json`) go in the `rooms` volume at `/data/rooms`. Each room logs `room ready key=<key>`.
- `restart: on-failure`, never `always`: exit code 3 means a newer owner fenced this process, and restarting it makes two instances fight over one database.
- The image runs as a non-root user. It listens on plain HTTP and WebSocket; TLS and a reverse proxy are outside this file.

## DNS

A client reaches a room at `<key>.<host>:<port>` (the first label of the `Host` header is the room key). A deployment needs a wildcard DNS record `*.<host>` pointing at the server (and a wildcard TLS certificate if TLS ends in front of it). A proxy must pass `Host` through unchanged. For a trial on one machine, `<key>.127.0.0.1.sslip.io` resolves without DNS setup.

## Releases

Pushing a tag `v*` runs `.github/workflows/release.yml`, which pushes `linux/amd64` to `ghcr.io/<owner>/constellation-server`.
