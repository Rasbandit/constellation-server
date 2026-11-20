import { renderToString } from 'react-dom/server';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { HttpError } from './client';

// The pages only ask for a token against a real server (liveRooms); the vitest build is the stub.
vi.mock('./client', async (orig) => ({ ...(await orig<typeof import('./client')>()), liveRooms: true }));

const { api } = await import('./client');
const { getAdminToken, rejectAdminToken, setAdminToken } = await import('./token');
const { openEventStream } = await import('./sse');
const { NeedsToken } = await import('../components/TokenPrompt');

const TOKEN = 't'.repeat(32);
let session: Map<string, string>;
let local: Map<string, string>;
const storage = (m: Map<string, string>) => ({
  getItem: (k: string) => m.get(k) ?? null,
  setItem: (k: string, v: string) => void m.set(k, v),
  removeItem: (k: string) => void m.delete(k),
});

beforeEach(() => {
  session = new Map();
  local = new Map();
  vi.stubGlobal('sessionStorage', storage(session));
  vi.stubGlobal('localStorage', storage(local));
  rejectAdminToken();
});
afterEach(() => vi.unstubAllGlobals());

function fetchAnswering(status: number) {
  const seen: { url: string; headers: Record<string, string> }[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
    seen.push({ url, headers: (init.headers ?? {}) as Record<string, string> });
    return new Response('[]', { status });
  });
  return seen;
}

describe('the admin token prompt', () => {
  it('shows the prompt, not the page, until a token is entered', () => {
    const page = () => renderToString(<NeedsToken>the page</NeedsToken>);
    expect(page()).toContain('Admin token');
    expect(page()).not.toContain('the page');
    setAdminToken(`  ${TOKEN}  `);
    expect(page()).toBe('the page');
    expect(getAdminToken()).toBe(TOKEN);
  });

  it('keeps the token in sessionStorage only, never localStorage', () => {
    setAdminToken(TOKEN);
    expect([...session.values()]).toEqual([TOKEN]);
    expect(local.size).toBe(0);
  });

  it('sends it as the Authorization header on an /api call, never in the URL', async () => {
    setAdminToken(TOKEN);
    const seen = fetchAnswering(200);
    await api.games();
    expect(seen[0].headers.Authorization).toBe(`Bearer ${TOKEN}`);
    expect(seen[0].url).not.toContain(TOKEN);
  });

  it('sends no header before a token is entered', async () => {
    const seen = fetchAnswering(200);
    await api.games();
    expect(seen[0].headers.Authorization).toBeUndefined();
  });

  it('drops the token on a 401, says why, and brings the prompt back', async () => {
    setAdminToken(TOKEN);
    fetchAnswering(401);
    await expect(api.games()).rejects.toBeInstanceOf(HttpError);
    expect(getAdminToken()).toBeNull();
    expect(session.size).toBe(0);
    const page = renderToString(<NeedsToken>the page</NeedsToken>);
    expect(page).toContain('Admin token');
    expect(page).toContain('refused the admin token');
  });

  it('sends the header on the event stream too, and a 401 there drops the token', async () => {
    setAdminToken(TOKEN);
    const seen = fetchAnswering(401);
    const closed = new Promise<void>((done) => {
      openEventStream('/api/worlds/x/room/events', 'message', { onOpen() {}, onEvent() {}, onRetry() {}, onClosed: done });
    });
    await closed;
    expect(seen[0].headers.Authorization).toBe(`Bearer ${TOKEN}`);
    expect(seen[0].url).not.toContain(TOKEN);
    expect(getAdminToken()).toBeNull();
  });

  it('reads events from the stream', async () => {
    const body = new ReadableStream({
      start(c) {
        c.enqueue(new TextEncoder().encode('id: 1\ndata: {"a":1}\n\n: keepalive\n\ndata: two\n'));
        c.enqueue(new TextEncoder().encode('data: lines\n\n'));
        c.close();
      },
    });
    vi.stubGlobal('fetch', async () => new Response(body, { status: 200 }));
    const got: string[] = [];
    await new Promise<void>((done) => {
      const stop = openEventStream('/s', 'message', {
        onOpen() {},
        onEvent: (d) => got.push(d),
        onRetry: () => {
          stop();
          done();
        },
        onClosed() {},
      });
    });
    expect(got).toEqual(['{"a":1}', 'two\nlines']);
  });
});
