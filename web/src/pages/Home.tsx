import { useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { api, liveRooms } from '../api/client';
import type { RoomState, RoomSummary } from '../api/types';
import { Card, Empty, RoomStatePill } from '../components/ui';
import { ago } from '../lib/format';
import { joinPath } from '../lib/join';
import { useLoad } from '../lib/live';

/** Where the join box goes; live rooms have no codes yet, only links. */
function openable(text: string): string | null {
  const path = joinPath(text);
  return path && (!liveRooms || path.startsWith('/rooms/')) ? path : null;
}

export function Home() {
  // A live server has no room list yet; the stub's list is demo data.
  const rooms = useLoad(() => (liveRooms ? Promise.resolve([]) : api.rooms()), []);
  const [join, setJoin] = useState('');
  const [tab, setTab] = useState<'active' | 'closed'>('active');
  const nav = useNavigate();
  const list = (rooms.data ?? []).filter((r) => (tab === 'closed' ? r.state === 'closed' : r.state !== 'closed'));
  const counts = (s: (r: RoomSummary) => boolean) => (rooms.data ?? []).filter(s).length;

  return (
    <div className="home">
      <section className="hero">
        <div>
          <h1>Run a multiworld without the guesswork</h1>
          <p className="lead">
            Upload your players and game files, see what is missing before you generate, then share one link. Everyone sees who
            is online, what was found, and which hints matter.
          </p>
        </div>
        <div className="hero-actions">
          <Link to="/create" className="action-card primary">
            <strong>Create a room</strong>
            <span>Players, APWorlds, rules. A checklist tells you when it is ready.</span>
          </Link>
          <form
            className="action-card"
            onSubmit={(e) => {
              e.preventDefault();
              const path = openable(join);
              if (path) nav(path);
            }}
          >
            <strong>Join a room</strong>
            <span>{liveRooms ? 'Paste the link the host sent you.' : 'Type the room code or paste the link the host sent you.'}</span>
            <div className="inline-form">
              <input
                value={join}
                onChange={(e) => setJoin(e.target.value)}
                placeholder={liveRooms ? 'Room link' : 'Room code, e.g. K7Q2XM'}
                aria-label={liveRooms ? 'Room link' : 'Room link or code'}
              />
              <button className="btn" type="submit" disabled={!openable(join)}>
                Open
              </button>
            </div>
          </form>
        </div>
      </section>

      <Card
        title="Rooms"
        actions={
          <div className="seg" role="group" aria-label="Which rooms">
            <button type="button" className={tab === 'active' ? 'on' : ''} onClick={() => setTab('active')}>
              Active <span className="count">{counts((r) => r.state !== 'closed')}</span>
            </button>
            <button type="button" className={tab === 'closed' ? 'on' : ''} onClick={() => setTab('closed')}>
              Closed <span className="count">{counts((r) => r.state === 'closed')}</span>
            </button>
          </div>
        }
      >
        {rooms.error && <p className="error-text">Could not load rooms: {rooms.error.message}</p>}
        {!rooms.data && !rooms.error && <p className="muted">Loading rooms...</p>}
        {liveRooms && <p className="muted">Not available yet: the server has no room list. Open a room from its link, or create one.</p>}
        {!liveRooms && rooms.data && list.length === 0 && (
          <Empty>{tab === 'active' ? 'No active rooms. Create one to get started.' : 'No closed rooms.'}</Empty>
        )}
        <ul className="room-list">
          {list.map((r) => (
            <RoomRow key={r.id} room={r} />
          ))}
        </ul>
      </Card>
    </div>
  );
}

function RoomRow({ room: r }: { room: RoomSummary }) {
  const dim: Record<RoomState, boolean> = { generating: false, lobby: false, running: false, paused: false, closed: true };
  return (
    <li className={`room-row ${dim[r.state] ? 'dim' : ''}`}>
      <div className="room-row-main">
        <div className="room-row-title">
          <Link to={`/rooms/${r.id}`}>{r.name}</Link>
          <RoomStatePill state={r.state} />
        </div>
        <div className="muted small">
          {r.code && <span className="code-inline">{r.code}</span>}
          {r.games.join(' · ')} · created {ago(r.createdAt)}
        </div>
      </div>
      <div className="room-row-stats">
        <span title="Players online">
          <strong>{r.slotsOnline}</strong>/{r.slots} online
        </span>
        <span title="Players who reached their goal">
          <strong>{r.slotsDone}</strong> done
        </span>
      </div>
      <div className="room-row-actions">
        <Link className="btn" to={`/rooms/${r.id}`}>
          Open
        </Link>
        {r.adminKey && (
          <Link className="btn btn-ghost" to={`/rooms/${r.id}/admin?key=${encodeURIComponent(r.adminKey)}`}>
            Manage
          </Link>
        )}
      </div>
    </li>
  );
}
