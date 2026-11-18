import { toApWorld, toPlayers, type ApWorldRecord, type YamlRecord } from './uploads';
import type {
  ApiError,
  AuditEntry,
  CreateRoomRequest,
  FeedItem,
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

export const api = {
  games: () => call<{ builtin: string[]; known: string[] }>('GET', '/api/games'),
  catalog: (game: string) => call<GameCatalog>('GET', `/api/games/${enc(game)}`),
  /** Inspected and stored by the server; the same file again returns the stored one. */
  uploadApWorld: (f: File) => call<ApWorldRecord>('PUT', `/api/apworlds?name=${enc(f.name)}`, { raw: f }).then(toApWorld),
  /** One player per document in the file. */
  uploadPlayerFile: (f: File) => call<YamlRecord>('PUT', `/api/yamls?name=${enc(f.name)}`, { raw: f }).then(toPlayers),

  rooms: () => call<RoomSummary[]>('GET', '/api/rooms'),
  resolveCode: (code: string) => call<{ roomId: string }>('GET', `/api/codes/${enc(code)}`),
  createRoom: (req: CreateRoomRequest) => call<Room>('POST', '/api/rooms', { body: req }),
  room: (id: string, adminKey?: string) =>
    call<Room>('GET', `/api/rooms/${enc(id)}`, { headers: adminKey ? { 'X-Admin-Key': adminKey } : {} }),
  feed: (id: string, after = 0) => call<FeedItem[]>('GET', `/api/rooms/${enc(id)}/feed?after=${after}`),
  hints: (id: string) => call<Hint[]>('GET', `/api/rooms/${enc(id)}/hints`),
  chat: (id: string, slot: SlotNo, text: string) => call<FeedItem>('POST', `/api/rooms/${enc(id)}/chat`, { body: { slot, text } }),
  setHintStatus: (id: string, hint: string, status: HintStatus) =>
    call<Hint>('POST', `/api/rooms/${enc(id)}/hints/${enc(hint)}`, { body: { status } }),
  streamUrl: (id: string) => `/api/rooms/${enc(id)}/stream`,

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
