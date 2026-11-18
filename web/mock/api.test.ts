import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type { CreateRoomRequest, Room, RoomSummary } from '../src/api/types';
import { handle } from './plugin';
import { DEMO_ADMIN_KEY, GENERATE_MS } from './store';

let server: Server;
let base = '';

beforeAll(async () => {
  server = createServer((req, res) => {
    handle(req, res).then((done) => {
      if (!done) res.writeHead(404).end();
    });
  });
  await new Promise<void>((ok) => server.listen(0, ok));
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});
afterAll(() => server.close());

const json = (method: string, path: string, body?: unknown, headers: Record<string, string> = {}) =>
  fetch(base + path, {
    method,
    headers: { 'Content-Type': 'application/json', ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

describe('stub API', () => {
  it('lists the seeded rooms', async () => {
    const rooms = (await (await json('GET', '/api/rooms')).json()) as RoomSummary[];
    expect(rooms.map((r) => r.id)).toContain('friday-7k2q');
  });

  it('reads the game from an uploaded YAML and an APWorld file name', async () => {
    const p = await (
      await fetch(`${base}/api/player-files`, { method: 'POST', headers: { 'X-File-Name': 'a.yaml' }, body: 'name: Alice\ngame: Starfall Keep\n' })
    ).json();
    expect(p).toMatchObject({ name: 'Alice', game: 'Starfall Keep' });
    const w = await (
      await fetch(`${base}/api/apworlds`, { method: 'POST', headers: { 'X-File-Name': 'starfall_keep-1.2.0.apworld' }, body: 'x' })
    ).json();
    expect(w).toMatchObject({ game: 'Starfall Keep', version: '1.2.0' });
  });

  it('refuses a room that is not ready and creates one that is', async () => {
    const req: CreateRoomRequest = {
      name: 'Test',
      apworlds: [],
      players: [{ id: 'p', fileName: 'a.yaml', name: 'Alice', game: 'Starfall Keep' }],
      rules: { hintCostPct: 10, releaseMode: 'goal', collectMode: 'goal', password: '', spoilerLog: true },
    };
    const refused = await json('POST', '/api/rooms', req);
    expect(refused.status).toBe(422);

    const ok = await json('POST', '/api/rooms', { ...req, players: [{ ...req.players[0], game: 'Archipela-Go' }] });
    expect(ok.status).toBe(201);
    const room = (await ok.json()) as Room;
    expect(room.slotList[0]).toMatchObject({ name: 'Alice', status: 'offline' });
    expect(room.adminKey).toBeTruthy();
  });

  it('needs the admin key for admin actions, and records them', async () => {
    const path = '/api/rooms/friday-7k2q/admin/items';
    expect((await json('POST', path, { to: 1, item: 'Moonlit Key' })).status).toBe(403);
    expect((await json('POST', path, { to: 1, item: 'Moonlit Key' }, { 'X-Admin-Key': DEMO_ADMIN_KEY })).status).toBe(200);
    const audit = await (await json('GET', '/api/rooms/friday-7k2q/admin/audit', undefined, { 'X-Admin-Key': DEMO_ADMIN_KEY })).json();
    expect(audit[0]).toMatchObject({ action: 'Sent item' });
  });

  it('hides the admin key from players', async () => {
    const room = (await (await json('GET', '/api/rooms/friday-7k2q')).json()) as Room;
    expect(room.adminKey).toBeUndefined();
  });

  it('answers unknown rooms and endpoints with 404', async () => {
    expect((await json('GET', '/api/rooms/nope')).status).toBe(404);
    expect((await json('GET', '/api/nothing')).status).toBe(404);
  });

  it('generates, waits in the lobby, starts, and frees its code on close', async () => {
    const req: CreateRoomRequest = {
      name: 'Lifecycle',
      apworlds: [],
      players: [{ id: 'p', fileName: null, name: 'Solo', game: 'Archipela-Go', defaults: true }],
      rules: { hintCostPct: 0, releaseMode: 'goal', collectMode: 'goal', password: '', spoilerLog: false },
    };
    const room = (await (await json('POST', '/api/rooms', req)).json()) as Room;
    expect(room.state).toBe('generating');
    expect(room.code).toMatch(/^[A-Z2-9]{6}$/);

    const found = await (await json('GET', `/api/codes/${room.code!.toLowerCase()}`)).json();
    expect(found).toEqual({ roomId: room.id });

    const key = { 'X-Admin-Key': room.adminKey! };
    const state = (s: string) => json('POST', `/api/rooms/${room.id}/admin/state`, { state: s }, key);
    expect((await state('running')).status).toBe(400); // not before generation ends

    await new Promise((ok) => setTimeout(ok, GENERATE_MS + 200));
    expect(((await (await json('GET', `/api/rooms/${room.id}`)).json()) as Room).state).toBe('lobby');
    expect((await state('running')).status).toBe(200);
    expect((await state('closed')).status).toBe(200);

    expect((await json('GET', `/api/codes/${room.code}`)).status).toBe(404);
    expect(((await (await json('GET', `/api/rooms/${room.id}`)).json()) as Room).code).toBeNull();
  }, 10_000);
});
