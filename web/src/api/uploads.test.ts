import { describe, expect, it } from 'vitest';
import { gamesOf, toApWorld, toManifest, toPlayers } from './uploads';

const id = 'a'.repeat(64);

describe('upload records', () => {
  it('maps an apworld record, falling back to the module without a game', () => {
    const rec = { id, name: 'sk.apworld', bytes: 10, module: 'starfall_keep', game: 'Starfall Keep', world_version: '1.2.0', authors: ['x'] };
    expect(toApWorld(rec)).toEqual({ id, fileName: 'sk.apworld', game: 'Starfall Keep', version: '1.2.0', sizeBytes: 10, module: 'starfall_keep' });
    expect(toApWorld({ ...rec, game: null, world_version: null })).toMatchObject({ game: 'starfall_keep', version: 'unknown' });
  });

  it('maps a manifest record to the world the page shows', () => {
    const rec = { id, name: 'world.json', bytes: 830, game: 'Fixture', version: '1.2', items: 237, locations: 2641 };
    expect(toManifest(rec)).toEqual({ id, fileName: 'world.json', game: 'Fixture', version: '1.2', sizeBytes: 830 });
  });

  it('turns each player document into a player that remembers its YAML', () => {
    const players = toPlayers({ id, name: 'group.yaml', bytes: 50, players: [{ name: 'A', game: 'G' }, { name: 'B', game: 'H' }] });
    expect(players.map((p) => [p.id, p.name, p.yamlId, p.yamlName, p.fileName])).toEqual([
      [`${id}#0`, 'A', id, 'A', 'group.yaml (1 of 2)'],
      [`${id}#1`, 'B', id, 'B', 'group.yaml (2 of 2)'],
    ]);
    expect(toPlayers({ id, name: 'one.yaml', bytes: 5, players: [{ name: 'A', game: 'G' }] })[0]).toMatchObject({ id, fileName: 'one.yaml' });
  });

  it('splits a weighted pick into its games', () => {
    expect(gamesOf('Lantern Run, Tidewater Farm')).toEqual(['Lantern Run', 'Tidewater Farm']);
    expect(gamesOf('Starfall Keep')).toEqual(['Starfall Keep']);
    expect(gamesOf(null)).toEqual([]);
  });
});
