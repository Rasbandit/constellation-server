import { useMemo, useState, type ReactNode } from 'react';
import type { FeedItem, Hint, HintStatus, Slot, SlotNo } from '../api/types';
import { ago, clock, pct } from '../lib/format';
import type { LinkState } from '../lib/live';
import { Empty, FlagTag, HintTag, Progress, StatusPill } from './ui';

export function LinkBadge({ link }: { link: LinkState }) {
  const text = { connecting: 'Connecting', live: 'Live', retrying: 'Reconnecting', closed: 'Disconnected: reload the page' }[link];
  return <span className={`link link-${link}`}>{text}</span>;
}

export function SlotTable({ slots, me, actions }: { slots: Slot[]; me?: SlotNo | null; actions?: (s: Slot) => ReactNode }) {
  const [sort, setSort] = useState<'slot' | 'progress' | 'status'>('slot');
  const order = { playing: 0, ready: 1, connected: 2, goal: 3, offline: 4 };
  const sorted = useMemo(() => {
    const s = [...slots];
    if (sort === 'progress') s.sort((a, b) => pct(b.checksDone, b.checksTotal) - pct(a.checksDone, a.checksTotal));
    if (sort === 'status') s.sort((a, b) => order[a.status] - order[b.status]);
    return s;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [slots, sort]);
  const th = (key: typeof sort, label: string) => (
    <th>
      <button type="button" className={`th-sort ${sort === key ? 'on' : ''}`} onClick={() => setSort(key)}>
        {label}
      </button>
    </th>
  );
  return (
    <div className="table-wrap">
      <table className="table">
        <thead>
          <tr>
            {th('slot', 'Player')}
            {th('status', 'Status')}
            {th('progress', 'Checks')}
            <th>Received</th>
            <th>Last seen</th>
            {actions && <th aria-label="Actions" />}
          </tr>
        </thead>
        <tbody>
          {sorted.map((s) => (
            <tr key={s.slot} className={s.slot === me ? 'is-me' : ''}>
              <td>
                <div className="slot-name">
                  <span className="slot-no">{s.slot}</span>
                  <div>
                    <strong>{s.name}</strong>
                    {s.slot === me && <span className="you">you</span>}
                    <div className="muted small">{s.game}</div>
                  </div>
                </div>
              </td>
              <td>
                <StatusPill status={s.status} />
              </td>
              <td className="col-progress">
                <Progress done={s.checksDone} total={s.checksTotal} label={`${s.name} checks`} />
              </td>
              <td>{s.itemsReceived}</td>
              <td className="muted">{s.status !== 'offline' ? 'now' : s.lastSeen === undefined ? '' : ago(s.lastSeen)}</td>
              {actions && <td className="col-actions">{actions(s)}</td>}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

type FeedFilter = 'all' | 'item' | 'hint' | 'chat';

export function Feed({ feed, me, footer }: { feed: FeedItem[]; me?: SlotNo | null; footer?: ReactNode }) {
  const [filter, setFilter] = useState<FeedFilter>('all');
  const [mine, setMine] = useState(false);
  const [important, setImportant] = useState(false);
  const shown = useMemo(
    () =>
      feed
        .filter((f) => filter === 'all' || f.kind === filter || (filter === 'item' && f.kind === 'admin' && f.item))
        .filter((f) => !mine || me == null || f.slots.includes(me))
        .filter((f) => !important || (f.item ? f.item.flag === 'progression' : f.kind !== 'join' && f.kind !== 'leave'))
        .slice()
        .reverse(),
    [feed, filter, mine, important, me],
  );
  return (
    <div className="feed">
      <div className="feed-tools">
        <div className="seg" role="group" aria-label="Show">
          {(['all', 'item', 'hint', 'chat'] as const).map((k) => (
            <button key={k} type="button" className={filter === k ? 'on' : ''} onClick={() => setFilter(k)}>
              {{ all: 'All', item: 'Items', hint: 'Hints', chat: 'Chat' }[k]}
            </button>
          ))}
        </div>
        {me != null && (
          <label className="check">
            <input type="checkbox" checked={mine} onChange={(e) => setMine(e.target.checked)} /> Only mine
          </label>
        )}
        <label className="check">
          <input type="checkbox" checked={important} onChange={(e) => setImportant(e.target.checked)} /> Progression only
        </label>
      </div>
      <ol className="feed-list">
        {shown.length === 0 && <Empty>Nothing here yet.</Empty>}
        {shown.map((f) => (
          <li key={f.seq} className={`feed-row kind-${f.kind} ${me != null && f.item?.to === me ? 'to-me' : ''}`}>
            {f.at ? <time dateTime={f.at}>{clock(f.at)}</time> : <span aria-hidden />}
            <span className="feed-text">{f.text}</span>
            {f.item && <FlagTag flag={f.item.flag} />}
          </li>
        ))}
      </ol>
      {footer}
    </div>
  );
}

const statusOrder: HintStatus[] = ['priority', 'unspecified', 'no_priority', 'avoid', 'found'];

/**
 * Hints grouped into "you are looking for" and "others need from your world", each sorted by
 * status, so a player sees at a glance what to do next.
 */
export function HintBoard({
  hints,
  slots,
  me,
  onStatus,
}: {
  hints: Hint[];
  slots: Slot[];
  me: SlotNo | null;
  onStatus?: (h: Hint, s: HintStatus) => void;
}) {
  const [showFound, setShowFound] = useState(false);
  const name = (n: SlotNo) => slots.find((s) => s.slot === n)?.name ?? `Slot ${n}`;
  const sort = (a: Hint, b: Hint) => statusOrder.indexOf(a.status) - statusOrder.indexOf(b.status);
  const visible = hints.filter((h) => showFound || h.status !== 'found');
  const groups: { title: string; list: Hint[]; mineIsReceiver: boolean }[] =
    me == null
      ? [{ title: 'All hints', list: visible.slice().sort(sort), mineIsReceiver: false }]
      : [
          { title: 'Items you are looking for', list: visible.filter((h) => h.receiving === me).sort(sort), mineIsReceiver: true },
          {
            title: 'Items in your world for others',
            list: visible.filter((h) => h.finding === me && h.receiving !== me).sort(sort),
            mineIsReceiver: false,
          },
        ];
  return (
    <div className="hints">
      <label className="check">
        <input type="checkbox" checked={showFound} onChange={(e) => setShowFound(e.target.checked)} /> Show found
      </label>
      {groups.map((g) => (
        <div key={g.title} className="hint-group">
          <h3>
            {g.title} <span className="count">{g.list.length}</span>
          </h3>
          {g.list.length === 0 ? (
            <Empty>No hints.</Empty>
          ) : (
            <ul className="hint-list">
              {g.list.map((h) => (
                <li key={h.id} className={`hint-row hint-row-${h.status}`}>
                  <div className="hint-main">
                    <div>
                      <strong>{h.item}</strong> <FlagTag flag={h.flag} />
                      {me == null && <span className="muted"> for {name(h.receiving)}</span>}
                    </div>
                    <div className="muted small">
                      {h.location} in {name(h.finding)}'s world{h.entrance ? `, ${h.entrance}` : ''}
                    </div>
                  </div>
                  {onStatus && g.mineIsReceiver && h.status !== 'found' ? (
                    <select
                      aria-label={`Mark ${h.item}`}
                      value={h.status}
                      onChange={(e) => onStatus(h, e.target.value as HintStatus)}
                    >
                      {statusOrder
                        .filter((s) => s !== 'found')
                        .map((s) => (
                          <option key={s} value={s}>
                            {{ priority: 'Priority', unspecified: 'Unmarked', no_priority: 'Not needed', avoid: 'Avoid', found: '' }[s]}
                          </option>
                        ))}
                    </select>
                  ) : (
                    <HintTag status={h.status} />
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      ))}
    </div>
  );
}
