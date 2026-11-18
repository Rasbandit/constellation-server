import { Link } from 'react-router-dom';

export function NotFound() {
  return (
    <div className="center-msg">
      <h1>Page not found</h1>
      <p className="muted">That link does not lead anywhere.</p>
      <Link className="btn btn-primary" to="/">
        Back to rooms
      </Link>
    </div>
  );
}
