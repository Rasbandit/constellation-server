import { useState } from 'react';
import { logIn, logOut, signUp } from '../api/account';

/** Log in or create an account, or (signed in) who you are and a way out. */
export function AccountPanel({ name, signupOpen }: { name: string | null; signupOpen: boolean }) {
  const [user, setUser] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);

  if (name) {
    return (
      <p>
        Signed in as <strong>{name}</strong>{' '}
        <button type="button" className="btn btn-ghost" onClick={() => void logOut()}>
          Log out
        </button>
      </p>
    );
  }

  const submit = (enter: typeof logIn) => () => {
    setError(null);
    enter(user, password).then(
      () => setPassword(''),
      (e: Error) => setError(e.message),
    );
  };
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit(logIn)();
      }}
    >
      <p className="muted">Log in to see the rooms you made.</p>
      {error && (
        <p className="callout warn" role="alert">
          {error}
        </p>
      )}
      <label className="field">
        Username <input value={user} onChange={(e) => setUser(e.target.value)} autoComplete="username" required />
      </label>
      <label className="field">
        Password{' '}
        <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
      </label>
      <button type="submit" className="btn btn-primary">
        Log in
      </button>{' '}
      {signupOpen && (
        <button type="button" className="btn" onClick={submit(signUp)} disabled={!user || !password}>
          Create account
        </button>
      )}
    </form>
  );
}
