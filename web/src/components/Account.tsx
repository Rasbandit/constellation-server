import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Link, Navigate, useNavigate } from 'react-router-dom';
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

/** Ends the session, then goes to the login page. */
export const signOutAndLeave = (to: (path: string) => void) => logOut().then(() => to('/login'));

/**
 * The header's account part: one Log in button when signed out; when signed in one profile
 * button that opens a menu (the native popover: outside click and Escape close it, and focus
 * returns to the button) with who you are and Log out.
 */
export function UserMenu({ live = liveRooms }: { live?: boolean }) {
  const { name } = useAccount();
  const nav = useNavigate();
  const [open, setOpen] = useState(false);
  const menu = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = menu.current;
    const on = (e: Event) => setOpen((e as Event & { newState?: string }).newState === 'open');
    el?.addEventListener('toggle', on);
    return () => el?.removeEventListener('toggle', on);
  }, [name]);
  if (!live) return null;
  if (!name) {
    return (
      <Link className="btn user-menu" to="/login">
        Log in
      </Link>
    );
  }
  return (
    <>
      <button type="button" className="btn btn-ghost user-menu" aria-haspopup="menu" aria-expanded={open} {...{ popovertarget: 'user-menu' }}>
        <span className="avatar" aria-hidden>
          {name.charAt(0).toUpperCase()}
        </span>
        {name}
      </button>
      <div id="user-menu" className="menu" role="menu" ref={menu} {...{ popover: '' }}>
        <p>
          Signed in as <strong>{name}</strong>
        </p>
        <button type="button" className="btn btn-ghost" role="menuitem" onClick={() => void signOutAndLeave(nav)}>
          Log out
        </button>
      </div>
    </>
  );
}
