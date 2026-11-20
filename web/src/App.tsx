import { useEffect, useState } from 'react';
import { NavLink, Route, Routes } from 'react-router-dom';
import { adminApiReachable } from './api/reachable';
import { liveRooms } from './api/client';
import { AdminRoom } from './pages/AdminRoom';
import { CodeRedirect } from './pages/CodeRedirect';
import { CreateRoom } from './pages/CreateRoom';
import { Home } from './pages/Home';
import { NotFound } from './pages/NotFound';
import { PlayRoom } from './pages/PlayRoom';
import { VersionTag } from './components/ui';

export function App() {
  // Only a real server can lack the API; the dev stub always has it.
  const [apiDown, setApiDown] = useState(false);
  useEffect(() => {
    if (liveRooms) void adminApiReachable().then((up) => setApiDown(!up));
  }, []);
  return (
    <div className="shell">
      <header className="topbar">
        <NavLink to="/" className="brand">
          <span className="brand-mark" aria-hidden>
            ✦
          </span>
          Constellation
        </NavLink>
        <nav className="topnav">
          <NavLink to="/" end>
            Rooms
          </NavLink>
          {!apiDown && <NavLink to="/create">Create room</NavLink>}
          {/* Temporary shortcut to a seeded room's admin page while there is no host sign-in. */}
          {!liveRooms && (
            <NavLink to="/rooms/friday-7k2q/admin?key=demo-admin" className="nav-temp">
              Admin (demo)
            </NavLink>
          )}
        </nav>
      </header>
      <main className="page">
        {apiDown && (
          <p className="callout warn" role="alert">
            <strong>Admin API not connected.</strong> Creating rooms and uploading files need the server's admin API; this address does
            not forward <code>/api</code> to it. An operator can add a proxy (see docs/container.md, "The admin API behind the UI").
          </p>
        )}
        {!liveRooms && (
          <p className="callout warn" role="note">
            <strong>Demo data.</strong> Nothing on these pages is a real room: rooms, players, checks and activity are made up by the
            development stub. Set CONSTELLATION_ADMIN_URL to show a real server.
          </p>
        )}
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/create" element={<CreateRoom />} />
          <Route path="/c/:code" element={<CodeRedirect />} />
          <Route path="/rooms/:id" element={<PlayRoom />} />
          <Route path="/rooms/:id/admin" element={<AdminRoom />} />
          <Route path="*" element={<NotFound />} />
        </Routes>
      </main>
      <VersionTag />
    </div>
  );
}
