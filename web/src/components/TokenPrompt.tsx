import { useState, type ReactNode } from 'react';
import { liveRooms } from '../api/client';
import { setAdminToken, useAdminToken } from '../api/token';

/** Shows its page once the admin token is known; against the demo stub there is no token. */
export function NeedsToken({ children }: { children: ReactNode }) {
  const { token } = useAdminToken();
  return !liveRooms || token ? <>{children}</> : <TokenPrompt />;
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
