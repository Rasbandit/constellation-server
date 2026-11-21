import { useSyncExternalStore } from 'react';

/**
 * The admin token (docs/container.md): typed by the operator, kept in this tab's sessionStorage
 * only (gone when the tab closes), sent as `Authorization: Bearer` on every /api call. It is never
 * put in a URL, logged, or written to localStorage.
 */
const KEY = 'constellation.adminToken';

interface TokenState {
  token: string | null;
  /** Why the token was dropped (shown by the prompt), until a new one is entered. */
  notice: string | null;
  /** From `GET /api/auth`: null until known. False: the server is open (no token), no prompt. */
  required: boolean | null;
  /** From `GET /api/auth`: whether the server lets anyone make an account. */
  signupOpen: boolean;
}

const read = (): string | null => {
  try {
    return sessionStorage.getItem(KEY);
  } catch {
    return null; // storage blocked: the token then lives in memory for this page load
  }
};

let state: TokenState = { token: read(), notice: null, required: null, signupOpen: false };
const listeners = new Set<() => void>();

function set(next: TokenState) {
  state = next;
  try {
    if (next.token) sessionStorage.setItem(KEY, next.token);
    else sessionStorage.removeItem(KEY);
  } catch {
    // blocked: memory only
  }
  listeners.forEach((l) => l());
}

export const getAdminToken = () => state.token;
export const setAdminToken = (token: string) => set({ ...state, token: token.trim() || null, notice: null });
/** The server refused the token (401): forget it and say so. */
export const rejectAdminToken = () =>
  set({
    ...state,
    token: null,
    notice: 'The server refused the admin token (missing or wrong). Enter it again.',
  });

export function useAdminToken(): TokenState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
    () => state,
  );
}

/**
 * Ask the server whether it wants a token (`GET /api/auth`, open to everyone, says nothing else).
 * An answer we cannot read counts as "required": the prompt is the safe side.
 */
export async function loadAuthMode(): Promise<void> {
  let required = true;
  let signupOpen = false;
  try {
    const res = await fetch('/api/auth');
    const body = (res.ok ? await res.json() : null) as { token_required?: unknown; signup_open?: unknown } | null;
    if (typeof body?.token_required === 'boolean') required = body.token_required;
    signupOpen = body?.signup_open === true;
  } catch {
    // unreadable: keep the safe defaults
  }
  set({ ...state, required, signupOpen });
}
