import type { CollectMode, FeedItem, ItemFlag, ReleaseMode, Room, Slot } from './types';

// The server's read-only room view (`GET /api/worlds/:id/room` and its SSE stream
// `/api/worlds/:id/room/events`; crates/edge-compat/src/view.rs) turned into what the pages use.
// Everything comes from the running room; what the server does not track stays empty (null or
// undefined), never a made-up value.

export interface RoomView {
  seed: string;
  room_key: string;
  address: string;
  options: { password: boolean; hint_cost: number; release_mode: ReleaseMode; collect_mode: CollectMode };
  slots: {
    slot: number;
    team: number;
    name: string;
    game: string;
    connected: boolean;
    clients: number;
    checked: number;
    locations: number;
    items_received: number;
    goal: boolean;
  }[];
}

/** One entry of the room's event stream. `seq` is set on durable events only. */
export type RoomEvent =
  | { kind: 'checked'; seq: number; slot: number; name?: string; location: number; location_name?: string | null }
  | {
      kind: 'item';
      seq: number;
      from: number;
      from_name: string | null;
      to: number;
      to_name: string | null;
      item: number;
      item_name: string | null;
      location: number;
      location_name: string | null;
      flags: number;
    }
  | { kind: 'goal'; seq: number; slot: number; name: string | null }
  | { kind: 'join' | 'leave' | 'chat'; slot: number; name: string | null; text?: string | null }
  /** The room restarted (or the viewer fell too far behind): drop what was shown. */
  | { kind: 'reset' };

/** Feed lines a page keeps. */
export const FEED_LINES = 400;

/** Item flags: bit 0 progression, bit 1 useful, bit 2 trap. */
function flagOf(flags: number): ItemFlag {
  if (flags & 1) return 'progression';
  if (flags & 2) return 'useful';
  if (flags & 4) return 'trap';
  return 'filler';
}

export function toRoom(id: string, v: RoomView): Room {
  const slotList: Slot[] = v.slots.map((s) => ({
    slot: s.slot,
    name: s.name,
    game: s.game,
    status: s.goal ? 'goal' : s.connected ? 'connected' : 'offline',
    checksDone: s.checked,
    checksTotal: s.locations,
    itemsReceived: s.items_received,
    hintPoints: null,
    lastSeen: undefined,
    clients: s.clients,
  }));
  return {
    id,
    name: `Room ${v.room_key}`,
    // The server runs a room as soon as it hosts it: checks and items flow, there is no lobby or pause.
    state: 'running',
    createdAt: '',
    slots: slotList.length,
    slotsOnline: v.slots.filter((s) => s.connected).length,
    slotsDone: v.slots.filter((s) => s.goal).length,
    games: [...new Set(v.slots.map((s) => s.game))],
    code: null,
    address: v.address,
    seed: v.seed,
    rules: {
      hintCostPct: v.options.hint_cost,
      releaseMode: v.options.release_mode,
      collectMode: v.options.collect_mode,
      // The server says only whether one is set; the pages only ask that.
      password: v.options.password ? 'set' : '',
      spoilerLog: false,
    },
    slotList,
  };
}

/** A feed line for an event, or null when it has nothing to show. `n` orders the lines. */
export function toFeed(e: RoomEvent, n: number): FeedItem | null {
  const base = { seq: n, at: '' };
  switch (e.kind) {
    case 'item': {
      const from = e.from_name ?? `player ${e.from}`;
      const name = e.item_name ?? `item ${e.item}`;
      const where = e.location_name ?? `location ${e.location}`;
      const text = e.from === e.to ? `${from} found their ${name} (${where})` : `${from} sent ${name} to ${e.to_name ?? `player ${e.to}`} (${where})`;
      return {
        ...base,
        kind: 'item',
        text,
        slots: [...new Set([e.from, e.to])],
        item: { name, flag: flagOf(e.flags), from: e.from, to: e.to, location: where },
      };
    }
    case 'goal':
      return { ...base, kind: 'goal', text: `${e.name ?? `player ${e.slot}`} reached their goal`, slots: [e.slot] };
    case 'join':
    case 'leave':
    case 'chat': {
      const who = e.name ?? `player ${e.slot}`;
      const text = e.kind === 'chat' ? `${who}: ${e.text ?? ''}` : `${who} ${e.kind === 'join' ? 'connected' : 'disconnected'}`;
      return { ...base, kind: e.kind, text, slots: [e.slot] };
    }
    default:
      return null;
  }
}

/** The feed after one stream event: a reset empties it, a shown event is appended. */
export function applyEvent(feed: FeedItem[], e: RoomEvent): FeedItem[] {
  if (e.kind === 'reset') return [];
  const item = toFeed(e, (feed.at(-1)?.seq ?? 0) + 1);
  return item ? [...feed, item].slice(-FEED_LINES) : feed;
}
