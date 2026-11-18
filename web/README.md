# Web UI (React prototype)

A React + Vite front end for creating, playing and hosting rooms. Every `/api/*` endpoint is a stub: `mock/` answers it inside the Vite dev server with seeded, in-memory data, and always takes the happy path. The real server implements the same contract later.

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
| POST | `/api/apworlds` | raw file, header `X-File-Name` | `ApWorld` |
| POST | `/api/player-files` | raw YAML, header `X-File-Name` | `PlayerFile` |
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

## Moving to the real server

Remove `mockApi()` from `vite.config.ts` and proxy `/api` to the server (`server.proxy`). The stub's shortcuts the server must replace: APWorld game and version come from the file name (the server should read the archive), YAML fields come from two regexes (the server should parse and validate options), and the admin key is a shared demo value.
