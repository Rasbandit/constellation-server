import type { ProxyOptions } from 'vite';

// Sends /api to the server instead of the stub, when configured:
//   CONSTELLATION_ADMIN_URL    the server's port, e.g. http://127.0.0.1:38281 (the API shares it;
//                              or an SSH tunnel to it)
// Nothing else is needed: sign-up, login and the session cookie pass through like any request.

/** Everything under `/api` is the server's once it is configured. */
export const API_PATHS = /^\/api([/?]|$)/;

export interface ApiProxy {
  /** Vite `server.proxy` entries; empty when not configured. */
  proxy: Record<string, ProxyOptions>;
  /** Paths the stub must leave alone; undefined when not configured. */
  passthrough?: RegExp;
  /** One line for the dev server log. */
  summary: string;
}

export function apiProxy(env: Record<string, string | undefined>): ApiProxy {
  const target = env.CONSTELLATION_ADMIN_URL?.trim();
  if (!target) return { proxy: {}, summary: 'api: stub (set CONSTELLATION_ADMIN_URL to use the server)' };
  return {
    proxy: { [API_PATHS.source]: { target, changeOrigin: true } },
    passthrough: API_PATHS,
    summary: `api: ${target}`,
  };
}
