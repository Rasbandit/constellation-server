import { useSyncExternalStore } from 'react';

/**
 * The signed-in account (`/api/login`, `/api/signup`, `/api/me`). The browser holds the session
 * as an HttpOnly cookie the page cannot read, so all it keeps here is the name to show. These
 * calls use plain `fetch`, not the api client: a wrong password is a normal answer here, not
 * a lost session.
 */
interface AccountState {
  name: string | null;
  /** False until the first `/api/me` answer. */
  known: boolean;
  /** From `GET /api/auth`: whether the server lets anyone make an account (assumed until asked). */
  signupOpen: boolean;
}

let state: AccountState = { name: null, known: false, signupOpen: true };
const listeners = new Set<() => void>();
const set = (next: Partial<AccountState>) => {
  state = { ...state, ...next };
  listeners.forEach((l) => l());
};

/** The server said 401 (the session ended): the page sends the person to `/login`. */
export const markSignedOut = () => set({ name: null, known: true });

/** What a page that needs a session does: wait for `/api/me`, show itself, or go to `/login`. */
export function sessionGate(s: Pick<AccountState, 'name' | 'known'>, live: boolean): 'wait' | 'show' | 'login' {
  if (!live || s.name) return 'show';
  return s.known ? 'login' : 'wait';
}

/** Whether to offer sign-up (`GET /api/auth`, open to everyone; an unreadable answer keeps the default). */
export async function loadSignupOpen(): Promise<void> {
  try {
    const res = await fetch('/api/auth');
    const body = (res.ok ? await res.json() : null) as { signup_open?: unknown } | null;
    if (typeof body?.signup_open === 'boolean') set({ signupOpen: body.signup_open });
  } catch {
    // unreadable: keep the default
  }
}

export const accountName = () => state.name;

export function useAccount(): AccountState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
    () => state,
  );
}

async function post(path: string, body?: unknown): Promise<Response> {
  const init: RequestInit = { method: 'POST' };
  if (body !== undefined) {
    init.headers = { 'Content-Type': 'application/json' };
    init.body = JSON.stringify(body);
  }
  return fetch(path, init);
}

/** Who the server says is signed in (nobody on a 401 or any answer we cannot read). */
export async function loadAccount(): Promise<void> {
  let name: string | null = null;
  try {
    const res = await fetch('/api/me');
    const body: unknown = res.ok ? await res.json() : null;
    const v = (body as { name?: unknown } | null)?.name;
    if (typeof v === 'string') name = v;
  } catch {
    // unreadable: signed out
  }
  set({ name, known: true });
}

/** Why a login or sign-up was refused, in words for the person. */
async function refusal(res: Response): Promise<string> {
  if (res.status === 401) return 'Wrong username or password.';
  if (res.status === 429) return 'Too many tries. Wait a little and try again.';
  const text = (await res.text().catch(() => '')).trim();
  return text || `The server refused that (${res.status}).`;
}

async function enter(path: string, username: string, password: string): Promise<void> {
  const res = await post(path, { username, password });
  if (!res.ok) throw new Error(await refusal(res));
  const body = (await res.json()) as { name: string };
  set({ name: body.name, known: true });
}

export const logIn = (username: string, password: string) => enter('/api/login', username, password);
export const signUp = (username: string, password: string) => enter('/api/signup', username, password);

export async function logOut(): Promise<void> {
  await post('/api/logout').catch(() => undefined);
  set({ name: null, known: true });
}
