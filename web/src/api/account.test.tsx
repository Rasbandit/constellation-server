import { renderToString } from 'react-dom/server';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AccountPanel } from '../components/Account';
import { ownedRoomToSummary } from './client';
import { accountName, loadAccount, logIn, logOut, signUp } from './account';
import { rejectAdminToken } from './token';

type Call = { method: string; path: string; body?: unknown };

/** A fake server: answers each call with the next reply and records what was asked. */
function server(...replies: { status: number; body?: unknown }[]) {
  const calls: Call[] = [];
  vi.stubGlobal('fetch', async (path: string, init: RequestInit = {}) => {
    calls.push({ method: init.method ?? 'GET', path, body: init.body ? JSON.parse(String(init.body)) : undefined });
    const r = replies.shift() ?? { status: 500 };
    return new Response(typeof r.body === 'string' ? r.body : JSON.stringify(r.body ?? {}), { status: r.status });
  });
  return calls;
}

beforeEach(() => rejectAdminToken());
afterEach(() => vi.unstubAllGlobals());

describe('accounts', () => {
  it('learns who is signed in from /api/me, and a 401 means nobody', async () => {
    server({ status: 200, body: { name: 'ann' } });
    await loadAccount();
    expect(accountName()).toBe('ann');
    server({ status: 401 });
    await loadAccount();
    expect(accountName()).toBeNull();
  });

  it('a failed /api/me never drops the admin token (it is not an admin call)', async () => {
    const calls = server({ status: 401 });
    await loadAccount();
    expect(calls[0].path).toBe('/api/me');
  });

  it('logs in and signs up by posting the pair as JSON, and remembers the name', async () => {
    const calls = server({ status: 200, body: { name: 'ann' } }, { status: 201, body: { name: 'bo' } });
    await logIn('ann', 'a long enough password');
    expect(calls[0]).toEqual({ method: 'POST', path: '/api/login', body: { username: 'ann', password: 'a long enough password' } });
    expect(accountName()).toBe('ann');
    await signUp('bo', 'another long password');
    expect(calls[1].path).toBe('/api/signup');
    expect(accountName()).toBe('bo');
  });

  it('says why in plain words and stays signed out when the server refuses', async () => {
    server({ status: 401 });
    await expect(logIn('ann', 'wrong')).rejects.toThrow('Wrong username or password');
    server({ status: 429 });
    await expect(logIn('ann', 'wrong')).rejects.toThrow('Too many tries');
    server({ status: 409, body: 'that name is not available' });
    await expect(signUp('ann', 'a long enough password')).rejects.toThrow('that name is not available');
    server({ status: 403, body: 'sign-up is closed on this server' });
    await expect(signUp('ann', 'a long enough password')).rejects.toThrow('sign-up is closed');
  });

  it('logs out through the server and forgets the name', async () => {
    server({ status: 200, body: { name: 'ann' } }, { status: 204 });
    await logIn('ann', 'a long enough password');
    await logOut();
    expect(accountName()).toBeNull();
  });
});

describe('the account panel', () => {
  it('offers log in and create account when signed out, and only log in when sign-up is closed', () => {
    const open = renderToString(<AccountPanel name={null} signupOpen />);
    expect(open).toContain('Log in');
    expect(open).toContain('Create account');
    const closed = renderToString(<AccountPanel name={null} signupOpen={false} />);
    expect(closed).toContain('Log in');
    expect(closed).not.toContain('Create account');
  });

  it('shows the name and a log out button when signed in', () => {
    const html = renderToString(<AccountPanel name="ann" signupOpen />);
    expect(html).toContain('ann');
    expect(html).toContain('Log out');
    expect(html).not.toContain('type="password"');
  });
});

describe('the room list', () => {
  it('turns the server row into a summary the page can show', () => {
    const s = ownedRoomToSummary({ id: 'w1', name: 'seed', room_key: 'abcdefghij', owner: 'pw:ann', age_secs: 3600 }, Date.parse('2026-01-01T01:00:00Z'));
    expect(s.id).toBe('w1');
    expect(s.name).toBe('seed');
    expect(s.createdAt).toBe('2026-01-01T00:00:00.000Z');
  });

  it('opens a room made before accounts (no world id) by its key', () => {
    expect(ownedRoomToSummary({ id: null, name: 's', room_key: 'abcdefghij', owner: null, age_secs: 0 }, 0).id).toBe('abcdefghij');
  });
});
