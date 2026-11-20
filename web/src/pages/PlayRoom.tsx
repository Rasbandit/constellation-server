import { useState, type ReactNode } from 'react';
import { Link, useParams } from 'react-router-dom';
import { api, liveRooms } from '../api/client';
import type { Hint, HintStatus, Room, Slot } from '../api/types';
import { Feed, HintBoard, LinkBadge, SlotTable } from '../components/room';
import { shortLink } from '../components/phases';
import { Card, CopyButton, RoomStatePill, StatusPill, useToast } from '../components/ui';
import { pct } from '../lib/format';
import { useLiveRoom, useStored } from '../lib/live';

export function PlayRoom() {
  const { id = '' } = useParams();
  const { room, hints, feed, link } = useLiveRoom(id);
  const [me, setMe] = useStored<number | null>(`constellation.me.${id}`, null);
  const toast = useToast();

  if (room.error) {
    return (
      <div className="center-msg">
        <h1>Room not found</h1>
        <p className="muted">{room.error.message}</p>
        <Link className="btn" to="/">
          Back to rooms
        </Link>
      </div>
    );
  }
  if (!room.data) return <p className="muted">Loading room...</p>;
  const r = room.data;
  const mine = r.slotList.find((s) => s.slot === me) ?? null;

  const onHintStatus = (h: Hint, status: HintStatus) =>
    api.setHintStatus(id, h.id, status).then(
      () => hints.reload(),
      (e: Error) => toast(e.message, 'error'),
    );

  return (
    <div className="room">
      <div className="page-head">
        <div>
          <div className="title-row">
            <h1>{r.name}</h1>
            <RoomStatePill state={r.state} />
            <LinkBadge link={link} />
          </div>
          <p className="muted small">
            {r.slotsOnline} of {r.slots} online · {r.slotsDone} done · seed {r.seed}
          </p>
        </div>
        <label className="whoami">
          <span>I am</span>
          <select value={me ?? ''} onChange={(e) => setMe(e.target.value ? Number(e.target.value) : null)}>
            <option value="">Just watching</option>
            {r.slotList.map((s) => (
              <option key={s.slot} value={s.slot}>
                {s.name} ({s.game})
              </option>
            ))}
          </select>
        </label>
      </div>

      {r.state === 'generating' && (
        <div className="callout">
          <span>
            <span className="spinner" aria-hidden />
            The host is generating the world. Connect details work in a moment.
          </span>
        </div>
      )}
      {r.state === 'lobby' && (
        <div className="callout ok">Connect now. Nothing is sent until the host starts the game, so you will not miss anything.</div>
      )}
      {r.state === 'paused' && <div className="callout warn">The host paused this room. Your client stays connected; checks resume when the host continues.</div>}
      {r.state === 'closed' && <div className="callout">This room is closed. The results below are final.</div>}

      <div className="room-top">
        <ConnectCard room={r} me={mine} />
        {mine ? <MyProgress slot={mine} hints={hints.data ?? []} /> : <PickSlotCard />}
      </div>

      <Card title="Players">
        <SlotTable slots={r.slotList} me={me} />
      </Card>

      <div className="room-cols">
        <Card title="Activity">
          {/* Chat from the page needs a server endpoint; live rooms show their players' chat only. */}
          <Feed feed={feed} me={me} footer={mine && r.state !== 'closed' && !liveRooms ? <ChatBox roomId={id} slot={mine} /> : undefined} />
        </Card>
        <Card title="Hints">
          {/* Live rooms are read-only here: a player marks hints from their client. */}
          <HintBoard hints={hints.data ?? []} slots={r.slotList} me={me} onStatus={liveRooms ? undefined : onHintStatus} />
        </Card>
      </div>
    </div>
  );
}

function ConnectCard({ room: r, me }: { room: Room; me: Slot | null }) {
  return (
    <Card title="How to connect" className="connect">
      <div className="join-ways">
        {/* Live rooms have no short codes yet; the page link is the way to share. */}
        {!liveRooms && <div className="join-way">
          <span className="small muted">Room code (open the site, or share it)</span>
          {r.code ? (
            <>
              <span className="code-big">{r.code}</span>
              <div className="row-inline">
                <code>{shortLink(r.code)}</code>
                <CopyButton text={shortLink(r.code)} label="Copy link" />
              </div>
            </>
          ) : (
            <span className="muted">The room is closed, so its code was released.</span>
          )}
        </div>}
        <div className="join-way">
          <span className="small muted">Server (type into your game's client)</span>
          <code>{r.address}</code>
          <div className="row-inline">
            <CopyButton text={r.address} />
          </div>
        </div>
      </div>
      <ol className="connect-steps">
        <li>
          <span>Slot name</span>
          {me ? (
            <>
              <code>{me.name}</code>
              <CopyButton text={me.name} />
            </>
          ) : (
            <span className="muted">Pick yourself under "I am" to copy it here.</span>
          )}
        </li>
        <li>
          <span>Password</span>
          {r.rules.password ? <span>Ask the host.</span> : <span className="muted">None</span>}
        </li>
      </ol>
    </Card>
  );
}

function PickSlotCard() {
  return (
    <Card title="Which player are you?">
      <p className="muted">
        Choose your slot under <strong>I am</strong> at the top. The page then shows your connect details, your progress, and the
        hints that matter to you. Nothing is sent to the server; it is remembered in this browser only.
      </p>
    </Card>
  );
}

function MyProgress({ slot, hints }: { slot: Slot; hints: Hint[] }) {
  const looking = hints.filter((h) => h.receiving === slot.slot && h.status !== 'found').length;
  const owed = hints.filter((h) => h.finding === slot.slot && h.receiving !== slot.slot && h.status !== 'found').length;
  return (
    <Card title={`${slot.name}'s progress`} actions={<StatusPill status={slot.status} />}>
      <div className="stats">
        <Stat label="Checks" value={`${pct(slot.checksDone, slot.checksTotal)}%`} sub={`${slot.checksDone} of ${slot.checksTotal}`} />
        <Stat label="Items received" value={slot.itemsReceived} />
        {slot.hintPoints !== null && <Stat label="Hint points" value={slot.hintPoints} />}
        {!liveRooms && <Stat label="Hints open" value={looking} sub={`${owed} for others in your world`} />}
      </div>
    </Card>
  );
}

export function Stat({ label, value, sub }: { label: string; value: ReactNode; sub?: string }) {
  return (
    <div className="stat">
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value}</span>
      {sub && <span className="stat-sub">{sub}</span>}
    </div>
  );
}

function ChatBox({ roomId, slot }: { roomId: string; slot: Slot }) {
  const [text, setText] = useState('');
  const toast = useToast();
  return (
    <form
      className="chat"
      onSubmit={(e) => {
        e.preventDefault();
        const t = text.trim();
        if (!t) return;
        api.chat(roomId, slot.slot, t).then(
          () => setText(''),
          (err: Error) => toast(err.message, 'error'),
        );
      }}
    >
      <input value={text} onChange={(e) => setText(e.target.value)} placeholder={`Message as ${slot.name}`} aria-label="Chat message" maxLength={400} />
      <button type="submit" className="btn" disabled={!text.trim()}>
        Send
      </button>
    </form>
  );
}
