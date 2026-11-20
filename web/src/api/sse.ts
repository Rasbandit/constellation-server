import { getAdminToken, rejectAdminToken } from './token';

/** What the page needs from a server-sent event stream. */
export interface StreamHandlers {
  onOpen: () => void;
  /** The `data:` of one event whose name is `event` (or unnamed, for the default). */
  onEvent: (data: string) => void;
  /** Connection lost; it is retried. */
  onRetry: () => void;
  /** The server refused the stream (not a 200): it is not retried. */
  onClosed: () => void;
}

const RETRY_MS = 2000;

/**
 * An event stream read with `fetch`, because `EventSource` cannot send the `Authorization` header
 * and the token must never go in the URL. Resumes with `Last-Event-ID`; a 401 drops the token.
 * Returns the function that stops it.
 */
export function openEventStream(url: string, event: string, h: StreamHandlers): () => void {
  const abort = new AbortController();
  let lastId = '';
  void (async () => {
    while (!abort.signal.aborted) {
      try {
        const headers: Record<string, string> = { Accept: 'text/event-stream' };
        const token = getAdminToken();
        if (token) headers.Authorization = `Bearer ${token}`;
        if (lastId) headers['Last-Event-ID'] = lastId;
        const res = await fetch(url, { headers, signal: abort.signal });
        if (res.status === 401) rejectAdminToken();
        if (!res.ok || !res.body) return h.onClosed();
        h.onOpen();
        const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
        let buf = '';
        let name = 'message';
        let data: string[] = [];
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          buf += value;
          for (let nl = buf.search(/\r\n|\n|\r/); nl >= 0; nl = buf.search(/\r\n|\n|\r/)) {
            const line = buf.slice(0, nl);
            buf = buf.slice(nl + (buf.startsWith('\r\n', nl) ? 2 : 1));
            if (line === '') {
              if (data.length && name === event) h.onEvent(data.join('\n'));
              name = 'message';
              data = [];
            } else if (!line.startsWith(':')) {
              const colon = line.indexOf(':');
              const field = colon < 0 ? line : line.slice(0, colon);
              const val = colon < 0 ? '' : line.slice(colon + 1).replace(/^ /, '');
              if (field === 'data') data.push(val);
              else if (field === 'event') name = val;
              else if (field === 'id') lastId = val;
            }
          }
        }
      } catch {
        if (abort.signal.aborted) return;
      }
      h.onRetry();
      await new Promise((r) => setTimeout(r, RETRY_MS));
    }
  })();
  return () => abort.abort();
}
