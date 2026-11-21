import { useEffect, useMemo, useState } from 'react';
import { Link, useParams, useSearchParams } from 'react-router-dom';
import { api, liveRooms } from '../api/client';
import type { GameCatalog, Room, RoomRules, Slot } from '../api/types';
import { Phases, shortLink } from '../components/phases';
import { Feed, HintBoard, LinkBadge, SlotTable } from '../components/room';
import { Card, CopyButton, Empty, FlagTag, Modal, RoomStatePill, StatusPill, useToast } from '../components/ui';
import { clock, pct } from '../lib/format';
import { useLiveRoom, useLoad, useStored } from '../lib/live';
import { RulesForm } from './CreateRoom';
import { Stat } from './PlayRoom';

/** The admin key comes from the link once, then lives in this browser. */
function useAdminKey(id: string): [string, (k: string) => void] {
  const [params, setParams] = useSearchParams();
  const [stored, setStored] = useStored<string>(`constellation.admin.${id}`, '');
  const fromUrl = params.get('key');
  useEffect(() => {
    if (fromUrl && fromUrl !== stored) setStored(fromUrl);
    if (fromUrl) {
      params.delete('key');
      setParams(params, { replace: true });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fromUrl]);
  return [fromUrl ?? stored, setStored];
}

export function AdminRoom() {
  const { id = '' } = useParams();
  const [key, setKey] = useAdminKey(id);
  // A live room is reached by the signed-in account's session; it needs no admin key.
  if (liveRooms) return <AdminView id={id} adminKey="" onBadKey={() => undefined} />;
  if (!key) return <KeyPrompt onKey={setKey} />;
  return <AdminView id={id} adminKey={key} onBadKey={() => setKey('')} />;
}

function KeyPrompt({ onKey }: { onKey: (k: string) => void }) {
  const [v, setV] = useState('');
  return (
    <div className="center-msg">
      <h1>Host access</h1>
      <p className="muted">Open the admin link you got when you created the room, or paste its key.</p>
      <form
        className="inline-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (v.trim()) onKey(v.trim());
        }}
      >
        <input value={v} onChange={(e) => setV(e.target.value)} placeholder="Admin key" aria-label="Admin key" autoComplete="off" />
        <button className="btn btn-primary" type="submit">
          Continue
        </button>
      </form>
    </div>
  );
}

function AdminView({ id, adminKey, onBadKey }: { id: string; adminKey: string; onBadKey: () => void }) {
  const [params] = useSearchParams();
  const { room, hints, feed, link } = useLiveRoom(id, adminKey);
  const admin = useMemo(() => api.admin(id, adminKey), [id, adminKey]);
  const audit = useLoad(() => (liveRooms ? Promise.resolve([]) : admin.audit()), [admin, feed.length]);
  const toast = useToast();
  const [acting, setActing] = useState<Slot | 'pick' | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);
  const [side, setSide] = useState<'feed' | 'audit'>('feed');

  useEffect(() => {
    if ((audit.error as { status?: number } | undefined)?.status === 403) onBadKey();
  }, [audit.error, onBadKey]);

  if (room.error) return <p className="error-text">Could not load the room: {room.error.message}</p>;
  if (!room.data) return <p className="muted">Loading room...</p>;
  const r = room.data;
  const done = r.slotList.reduce((a, s) => a + s.checksDone, 0);
  const total = r.slotList.reduce((a, s) => a + s.checksTotal, 0);

  const run = (p: Promise<unknown>, ok: string) =>
    p.then(
      () => {
        toast(ok);
        room.reload();
        audit.reload();
      },
      (e: Error) => toast(e.message, 'error'),
    );

  const playerLink = `${location.origin}/rooms/${r.id}`;

  return (
    <div className="room admin">
      <div className="page-head">
        <div>
          <div className="title-row">
            <h1>{r.name}</h1>
            <RoomStatePill state={r.state} />
            <LinkBadge link={link} />
            <span className="tag-admin">Host</span>
          </div>
          <p className="muted small">
            {r.address} · seed {r.seed}
          </p>
        </div>
        <div className="head-actions">
          <Link className="btn btn-ghost" to={`/rooms/${r.id}`}>
            Player view
          </Link>
          {!liveRooms && r.state === 'running' && (
            <button type="button" className="btn" onClick={() => run(admin.setState('paused'), 'Room paused')}>
              Pause
            </button>
          )}
          {!liveRooms && r.state === 'paused' && (
            <button type="button" className="btn" onClick={() => run(admin.setState('running'), 'Room resumed')}>
              Resume
            </button>
          )}
          {!liveRooms && r.state !== 'closed' && (
            <button type="button" className="btn btn-danger" onClick={() => setConfirmClose(true)}>
              Close room
            </button>
          )}
          {!liveRooms && (r.state === 'running' || r.state === 'paused') && (
            <button type="button" className="btn btn-primary" onClick={() => setActing('pick')}>
              Send item / hint
            </button>
          )}
        </div>
      </div>

      <Phases state={r.state} />

      {liveRooms && (
        <div className="callout">
          <strong>Host controls are not available yet.</strong> The server cannot yet send items, give free hints, rename players,
          set slot passwords, release, collect, disconnect, pause, start, close, broadcast, edit rules or keep a host action log,
          so this page only shows the live room.
        </div>
      )}

      {params.get('created') && (
        <div className="callout ok">
          <strong>Your room is set up.</strong> Send players the {r.code ? 'room code or link' : 'player link'}; it shows them the server
          address and their slot name.
          <div className="share-line">
            {r.code ? (
              <>
                <span className="code-big">{r.code}</span>
                <code>{shortLink(r.code)}</code> <CopyButton text={shortLink(r.code)} label="Copy link" />
              </>
            ) : (
              <>
                <code>{playerLink}</code> <CopyButton text={playerLink} label="Copy player link" />
              </>
            )}
          </div>
          <div className="small">Keep the admin link private: anyone with it can manage this room.</div>
        </div>
      )}

      {r.state === 'generating' && (
        <Card>
          <p>
            <span className="spinner" aria-hidden />
            <strong>Generating the world file.</strong> <span className="muted">This places every item in every game. Players can connect when it is done.</span>
          </p>
        </Card>
      )}
      {!liveRooms && r.state === 'lobby' && <LobbyCard room={r} onStart={() => run(admin.setState('running'), 'Game started')} />}

      <div className="stats stats-row">
        <Stat label="Online" value={`${r.slotsOnline}/${r.slots}`} />
        <Stat label="Goals done" value={`${r.slotsDone}/${r.slots}`} />
        <Stat label="Checks" value={`${pct(done, total)}%`} sub={`${done} of ${total}`} />
        {!liveRooms && <Stat label="Open hints" value={(hints.data ?? []).filter((h) => h.status !== 'found').length} />}
      </div>

      <Card title="Players" actions={liveRooms ? undefined : <span className="muted small">Actions per player: items, hints, rename, release, kick</span>}>
        <SlotTable
          slots={r.slotList}
          actions={liveRooms ? undefined : (s) => (
            <button type="button" className="btn btn-small" onClick={() => setActing(s)} disabled={r.state === 'closed' || r.state === 'generating'}>
              Manage
            </button>
          )}
        />
      </Card>

      <div className="room-cols">
        <Card
          title={liveRooms ? 'Activity' : (
            <div className="seg" role="group" aria-label="Show">
              <button type="button" className={side === 'feed' ? 'on' : ''} onClick={() => setSide('feed')}>
                Activity
              </button>
              <button type="button" className={side === 'audit' ? 'on' : ''} onClick={() => setSide('audit')}>
                Host actions
              </button>
            </div>
          )}
        >
          {liveRooms || side === 'feed' ? (
            <Feed feed={feed} footer={!liveRooms && r.state !== 'closed' ? <Broadcast onSend={(t) => run(admin.broadcast(t), 'Message sent')} /> : undefined} />
          ) : (
            <ol className="audit">
              {(audit.data ?? []).length === 0 && <Empty>No host actions yet.</Empty>}
              {(audit.data ?? []).map((a, i) => (
                <li key={i}>
                  <time dateTime={a.at}>{clock(a.at)}</time>
                  <strong>{a.action}</strong>
                  <span className="muted">{a.detail}</span>
                </li>
              ))}
            </ol>
          )}
        </Card>
        {!liveRooms && (
          <div className="stack">
            <RulesCard room={r} onSave={(patch) => run(admin.updateRules(patch), 'Rules saved')} />
            <Card title="All hints">
              <HintBoard hints={hints.data ?? []} slots={r.slotList} me={null} />
            </Card>
          </div>
        )}
      </div>

      <Modal open={acting !== null} title={acting === 'pick' ? 'Send an item or hint' : acting ? `Manage ${acting.name}` : ''} onClose={() => setActing(null)}>
        {acting && (
          <SlotActions
            room={r}
            initial={acting === 'pick' ? null : acting}
            onDone={() => setActing(null)}
            run={run}
            admin={admin}
            quick={acting === 'pick'}
          />
        )}
      </Modal>

      <Modal open={confirmClose} title="Close this room?" onClose={() => setConfirmClose(false)}>
        <p>Every player is disconnected and nobody can join again. The activity and results stay visible.</p>
        <div className="modal-actions">
          <button type="button" className="btn" onClick={() => setConfirmClose(false)}>
            Keep it open
          </button>
          <button
            type="button"
            className="btn btn-danger"
            onClick={() => {
              setConfirmClose(false);
              run(admin.setState('closed'), 'Room closed');
            }}
          >
            Close room
          </button>
        </div>
      </Modal>
    </div>
  );
}

/** Players can connect, nothing is sent yet; the host starts when enough are in. */
function LobbyCard({ room: r, onStart }: { room: Room; onStart: () => void }) {
  const [sure, setSure] = useState(false);
  const inRoom = r.slotList.filter((s) => s.status !== 'offline');
  const all = inRoom.length === r.slotList.length;
  return (
    <Card title="Waiting for players" actions={<span className="muted small">{inRoom.length} of {r.slotList.length} connected</span>}>
      <ul className="lobby-list">
        {r.slotList.map((s) => (
          <li key={s.slot}>
            <div>
              <strong>{s.name}</strong>
              <div className="small muted">{s.game}</div>
            </div>
            <StatusPill status={s.status} />
          </li>
        ))}
      </ul>
      <div className="lobby-foot">
        <span className="small muted">
          {all ? 'Everyone is here.' : 'Players who join late still get every item sent to them.'} Starting lets checks and items flow.
        </span>
        {all || sure ? (
          <button type="button" className="btn btn-primary" onClick={onStart}>
            {all ? 'Start game' : `Start with ${inRoom.length} of ${r.slotList.length}`}
          </button>
        ) : (
          <button type="button" className="btn" onClick={() => setSure(true)}>
            Start without everyone...
          </button>
        )}
      </div>
    </Card>
  );
}

function Broadcast({ onSend }: { onSend: (t: string) => void }) {
  const [t, setT] = useState('');
  return (
    <form
      className="chat"
      onSubmit={(e) => {
        e.preventDefault();
        if (t.trim()) {
          onSend(t.trim());
          setT('');
        }
      }}
    >
      <input value={t} onChange={(e) => setT(e.target.value)} placeholder="Message everyone as the host" aria-label="Host message" maxLength={400} />
      <button type="submit" className="btn" disabled={!t.trim()}>
        Send
      </button>
    </form>
  );
}

function RulesCard({ room, onSave }: { room: Room; onSave: (patch: Partial<RoomRules>) => void }) {
  const [draft, setDraft] = useState(room.rules);
  useEffect(() => setDraft(room.rules), [room.rules]);
  const dirty = (Object.keys(draft) as (keyof RoomRules)[]).filter((k) => draft[k] !== room.rules[k]);
  return (
    <Card
      title="Rules"
      actions={
        dirty.length > 0 && (
          <>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => setDraft(room.rules)}>
              Discard
            </button>
            <button
              type="button"
              className="btn btn-primary btn-small"
              onClick={() => onSave(Object.fromEntries(dirty.map((k) => [k, draft[k]])) as Partial<RoomRules>)}
            >
              Save {dirty.length} change{dirty.length === 1 ? '' : 's'}
            </button>
          </>
        )
      }
    >
      <RulesForm rules={draft} onChange={setDraft} />
    </Card>
  );
}

type Admin = ReturnType<typeof api.admin>;
type Tab = 'item' | 'hint' | 'slot' | 'danger';

function SlotActions({
  room,
  initial,
  onDone,
  run,
  admin,
  quick,
}: {
  room: Room;
  initial: Slot | null;
  onDone: () => void;
  run: (p: Promise<unknown>, ok: string) => Promise<void>;
  admin: Admin;
  quick: boolean;
}) {
  const [slotNo, setSlotNo] = useState<number>(initial?.slot ?? room.slotList[0]?.slot ?? 1);
  const slot = room.slotList.find((s) => s.slot === slotNo)!;
  const [tab, setTab] = useState<Tab>('item');
  const [catalog, setCatalog] = useState<GameCatalog | null>(null);
  const [item, setItem] = useState('');
  const [count, setCount] = useState(1);
  const [name, setName] = useState(slot.name);
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState<'release' | 'collect' | 'kick' | null>(null);

  useEffect(() => {
    setCatalog(null);
    setItem('');
    setName(slot.name);
    api.catalog(slot.game).then(setCatalog, () => undefined);
  }, [slot.game, slot.name]);

  const known = catalog?.items.find((i) => i.name === item);
  const tabs: [Tab, string][] = quick
    ? [
        ['item', 'Send item'],
        ['hint', 'Give hint'],
      ]
    : [
        ['item', 'Send item'],
        ['hint', 'Give hint'],
        ['slot', 'Name & password'],
        ['danger', 'Release, collect, kick'],
      ];

  return (
    <div className="slot-actions">
      <div className="slot-actions-head">
        {quick ? (
          <label className="field">
            <span>Player</span>
            <select value={slotNo} onChange={(e) => setSlotNo(Number(e.target.value))}>
              {room.slotList.map((s) => (
                <option key={s.slot} value={s.slot}>
                  {s.name} ({s.game})
                </option>
              ))}
            </select>
          </label>
        ) : (
          <div className="muted small">
            Slot {slot.slot} · {slot.game} · <StatusPill status={slot.status} />
          </div>
        )}
      </div>
      <div className="tabs" role="tablist">
        {tabs.map(([k, label]) => (
          <button key={k} type="button" role="tab" aria-selected={tab === k} className={tab === k ? 'on' : ''} onClick={() => setTab(k)}>
            {label}
          </button>
        ))}
      </div>

      {(tab === 'item' || tab === 'hint') && (
        <form
          className="form"
          onSubmit={(e) => {
            e.preventDefault();
            if (!item.trim()) return;
            const p = tab === 'item' ? admin.sendItem(slot.slot, item.trim(), count) : admin.hint(slot.slot, item.trim());
            run(p, tab === 'item' ? `Sent ${count} x ${item} to ${slot.name}` : `Hint given to ${slot.name}`).then(onDone);
          }}
        >
          <label className="field">
            <span>Item from {slot.game}</span>
            <input list="catalog-items" value={item} onChange={(e) => setItem(e.target.value)} placeholder="Start typing an item name" autoFocus />
            <datalist id="catalog-items">
              {catalog?.items.map((i) => (
                <option key={i.name} value={i.name}>
                  {i.flag}
                </option>
              ))}
            </datalist>
          </label>
          {catalog && (
            <div className="chips">
              {catalog.items
                .filter((i) => i.flag === 'progression' || i.flag === 'useful')
                .map((i) => (
                  <button type="button" key={i.name} className={`chip ${item === i.name ? 'on' : ''}`} onClick={() => setItem(i.name)}>
                    {i.name}
                  </button>
                ))}
            </div>
          )}
          <div className="small muted">
            {known ? (
              <>
                Known item: <FlagTag flag={known.flag} />
              </>
            ) : item ? (
              'Not in this game\'s item list. The server will refuse it.'
            ) : tab === 'item' ? (
              'The item is added to the player\'s inventory as if found.'
            ) : (
              'Reveals where the item is, free of hint points.'
            )}
          </div>
          {tab === 'item' && (
            <label className="field field-narrow">
              <span>How many</span>
              <input type="number" min={1} max={99} value={count} onChange={(e) => setCount(Math.max(1, Math.min(99, Number(e.target.value) || 1)))} />
            </label>
          )}
          <div className="modal-actions">
            <button type="button" className="btn" onClick={onDone}>
              Cancel
            </button>
            <button type="submit" className="btn btn-primary" disabled={!item.trim()}>
              {tab === 'item' ? `Send to ${slot.name}` : `Hint for ${slot.name}`}
            </button>
          </div>
        </form>
      )}

      {tab === 'slot' && (
        <div className="form">
          <form
            className="inline-form"
            onSubmit={(e) => {
              e.preventDefault();
              run(admin.updateSlot(slot.slot, { name: name.trim() }), `Renamed to ${name.trim()}`);
            }}
          >
            <label className="field grow">
              <span>Connection name</span>
              <input value={name} onChange={(e) => setName(e.target.value)} maxLength={16} />
            </label>
            <button type="submit" className="btn" disabled={!name.trim() || name.trim() === slot.name}>
              Rename
            </button>
          </form>
          <p className="small muted">The player must reconnect with the new name.</p>
          <form
            className="inline-form"
            onSubmit={(e) => {
              e.preventDefault();
              run(admin.updateSlot(slot.slot, { password }), password ? 'Slot password set' : 'Slot password cleared');
              setPassword('');
            }}
          >
            <label className="field grow">
              <span>Slot password</span>
              <input value={password} onChange={(e) => setPassword(e.target.value)} placeholder="Empty clears it" autoComplete="off" />
            </label>
            <button type="submit" className="btn">
              {password ? 'Set' : 'Clear'}
            </button>
          </form>
        </div>
      )}

      {tab === 'danger' && (
        <div className="danger-list">
          {(
            [
              ['release', 'Release', `Send every item still in ${slot.name}'s world to its owner. Cannot be undone.`],
              ['collect', 'Collect', `Give ${slot.name} every one of their items still in other worlds.`],
              ['kick', 'Disconnect', `Drop ${slot.name}'s clients. They can reconnect.`],
            ] as const
          ).map(([k, label, text]) => (
            <div key={k} className="danger-row">
              <div>
                <strong>{label}</strong>
                <div className="small muted">{text}</div>
              </div>
              {confirm === k ? (
                <div className="confirm">
                  <button type="button" className="btn btn-small" onClick={() => setConfirm(null)}>
                    Cancel
                  </button>
                  <button
                    type="button"
                    className="btn btn-danger btn-small"
                    onClick={() => {
                      const p = k === 'release' ? admin.release(slot.slot) : k === 'collect' ? admin.collect(slot.slot) : admin.kick(slot.slot);
                      run(p, `${label}: ${slot.name}`).then(onDone);
                    }}
                  >
                    Yes, {label.toLowerCase()}
                  </button>
                </div>
              ) : (
                <button type="button" className="btn btn-small" onClick={() => setConfirm(k)}>
                  {label}
                </button>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
