# Running the room server in a container

The image holds the room server and its upload and generation API (no web UI). Flags and environment variables are in the README; the compose file runs `constellation-server --host 0.0.0.0 --port 38281 --rooms /data/rooms --data-dir /data` plus the admin listener (the image's own default is the same without `--data-dir`).

```sh
cp .env.example .env   # set GEN_REPO_URL and GEN_REPO_REF too (see Building)
mkdir -p secrets
openssl rand -hex 24 > secrets/postgres_password
openssl rand -hex 16 > secrets/admin_token
printf 'postgres://constellation:%s@postgres/constellation' "$(cat secrets/postgres_password)" > secrets/database_url
chmod 400 secrets/database_url secrets/admin_token && sudo chown 10001 secrets/database_url secrets/admin_token
docker compose up -d
```

- Secrets are files mounted as Docker secrets; the server reads the database URL from `CONSTELLATION_DATABASE_URL_FILE`. The server runs as uid 10001 and the mount keeps the host file's owner and mode, so `database_url` must be readable by that uid: the `chmod 400` and `chown 10001` above keep it private to it, and the same for `admin_token` (the Postgres entrypoint reads its password file as root, so `postgres_password` can stay `600`).
- Seed bundles (`*.json`) go in `/data/rooms` on the `data` volume, for example `docker compose cp seed.json server:/data/rooms/` (the server lists the folder, no restart needed for a new room) or a bind mount over `/data/rooms`. Each room logs `room ready key=<key>`.
- Postgres 18: the volume is mounted at `/var/lib/postgresql` with `PGDATA=/var/lib/postgresql/18/docker`, the layout the official image has used since 18.
- `restart: on-failure`, never `always`. Exit code 3 means a newer owner fenced this process, and restarting it makes two instances fight over one database. The image entrypoint (`docker/entrypoint.sh`) runs the server as a child, forwards SIGTERM and SIGINT to it, and turns exit 3 into 0 (logging `server exited 3 (fenced ...)`), so `on-failure` leaves it stopped. Every other exit code passes through and restarts.
- The image runs as a non-root user. It listens on plain HTTP and WebSocket; TLS and a reverse proxy are outside this file.

## DNS

A client reaches a room at `<key>.<host>:<port>` (the first label of the `Host` header is the room key). A deployment needs a wildcard DNS record `*.<host>` pointing at the server (and a wildcard TLS certificate if TLS ends in front of it). A proxy must pass `Host` through unchanged. For a trial on one machine, `<key>.127.0.0.1.sslip.io` resolves without DNS setup.

## Building
Generation needs the generator's source, which is not in this repo and whose URL is not committed. Pass it as build args `GEN_REPO_URL` and `GEN_REPO_REF` (a pinned commit SHA); the build fails with a message when either is empty. Compose reads them from `.env`; the release workflow reads `GEN_REPO_REF` from a repository variable (a SHA is harmless) and `GEN_REPO_URL` from a repository secret, so the URL is masked in public logs; the push job also turns off the build attestation, which would embed it. The image also holds python with the generator's dependencies, bubblewrap and `prlimit`.

## Uploads
The admin API (`--data-dir /data`, port 38282) stores uploaded files as `/data/<kind>/<game>/<sha256>.<ext>` (`/data/apworlds/...`, `/data/yamls/...`; layout in the README). Game manifests will land in `/data/manifests/<game>/` once their upload route ships (#101). Back up the `data` volume and the `pgdata` volume together: uploads are files, the room logs and finished seeds are in Postgres.

The API runs uploaded code, so the compose file publishes it on `127.0.0.1` only. Reach it by SSH (`ssh -L 38282:127.0.0.1:38282 host`) or through a TLS proxy. Every route needs `Authorization: Bearer <token>`; the token is the Docker secret `secrets/admin_token` (at least 32 bytes: `openssl rand -hex 16 > secrets/admin_token`; `chmod 400` and `sudo chown 10001` it like the database URL).

```sh
TOKEN=$(sudo cat secrets/admin_token)
curl -X PUT -H "Authorization: Bearer $TOKEN" --data-binary @player.yaml \
  "http://127.0.0.1:38282/api/yamls?name=player.yaml"
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:38282/api/yamls
```

### Sandbox inside a container
Uploaded code runs in bubblewrap (README, "Security model"). Two things differ in a container, and both are explicit opt-ins:
- No per-job cgroup: `CONSTELLATION_SANDBOX_MODE=container` skips `systemd-run --user`. Jobs are then bounded by `prlimit` (as before) and by the container: compose sets `mem_limit: 6g` and `pids_limit: 512` for the whole container, so one job can use more of the container's memory than the 4 GiB per-job cgroup allowed. Every bubblewrap isolation flag is unchanged. The server logs a warning at start in this mode.
- bubblewrap needs user namespaces, mounts and a fresh `/proc`, which Docker blocks by default. The compose file sets three `security_opt` entries, each shown necessary by a CI run (without the seccomp change bubblewrap failed on `unshare`/`mount`; with only that, on `make / slave`; then on mounting `/proc`):
  - `seccomp=./docker/seccomp-bwrap.json`: Docker's default profile plus an allow rule for `clone`, `unshare`, `setns`, `mount`, `umount`, `umount2` and `pivot_root`, without `CAP_SYS_ADMIN`. Re-sync it with Docker's default when you upgrade Docker.
  - `apparmor=unconfined`: the `docker-default` AppArmor profile denies `mount`. This drops AppArmor for this container (it only matters on hosts that use AppArmor).
  - `systempaths=unconfined`: Docker masks parts of `/proc` and `/sys`, and the kernel then refuses a new `/proc` mount inside a user namespace. This unmasks them for this container (for example `/proc/kcore` and `/proc/sys`; `/proc/sys` stays read-only to a process without capabilities).
  The container is not privileged and has no added capabilities. The tradeoff: this container (the server as well as its jobs) has a weaker container boundary than a default one, so a job that escaped bubblewrap would face less. Do not run other services in it, keep the admin API on loopback, and keep the admin token secret.

## Versions and releases
One version covers the server, the web UI and the image: `version` under `[workspace.package]` in `Cargo.toml` (every crate inherits it). `web/package.json` must carry the same one; `tools/version-check/check.sh` checks that and the `version` workflow runs it on PRs that touch those files, so there is no per-PR bump. The server answers `GET /version` with `{"version":"<version>"}`; a build made with `CONSTELLATION_BUILD_SHA` set (the image's `BUILD_SHA` build arg, used for every non-release build) reads `<version>+<short sha>`. The web UI shows `server <v> / web <v>` in a corner badge and flags a difference.

To release: bump the version in `Cargo.toml` and `web/package.json` (and `Cargo.lock`, by building once), merge, then tag `v<version>`, for example `git tag v0.2.0 && git push origin v0.2.0`. The release workflow refuses a tag whose commit is not on main, and a tag that is not `v<Cargo version>`, then builds and pushes the image tagged `0.2.0` and `0.2` (and `latest`, unless the tag has a suffix such as `-rc1`) with a bare version at `/version`.

Pushing a tag `v*` runs `.github/workflows/release.yml`, which pushes `linux/amd64` to `ghcr.io/<owner>/constellation-server`. Pre-release tags (such as `v0.2.0-rc1`) do not move `latest`.

## CI
PRs never build the image. The `container-lint` workflow (Dockerfile lint, compose config check and the entrypoint test) runs only on PRs that touch the packaging. The `release` workflow builds the image and runs it under compose on every push to main and, when that passes, publishes it to GHCR as `main` and `sha-<short sha>` (one publish at a time, never cancelled mid-push); a `v*` tag publishes the version tags and `latest`; it can also be started by hand on any branch (Actions, release, Run workflow), which builds and runs without pushing.
