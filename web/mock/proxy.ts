import { readFileSync } from 'node:fs';
import type { ProxyOptions } from 'vite';

// Sends the upload routes to the server's admin API instead of the stub, when configured:
//   CONSTELLATION_ADMIN_URL    the admin listener, e.g. http://127.0.0.1:8081 (see the server's
//                              `admin api addr=` log line, or an SSH tunnel to it)
//   CONSTELLATION_ADMIN_TOKEN  the admin token, or CONSTELLATION_ADMIN_TOKEN_FILE a file holding it
// The token is added here, in the dev server, so it never reaches the browser.

/**
 * The server's upload and generation routes (branch feat/web-upload-rooms, worlds.rs). Vite
 * tests proxy keys against the URL with its query (`/api/yamls?name=...`), the stub against the
 * path alone, so both must match.
 */
export const ADMIN_PATHS = /^\/api\/(apworlds|yamls|worlds)([/?]|$)/;

export interface AdminProxy {
  /** Vite `server.proxy` entries; empty when not configured. */
  proxy: Record<string, ProxyOptions>;
  /** Paths the stub must leave alone; undefined when not configured. */
  passthrough?: RegExp;
  /** One line for the dev server log. */
  summary: string;
}

export function adminProxy(env: Record<string, string | undefined>): AdminProxy {
  const target = env.CONSTELLATION_ADMIN_URL?.trim();
  if (!target) return { proxy: {}, summary: 'uploads: stub (set CONSTELLATION_ADMIN_URL to use the server)' };
  const token = (env.CONSTELLATION_ADMIN_TOKEN ?? (env.CONSTELLATION_ADMIN_TOKEN_FILE ? readFileSync(env.CONSTELLATION_ADMIN_TOKEN_FILE, 'utf8') : '')).trim();
  if (!token) throw new Error('CONSTELLATION_ADMIN_URL is set but CONSTELLATION_ADMIN_TOKEN(_FILE) is not');
  return {
    proxy: {
      [ADMIN_PATHS.source]: {
        target,
        changeOrigin: true,
        headers: { Authorization: `Bearer ${token}` },
      },
    },
    passthrough: ADMIN_PATHS,
    summary: `uploads: ${target} (admin API)`,
  };
}
