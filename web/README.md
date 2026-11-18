# Web UI (React prototype)

A React + Vite front end for creating, playing and hosting rooms. On its own it runs on a stub: `mock/` answers every `/api/*` endpoint inside the Vite dev server with seeded, in-memory demo data and always takes the happy path. Pointed at a real server (see "Uploads through the server"), it creates and shows real rooms instead.

Note: `docs/ROADMAP.md` (Tech choices) picks server-rendered Askama + htmx for the web UI. This prototype is a separate exploration; which stack ships is the maintainers' decision.

## Run

```sh
cd web
bun install          # or npm install
bun --bun run dev    # or npm run dev; http://localhost:5173
bun --bun run test   # vitest: readiness rules and the stub API
bun --bun run build  # type-check and bundle
```

The seeded rooms use the admin key `demo-admin`. The stub hands every room's admin key to the home page so you can open each admin view.

## Pages

| Route | For | What it does |
|---|---|---|
| `/` | everyone | Create or join a room; active and closed rooms with who is online and who is done |
| `/create` | host | Room name and rules, player YAMLs (or a player with game defaults), APWorlds. A checklist shows what blocks generation: missing APWorlds per game, duplicate world versions, unreadable YAMLs, empty, padded, overlong or duplicate connection names. Connection names are editable inline. |
| `/rooms/:id` | players | Connect details (server, slot name, password) with copy buttons; "I am" picks your slot (kept in this browser) and personalises the page: your progress, items sent to you highlighted, hints split into "you are looking for" and "in your world for others", and hint status you can set. Live feed with filters; chat. |
| `/rooms/:id/admin` | host | Room stats, pause, resume, close (with confirmation). Per player: send item (autocomplete from the game's item list), give a free hint, rename, set slot password, release, collect, disconnect (each destructive action asks twice). Editable rules with a save bar, broadcast, and a log of every host action. |

The admin link carries `?key=`; the page stores the key in this browser and removes it from the address bar.

## API contract

Types are in `src/api/types.ts`. Names follow CWP v1 (`crates/proto/schema/cwp-v1.json`) where they overlap.

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/api/games` | | `{ builtin, known }` game names |
| GET | `/api/games/:game` | | `GameCatalog` (items with flags, locations) |
| PUT | `/api/apworlds?name=<file>` | raw `.apworld` | 201 (200 if already stored) `{id, name, bytes, module, game, world_version, ...}` |
| PUT | `/api/yamls?name=<file>` | raw `.yaml` | 201 (200 if already stored) `{id, name, bytes, players: [{name, game}]}` |
| GET | `/api/rooms` | | `RoomSummary[]` |
| POST | `/api/rooms/check` | `CreateRoomRequest` | readiness checks |
| POST | `/api/rooms` | `CreateRoomRequest` | 201 `Room` with `adminKey`, or 422 with the failing checks |
| GET | `/api/rooms/:id` | | `Room` (`adminKey` only with a matching `X-Admin-Key`) |
| GET | `/api/rooms/:id/feed?after=seq` | | `FeedItem[]` after that `seq` |
| GET | `/api/rooms/:id/stream` | | server-sent events, `event: feed`, `id:` = `seq` |
| GET | `/api/rooms/:id/hints` | | `Hint[]` |
| POST | `/api/rooms/:id/hints/:hint` | `{ status }` | `Hint` |
| POST | `/api/rooms/:id/chat` | `{ slot, text }` | `FeedItem` |

Admin endpoints need `X-Admin-Key` (403 without it), under `/api/rooms/:id/admin`:

| Method | Path | Body |
|---|---|---|
| GET | `/audit` | |
| POST | `/items` | `{ to, item, count }` |
| POST | `/hints` | `{ slot, item }` |
| POST | `/slots/:slot/release`, `/collect`, `/kick` | |
| PATCH | `/slots/:slot` | `{ name?, password? }` |
| POST | `/broadcast` | `{ text }` |
| PATCH | `/rules` | partial `RoomRules` |
| POST | `/state` | `{ state: "running" \| "paused" \| "closed" }` |

The feed is resumable: entries carry the room log `seq`, and after a reconnect the page asks for `feed?after=<last seq>` before relying on the stream again.

The two upload routes are the server's own (branch `feat/web-upload-rooms`, `crates/server/src/worlds.rs`): same paths, raw bodies, records and status codes, with errors as plain text. `src/api/uploads.ts` turns the records into what the pages use: one player per YAML document, and a weighted `game` ("A, B") needs an APWorld for each game it can roll.

## Uploads through the server

Point the dev server at the server's admin API and the upload, generation and room view routes (`/api/apworlds`, `/api/yamls`, `/api/worlds`) go there:

```sh
CONSTELLATION_ADMIN_URL=http://127.0.0.1:<admin port> \
CONSTELLATION_ADMIN_TOKEN_FILE=<file with the token> \
bun --bun run dev
```

The admin port is the server's `admin api addr=` log line (it listens on loopback; use an SSH tunnel for a remote one). `CONSTELLATION_ADMIN_TOKEN` works instead of the file. The dev server adds `Authorization: Bearer <token>` when it forwards, so the token never reaches the browser. The server inspects uploads in a bubblewrap sandbox, so it needs Linux.

With the admin API configured, the pages show the real server and nothing from the stub:

- **Create room** posts the uploaded ids to `POST /api/worlds` and polls `GET /api/worlds/:id` until the server hosts the world as a room (the server needs `--rooms` as well as `--data-dir`). The room is the world: its page is `/rooms/<world id>`, named by its room key. Every player must come from an uploaded YAML. The page asks for no room name, password or rules, because the server cannot take them from it yet (the password is the server's `CONSTELLATION_ROOM_PASSWORD`); a second world with a seed the server already hosts fails instead of sharing that room.
- **The room pages** read `GET /api/worlds/:id/room` (seed, key, join address `{room_key}.{host}:{port}` from the server's `--public-host`, and per slot: connected from live sessions, checks of total locations, items received, goal) and the event stream `GET /api/worlds/:id/room/events` (server-sent events: items, checks, goals, joins, leaves, chat). The stream resumes by itself; after a server restart it sends a reset and the page starts its feed over. A new stream replays at most the newest `view_history_events` (400) events, and at most `view_streams` (64) are open at once (`crates/config`).
- **Not available yet**, so hidden: send item, free hints, rename, slot passwords, release, collect, disconnect, pause, start, close, broadcast, rules, the host action log, chat from the page, hints, the room list and room codes. The admin page says so. A renamed player still connects with the name in their YAML.

Only the dev server's own pages can use the proxy: a request for a proxied path whose `Host` is not a loopback name, or whose `Origin` or `Sec-Fetch-Site` says it comes from another page, gets 403 before the token is added (`mock/proxy.ts`, `adminGuard`). Without the proxy, every page carries a "Demo data" banner: rooms, players, checks and activity are made up by the stub, and the shared demo admin key stands in for host sign-in.
