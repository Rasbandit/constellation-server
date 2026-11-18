import { afterEach, describe, expect, it, vi } from 'vitest';
import { generateWorld } from './client';
import type { CreateRoomRequest } from './types';

const W = 'a'.repeat(64);
const Y = 'b'.repeat(64);
const JOB = 'c'.repeat(64);

const req = (players: CreateRoomRequest['players']): CreateRoomRequest => ({
  name: 'MM',
  apworlds: [{ id: W, fileName: 'mm_2ship.apworld', game: 'MM', version: '1', sizeBytes: 1 }],
  players,
  rules: {} as CreateRoomRequest['rules'],
});
const player = (id: string, yamlId?: string) => ({ id, fileName: 'p.yaml', name: id, game: 'MM', yamlId });

/** A fake admin API: answers each call with the next status, and records what was asked. */
function server(statuses: object[]) {
  const calls: { method: string; path: string; body?: unknown }[] = [];
  vi.stubGlobal('fetch', async (path: string, init: RequestInit) => {
    calls.push({ method: init.method ?? 'GET', path, body: init.body ? JSON.parse(String(init.body)) : undefined });
    const reply = init.method === 'POST' ? { id: JOB, status: 'queued' } : statuses.shift();
    return new Response(JSON.stringify(reply), { status: init.method === 'POST' ? 202 : 200 });
  });
  return calls;
}
const noWait = async () => {};

afterEach(() => vi.unstubAllGlobals());

describe('generateWorld', () => {
  it('posts the uploaded ids once each and polls until the server hosts the room', async () => {
    const calls = server([
      { id: JOB, status: 'running' },
      { id: JOB, status: 'done', seed_name: '42' },
      { id: JOB, status: 'done', seed_name: '42', room_key: 'k3y', address: 'k3y.localhost:38281' },
    ]);
    const w = await generateWorld(req([player('a', Y), player('b', Y)]), noWait);
    expect(w).toEqual({ id: JOB, seed: '42', roomKey: 'k3y', address: 'k3y.localhost:38281' });
    expect(calls[0]).toEqual({ method: 'POST', path: '/api/worlds', body: { apworlds: [W], yamls: [Y] } });
    expect(calls.slice(1).map((c) => c.path)).toEqual(Array(3).fill(`/api/worlds/${JOB}`));
  });

  it('reports a failed generation with the server error', async () => {
    server([{ id: JOB, status: 'failed', error: 'yaml broken' }]);
    await expect(generateWorld(req([player('a', Y)]), noWait)).rejects.toThrow(/yaml broken/);
  });

  it('gives up when a done world never becomes a room (server without --rooms)', async () => {
    server(Array(100).fill({ id: JOB, status: 'done', seed_name: '42' }));
    await expect(generateWorld(req([player('a', Y)]), noWait)).rejects.toThrow(/--rooms/);
  });

  it('refuses players without an uploaded yaml before calling the server', async () => {
    const calls = server([]);
    await expect(generateWorld(req([player('a', Y), player('d')]), noWait)).rejects.toThrow(/YAML/);
    await expect(generateWorld(req([]), noWait)).rejects.toThrow(/YAML/);
    expect(calls).toEqual([]);
  });
});
