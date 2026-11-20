import type { ApWorld, ManifestWorld, PlayerFile } from './types';

// Records of the server's upload API (`PUT /api/apworlds`, `PUT /api/yamls`; branch
// feat/web-upload-rooms, crates/server/src/worlds.rs). Uploads are content-addressed: the same
// file again returns the stored record with 200 instead of 201.

/** What the server stores for an `.apworld`: its own fields plus what inspect read from the manifest. */
export interface ApWorldRecord {
  id: string;
  name: string;
  bytes: number;
  module: string;
  game: string | null;
  world_version: string | null;
  [manifestField: string]: unknown;
}

/** What the server stores for a manifest (`PUT /api/manifests`): its own fields plus its game and version. */
export interface ManifestRecord {
  id: string;
  name: string;
  bytes: number;
  game: string;
  version: string;
  items: number;
  locations: number;
}

/** What the server stores for a player `.yaml`: one entry per player document in the file. */
export interface YamlRecord {
  id: string;
  name: string;
  bytes: number;
  /** `game` is one name, or for a weighted pick "A, B" (sorted, comma and space). */
  players: { name: string; game: string }[];
}

export function toManifest(r: ManifestRecord): ManifestWorld {
  return { id: r.id, fileName: r.name, game: r.game, version: r.version, sizeBytes: r.bytes };
}

export function toApWorld(r: ApWorldRecord): ApWorld {
  return {
    id: r.id,
    fileName: r.name,
    // A manifest without a game still has a module; the generator would load it by that name.
    game: r.game ?? r.module,
    version: r.world_version ?? 'unknown',
    sizeBytes: r.bytes,
    module: r.module,
  };
}

/** One `PlayerFile` per player document; a file with three documents adds three players. */
export function toPlayers(r: YamlRecord): PlayerFile[] {
  return r.players.map((p, i) => ({
    id: r.players.length === 1 ? r.id : `${r.id}#${i}`,
    yamlId: r.id,
    fileName: r.players.length === 1 ? r.name : `${r.name} (${i + 1} of ${r.players.length})`,
    name: p.name,
    yamlName: p.name,
    game: p.game,
  }));
}

/** The games a player can end up with: one, or each choice of a weighted pick. */
export function gamesOf(game: string | null): string[] {
  if (!game) return [];
  return game.split(', ').filter((g) => g.trim() !== '');
}
