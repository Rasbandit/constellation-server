import { renderToString } from 'react-dom/server';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter } from 'react-router-dom';
import { RequireSession, signOutAndLeave, UserMenu } from '../components/Account';
import { AuthPage } from '../pages/AuthPage';
import { api, HttpError, ownedRoomToSummary } from './client';
import { accountName, loadAccount, loadSignupOpen, logIn, logOut, markSignedOut, sessionGate, signUp } from './account';

type Call = { method: string; path: string; body?: unknown };

/** A fake server: answers each call with the next reply and records what was asked. */
function server(...replies: { status: number; body?: unknown }[]) {
  const calls: Call[] = [];
  vi.stubGlobal('fetch', async (path: string, init: RequestInit = {}) => {
    calls.push({ method: init.method ?? 'GET', path, body: init.body ? JSON.parse(String(init.body)) : undefined });
    const r = replies.shift() ?? { status: 500 };
    const body = r.status === 204 ? null : typeof r.body === 'string' ? r.body : JSON.stringify(r.body ?? {});
    return new Response(body, { status: r.status });
  });
  return calls;
}

beforeEach(() => markSignedOut());
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

  it('asks /api/me with plain fetch, so a 401 there is not a lost session', async () => {
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

const page = (el: React.ReactElement) => renderToString(<MemoryRouter>{el}</MemoryRouter>);

describe('the login and sign-up pages', () => {
  it('the login page has a username and a password with the right autocomplete, and a link to sign up', () => {
    const html = page(<AuthPage mode="login" />);
    expect(html).toContain('autoComplete="username"');
    expect(html).toContain('autoComplete="current-password"');
    expect(html).toContain('href="/signup"');
    expect(html).toContain('type="submit"');
  });

  it('the sign-up page asks for a new password and links back to login', () => {
    const html = page(<AuthPage mode="signup" />);
    expect(html).toContain('autoComplete="new-password"');
    expect(html).toContain('href="/login"');
    expect(html).not.toContain('href="/signup"');
  });

  it('a server that closed sign-up shows no form on /signup and no sign-up link on /login', async () => {
    server({ status: 200, body: { signup_open: false } });
    await loadSignupOpen();
    expect(page(<AuthPage mode="signup" />)).not.toContain('type="password"');
    expect(page(<AuthPage mode="login" />)).not.toContain('href="/signup"');
    server({ status: 200, body: { signup_open: true } });
    await loadSignupOpen();
  });
});

describe('the header account menu', () => {
  it('signed out shows exactly one Log in link to /login and no Sign up or Log out', () => {
    const html = page(<UserMenu live />);
    expect(html.match(/<a /g)).toHaveLength(1);
    expect(html).toContain('href="/login"');
    expect(html).toContain('Log in');
    expect(html).not.toContain('Sign up');
    expect(html).not.toContain('Log out');
  });

  it('signed in shows one profile button with the initial and name, and Log out inside its popover menu', async () => {
    server({ status: 200, body: { name: 'ann' } });
    await loadAccount();
    const html = page(<UserMenu live />);
    expect(html).toContain('aria-haspopup="menu"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('popovertarget="user-menu"');
    expect(html).toMatch(/class="avatar"[^>]*>A</);
    expect(html).toMatch(/id="user-menu"[^>]*popover=""/);
    expect(html).toContain('Signed in as <strong>ann</strong>');
    expect(html.match(/Log out/g)).toHaveLength(1);
    expect(html).not.toContain('Log in');
  });

  it('Log out calls the logout api, forgets the name and returns to /login', async () => {
    const calls = server({ status: 200, body: { name: 'ann' } }, { status: 204 });
    await loadAccount();
    const went: string[] = [];
    await signOutAndLeave((to) => went.push(to));
    expect(calls[1]).toMatchObject({ method: 'POST', path: '/api/logout' });
    expect(accountName()).toBeNull();
    expect(went).toEqual(['/login']);
  });
});

describe('pages that need a session', () => {
  it('waits for /api/me, shows itself when signed in and goes to login when signed out', () => {
    expect(sessionGate({ name: null, known: false }, true)).toBe('wait');
    expect(sessionGate({ name: 'ann', known: true }, true)).toBe('show');
    expect(sessionGate({ name: null, known: true }, true)).toBe('login');
    expect(sessionGate({ name: null, known: true }, false)).toBe('show');
  });

  it('shows the page to a signed-in account and nothing while redirecting a signed-out one', async () => {
    server({ status: 200, body: { name: 'ann' } });
    await loadAccount();
    expect(page(<RequireSession live>secret page</RequireSession>)).toContain('secret page');
    markSignedOut();
    expect(page(<RequireSession live>secret page</RequireSession>)).not.toContain('secret page');
  });

  it('a 401 from any api call marks the page signed out', async () => {
    server({ status: 200, body: { name: 'ann' } }, { status: 401 });
    await loadAccount();
    await expect(api.rooms()).rejects.toBeInstanceOf(HttpError);
    expect(accountName()).toBeNull();
  });
});

describe('room admins calls', () => {
  it('lists, adds by name and removes with the session cookie only (no auth header)', async () => {
    const calls = server({ status: 200, body: { owner: 'ann', admins: [], role: 'owner' } }, { status: 200, body: { owner: 'ann', admins: ['bob'], role: 'owner' } }, { status: 204 });
    const r = api.roomAdmins('abcdefghij');
    expect((await r.list()).owner).toBe('ann');
    expect((await r.add('bob')).admins).toEqual(['bob']);
    expect(await r.remove('bob')).toBeUndefined();
    expect(calls.map((c) => `${c.method} ${c.path}`)).toEqual([
      'GET /api/rooms/abcdefghij/admins',
      'PUT /api/rooms/abcdefghij/admins/bob',
      'DELETE /api/rooms/abcdefghij/admins/bob',
    ]);
  });
});

describe('the room list', () => {
  it('turns the server row into a summary the page can show', () => {
    const s = ownedRoomToSummary({ id: 'w1', name: 'seed', room_key: 'abcdefghij', owner: 'ann', age_secs: 3600, role: 'owner' }, Date.parse('2026-01-01T01:00:00Z'));
    expect(s.id).toBe('w1');
    expect(s.name).toBe('seed');
    expect(s.createdAt).toBe('2026-01-01T00:00:00.000Z');
  });

  it('opens a room made before accounts (no world id) by its key', () => {
    expect(ownedRoomToSummary({ id: null, name: 's', room_key: 'abcdefghij', owner: null, age_secs: 0, role: 'admin' }, 0).id).toBe('abcdefghij');
  });
});
