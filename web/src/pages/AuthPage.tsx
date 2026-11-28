import { useEffect, useState, type FormEvent } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { loadAccount, loadSignupOpen, logIn, signUp, useAccount } from '../api/account';
import { Card } from '../components/ui';

/** `/login` and `/signup`: one form, two modes. Signed in (already or just now), it goes Home. */
export function AuthPage({ mode }: { mode: 'login' | 'signup' }) {
  const login = mode === 'login';
  const { name, signupOpen } = useAccount();
  const nav = useNavigate();
  const [user, setUser] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    void loadAccount();
    void loadSignupOpen();
  }, []);
  useEffect(() => {
    if (name) nav('/', { replace: true });
  }, [name, nav]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    (login ? logIn : signUp)(user, password).then(
      () => setPassword(''),
      (err: Error) => {
        setError(err.message);
        setBusy(false);
      },
    );
  };
  if (!login && !signupOpen) {
    return (
      <Card title="Sign up">
        <p>Sign-up is closed on this server. <Link to="/login">Log in</Link> instead.</p>
      </Card>
    );
  }
  return (
    <Card title={login ? 'Log in' : 'Sign up'}>
      <form onSubmit={submit}>
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
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete={login ? 'current-password' : 'new-password'}
            required
          />
        </label>
        <button type="submit" className="btn btn-primary" disabled={busy}>
          {login ? 'Log in' : 'Create account'}
        </button>
      </form>
      <p className="muted">
        {login ? (
          signupOpen && (
            <>
              No account? <Link to="/signup">Sign up</Link>
            </>
          )
        ) : (
          <>
            Have an account? <Link to="/login">Log in</Link>
          </>
        )}
      </p>
    </Card>
  );
}
