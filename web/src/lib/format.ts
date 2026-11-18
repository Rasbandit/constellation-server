import type { HintStatus, ItemFlag, RoomState, SlotStatus } from '../api/types';

export function ago(iso: string | null, now = Date.now()): string {
  if (!iso) return 'never';
  const s = Math.max(0, Math.round((now - new Date(iso).getTime()) / 1000));
  if (s < 45) return 'just now';
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 36) return `${h} h ago`;
  return `${Math.round(h / 24)} d ago`;
}

export function clock(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export const slotStatusLabel: Record<SlotStatus, string> = {
  offline: 'Offline',
  connected: 'Connected',
  ready: 'Ready',
  playing: 'Playing',
  goal: 'Goal done',
};

export const roomStateLabel: Record<RoomState, string> = {
  generating: 'Generating',
  lobby: 'Waiting to start',
  running: 'Running',
  paused: 'Paused',
  closed: 'Closed',
};

export const hintStatusLabel: Record<HintStatus, string> = {
  priority: 'Priority',
  unspecified: 'Unmarked',
  no_priority: 'Not needed',
  avoid: 'Avoid',
  found: 'Found',
};

export const flagLabel: Record<ItemFlag, string> = {
  progression: 'Progression',
  useful: 'Useful',
  trap: 'Trap',
  filler: 'Filler',
};

export function pct(done: number, total: number): number {
  return total === 0 ? 0 : Math.round((done / total) * 100);
}

/** The text a player pastes into most clients' connect box. */
export function address(host: string, port: number): string {
  return `${host}:${port}`;
}
