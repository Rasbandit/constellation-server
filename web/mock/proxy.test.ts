import { describe, expect, it } from 'vitest';
import { ADMIN_PATHS, adminProxy } from './proxy';

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
