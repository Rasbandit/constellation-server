import { readFileSync } from 'node:fs';
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin, ProxyOptions } from 'vite';

// Sends the upload routes to the server's admin API instead of the stub, when configured:
//   CONSTELLATION_ADMIN_URL    the server's port, e.g. http://127.0.0.1:38281 (the admin API
//                              shares it; or an SSH tunnel to it)
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

/** The host names the dev server answers the proxy on: this machine only. */
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

/**
 * Why a request must not reach the token-adding proxy, or null when it may. Only the dev
 * server's own pages may use it: the Host must be a loopback name (no DNS rebinding), and a
 * request a browser marks as coming from another page (Origin, Sec-Fetch-Site) must come from
 * this same origin. Without this, any website open in the browser could use the admin API
 * through the proxy (it adds the token; GHSA-vg6x-rcgg-rjx6 is the same class of hole in Vite).
 */
export function refusal(h: { host?: string; origin?: string; 'sec-fetch-site'?: string }): string | null {
  let host: URL;
  try {
    host = new URL(`http://${h.host ?? ''}`);
  } catch {
    return 'bad host';
  }
  if (!h.host || !LOOPBACK.has(host.hostname)) return `host ${h.host ?? '(none)'} is not this machine`;
  if (h.origin !== undefined) {
    let from: string;
    try {
      from = new URL(h.origin).host;
    } catch {
      from = '';
    }
    if (from !== host.host) return `origin ${h.origin} is not this dev server`;
  }
  const site = h['sec-fetch-site'];
  if (site !== undefined && site !== 'same-origin' && site !== 'none') return `sec-fetch-site ${site}`;
  return null;
}

/** Answers 403 to a request for the proxied `paths` that `refusal` turns away. Runs before Vite's proxy. */
export function adminGuard(paths: RegExp): Plugin {
  return {
    name: 'constellation-admin-guard',
    configureServer(server) {
      server.middlewares.use((req: IncomingMessage, res: ServerResponse, next: () => void) => {
        const why = paths.test(new URL(req.url ?? '/', 'http://localhost').pathname) ? refusal(req.headers as Parameters<typeof refusal>[0]) : null;
        if (!why) return next();
        res.statusCode = 403;
        res.end(`refused: ${why}`);
      });
    },
  };
}
