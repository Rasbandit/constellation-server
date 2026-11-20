import { afterEach, describe, expect, it, vi } from 'vitest';
import { adminApiReachable } from './reachable';

afterEach(() => vi.unstubAllGlobals());

const status = (s: number) => vi.stubGlobal('fetch', async () => new Response('', { status: s }));

describe('adminApiReachable', () => {
  it('is false when nothing answers /api (the public port has no admin API)', async () => {
    status(404);
    expect(await adminApiReachable()).toBe(false);
    vi.stubGlobal('fetch', async () => {
      throw new Error('offline');
    });
    expect(await adminApiReachable()).toBe(false);
  });
  it('is true when the API answers, even with a refusal (the proxy or token is the operator concern)', async () => {
    for (const s of [200, 401, 403]) {
      status(s);
      expect(await adminApiReachable()).toBe(true);
    }
  });
});
