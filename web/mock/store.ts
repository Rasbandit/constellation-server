import type {
  AuditEntry,
  CreateRoomRequest,
  FeedItem,
  Hint,
  HintStatus,
  Room,
  RoomRules,
  RoomState,
  RoomSummary,
  Slot,
  SlotNo,
  SlotStatus,
} from '../src/api/types';
import type { ApWorldRecord, YamlRecord } from '../src/api/uploads';
import { BUILTIN_GAMES, catalogFor } from './catalog';

// In-memory state for the stub API. It resets when the dev server restarts.

interface RoomRecord {
  room: Room;
  feed: FeedItem[];
  hints: Hint[];
  audit: AuditEntry[];
  passwords: Map<SlotNo, string>;
}

/** Small seeded generator so the seeded rooms look the same on every start. */
function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

const rooms = new Map<string, RoomRecord>();
type Listener = (item: FeedItem) => void;
const listeners = new Map<string, Set<Listener>>();

const apworlds = new Map<string, ApWorldRecord>();
/** Live join codes to room ids. A closed room's code goes back to the pool. */
const codes = new Map<string, string>();
const CODE_CHARS = 'ABCDEFGHJKMNPQRSTUVWXYZ23456789'; // no 0/O, 1/I/L

function newCode(roomId: string, r: () => number = Math.random): string {
  for (;;) {
    const code = Array.from({ length: 6 }, () => CODE_CHARS[Math.floor(r() * CODE_CHARS.length)]).join('');
    if (!codes.has(code)) {
      codes.set(code, roomId);
      return code;
    }
  }
}
let nextId = 1;
const id = (prefix: string) => `${prefix}-${(nextId++).toString(36)}${Math.random().toString(36).slice(2, 6)}`;

export const DEMO_ADMIN_KEY = 'demo-admin';
/** How long the stub pretends generation takes. */
export const GENERATE_MS = 2500;

const defaultRules: RoomRules = {
  hintCostPct: 10,
  releaseMode: 'goal',
  collectMode: 'goal',
  password: '',
  spoilerLog: true,
};

function summary(r: Room): RoomSummary {
  return {
    id: r.id,
    name: r.name,
    state: r.state,
    createdAt: r.createdAt,
    slots: r.slotList.length,
    slotsOnline: r.slotList.filter((s) => s.status !== 'offline').length,
    slotsDone: r.slotList.filter((s) => s.status === 'goal').length,
    games: [...new Set(r.slotList.map((s) => s.game))].sort(),
    code: r.code,
    adminKey: r.adminKey,
  };
}

function push(rec: RoomRecord, entry: Omit<FeedItem, 'seq' | 'at'> & { at?: string }): FeedItem {
  const last = rec.feed[rec.feed.length - 1];
  const item: FeedItem = { ...entry, seq: (last?.seq ?? 0) + 1, at: entry.at ?? new Date().toISOString() };
  rec.feed.push(item);
  if (rec.feed.length > 500) rec.feed.shift();
  for (const l of listeners.get(rec.room.id) ?? []) l(item);
  return item;
}

function audit(rec: RoomRecord, action: string, detail: string) {
  rec.audit.unshift({ at: new Date().toISOString(), action, detail });
}

const name = (rec: RoomRecord, slot: SlotNo) => rec.room.slotList.find((s) => s.slot === slot)?.name ?? `Slot ${slot}`;

/** One location check: `from` finds an item for `to`. Updates counters and the feed. */
function sendItem(rec: RoomRecord, from: SlotNo, to: SlotNo, r: () => number, at?: string, itemName?: string) {
  const fromSlot = rec.room.slotList.find((s) => s.slot === from)!;
  const toSlot = rec.room.slotList.find((s) => s.slot === to)!;
  const cat = catalogFor(toSlot.game);
  const item = itemName
    ? (cat.items.find((i) => i.name === itemName) ?? { name: itemName, flag: 'filler' as const })
    : cat.items[Math.floor(r() * cat.items.length)];
  const fromCat = catalogFor(fromSlot.game);
  // Each check takes the slot's next location; past the named ones, numbered copies.
  const n = fromSlot.checksDone;
  const base = fromCat.locations[n % fromCat.locations.length];
  const round = Math.floor(n / fromCat.locations.length);
  const location = round === 0 ? base : `${base} ${round + 1}`;
  if (fromSlot.checksDone < fromSlot.checksTotal) fromSlot.checksDone++;
  toSlot.itemsReceived++;
  const text =
    from === to
      ? `${fromSlot.name} found their ${item.name} (${location})`
      : `${fromSlot.name} sent ${item.name} to ${toSlot.name} (${location})`;
  push(rec, { kind: 'item', text, slots: [from, to], at, item: { name: item.name, flag: item.flag, from, to, location } });
  for (const h of rec.hints) {
    if (h.finding === from && h.location === location && h.status !== 'found') h.status = 'found';
  }
}

function seedRoom(opts: {
  id: string;
  name: string;
  state: RoomState;
  createdMinutesAgo: number;
  port: number;
  players: [string, string, SlotStatus, number][];
  seed: number;
}): RoomRecord {
  const r = rng(opts.seed);
  const created = Date.now() - opts.createdMinutesAgo * 60_000;
  const slotList: Slot[] = opts.players.map(([n, game, status, total], i) => ({
    slot: i + 1,
    name: n,
    game,
    status,
    checksDone: 0,
    checksTotal: total,
    itemsReceived: 0,
    hintPoints: 0,
    lastSeen: status === 'offline' ? new Date(created + 20 * 60_000).toISOString() : new Date().toISOString(),
    clients: status === 'offline' ? 0 : 1,
  }));
  const room: Room = {
    id: opts.id,
    name: opts.name,
    state: opts.state,
    createdAt: new Date(created).toISOString(),
    slots: slotList.length,
    slotsOnline: 0,
    slotsDone: 0,
    games: [],
    code: opts.state === 'closed' ? null : newCode(opts.id, r),
    adminKey: DEMO_ADMIN_KEY,
    address: `constellation.example.net:${opts.port}/${opts.id}`,
    seed: Math.floor(r() * 1e12).toString().padStart(12, '0'),
    rules: { ...defaultRules },
    slotList,
  };
  const rec: RoomRecord = { room, feed: [], hints: [], audit: [], passwords: new Map() };
  let t = created;
  for (const s of slotList) {
    t += 30_000;
    push(rec, { kind: 'join', text: `${s.name} (${s.game}) joined`, slots: [s.slot], at: new Date(t).toISOString() });
  }
  const steps = Math.floor(slotList.reduce((a, s) => a + s.checksTotal, 0) * 0.45);
  for (let i = 0; i < steps; i++) {
    t += 25_000 + r() * 40_000;
    const from = slotList[Math.floor(r() * slotList.length)];
    if (from.checksDone >= from.checksTotal) continue;
    const to = slotList[Math.floor(r() * slotList.length)];
    sendItem(rec, from.slot, to.slot, r, new Date(Math.min(t, Date.now())).toISOString());
  }
  for (const s of slotList) {
    s.hintPoints = s.checksDone * 2;
    if (s.status === 'goal') {
      s.checksDone = s.checksTotal;
      push(rec, { kind: 'goal', text: `${s.name} reached their goal`, slots: [s.slot] });
    }
  }
  const statuses: HintStatus[] = ['priority', 'unspecified', 'no_priority', 'avoid', 'priority', 'found'];
  for (let i = 0; i < 9; i++) {
    const finding = slotList[Math.floor(r() * slotList.length)];
    const receiving = slotList[Math.floor(r() * slotList.length)];
    const cat = catalogFor(receiving.game);
    const item = cat.items[Math.floor(r() * 3)];
    const fc = catalogFor(finding.game);
    const hint: Hint = {
      id: `h${i + 1}`,
      finding: finding.slot,
      receiving: receiving.slot,
      item: item.name,
      flag: item.flag,
      location: fc.locations[Math.floor(r() * fc.locations.length)],
      entrance: r() < 0.3 ? 'via Sunken Stairs' : undefined,
      status: statuses[i % statuses.length],
    };
    rec.hints.push(hint);
    push(rec, {
      kind: 'hint',
      text: `[Hint] ${receiving.name}'s ${hint.item} is at ${hint.location} in ${finding.name}'s world`,
      slots: [finding.slot, receiving.slot],
    });
  }
  push(rec, { kind: 'chat', text: 'Kestrel: anyone have my Moonlit Key? stuck at the gate', slots: [1] });
  push(rec, { kind: 'chat', text: 'Juniper: checking the mine now, hang on', slots: [2] });
  audit(rec, 'Room created', `${slotList.length} slots, seed ${room.seed}`);
  rooms.set(room.id, rec);
  return rec;
}

seedRoom({
  id: 'friday-7k2q',
  name: 'Friday Night Multiworld',
  state: 'running',
  createdMinutesAgo: 140,
  port: 38281,
  seed: 7,
  players: [
    ['Kestrel', 'Starfall Keep', 'playing', 120],
    ['Juniper', 'Tidewater Farm', 'playing', 160],
    ['Brass', 'Clockwork Depths', 'connected', 95],
    ['Wick', 'Lantern Run', 'goal', 80],
    ['Pathfinder', 'Archipela-Go', 'offline', 60],
  ],
});
seedRoom({
  id: 'async-h4x9',
  name: 'Async: The Long Haul',
  state: 'running',
  createdMinutesAgo: 60 * 24 * 6,
  port: 38282,
  seed: 21,
  players: [
    ['Moss', 'Tidewater Farm', 'offline', 160],
    ['Cinder', 'Starfall Keep', 'playing', 120],
    ['Gale', 'Clockwork Depths', 'offline', 95],
  ],
});
seedRoom({
  id: 'sunday-p2m8',
  name: 'Sunday Sprint',
  state: 'closed',
  createdMinutesAgo: 60 * 24 * 2,
  port: 38283,
  seed: 3,
  players: [
    ['Rook', 'Lantern Run', 'goal', 80],
    ['Vale', 'Archipela-Go', 'goal', 60],
  ],
});

// ---- Live updates --------------------------------------------------------------------------

export function subscribe(roomId: string, l: Listener): () => void {
  const set = listeners.get(roomId) ?? new Set();
  set.add(l);
  listeners.set(roomId, set);
  return () => set.delete(l);
}

/** Pretend players are playing: one random check per tick in every running room. */
const live = rng(99);
setInterval(() => {
  for (const rec of rooms.values()) {
    if (!listeners.get(rec.room.id)?.size) continue;
    if (rec.room.state === 'lobby') {
      // Pretend players connect one by one while the host waits.
      const waiting = rec.room.slotList.filter((s) => s.status === 'offline');
      if (waiting.length > 0 && live() < 0.7) {
        const s = waiting[Math.floor(live() * waiting.length)];
        s.status = 'connected';
        s.clients = 1;
        s.lastSeen = new Date().toISOString();
        push(rec, { kind: 'join', text: `${s.name} (${s.game}) connected`, slots: [s.slot] });
      }
      continue;
    }
    if (rec.room.state !== 'running') continue;
    const playing = rec.room.slotList.filter((s) => s.status === 'playing' && s.checksDone < s.checksTotal);
    if (playing.length === 0) continue;
    const from = playing[Math.floor(live() * playing.length)];
    const to = rec.room.slotList[Math.floor(live() * rec.room.slotList.length)];
    from.lastSeen = new Date().toISOString();
    sendItem(rec, from.slot, to.slot, live);
  }
}, 6000).unref?.();

// ---- Queries and commands the API calls ----------------------------------------------------

export class NotFound extends Error {}
export class Refused extends Error {}
/** The upload API's 422: the file was read and refused. */
export class Unprocessable extends Error {}

/** Stand-in for the server's SHA-256 content address: 64 hex characters, same bytes same id. */
function fakeContentId(bytes: Uint8Array, salt: string): string {
  let h = 2166136261;
  const feed = (b: number) => {
    h ^= b;
    h = Math.imul(h, 16777619) >>> 0;
  };
  for (const b of bytes) feed(b);
  if (bytes.length === 0) for (const c of salt) feed(c.charCodeAt(0));
  let out = '';
  for (let i = 0; i < 8; i++) {
    feed(i);
    out += h.toString(16).padStart(8, '0');
  }
  return out;
}

function get(roomId: string): RoomRecord {
  const rec = rooms.get(roomId);
  if (!rec) throw new NotFound(`No room ${roomId}`);
  return rec;
}

function room(rec: RoomRecord): Room {
  return { ...rec.room, ...summary(rec.room) };
}

function slotOf(rec: RoomRecord, slot: SlotNo): Slot {
  const s = rec.room.slotList.find((x) => x.slot === slot);
  if (!s) throw new Refused(`No slot ${slot}`);
  return s;
}

export const store = {
  builtinGames: () => BUILTIN_GAMES,

  listRooms: (): RoomSummary[] =>
    [...rooms.values()].map((r) => summary(r.room)).sort((a, b) => b.createdAt.localeCompare(a.createdAt)),

  room: (roomId: string) => room(get(roomId)),
  feed: (roomId: string, after: number) => get(roomId).feed.filter((f) => f.seq > after),
  hints: (roomId: string) => get(roomId).hints,
  audit: (roomId: string) => get(roomId).audit,
  adminKeyOk: (roomId: string, key: string | undefined) => get(roomId).room.adminKey === key,

  /** The room a join code points at. Codes are case-insensitive. */
  resolveCode(code: string): { roomId: string } {
    const roomId = codes.get(code.toUpperCase());
    if (!roomId) throw new NotFound(`No open room has code ${code}`);
    return { roomId };
  },

  /**
   * Stub of `PUT /api/apworlds`. The real server unzips the file in a sandbox and reads its
   * manifest; the stub reads game and version from the name (`starfall_keep-1.2.0.apworld`).
   */
  addApWorld(fileName: string, bytes: Uint8Array): ApWorldRecord {
    if (!fileName.toLowerCase().endsWith('.apworld')) throw new Unprocessable('not an .apworld file');
    const id = fakeContentId(bytes, fileName);
    const stored = apworlds.get(id);
    if (stored) return stored;
    const stem = fileName.replace(/\.apworld$/i, '');
    const m = /^(.*?)[-_ ]v?(\d+(?:\.\d+){1,2})$/.exec(stem);
    const raw = m ? m[1] : stem;
    const game = raw
      .replace(/[_-]+/g, ' ')
      .trim()
      .replace(/\b\w/g, (c) => c.toUpperCase());
    const module = raw.toLowerCase().replace(/[^a-z0-9_]+/g, '_').replace(/^(\d)/, '_$1');
    const rec: ApWorldRecord = { module, game, world_version: m ? m[2] : null, id, name: fileName, bytes: bytes.length };
    apworlds.set(id, rec);
    return rec;
  },

  /**
   * Stub of `PUT /api/yamls`: `name:` and `game:` of each `---` document. The real server reads
   * it with a YAML parser in a sandbox. A weighted `game:` mapping becomes "A, B", as there.
   */
  addYaml(fileName: string, bytes: Uint8Array): YamlRecord {
    const text = new TextDecoder().decode(bytes);
    const players = text
      .split(/^---[ \t]*$/m)
      .filter((doc) => doc.trim() !== '')
      .map((doc, i) => {
        const field = (k: string) => {
          const m = new RegExp(`^${k}:[ \\t]*(.*)$`, 'm').exec(doc);
          return m ? m[1].trim().replace(/^['"]|['"]$/g, '') : null;
        };
        const name = field('name');
        let game = field('game');
        if (game === '') {
          // `game:` followed by an indented mapping of weights
          const block = /^game:[ \t]*\n((?:[ \t]+.*\n?)+)/m.exec(doc)?.[1] ?? '';
          game = [...block.matchAll(/^[ \t]+['"]?([^'":\n]+?)['"]?:/gm)].map((x) => x[1]).sort().join(', ') || null;
        }
        if (!name) throw new Unprocessable(`document ${i + 1} has no name`);
        if (!game) throw new Unprocessable(`document ${i + 1} has no game`);
        return { name, game };
      });
    if (players.length === 0) throw new Unprocessable('no player documents');
    return { players, id: fakeContentId(bytes, fileName), name: fileName, bytes: bytes.length };
  },

  createRoom(req: CreateRoomRequest): Room {
    const roomId = `${req.name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 20) || 'room'}-${Math.random().toString(36).slice(2, 6)}`;
    const now = new Date().toISOString();
    const slotList: Slot[] = req.players.map((p, i) => ({
      slot: i + 1,
      name: p.name,
      game: p.game!,
      status: 'offline',
      checksDone: 0,
      checksTotal: catalogFor(p.game!).locations.length * 12,
      itemsReceived: 0,
      hintPoints: 0,
      lastSeen: null,
      clients: 0,
    }));
    const r: Room = {
      id: roomId,
      name: req.name.trim(),
      state: 'generating',
      createdAt: now,
      slots: slotList.length,
      slotsOnline: 0,
      slotsDone: 0,
      games: [],
      code: newCode(roomId),
      adminKey: Math.random().toString(36).slice(2, 12),
      address: `constellation.example.net:${38281 + rooms.size}/${roomId}`,
      seed: Math.floor(Math.random() * 1e12).toString().padStart(12, '0'),
      rules: { ...req.rules },
      slotList,
    };
    const rec: RoomRecord = { room: r, feed: [], hints: [], audit: [], passwords: new Map() };
    rooms.set(roomId, rec);
    audit(rec, 'Room created', `${slotList.length} slots, seed ${r.seed}`);
    push(rec, { kind: 'admin', text: `Generating the world for ${slotList.length} slots`, slots: [] });
    // Stub: generation "takes" a few seconds, then the room waits for players.
    setTimeout(() => {
      if (rec.room.state !== 'generating') return;
      rec.room.state = 'lobby';
      push(rec, { kind: 'admin', text: 'World generated. Players can connect now.', slots: [] });
      audit(rec, 'World generated', `seed ${r.seed}`);
    }, GENERATE_MS);
    return room(rec);
  },

  chat(roomId: string, slot: SlotNo, text: string): FeedItem {
    const rec = get(roomId);
    return push(rec, { kind: 'chat', text: `${name(rec, slot)}: ${text}`, slots: [slot] });
  },

  setHintStatus(roomId: string, hintId: string, status: HintStatus): Hint {
    const h = get(roomId).hints.find((x) => x.id === hintId);
    if (!h) throw new NotFound(`No hint ${hintId}`);
    if (h.status === 'found' || status === 'found') throw new Refused('A found hint cannot change');
    h.status = status;
    return h;
  },

  admin: {
    sendItem(roomId: string, to: SlotNo, item: string, count: number) {
      const rec = get(roomId);
      const s = slotOf(rec, to);
      for (let i = 0; i < count; i++) {
        s.itemsReceived++;
        push(rec, {
          kind: 'admin',
          text: `Host sent ${item} to ${s.name}`,
          slots: [to],
          item: { name: item, flag: catalogFor(s.game).items.find((x) => x.name === item)?.flag ?? 'filler', from: 0, to, location: 'Host' },
        });
      }
      audit(rec, 'Sent item', `${count} x ${item} to ${s.name}`);
    },
    hint(roomId: string, slot: SlotNo, item: string) {
      const rec = get(roomId);
      const s = slotOf(rec, slot);
      const finding = rec.room.slotList[Math.floor(Math.random() * rec.room.slotList.length)];
      const loc = catalogFor(finding.game).locations[0];
      const hint: Hint = {
        id: id('h'),
        finding: finding.slot,
        receiving: slot,
        item,
        flag: catalogFor(s.game).items.find((x) => x.name === item)?.flag ?? 'filler',
        location: loc,
        status: 'priority',
      };
      rec.hints.unshift(hint);
      push(rec, { kind: 'hint', text: `[Hint] ${s.name}'s ${item} is at ${loc} in ${finding.name}'s world`, slots: [slot, finding.slot] });
      audit(rec, 'Gave hint', `${item} for ${s.name} (free)`);
      return hint;
    },
    release(roomId: string, slot: SlotNo) {
      const rec = get(roomId);
      const s = slotOf(rec, slot);
      const left = s.checksTotal - s.checksDone;
      s.checksDone = s.checksTotal;
      push(rec, { kind: 'release', text: `${s.name} released ${left} remaining items`, slots: [slot] });
      audit(rec, 'Released', `${s.name} (${left} items)`);
    },
    collect(roomId: string, slot: SlotNo) {
      const rec = get(roomId);
      const s = slotOf(rec, slot);
      push(rec, { kind: 'collect', text: `${s.name} collected their items from every world`, slots: [slot] });
      audit(rec, 'Collected', s.name);
    },
    kick(roomId: string, slot: SlotNo) {
      const rec = get(roomId);
      const s = slotOf(rec, slot);
      s.status = 'offline';
      s.clients = 0;
      push(rec, { kind: 'leave', text: `${s.name} was disconnected by the host`, slots: [slot] });
      audit(rec, 'Kicked', s.name);
    },
    broadcast(roomId: string, text: string) {
      const rec = get(roomId);
      push(rec, { kind: 'admin', text: `Host: ${text}`, slots: [] });
      audit(rec, 'Broadcast', text);
    },
    updateSlot(roomId: string, slot: SlotNo, patch: { name?: string; password?: string }) {
      const rec = get(roomId);
      const s = slotOf(rec, slot);
      if (patch.name !== undefined) {
        const taken = rec.room.slotList.some((x) => x.slot !== slot && x.name.toLowerCase() === patch.name!.toLowerCase());
        if (taken || patch.name.trim() === '') throw new Refused('That name is empty or taken');
        audit(rec, 'Renamed slot', `${s.name} to ${patch.name}`);
        s.name = patch.name;
      }
      if (patch.password !== undefined) {
        rec.passwords.set(slot, patch.password);
        audit(rec, 'Set slot password', patch.password ? s.name : `${s.name} (cleared)`);
      }
      return s;
    },
    updateRules(roomId: string, patch: Partial<RoomRules>) {
      const rec = get(roomId);
      rec.room.rules = { ...rec.room.rules, ...patch };
      audit(rec, 'Changed rules', Object.entries(patch).map(([k, v]) => `${k} = ${k === 'password' ? '***' : String(v)}`).join(', '));
      return rec.room.rules;
    },
    setState(roomId: string, state: RoomState) {
      const rec = get(roomId);
      const from = rec.room.state;
      const allowed: Record<RoomState, RoomState[]> = {
        generating: ['closed'],
        lobby: ['running', 'closed'],
        running: ['paused', 'closed'],
        paused: ['running', 'closed'],
        closed: [],
      };
      if (!allowed[from].includes(state)) throw new Refused(`A ${from} room cannot become ${state}`);
      rec.room.state = state;
      if (from === 'lobby' && state === 'running') {
        for (const s of rec.room.slotList) if (s.status === 'connected' || s.status === 'ready') s.status = 'playing';
        push(rec, { kind: 'admin', text: 'The host started the game. Good luck!', slots: [] });
        audit(rec, 'Started', `${rec.room.slotList.filter((s) => s.status === 'playing').length} of ${rec.room.slotList.length} connected`);
        return;
      }
      if (state === 'closed' && rec.room.code) {
        codes.delete(rec.room.code);
        rec.room.code = null;
      }
      push(rec, { kind: 'admin', text: `Room ${state === 'running' ? 'resumed' : state}`, slots: [] });
      audit(rec, 'Room state', state);
    },
  },
};
