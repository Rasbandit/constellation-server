import { useState, type FormEvent } from 'react';
import { accountName } from '../api/account';
import { api, HttpError, type RoomAdmins as Admins } from '../api/client';
import { useLoad } from '../lib/live';
import { Card, useToast } from './ui';

/**
 * Who may manage a room (its owner and its admins). Shown only to them: anyone else gets 404
 * from the server and sees nothing. The owner may remove any admin, an admin only themself.
 */
export function RoomAdmins({ roomKey }: { roomKey: string }) {
  const admins = useLoad<Admins>(() => api.roomAdmins(roomKey).list(), [roomKey]);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const list = admins.data;
  if (!list) return null;
  const me = accountName();

  const run = (go: () => Promise<Admins | undefined>) => {
    setBusy(true);
    go().then(
      (next) => (next ? admins.setData(next) : admins.reload()),
      (e: Error) => toast(e instanceof HttpError && e.status === 404 ? 'No such user.' : e.message, 'error'),
    ).finally(() => setBusy(false));
  };
  const add = (e: FormEvent) => {
    e.preventDefault();
    run(() => api.roomAdmins(roomKey).add(name.trim()));
    setName('');
  };

  return (
    <Card title="Room admins">
      <p className="muted small">
        The owner is <strong>{list.owner}</strong>. Admins can add other admins; only the owner can remove another admin.
      </p>
      <ul>
        {list.admins.map((a) => (
          <li key={a}>
            {a}{' '}
            {(list.role === 'owner' || a === me) && (
              <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => run(() => api.roomAdmins(roomKey).remove(a))}>
                {a === me ? 'Leave' : 'Remove'}
              </button>
            )}
          </li>
        ))}
      </ul>
      <form className="inline-form" onSubmit={add}>
        <label className="field">
          Add an admin by username <input value={name} onChange={(e) => setName(e.target.value)} autoComplete="off" required />
        </label>
        <button type="submit" className="btn" disabled={busy || !name.trim()}>
          Add
        </button>
      </form>
    </Card>
  );
}
