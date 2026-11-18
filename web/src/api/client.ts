import { toRoom, type RoomView } from './roomView';
import { toApWorld, toPlayers, type ApWorldRecord, type YamlRecord } from './uploads';
import type {
  ApiError,
  AuditEntry,
  CreateRoomRequest,
  FeedItem,
  GeneratedWorld,
  GameCatalog,
  Hint,
  HintStatus,
  Room,
  RoomRules,
  RoomState,
  RoomSummary,
  Slot,
  SlotNo,
} from './types';

export class HttpError extends Error {
  constructor(
    public status: number,
    public body: ApiError,
  ) {
    super(body.error);
  }
}

async function call<T>(method: string, path: string, opts: { body?: unknown; raw?: Blob; headers?: Record<string, string> } = {}): Promise<T> {
  const headers: Record<string, string> = { ...opts.headers };
  let body: BodyInit | undefined;
  if (opts.raw) body = opts.raw;
  else if (opts.body !== undefined) {
    headers['Content-Type'] = 'application/json';
    body = JSON.stringify(opts.body);
  }
  const res = await fetch(path, { method, headers, body });
  const text = await res.text();
  let data: unknown;
  try {
    data = text ? JSON.parse(text) : undefined;
  } catch {
    data = undefined;
  }
  if (!res.ok) {
    // The upload API answers errors in plain text; the stub answers `{ error }`.
    const body = (data as ApiError | undefined)?.error ? (data as ApiError) : { error: text || res.statusText };
    throw new HttpError(res.status, body);
  }
  return data as T;
}

const enc = encodeURIComponent;

// Set by vite.config.ts when the dev server proxies /api/worlds to the server's admin API.
declare const __CONSTELLATION_ADMIN_API__: boolean | undefined;
const adminApi = typeof __CONSTELLATION_ADMIN_API__ !== 'undefined' && __CONSTELLATION_ADMIN_API__;

/**
 * True when the pages show a real room from the server (the admin proxy is on): rooms are
 * generated worlds, by world id, read through `/api/worlds/:id/room`. False: the stub's demo data.
 */
export const liveRooms = adminApi;

/** `GET /api/worlds/:id` (crates/server/src/worlds.rs). */
interface WorldStatus {
  id: string;
  status: 'queued' | 'running' | 'done' | 'failed';
  error?: string;
  seed_name?: string;
  room_key?: string;
  address?: string;
}

const POLL_MS = 1000;
/** Polls after `done` before deciding the server does not host worlds (no `--rooms`); it scans every 2 s. */
const ROOM_POLLS = 30;

/**
 * Generate the room's world on the server from the uploaded files and wait until the server
 * hosts it as a room. Every player must come from an uploaded YAML (the server has no defaults).
 */
export async function generateWorld(
  req: CreateRoomRequest,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<GeneratedWorld> {
  if (req.players.length === 0 || req.players.some((p) => !p.yamlId)) {
    throw new Error('Every player needs an uploaded YAML to generate on the server');
  }
  const body = { apworlds: [...new Set(req.apworlds.map((w) => w.id))], yamls: [...new Set(req.players.map((p) => p.yamlId))] };
  const job = await call<{ id: string }>('POST', '/api/worlds', { body });
  for (let done = 0; done < ROOM_POLLS; ) {
    const s = await call<WorldStatus>('GET', `/api/worlds/${enc(job.id)}`);
    if (s.status === 'failed') throw new Error(`Generation failed: ${s.error ?? 'no reason given'}`);
    if (s.room_key && s.address) return { id: s.id, seed: s.seed_name ?? '', roomKey: s.room_key, address: s.address };
    if (s.status === 'done') done += 1;
    await wait(POLL_MS);
  }
  throw new Error('The world was generated but the server never hosted it; start the server with --rooms');
}

export const api = {
  games: () => call<{ builtin: string[]; known: string[] }>('GET', '/api/games'),
  catalog: (game: string) => call<GameCatalog>('GET', `/api/games/${enc(game)}`),
  /** Inspected and stored by the server; the same file again returns the stored one. */
  uploadApWorld: (f: File) => call<ApWorldRecord>('PUT', `/api/apworlds?name=${enc(f.name)}`, { raw: f }).then(toApWorld),
  /** One player per document in the file. */
  uploadPlayerFile: (f: File) => call<YamlRecord>('PUT', `/api/yamls?name=${enc(f.name)}`, { raw: f }).then(toPlayers),

  rooms: () => call<RoomSummary[]>('GET', '/api/rooms'),
  resolveCode: (code: string) => call<{ roomId: string }>('GET', `/api/codes/${enc(code)}`),
  /** With the admin API, the server generates and hosts the world, and the room is that world. */
  createRoom: (req: CreateRoomRequest): Promise<Room> =>
    adminApi ? generateWorld(req).then((w) => api.room(w.id)) : call<Room>('POST', '/api/rooms', { body: req }),
  room: (id: string, adminKey?: string): Promise<Room> =>
    adminApi
      ? call<RoomView>('GET', `/api/worlds/${enc(id)}/room`).then((v) => toRoom(id, v))
      : call<Room>('GET', `/api/rooms/${enc(id)}`, { headers: adminKey ? { 'X-Admin-Key': adminKey } : {} }),
  /** Live rooms: the event stream itself carries the history (and resumes with Last-Event-ID). */
  feed: (id: string, after = 0): Promise<FeedItem[]> =>
    adminApi ? Promise.resolve([]) : call<FeedItem[]>('GET', `/api/rooms/${enc(id)}/feed?after=${after}`),
  /** Live rooms: the server stores no hints yet, so there are none to show. */
  hints: (id: string): Promise<Hint[]> => (adminApi ? Promise.resolve([]) : call<Hint[]>('GET', `/api/rooms/${enc(id)}/hints`)),
  chat: (id: string, slot: SlotNo, text: string) => call<FeedItem>('POST', `/api/rooms/${enc(id)}/chat`, { body: { slot, text } }),
  setHintStatus: (id: string, hint: string, status: HintStatus) =>
    call<Hint>('POST', `/api/rooms/${enc(id)}/hints/${enc(hint)}`, { body: { status } }),
  streamUrl: (id: string) => (adminApi ? `/api/worlds/${enc(id)}/room/events` : `/api/rooms/${enc(id)}/stream`),
  /** The SSE event name: the server's stream uses the default one, the stub names its entries `feed`. */
  streamEvent: adminApi ? 'message' : 'feed',

  admin: (id: string, key: string) => {
    const h = { 'X-Admin-Key': key };
    const base = `/api/rooms/${enc(id)}/admin`;
    return {
      audit: () => call<AuditEntry[]>('GET', `${base}/audit`, { headers: h }),
      sendItem: (to: SlotNo, item: string, count: number) => call('POST', `${base}/items`, { headers: h, body: { to, item, count } }),
      hint: (slot: SlotNo, item: string) => call<Hint>('POST', `${base}/hints`, { headers: h, body: { slot, item } }),
      release: (slot: SlotNo) => call('POST', `${base}/slots/${slot}/release`, { headers: h }),
      collect: (slot: SlotNo) => call('POST', `${base}/slots/${slot}/collect`, { headers: h }),
      kick: (slot: SlotNo) => call('POST', `${base}/slots/${slot}/kick`, { headers: h }),
      updateSlot: (slot: SlotNo, patch: { name?: string; password?: string }) =>
        call<Slot>('PATCH', `${base}/slots/${slot}`, { headers: h, body: patch }),
      broadcast: (text: string) => call('POST', `${base}/broadcast`, { headers: h, body: { text } }),
      updateRules: (patch: Partial<RoomRules>) => call<RoomRules>('PATCH', `${base}/rules`, { headers: h, body: patch }),
      setState: (state: RoomState) => call('POST', `${base}/state`, { headers: h, body: { state } }),
    };
  },
};
