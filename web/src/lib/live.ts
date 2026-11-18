import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api/client';
import type { FeedItem, Hint, Room } from '../api/types';

/** Loads a value once and on demand; keeps the last good value while reloading. */
export function useLoad<T>(load: () => Promise<T>, deps: unknown[]) {
  const [data, setData] = useState<T | undefined>();
  const [error, setError] = useState<Error | undefined>();
  const loadRef = useRef(load);
  loadRef.current = load;
  const reload = useCallback(() => {
    loadRef.current().then(
      (d) => {
        setData(d);
        setError(undefined);
      },
      (e: Error) => setError(e),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  useEffect(reload, [reload]);
  return { data, error, reload, setData };
}

export type LinkState = 'connecting' | 'live' | 'retrying';

/**
 * A room kept current: the room, its feed and its hints, refreshed when the server pushes a feed
 * entry over the event stream. Feed entries carry `seq`, so a reconnect asks only for what it missed.
 */
export function useLiveRoom(id: string, adminKey?: string) {
  const room = useLoad<Room>(() => api.room(id, adminKey), [id, adminKey]);
  const hints = useLoad<Hint[]>(() => api.hints(id), [id]);
  const [feed, setFeed] = useState<FeedItem[]>([]);
  const [link, setLink] = useState<LinkState>('connecting');
  const lastSeq = useRef(0);

  useEffect(() => {
    let cancelled = false;
    lastSeq.current = 0;
    setFeed([]);
    const add = (items: FeedItem[]) => {
      const fresh = items.filter((i) => i.seq > lastSeq.current);
      if (fresh.length === 0) return;
      lastSeq.current = fresh[fresh.length - 1].seq;
      setFeed((f) => [...f, ...fresh].slice(-400));
    };
    api.feed(id).then((items) => !cancelled && add(items), () => undefined);

    let pending: ReturnType<typeof setTimeout> | undefined;
    const refresh = () => {
      clearTimeout(pending);
      pending = setTimeout(() => {
        room.reload();
        hints.reload();
      }, 250);
    };
    const es = new EventSource(api.streamUrl(id));
    es.onopen = () => {
      setLink('live');
      api.feed(id, lastSeq.current).then((items) => !cancelled && add(items), () => undefined);
    };
    es.onerror = () => setLink('retrying');
    es.addEventListener('feed', (e) => {
      add([JSON.parse((e as MessageEvent).data) as FeedItem]);
      refresh();
    });
    return () => {
      cancelled = true;
      clearTimeout(pending);
      es.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  return { room, hints, feed, link };
}

/** A value kept in localStorage per browser (for "which slot am I"). Falls back to memory. */
export function useStored<T>(key: string, initial: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(key);
      return raw === null ? initial : (JSON.parse(raw) as T);
    } catch {
      return initial;
    }
  });
  const set = useCallback(
    (next: T) => {
      setV(next);
      try {
        localStorage.setItem(key, JSON.stringify(next));
      } catch {
        // storage blocked: keep it for this tab only
      }
    },
    [key],
  );
  return [v, set];
}
