import { describe, expect, it } from 'vitest';
import { API_PATHS, apiProxy } from './proxy';

describe('apiProxy', () => {
  it('leaves /api to the stub when no server is configured', () => {
    const p = apiProxy({});
    expect(p.proxy).toEqual({});
    expect(p.passthrough).toBeUndefined();
  });

  it('forwards all of /api, login included, with no credential added', () => {
    const p = apiProxy({ CONSTELLATION_ADMIN_URL: 'http://127.0.0.1:8081' });
    expect(Object.values(p.proxy)[0]).toEqual({ target: 'http://127.0.0.1:8081', changeOrigin: true });
    for (const path of ['/api', '/api/login', '/api/signup', '/api/rooms', '/api/yamls?name=a.yaml', '/api/worlds/abc']) {
      expect(API_PATHS.test(path)).toBe(true);
    }
    for (const path of ['/apix', '/login', '/rooms/a']) expect(API_PATHS.test(path)).toBe(false);
  });
});
