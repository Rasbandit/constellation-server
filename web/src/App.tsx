import { NavLink, Route, Routes } from 'react-router-dom';
import { AdminRoom } from './pages/AdminRoom';
import { CodeRedirect } from './pages/CodeRedirect';
import { CreateRoom } from './pages/CreateRoom';
import { Home } from './pages/Home';
import { NotFound } from './pages/NotFound';
import { PlayRoom } from './pages/PlayRoom';

export function App() {
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
          <NavLink to="/create">Create room</NavLink>
          {/* Temporary shortcut to a seeded room's admin page while there is no host sign-in. */}
          <NavLink to="/rooms/friday-7k2q/admin?key=demo-admin" className="nav-temp">
            Admin (demo)
          </NavLink>
        </nav>
      </header>
      <main className="page">
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/create" element={<CreateRoom />} />
          <Route path="/c/:code" element={<CodeRedirect />} />
          <Route path="/rooms/:id" element={<PlayRoom />} />
          <Route path="/rooms/:id/admin" element={<AdminRoom />} />
          <Route path="*" element={<NotFound />} />
        </Routes>
      </main>
    </div>
  );
}
