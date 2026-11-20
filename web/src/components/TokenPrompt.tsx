import { useEffect, useState, type ReactNode } from 'react';
import { liveRooms } from '../api/client';
import { loadAuthMode, setAdminToken, useAdminToken } from '../api/token';

/** Shows its page once the admin token is known, or at once when the server needs none (open mode) or is the demo stub. */
export function NeedsToken({ children, live = liveRooms }: { children: ReactNode; live?: boolean }) {
  const { token, required } = useAdminToken();
  useEffect(() => {
    if (live && required === null) void loadAuthMode();
  }, [live, required]);
  if (!live || required === false || token) return <>{children}</>;
  return required === null ? null : <TokenPrompt />;
}

function TokenPrompt() {
  const { notice } = useAdminToken();
  const [text, setText] = useState('');
  return (
    <form
      className="card"
      onSubmit={(e) => {
        e.preventDefault();
        setAdminToken(text);
      }}
    >
      <h2>Admin token</h2>
      <p className="muted">
        This page uses the server's admin API. Enter the admin token (the server's <code>admin_token</code> secret). It is kept in this tab
        only and sent as an <code>Authorization</code> header.
      </p>
      {notice && (
        <p className="callout warn" role="alert">
          {notice}
        </p>
      )}
      <label className="field">
        Token <input type="password" value={text} onChange={(e) => setText(e.target.value)} autoComplete="off" required />
      </label>{' '}
      <button type="submit" className="btn btn-primary">
        Use token
      </button>
    </form>
  );
}
