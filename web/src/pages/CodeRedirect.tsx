import { useEffect, useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { api } from '../api/client';

/** `/c/:code`: the short link a host shares. Finds the room and goes to its player page. */
export function CodeRedirect() {
  const { code = '' } = useParams();
  const nav = useNavigate();
  const [error, setError] = useState('');
  useEffect(() => {
    api.resolveCode(code).then(
      ({ roomId }) => nav(`/rooms/${roomId}`, { replace: true }),
      (e: Error) => setError(e.message),
    );
  }, [code, nav]);
  if (!error) return <p className="muted">Finding room {code.toUpperCase()}...</p>;
  return (
    <div className="center-msg">
      <h1>No room with code {code.toUpperCase()}</h1>
      <p className="muted">The code may be mistyped, or the room has closed and its code was released.</p>
      <Link className="btn" to="/">
        Back to rooms
      </Link>
    </div>
  );
}
