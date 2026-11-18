import { describe, expect, it } from 'vitest';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { ADMIN_PATHS, adminGuard, adminProxy, refusal } from './proxy';

describe('adminProxy', () => {
  it('leaves uploads to the stub when no server is configured', () => {
    const p = adminProxy({});
    expect(p.proxy).toEqual({});
    expect(p.passthrough).toBeUndefined();
  });

  it('forwards only the upload routes, with the token added server side', () => {
    const p = adminProxy({ CONSTELLATION_ADMIN_URL: 'http://127.0.0.1:8081', CONSTELLATION_ADMIN_TOKEN: ' t0ken \n' });
    expect(Object.values(p.proxy)[0]).toMatchObject({ target: 'http://127.0.0.1:8081', headers: { Authorization: 'Bearer t0ken' } });
    for (const path of ['/api/apworlds', '/api/yamls', '/api/yamls?name=a.yaml', '/api/worlds', '/api/worlds/abc']) expect(ADMIN_PATHS.test(path)).toBe(true);
    for (const path of ['/api/rooms', '/api/games', '/api/yamlsx']) expect(ADMIN_PATHS.test(path)).toBe(false);
  });

  it('refuses a server without a token', () => {
    expect(() => adminProxy({ CONSTELLATION_ADMIN_URL: 'http://127.0.0.1:8081' })).toThrow(/TOKEN/);
  });
});

describe('refusal', () => {
  it('lets the dev server\'s own pages through', () => {
    for (const host of ['localhost:5174', '127.0.0.1:5174', '[::1]:5174', 'localhost']) {
      expect(refusal({ host })).toBeNull();
      expect(refusal({ host, origin: `http://${host}`, 'sec-fetch-site': 'same-origin' })).toBeNull();
    }
    expect(refusal({ host: 'localhost:5174', 'sec-fetch-site': 'none' })).toBeNull();
  });

  it('refuses another site, another local port, a rebound host name and a missing host', () => {
    expect(refusal({ host: 'localhost:5174', origin: 'https://evil.example' })).toMatch(/origin/);
    expect(refusal({ host: 'localhost:5174', origin: 'http://localhost:3000' })).toMatch(/origin/);
    expect(refusal({ host: 'localhost:5174', origin: 'null' })).toMatch(/origin/);
    expect(refusal({ host: 'localhost:5174', 'sec-fetch-site': 'cross-site' })).toMatch(/site/);
    expect(refusal({ host: 'localhost:5174', 'sec-fetch-site': 'same-site' })).toMatch(/site/);
    expect(refusal({ host: 'evil.example:5174', origin: 'http://evil.example:5174' })).toMatch(/host/);
    expect(refusal({ host: 'localhost.evil.example' })).toMatch(/host/);
    expect(refusal({})).toMatch(/host/);
  });
});

describe('adminGuard', () => {
  type Mw = (req: IncomingMessage, res: ServerResponse, next: () => void) => void;
  function run(url: string, headers: Record<string, string>) {
    let mw: Mw | undefined;
    const server = { middlewares: { use: (fn: Mw) => (mw = fn) } };
    (adminGuard(ADMIN_PATHS).configureServer as (s: typeof server) => void)(server);
    const res = { statusCode: 200, body: '', end(b?: string) { this.body = b ?? ''; } };
    let passed = false;
    mw!({ url, headers } as IncomingMessage, res as unknown as ServerResponse, () => (passed = true));
    return { passed, status: res.statusCode, body: res.body };
  }

  it('answers a proxied path from another origin with 403 before the proxy adds the token', () => {
    const r = run('/api/worlds/abc/room', { host: 'localhost:5174', origin: 'https://evil.example' });
    expect(r).toMatchObject({ passed: false, status: 403 });
  });

  it('passes the dev server\'s own requests and leaves other paths alone', () => {
    expect(run('/api/yamls?name=a.yaml', { host: 'localhost:5174' }).passed).toBe(true);
    expect(run('/api/rooms', { host: 'evil.example' }).passed).toBe(true);
    expect(run('/', { host: 'evil.example' }).passed).toBe(true);
  });
});
