import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';
import type { CreateRoomRequest, HintStatus, RoomState } from '../src/api/types';
import { checkReadiness } from '../src/lib/readiness';
import { catalogFor, knownGames } from './catalog';
import { NotFound, Refused, store, subscribe } from './store';

// Stub of the web API. Every handler takes the happy path over seeded data in `store.ts`.
// The routes and shapes are the contract the real server implements (see README.md).

type Params = Record<string, string>;
interface Ctx {
  req: IncomingMessage;
  res: ServerResponse;
  params: Params;
  query: URLSearchParams;
  body: () => Promise<Uint8Array>;
  json: <T>() => Promise<T>;
}
type Handler = (ctx: Ctx) => unknown | Promise<unknown>;

const routes: { method: string; pattern: RegExp; keys: string[]; handler: Handler }[] = [];
function route(method: string, path: string, handler: Handler) {
  const keys: string[] = [];
  const pattern = new RegExp(
    `^${path.replace(/:(\w+)/g, (_, k: string) => {
      keys.push(k);
      return '([^/]+)';
    })}$`,
  );
  routes.push({ method, pattern, keys, handler });
}

function requireAdmin(ctx: Ctx) {
  const key = ctx.req.headers['x-admin-key'];
  if (!store.adminKeyOk(ctx.params.id, typeof key === 'string' ? key : undefined)) {
    throw Object.assign(new Error('Admin key missing or wrong'), { status: 403 });
  }
}
const slotParam = (ctx: Ctx) => Number(ctx.params.slot);

// ---- Games and files -----------------------------------------------------------------------

route('GET', '/api/games', () => ({ builtin: store.builtinGames(), known: knownGames() }));
route('GET', '/api/games/:game', (ctx) => catalogFor(decodeURIComponent(ctx.params.game)));

route('POST', '/api/apworlds', async (ctx) => {
  const name = decodeURIComponent(String(ctx.req.headers['x-file-name'] ?? ''));
  const body = await ctx.body();
  return store.addApWorld(name, body.length);
});

route('POST', '/api/player-files', async (ctx) => {
  const name = decodeURIComponent(String(ctx.req.headers['x-file-name'] ?? 'player.yaml'));
  const body = await ctx.body();
  return store.readPlayerFile(name, utf8(body));
});

// ---- Rooms ---------------------------------------------------------------------------------

route('GET', '/api/rooms', () => store.listRooms());
route('GET', '/api/codes/:code', (ctx) => store.resolveCode(decodeURIComponent(ctx.params.code)));

route('POST', '/api/rooms/check', async (ctx) => {
  const req = await ctx.json<CreateRoomRequest>();
  return checkReadiness({ roomName: req.name, apworlds: req.apworlds, players: req.players, builtinGames: store.builtinGames() });
});

route('POST', '/api/rooms', async (ctx) => {
  const req = await ctx.json<CreateRoomRequest>();
  const r = checkReadiness({ roomName: req.name, apworlds: req.apworlds, players: req.players, builtinGames: store.builtinGames() });
  if (!r.ready) {
    ctx.res.statusCode = 422;
    return { error: 'The room is not ready to generate', checks: r.checks.filter((c) => c.level === 'error') };
  }
  ctx.res.statusCode = 201;
  return store.createRoom(req);
});

route('GET', '/api/rooms/:id', (ctx) => {
  const room = store.room(ctx.params.id);
  // Only an admin sees the admin key.
  const key = ctx.req.headers['x-admin-key'];
  return key === room.adminKey ? room : { ...room, adminKey: undefined };
});
route('GET', '/api/rooms/:id/feed', (ctx) => store.feed(ctx.params.id, Number(ctx.query.get('after') ?? 0)));
route('GET', '/api/rooms/:id/hints', (ctx) => store.hints(ctx.params.id));

route('GET', '/api/rooms/:id/stream', (ctx) => {
  store.room(ctx.params.id); // 404 before opening the stream
  ctx.res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
  ctx.res.write(': connected\n\n');
  const off = subscribe(ctx.params.id, (item) => ctx.res.write(`id: ${item.seq}\nevent: feed\ndata: ${JSON.stringify(item)}\n\n`));
  const beat = setInterval(() => ctx.res.write(': beat\n\n'), 15000);
  ctx.req.on('close', () => {
    off();
    clearInterval(beat);
  });
  return STREAMING;
});

route('POST', '/api/rooms/:id/chat', async (ctx) => {
  const { slot, text } = await ctx.json<{ slot: number; text: string }>();
  return store.chat(ctx.params.id, slot, text);
});

route('POST', '/api/rooms/:id/hints/:hint', async (ctx) => {
  const { status } = await ctx.json<{ status: HintStatus }>();
  return store.setHintStatus(ctx.params.id, ctx.params.hint, status);
});

// ---- Admin ---------------------------------------------------------------------------------

route('GET', '/api/rooms/:id/admin/audit', (ctx) => {
  requireAdmin(ctx);
  return store.audit(ctx.params.id);
});
route('POST', '/api/rooms/:id/admin/items', async (ctx) => {
  requireAdmin(ctx);
  const { to, item, count } = await ctx.json<{ to: number; item: string; count?: number }>();
  store.admin.sendItem(ctx.params.id, to, item, Math.max(1, Math.min(99, count ?? 1)));
  return { ok: true };
});
route('POST', '/api/rooms/:id/admin/hints', async (ctx) => {
  requireAdmin(ctx);
  const { slot, item } = await ctx.json<{ slot: number; item: string }>();
  return store.admin.hint(ctx.params.id, slot, item);
});
for (const action of ['release', 'collect', 'kick'] as const) {
  route('POST', `/api/rooms/:id/admin/slots/:slot/${action}`, (ctx) => {
    requireAdmin(ctx);
    store.admin[action](ctx.params.id, slotParam(ctx));
    return { ok: true };
  });
}
route('PATCH', '/api/rooms/:id/admin/slots/:slot', async (ctx) => {
  requireAdmin(ctx);
  return store.admin.updateSlot(ctx.params.id, slotParam(ctx), await ctx.json());
});
route('POST', '/api/rooms/:id/admin/broadcast', async (ctx) => {
  requireAdmin(ctx);
  const { text } = await ctx.json<{ text: string }>();
  store.admin.broadcast(ctx.params.id, text);
  return { ok: true };
});
route('PATCH', '/api/rooms/:id/admin/rules', async (ctx) => {
  requireAdmin(ctx);
  return store.admin.updateRules(ctx.params.id, await ctx.json());
});
route('POST', '/api/rooms/:id/admin/state', async (ctx) => {
  requireAdmin(ctx);
  const { state } = await ctx.json<{ state: RoomState }>();
  store.admin.setState(ctx.params.id, state);
  return { ok: true };
});

// ---- Plumbing ------------------------------------------------------------------------------

const STREAMING = Symbol('streaming');

const utf8 = (b: Uint8Array) => new TextDecoder().decode(b);

// Plain Uint8Array (not Buffer) so the same handler also runs in a browser (see browser.ts).
function readBody(req: IncomingMessage): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    const chunks: Uint8Array[] = [];
    req.on('data', (c: Uint8Array) => chunks.push(c));
    req.on('end', () => {
      const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
      let at = 0;
      for (const c of chunks) {
        out.set(c, at);
        at += c.length;
      }
      resolve(out);
    });
    req.on('error', reject);
  });
}

/** Handles one request if it is for the API; returns false otherwise. Exported for tests. */
export async function handle(req: IncomingMessage, res: ServerResponse): Promise<boolean> {
  const url = new URL(req.url ?? '/', 'http://localhost');
  if (!url.pathname.startsWith('/api/')) return false;
  const send = (status: number, value: unknown) => {
    res.statusCode = status;
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify(value));
  };
  let pathMatched = false;
  for (const r of routes) {
    const m = r.pattern.exec(url.pathname);
    if (!m) continue;
    pathMatched = true;
    if (r.method !== req.method) continue;
    const params = Object.fromEntries(r.keys.map((k, i) => [k, m[i + 1]]));
    let cached: Promise<Uint8Array> | undefined;
    const body = () => (cached ??= readBody(req));
    const ctx: Ctx = {
      req,
      res,
      params,
      query: url.searchParams,
      body,
      json: async <T,>() => {
        const b = await body();
        return (b.length ? JSON.parse(utf8(b)) : {}) as T;
      },
    };
    try {
      // A little latency so loading states are visible.
      await new Promise((ok) => setTimeout(ok, 120));
      const out = await r.handler(ctx);
      if (out !== STREAMING) send(res.statusCode && res.statusCode !== 200 ? res.statusCode : 200, out);
    } catch (e) {
      const status = e instanceof NotFound ? 404 : e instanceof Refused ? 400 : ((e as { status?: number }).status ?? 500);
      send(status, { error: (e as Error).message });
    }
    return true;
  }
  send(pathMatched ? 405 : 404, { error: pathMatched ? 'Method not allowed' : 'No such endpoint' });
  return true;
}

export function mockApi(): Plugin {
  return {
    name: 'constellation-mock-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        handle(req, res).then((done) => done || next(), next);
      });
    },
    configurePreviewServer(server) {
      server.middlewares.use((req, res, next) => {
        handle(req, res).then((done) => done || next(), next);
      });
    },
  };
}
