# Context Doc: FastRaid deploy

_Last verified: 2026-11-21_

## Status
Working

## What This Is
How constellation-server and its Postgres run on FastRaid, and how every merge to `main` auto-deploys.

## Environment
FastRaid (Unraid). Two DockerMan containers, not compose: `constellation-server` and `constellation-postgres`, on Docker network `constellation`.
Templates: `/boot/config/plugins/dockerMan/templates-user/my-constellation-server.xml` and `my-constellation-postgres.xml`.
Appdata: `/mnt/cache/appdata/constellation/{pgdata,secrets,data}`.

## Auto-deploy
A User Scripts job `constellation-deploy` (`/boot/config/plugins/user.scripts/scripts/constellation-deploy/script`, state in `/mnt/cache/appdata/constellation-deploy/`):
1. Polls ghcr.io for a new `:main` digest.
2. Rewrites the template's `<Repository>` to `:sha-<7>`.
3. Syncs `docker/seccomp-bwrap.json` from the repo at that revision.
4. Recreates via Unraid's `update_container`.
5. Waits up to 60 s for `/healthz`; on failure rolls back to the previous tag.

So EVERY merge to main that publishes an image auto-deploys within minutes. The release workflow's smoke job gates image publish; if smoke fails nothing deploys (PR #132 fixed the `/api/nope` 401 expectation).

## DB roles
Runtime `constellation_app` (data rights only); migrations `constellation_migrate`. Set up by `docker/db-roles.sh`:
`docker exec -i -e PGHOST=/var/run/postgresql constellation-postgres sh -s < db-roles.sh`
Verify: `pg_stat_activity` shows only `constellation_app` connected; `pg_roles` has `rolsuper=f`.

## Gotchas
- A change needing a new env var, secret file or DB change must have the template/secrets updated BEFORE the image lands, or the container crash-loops and the script rolls back. Happened with PR #130 (`CONSTELLATION_MIGRATE_DATABASE_URL_FILE`). Fix: add the `<Config Type="Variable">` to the XML, then run `/usr/local/emhttp/plugins/dynamix.docker.manager/scripts/update_container constellation-server` inside the 60 s window.

## Backups (from the 2026-11-21 wipe)
- `/mnt/user/appdata/constellation-backups/pre-accounts-202611210344.dump`
- `pgdata.old-2026-11-20` and `secrets.old-2026-11-20` beside the live folders
- Template backup `my-constellation-server.xml.bak-pre-accounts`

## References
`docker/db-roles.sh`, `docker/seccomp-bwrap.json`, release workflow smoke job, PRs #130 and #132.
