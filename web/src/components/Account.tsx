import { useEffect, type ReactNode } from 'react';
import { Navigate, NavLink, useNavigate } from 'react-router-dom';
import { loadAccount, logOut, sessionGate, useAccount } from '../api/account';
import { liveRooms } from '../api/client';

/** Shows its page once a session is known; goes to `/login` when signed out. The demo stub needs none. */
export function RequireSession({ children, live = liveRooms }: { children: ReactNode; live?: boolean }) {
  const account = useAccount();
  useEffect(() => {
    if (live && !account.known) void loadAccount();
  }, [live, account.known]);
  const gate = sessionGate(account, live);
  if (gate === 'login') return <Navigate to="/login" replace />;
  return gate === 'show' ? <>{children}</> : null;
}

/** The header's account part: who is signed in and a way out, or the ways in. */
export function UserMenu() {
  const { name } = useAccount();
  const nav = useNavigate();
  if (!liveRooms) return null;
  if (name) {
    return (
      <p className="user-menu">
        Signed in as <strong>{name}</strong>{' '}
        <button type="button" className="btn btn-ghost" onClick={() => void logOut().then(() => nav('/login'))}>
          Log out
        </button>
      </p>
    );
  }
  return (
    <nav className="user-menu" aria-label="Account">
      <NavLink to="/login">Log in</NavLink>
      <NavLink to="/signup">Sign up</NavLink>
    </nav>
  );
}
