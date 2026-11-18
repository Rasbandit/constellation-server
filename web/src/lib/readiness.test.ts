import { describe, expect, it } from 'vitest';
import type { ApWorld, PlayerFile } from '../api/types';
import { checkReadiness } from './readiness';

const world = (game: string, version = '1.0.0'): ApWorld => ({
  id: `${game}-${version}`,
  fileName: `${game}.apworld`,
  game,
  version,
  sizeBytes: 1,
});
const player = (id: string, name: string, game: string | null): PlayerFile => ({
  id,
  fileName: `${id}.yaml`,
  name,
  game,
});
const base = { roomName: 'Room', builtinGames: ['Archipela-Go'] };

describe('checkReadiness', () => {
  it('is ready when every game has its world and names are unique', () => {
    const r = checkReadiness({
      ...base,
      apworlds: [world('Starfall Keep')],
      players: [player('a', 'Alice', 'Starfall Keep'), player('b', 'Bob', 'Archipela-Go')],
    });
    expect(r.ready).toBe(true);
    expect(r.missingGames).toEqual([]);
  });

  it('names the games that still need an APWorld', () => {
    const r = checkReadiness({
      ...base,
      apworlds: [],
      players: [player('a', 'Alice', 'Tidewater Farm'), player('b', 'Bob', 'Starfall Keep')],
    });
    expect(r.ready).toBe(false);
    expect(r.missingGames).toEqual(['Starfall Keep', 'Tidewater Farm']);
  });

  it('flags both players that share a name, ignoring case', () => {
    const r = checkReadiness({
      ...base,
      apworlds: [],
      players: [player('a', 'Alice', 'Archipela-Go'), player('b', 'alice', 'Archipela-Go')],
    });
    expect(r.ready).toBe(false);
    expect(Object.keys(r.nameProblems).sort()).toEqual(['a', 'b']);
  });

  it('refuses empty, padded and overlong names', () => {
    const r = checkReadiness({
      ...base,
      apworlds: [],
      players: [
        player('a', '', 'Archipela-Go'),
        player('b', ' Bob', 'Archipela-Go'),
        player('c', 'A'.repeat(17), 'Archipela-Go'),
      ],
    });
    expect(Object.keys(r.nameProblems).sort()).toEqual(['a', 'b', 'c']);
  });

  it('refuses two versions of one game but only warns about an unused world', () => {
    const dup = checkReadiness({
      ...base,
      apworlds: [world('Starfall Keep', '1.0.0'), world('Starfall Keep', '1.1.0')],
      players: [player('a', 'Alice', 'Starfall Keep')],
    });
    expect(dup.ready).toBe(false);

    const unused = checkReadiness({
      ...base,
      apworlds: [world('Tidewater Farm')],
      players: [player('a', 'Alice', 'Archipela-Go')],
    });
    expect(unused.ready).toBe(true);
    expect(unused.checks.some((c) => c.level === 'warning')).toBe(true);
  });

  it('refuses a player file that could not be read', () => {
    const r = checkReadiness({
      ...base,
      apworlds: [],
      players: [{ ...player('a', 'Alice', null), error: 'no game: key' }],
    });
    expect(r.ready).toBe(false);
  });

  it('needs an APWorld for every game a weighted pick can roll', () => {
    const r = checkReadiness({
      ...base,
      apworlds: [world('Starfall Keep')],
      players: [player('a', 'Alice', 'Starfall Keep, Tidewater Farm')],
    });
    expect(r.missingGames).toEqual(['Tidewater Farm']);
  });
});
