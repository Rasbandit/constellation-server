import { describe, expect, it } from 'vitest';
import { applyEvent, toFeed, toRoom, type RoomEvent, type RoomView } from './roomView';

const W = 'c'.repeat(64);

const view: RoomView = {
  seed: '0189',
  room_key: 'ilypozn5hc',
  address: 'localhost:38281/ilypozn5hc',
  options: { password: false, hint_cost: 10, release_mode: 'auto', collect_mode: 'goal' },
  slots: [
    { slot: 1, team: 0, name: 'Rasbandit-MM', game: '2 Ship 2 Harkinian (MM)', connected: true, clients: 1, checked: 1, locations: 1516, items_received: 1, goal: false },
    { slot: 2, team: 0, name: 'Bob', game: 'Other', connected: false, clients: 0, checked: 0, locations: 10, items_received: 0, goal: true },
  ],
};

describe('toRoom', () => {
  it('takes every number and status from the server, inventing none', () => {
    const r = toRoom(W, view);
    expect(r).toMatchObject({
      id: W,
      state: 'running',
      address: 'localhost:38281/ilypozn5hc',
      seed: '0189',
      code: null,
      slots: 2,
      slotsOnline: 1,
      slotsDone: 1,
      games: ['2 Ship 2 Harkinian (MM)', 'Other'],
    });
    expect(r.slotList[0]).toEqual({
      slot: 1,
      name: 'Rasbandit-MM',
      game: '2 Ship 2 Harkinian (MM)',
      status: 'connected',
      checksDone: 1,
      checksTotal: 1516,
      itemsReceived: 1,
      hintPoints: null,
      lastSeen: undefined,
      clients: 1,
    });
    expect(r.slotList[1].status).toBe('goal');
    expect(r.rules).toMatchObject({ hintCostPct: 10, releaseMode: 'auto', collectMode: 'goal', password: '' });
    expect(toRoom(W, { ...view, options: { ...view.options, password: true } }).rules.password).not.toBe('');
  });
});

describe('toFeed', () => {
  const item: RoomEvent = {
    kind: 'item', seq: 7, from: 1, from_name: 'Alice', to: 2, to_name: 'Bob',
    item: 126, item_name: 'Ocarina', location: 1, location_name: 'Clock Town Chest', flags: 1,
  };

  it('describes an item sent to another player with its real names and flag', () => {
    expect(toFeed(item, 3)).toEqual({
      seq: 3,
      at: '',
      kind: 'item',
      text: 'Alice sent Ocarina to Bob (Clock Town Chest)',
      slots: [1, 2],
      item: { name: 'Ocarina', flag: 'progression', from: 1, to: 2, location: 'Clock Town Chest' },
    });
  });

  it('describes an own-world item, unnamed ids, joins, leaves, chat and goals', () => {
    expect(toFeed({ ...item, to: 1, to_name: 'Alice', flags: 0 }, 1)?.text).toBe('Alice found their Ocarina (Clock Town Chest)');
    expect(toFeed({ ...item, item_name: null, location_name: null, flags: 4 }, 1)).toMatchObject({
      text: 'Alice sent item 126 to Bob (location 1)',
      item: { flag: 'trap' },
    });
    expect(toFeed({ kind: 'join', slot: 1, name: 'Alice' }, 1)).toMatchObject({ kind: 'join', text: 'Alice connected', slots: [1] });
    expect(toFeed({ kind: 'leave', slot: 1, name: 'Alice' }, 1)).toMatchObject({ kind: 'leave', text: 'Alice disconnected' });
    expect(toFeed({ kind: 'chat', slot: 2, name: 'Bob', text: 'hi' }, 1)).toMatchObject({ kind: 'chat', text: 'Bob: hi' });
    expect(toFeed({ kind: 'goal', seq: 9, slot: 2, name: 'Bob' }, 1)).toMatchObject({ kind: 'goal', text: 'Bob reached their goal' });
  });

  it('shows nothing for a check alone (its item event says it) or an unknown kind', () => {
    expect(toFeed({ kind: 'checked', seq: 6, slot: 1, location: 1 }, 1)).toBeNull();
    expect(toFeed({ kind: 'later' } as unknown as RoomEvent, 1)).toBeNull();
  });
});

describe('applyEvent', () => {
  const join: RoomEvent = { kind: 'join', slot: 1, name: 'Alice' };
  const leave: RoomEvent = { kind: 'leave', slot: 1, name: 'Alice' };

  it('appends each shown event numbered in stream order and skips the rest', () => {
    let feed = applyEvent([], join);
    feed = applyEvent(feed, { kind: 'checked', seq: 1, slot: 1, location: 1 });
    feed = applyEvent(feed, leave);
    expect(feed.map((f) => [f.seq, f.text])).toEqual([
      [1, 'Alice connected'],
      [2, 'Alice disconnected'],
    ]);
  });

  it('drops everything shown on a reset (the room restarted), then starts again', () => {
    const before = applyEvent(applyEvent([], join), leave);
    const after = applyEvent(before, { kind: 'reset' });
    expect(after).toEqual([]);
    expect(applyEvent(after, join).map((f) => f.seq)).toEqual([1]);
  });

  it('keeps the newest 400 lines', () => {
    let feed: ReturnType<typeof applyEvent> = [];
    for (let i = 0; i < 405; i++) feed = applyEvent(feed, join);
    expect(feed.length).toBe(400);
    expect(feed[399].seq).toBe(405);
  });
});
