import { afterEach, describe, expect, it, vi } from 'vitest';
import { fetchServerVersion, versionBadge } from './version';

afterEach(() => vi.unstubAllGlobals());

const reply = (status: number, body: unknown) => vi.stubGlobal('fetch', async () => new Response(JSON.stringify(body), { status }));

describe('versionBadge', () => {
  it('matches when the versions are equal, ignoring a build suffix', () => {
    expect(versionBadge('0.1.0', '0.1.0')).toEqual({ text: 'server 0.1.0 / web 0.1.0', mismatch: false });
    expect(versionBadge('0.1.0+abc1234', '0.1.0').mismatch).toBe(false);
  });
  it('flags a mismatch', () => {
    expect(versionBadge('0.2.0', '0.1.0')).toEqual({ text: 'server 0.2.0 / web 0.1.0', mismatch: true });
  });
  it('flags an unreadable server version', () => {
    expect(versionBadge(undefined, '0.1.0')).toEqual({ text: 'server ? / web 0.1.0', mismatch: true });
  });
});

describe('fetchServerVersion', () => {
  it('reads the version', async () => {
    reply(200, { version: '0.1.0+abc1234' });
    expect(await fetchServerVersion()).toBe('0.1.0+abc1234');
  });
  it('is undefined on an error status, a bad body or a network failure', async () => {
    reply(404, {});
    expect(await fetchServerVersion()).toBeUndefined();
    reply(200, { nope: 1 });
    expect(await fetchServerVersion()).toBeUndefined();
    vi.stubGlobal('fetch', async () => {
      throw new Error('offline');
    });
    expect(await fetchServerVersion()).toBeUndefined();
  });
});
