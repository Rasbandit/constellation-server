import type { RoomState } from '../api/types';

const steps = [
  { title: 'Set up', text: 'Players and game files' },
  { title: 'Generate', text: 'The world file is built' },
  { title: 'Players connect', text: 'Everyone joins the room' },
  { title: 'Play', text: 'Items start flowing' },
];

/** Index of the current step for a room state; `create` is the setup page itself. */
export function phaseOf(state: RoomState | 'create'): number {
  switch (state) {
    case 'create':
      return 0;
    case 'generating':
      return 1;
    case 'lobby':
      return 2;
    case 'running':
    case 'paused':
      return 3;
    case 'closed':
      return 4;
  }
}

/** Where a room is in its life: set up, generate, players connect, play. */
export function Phases({ state }: { state: RoomState | 'create' }) {
  const current = phaseOf(state);
  return (
    <ol className="phases" aria-label="Room progress">
      {steps.map((s, i) => {
        const status = i < current ? 'done' : i === current ? 'current' : 'todo';
        return (
          <li key={s.title} className={`phase phase-${status}`} aria-current={status === 'current' ? 'step' : undefined}>
            <span className="phase-dot" aria-hidden>
              {status === 'done' ? '✓' : i + 1}
            </span>
            <div>
              <strong>{s.title}</strong>
              <span className="small muted">{s.text}</span>
            </div>
          </li>
        );
      })}
    </ol>
  );
}

/** The short link a player can open or type: `<origin>/c/<code>`. */
export function shortLink(code: string): string {
  return `${location.origin}/c/${code}`;
}
