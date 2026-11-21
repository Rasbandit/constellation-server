# Running the room server in a container

The image holds the room server, the web UI and the upload and generation API. Rooms and the UI share the one public port, 38281: `/` is the UI, a WebSocket upgrade on `/<key>` is a room. Flags and environment variables are in the README; the compose file runs `constellation-server --host 0.0.0.0 --port 38281 --rooms /data/rooms --data-dir /data` (the admin API shares the port; the image's own default is the same without `--data-dir`; its default command already has `--web-dir`).

```sh
cp .env.example .env   # set GEN_REPO_URL and GEN_REPO_REF too (see Building)
mkdir -p secrets
openssl rand -hex 24 > secrets/postgres_password
openssl rand -hex 24 > secrets/app_password
openssl rand -hex 24 > secrets/migrate_password
printf 'postgres://constellation_app:%s@postgres/constellation' "$(cat secrets/app_password)" > secrets/database_url
printf 'postgres://constellation_migrate:%s@postgres/constellation' "$(cat secrets/migrate_password)" > secrets/migrate_database_url
chmod 400 secrets/database_url secrets/migrate_database_url && sudo chown 10001 secrets/database_url secrets/migrate_database_url
docker compose up -d   # sign-up is open, see "The world API on the same port"
```

- **Database roles (least privilege).** The server never logs in as a Postgres superuser. `docker/postgres-init.sh` runs once, when Postgres first makes its data folder, and creates `constellation_migrate` (owns the database and its tables; runs the migrations) and `constellation_app` (the runtime role). After each migration the server grants `constellation_app` SELECT, INSERT, UPDATE and DELETE on the tables and USAGE on the sequences, plus the same on tables later migrations make (default privileges); it never gets CREATE, DDL, the right to touch `_sqlx_migrations`, or superuser. `postgres_password` is the bootstrap superuser, used only by that script and the health check. The names are `DB_MIGRATE_ROLE` and `DB_APP_ROLE` in `crates/config`. The migration URL is `CONSTELLATION_MIGRATE_DATABASE_URL` (or `_FILE`); if it is unset the server migrates as the runtime role and logs a warning, so a deployment from before the split keeps working but has not gained the protection. The init script does not run on an existing data folder: to move one over, create the roles and `ALTER ... OWNER` the tables yourself (see the PR that introduced this) or start from an empty volume.
- Secrets are files mounted as Docker secrets; the server reads the database URL from `CONSTELLATION_DATABASE_URL_FILE`. The server runs as uid 10001 and the mount keeps the host file's owner and mode, so `database_url` must be readable by that uid: the `chmod 400` and `chown 10001` above keep it private to it, (the Postgres entrypoint reads its password file as root, so `postgres_password` can stay `600`).
- Seed bundles (`*.json`) go in `/data/rooms` on the `data` volume, for example `docker compose cp seed.json server:/data/rooms/` (the server lists the folder, no restart needed for a new room) or a bind mount over `/data/rooms`. Each room logs `room ready key=<key>`.
- Postgres 18: the volume is mounted at `/var/lib/postgresql` with `PGDATA=/var/lib/postgresql/18/docker`, the layout the official image has used since 18.
- `restart: on-failure`, never `always`. Exit code 3 means a newer owner fenced this process, and restarting it makes two instances fight over one database. The image entrypoint (`docker/entrypoint.sh`) runs the server as a child, forwards SIGTERM and SIGINT to it, and turns exit 3 into 0 (logging `server exited 3 (fenced ...)`), so `on-failure` leaves it stopped. Every other exit code passes through and restarts.
- The image runs as a non-root user. It listens on plain HTTP and WebSocket; TLS and a reverse proxy are outside this file.

## DNS

A client reaches a room at `<host>:<port>/<key>` (the path is the room key). One host name and one ordinary TLS certificate serve every room: no wildcard DNS or wildcard certificate is needed. A proxy must pass the path through unchanged.

## Building
Generation needs the generator's source, which is not in this repo and whose URL is not committed. Pass it as build args `GEN_REPO_URL` and `GEN_REPO_REF` (a pinned commit SHA); the build fails with a message when either is empty. Compose reads them from `.env`; the release workflow reads `GEN_REPO_REF` from a repository variable (a SHA is harmless) and `GEN_REPO_URL` from a repository secret, so the URL is masked in public logs; the push job also turns off the build attestation, which would embed it. The image also holds python with the generator's dependencies, bubblewrap and `prlimit`.

## Web UI
The web stage of the Dockerfile builds `web/` from `bun.lock` (`bun install --frozen-lockfile`), runs the type check and `vite build`, and compresses every text file once (`.gz` and `.br` next to it, `web/scripts/precompress.mjs`). Only `dist/` reaches the runtime image, in `/opt/constellation/web`; no bun, node or dev dependency does.

The server (`--web-dir`) answers, in order: `/healthz`, `/version`, `/api/*` (the world API, below), a WebSocket upgrade at `/<key>` (the room; with `--bundle` any path), then the UI for every other GET and HEAD: an existing file, else `index.html` for a page load (no extension, not under `/assets/`, `Accept` takes HTML). A missing file, a missing `/assets/*` path or a request that does not want HTML is a 404, never the app. So a plain GET of `/<key>` loads the UI (its `/c/:code` and room pages) and an upgrade of the same URL joins the room. Files carry `ETag` and `Last-Modified`; `/assets/*` (hashed names) is `public, max-age=31536000, immutable`, everything else `no-cache`; the client's `Accept-Encoding` picks the stored `.br` or `.gz`. Every answer has `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY` and a CSP (`default-src 'self'`, `frame-ancestors 'none'`). Paths are not decoded and the resolved file must lie inside the folder, so `..`, encoded `..` and a symlink out all get the app or a 404. The UI's version badge reads `GET /version` from its own origin.

### The world API on the same port
There is one public port (`38281`). `/api/*` is served on it, always on when the server has `--data-dir` (compose sets it), and it comes before the room upgrade and the UI, so a room key can never shadow it and the UI never answers under `/api`. **Every `/api` path, even an unknown one, needs a signed-in session**; without one the answer is a bare 401 with no body. Five routes are open: `POST /api/signup`, `POST /api/login`, `POST /api/logout`, `GET /api/me` and `GET /api/auth` (which answers only `{"signup_open":bool}`; the UI reads it to offer sign-up).

**Accounts.** Anyone can sign up (`POST /api/signup`, a name of letters, digits, `_` and `-` and a password of at least `password_min_chars`), log in (`POST /api/login`), log out (`POST /api/logout`) and ask who they are (`GET /api/me`). A login is a random id in an `HttpOnly`, `SameSite=Lax` cookie (`Secure` when the request came over https, or `X-Forwarded-Proto: https` from the proxy), kept server-side for `session_ttl_secs`; passwords are stored as argon2id hashes in Postgres (in memory without a database, lost on restart). Sign-ups are counted per client address (`signup_burst`) and failed logins too (`login_fail_burst`), both 429 past the burst; `CONSTELLATION_SIGNUP=closed` turns sign-up off (existing accounts still log in). Any signed-in account may upload worlds and player files, generate worlds and list its rooms; a world it makes is owned by it. **The risk:** with sign-up open, anyone who can reach the port can make an account and have an uploaded `.apworld` run inside the sandbox; the sandbox and the rate limits are the guard, and closing sign-up is the switch. Game clients join by room key and need no account. Rooms made before accounts have no owner and are listed to nobody.

**Rooms and roles.** There is no site-wide admin. A room has one owner (its creator, never removable) and any number of room admins. The owner or a room admin may add any existing account as an admin (`PUT /api/rooms/<key>/admins/<name>` or `POST /api/rooms/<key>/admins`); the owner may remove any admin and an admin only itself (`DELETE`); `GET /api/rooms/<key>/admins` lists them. Someone with no role on a room gets 404, as for a room that does not exist.

**Upgrading:** the admin token is gone. `CONSTELLATION_ADMIN_TOKEN(_FILE)` and `docker-compose.token.yml` are ignored or removed; delete `secrets/admin_token`. Rooms made with the token have no owner, so no account lists them.

**Known limit:** the room view a player opens from a join link is part of `/api`, so a player needs an account to open it. Game clients are unaffected. A read-only player credential is #111.

Behind a proxy every client has the proxy's address, so the sign-up and login counts are shared (an IPv6 address counts as its /64); start the server with `--trusted-proxy-header X-Forwarded-For` (or `X-Real-IP`) so it counts per real client. Only the last entry of the last line of that header is used (the one your proxy appended), and the proxy must overwrite or append it: never turn the flag on when clients can reach the port without the proxy, because they could then send any address.

TLS comes from the reverse proxy; never expose the port over plain HTTP, since passwords, sessions and the uploads travel on it. For `const.ras.band` with nginx (plain pass-through, no auth of its own):

```
map $http_upgrade $connection_upgrade { default upgrade; "" close; }

server {
    listen 443 ssl;
    server_name const.ras.band;
    # ssl_certificate / ssl_certificate_key as usual

    # Room uploads: at least apworld_max_bytes (64 MiB); the server enforces its own caps.
    client_max_body_size 80m;

    location / {
        proxy_pass http://127.0.0.1:38281;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        # Generation and the room event stream are long requests; do not buffer them.
        proxy_read_timeout 600s;
        proxy_send_timeout 600s;
        proxy_buffering off;
        proxy_request_buffering off;
    }
}
```

Add `--trusted-proxy-header X-Forwarded-For` to the server command, and only if the proxy is the only way to reach the port. The compose file publishes `38281` on all interfaces (it also serves players directly), so the header would be forgeable by anyone who reaches the port directly: firewall it to the proxy first, or change the publish to `127.0.0.1:38281:38281` in your own override (the shipped default is left as is).

## Uploads
The world API (`--data-dir /data`, on the room port) stores uploaded world and player files, and game manifests, in Postgres (README). So are the generations' statuses. The `data` volume holds only a running legacy generation's job folder, removed when it ends; back up the `pgdata` volume. Give each server instance its own `data` volume: at startup a server clears the job folders it finds, as its own that stopped. Files an older image kept under `/data/apworlds/`, `/data/yamls/` and `/data/worlds/<id>/status.json` are moved into Postgres on the first start of this one.

Every route needs a session cookie: sign up once, then send the cookie jar.

```sh
curl -c jar -X POST --data-binary '{"username":"me","password":"a long password"}' http://127.0.0.1:38281/api/signup
curl -b jar -X PUT --data-binary @player.yaml "http://127.0.0.1:38281/api/yamls?name=player.yaml"
curl -b jar http://127.0.0.1:38281/api/yamls
```

### Sandbox inside a container
Uploaded code runs in bubblewrap (README, "Security model"). Two things differ in a container, and both are explicit opt-ins:
- No per-job cgroup: `CONSTELLATION_SANDBOX_MODE=container` skips `systemd-run --user`. Jobs are then bounded by `prlimit` (as before) and by the container: compose sets `mem_limit: 6g` and `pids_limit: 512` for the whole container, so one job can use more of the container's memory than the 4 GiB per-job cgroup allowed. Every bubblewrap isolation flag is unchanged. The server logs a warning at start in this mode.
- bubblewrap needs user namespaces, mounts and a fresh `/proc`, which Docker blocks by default. The compose file sets three `security_opt` entries, each shown necessary by a CI run (without the seccomp change bubblewrap failed on `unshare`/`mount`; with only that, on `make / slave`; then on mounting `/proc`):
  - `seccomp=./docker/seccomp-bwrap.json`: Docker's default profile plus an allow rule for `clone`, `unshare`, `setns`, `mount`, `umount`, `umount2` and `pivot_root`, without `CAP_SYS_ADMIN`. Re-sync it with Docker's default when you upgrade Docker.
  - `apparmor=unconfined`: the `docker-default` AppArmor profile denies `mount`. This drops AppArmor for this container (it only matters on hosts that use AppArmor).
  - `systempaths=unconfined`: Docker masks parts of `/proc` and `/sys`, and the kernel then refuses a new `/proc` mount inside a user namespace. This unmasks them for this container (for example `/proc/kcore` and `/proc/sys`; `/proc/sys` stays read-only to a process without capabilities).
  The container is not privileged and has no added capabilities. The tradeoff: this container (the server as well as its jobs) has a weaker container boundary than a default one, so a job that escaped bubblewrap would face less. Do not run other services in it, and keep the admin token secret and the port behind a TLS proxy.

## Versions and releases
One version covers the server, the web UI and the image: `version` under `[workspace.package]` in `Cargo.toml` (every crate inherits it). `web/package.json` must carry the same one; `tools/version-check/check.sh` checks that and the `version` workflow runs it on PRs that touch those files, so there is no per-PR bump. The server answers `GET /version` with `{"version":"<version>"}`; a build made with `CONSTELLATION_BUILD_SHA` set (the image's `BUILD_SHA` build arg, used for every non-release build) reads `<version>+<short sha>`. The web UI shows `server <v> / web <v>` in a corner badge and flags a difference.

To release: bump the version in `Cargo.toml` and `web/package.json` (and `Cargo.lock`, by building once), merge, then tag `v<version>`, for example `git tag v0.2.0 && git push origin v0.2.0`. The release workflow refuses a tag whose commit is not on main, and a tag that is not `v<Cargo version>`, then builds and pushes the image tagged `0.2.0` and `0.2` (and `latest`, unless the tag has a suffix such as `-rc1`) with a bare version at `/version`.

Pushing a tag `v*` runs `.github/workflows/release.yml`, which pushes `linux/amd64` to `ghcr.io/<owner>/constellation-server`. Pre-release tags (such as `v0.2.0-rc1`) do not move `latest`.

## CI
PRs never build the image. The `container-lint` workflow (Dockerfile lint, compose config check and the entrypoint test) runs only on PRs that touch the packaging. The `release` workflow builds the image and runs it under compose on every push to main and, when that passes, publishes it to GHCR as `main` and `sha-<short sha>` (one publish at a time, never cancelled mid-push); a `v*` tag publishes the version tags and `latest`; it can also be started by hand on any branch (Actions, release, Run workflow), which builds and runs without pushing.
