import type { ApWorld, PlayerFile, ReadinessCheck } from '../api/types';

/** Longest connection name a slot may have (estimate taken from what existing clients accept). */
export const NAME_MAX = 16;

export interface ReadinessInput {
  roomName: string;
  apworlds: ApWorld[];
  players: PlayerFile[];
  /** Games the server can generate without an uploaded APWorld. */
  builtinGames: string[];
}

export interface Readiness {
  ready: boolean;
  checks: ReadinessCheck[];
  /** Games some player needs that have no APWorld yet. */
  missingGames: string[];
  /** For each player id, what is wrong with its connection name (if anything). */
  nameProblems: Record<string, string>;
}

/** Everything that must hold before a room can be generated. The UI and the API share it. */
export function checkReadiness(input: ReadinessInput): Readiness {
  const checks: ReadinessCheck[] = [];
  const { apworlds, players, builtinGames } = input;

  if (input.roomName.trim() === '') {
    checks.push({ id: 'room-name', level: 'error', message: 'Give the room a name.' });
  } else {
    checks.push({ id: 'room-name', level: 'ok', message: 'Room has a name.' });
  }

  if (players.length === 0) {
    checks.push({ id: 'players', level: 'error', message: 'Add at least one player.' });
  } else {
    checks.push({
      id: 'players',
      level: 'ok',
      message: `${players.length} player${players.length === 1 ? '' : 's'} added.`,
    });
  }

  const unreadable = players.filter((p) => p.error || !p.game);
  if (unreadable.length > 0) {
    checks.push({
      id: 'yaml',
      level: 'error',
      message: `${unreadable.length} player file${unreadable.length === 1 ? '' : 's'} could not be read: ${unreadable
        .map((p) => p.fileName ?? p.name)
        .join(', ')}.`,
    });
  }

  const nameProblems: Record<string, string> = {};
  const seen = new Map<string, string>();
  for (const p of players) {
    const key = p.name.trim().toLowerCase();
    if (p.name.trim() === '') nameProblems[p.id] = 'Name is empty.';
    else if (p.name !== p.name.trim()) nameProblems[p.id] = 'No spaces at the start or end.';
    else if (p.name.length > NAME_MAX) nameProblems[p.id] = `At most ${NAME_MAX} characters.`;
    else if (seen.has(key)) {
      nameProblems[p.id] = 'Another player has this name.';
      nameProblems[seen.get(key)!] ??= 'Another player has this name.';
    } else seen.set(key, p.id);
  }
  const badNames = Object.keys(nameProblems).length;
  if (badNames > 0) {
    checks.push({
      id: 'names',
      level: 'error',
      message: `${badNames} connection name${badNames === 1 ? ' needs' : 's need'} fixing.`,
    });
  } else if (players.length > 0) {
    checks.push({ id: 'names', level: 'ok', message: 'Connection names are unique.' });
  }

  const byGame = new Map<string, ApWorld[]>();
  for (const w of apworlds) byGame.set(w.game, [...(byGame.get(w.game) ?? []), w]);
  for (const [game, worlds] of byGame) {
    if (worlds.length > 1) {
      checks.push({
        id: `dup-${game}`,
        level: 'error',
        message: `${game} has ${worlds.length} APWorlds (${worlds.map((w) => w.version).join(', ')}). Keep one.`,
      });
    }
  }

  const needed = [...new Set(players.map((p) => p.game).filter((g): g is string => !!g))];
  const missingGames = needed.filter((g) => !byGame.has(g) && !builtinGames.includes(g)).sort();
  if (missingGames.length > 0) {
    checks.push({
      id: 'apworlds',
      level: 'error',
      message: `Missing APWorld for ${missingGames.join(', ')}.`,
    });
  } else if (needed.length > 0) {
    checks.push({ id: 'apworlds', level: 'ok', message: 'Every game has its APWorld.' });
  }

  const unused = [...byGame.keys()].filter((g) => !needed.includes(g)).sort();
  if (unused.length > 0) {
    checks.push({
      id: 'unused',
      level: 'warning',
      message: `No player uses ${unused.join(', ')}. It will be ignored.`,
    });
  }

  return {
    ready: !checks.some((c) => c.level === 'error'),
    checks,
    missingGames,
    nameProblems,
  };
}
