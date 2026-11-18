import type { IncomingMessage, ServerResponse } from 'node:http';
import { handle } from './plugin';

// Runs the stub API inside the page, for builds with no dev server (the Claude artifact).
// `fetch` and `EventSource` for /api/* are answered by the same `handle` the Vite plugin uses.

type Listener = (...args: unknown[]) => void;

function fakeReq(method: string, url: string, headers: Record<string, string>, body: Uint8Array) {
  const on: Record<string, Listener[]> = {};
  const req = {
    method,
    url,
    headers,
    on(event: string, cb: Listener) {
      (on[event] ??= []).push(cb);
      // Replay the body to whoever reads it, in order: data, then end.
      if (event === 'data' && body.length) setTimeout(() => cb(body), 0);
      if (event === 'end') setTimeout(() => cb(), 0);
      return req;
    },
    emit(event: string) {
      for (const cb of on[event] ?? []) cb();
    },
  };
  return req;
}

function fakeRes(onWrite: (chunk: string) => void, onEnd: (status: number, headers: Record<string, string>, body: string) => void) {
  const headers: Record<string, string> = {};
  const res = {
    statusCode: 200,
    setHeader(k: string, v: string) {
      headers[k.toLowerCase()] = v;
    },
    writeHead(status: number, h?: Record<string, string>) {
      res.statusCode = status;
      Object.assign(headers, h);
      return res;
    },
    write(chunk: string) {
      onWrite(chunk);
      return true;
    },
    end(chunk?: string) {
      onEnd(res.statusCode, headers, chunk ?? '');
      return res;
    },
  };
  return res;
}

const isApi = (url: string) => new URL(url, location.href).pathname.startsWith('/api/');
const pathOf = (url: string) => {
  const u = new URL(url, location.href);
  return u.pathname + u.search;
};

async function bodyBytes(body: BodyInit | null | undefined): Promise<Uint8Array> {
  if (body == null) return new Uint8Array();
  if (typeof body === 'string') return new TextEncoder().encode(body);
  if (body instanceof Blob) return new Uint8Array(await body.arrayBuffer());
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  return new TextEncoder().encode(String(body));
}

export function installBrowserMock() {
  const realFetch = window.fetch.bind(window);
  window.fetch = async (input: RequestInfo | URL, init: RequestInit = {}) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    if (!isApi(url)) return realFetch(input, init);
    const headers: Record<string, string> = {};
    new Headers(init.headers).forEach((v, k) => (headers[k] = v));
    const req = fakeReq(init.method ?? 'GET', pathOf(url), headers, await bodyBytes(init.body));
    return new Promise<Response>((resolve) => {
      const res = fakeRes(
        () => undefined,
        (status, h, body) => resolve(new Response(body || null, { status, headers: h })),
      );
      void handle(req as unknown as IncomingMessage, res as unknown as ServerResponse);
    });
  };

  class MockEventSource extends EventTarget {
    onopen: ((e: Event) => void) | null = null;
    onerror: ((e: Event) => void) | null = null;
    readyState = 0;
    private req: ReturnType<typeof fakeReq>;
    constructor(public url: string) {
      super();
      this.req = fakeReq('GET', pathOf(url), {}, new Uint8Array());
      let buf = '';
      const res = fakeRes(
        (chunk) => {
          if (this.readyState === 0) {
            this.readyState = 1;
            this.onopen?.(new Event('open'));
          }
          buf += chunk;
          let cut: number;
          while ((cut = buf.indexOf('\n\n')) >= 0) {
            const frame = buf.slice(0, cut);
            buf = buf.slice(cut + 2);
            let type = 'message';
            const data: string[] = [];
            for (const line of frame.split('\n')) {
              if (line.startsWith('event: ')) type = line.slice(7);
              else if (line.startsWith('data: ')) data.push(line.slice(6));
            }
            if (data.length) this.dispatchEvent(new MessageEvent(type, { data: data.join('\n') }));
          }
        },
        () => {
          this.readyState = 2;
          this.onerror?.(new Event('error'));
        },
      );
      void handle(this.req as unknown as IncomingMessage, res as unknown as ServerResponse);
    }
    close() {
      this.readyState = 2;
      this.req.emit('close');
    }
  }
  (window as unknown as { EventSource: unknown }).EventSource = MockEventSource;
}
