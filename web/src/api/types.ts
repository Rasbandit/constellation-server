// The web API contract. The mock in `mock/` implements it today; the real server implements the
// same shapes later. Names follow CWP v1 (crates/proto/schema/cwp-v1.json) where they overlap:
// slots, games, item flags, client states.

export type SlotNo = number;

/** A slot's presence. `ready` and `playing` are CWP `ClientState`; `goal` is `slot.goal`. */
export type SlotStatus = 'offline' | 'connected' | 'ready' | 'playing' | 'goal';

/** CWP `ItemFlag`, plus `filler` for an item with no flag. */
export type ItemFlag = 'progression' | 'useful' | 'trap' | 'filler';

export type ReleaseMode = 'disabled' | 'enabled' | 'auto' | 'goal';
export type CollectMode = 'disabled' | 'enabled' | 'auto' | 'goal';

/** Hint status, in the order a player cares about it. */
export type HintStatus = 'priority' | 'unspecified' | 'no_priority' | 'avoid' | 'found';

export interface ApWorld {
  id: string;
  fileName: string;
  game: string;
  version: string;
  sizeBytes: number;
  /** The Python module the generator loads it as. */
  module?: string;
}

/** A data-only world manifest the server stored; the engine fills it, no world code runs. */
export interface ManifestWorld {
  id: string;
  fileName: string;
  game: string;
  version: string;
  sizeBytes: number;
}

export interface PlayerFile {
  id: string;
  fileName: string | null;
  /** The connection (slot) name from the YAML; the host can change it before creating the room. */
  name: string;
  /** One game, or for a weighted pick "A, B": the generator rolls one of them. */
  game: string | null;
  /** Why the YAML could not be read, if it could not. */
  error?: string;
  /** No YAML: the game's default options. */
  defaults?: boolean;
  /** The stored YAML this player came from (several players can share one). */
  yamlId?: string;
  /** The name written in the YAML, when it came from one. */
  yamlName?: string;
}

export interface RoomRules {
  hintCostPct: number;
  releaseMode: ReleaseMode;
  collectMode: CollectMode;
  password: string;
  spoilerLog: boolean;
}

export interface CreateRoomRequest {
  name: string;
  apworlds: ApWorld[];
  /** A data-only world; a room has this or APWorlds, never both. */
  manifest?: ManifestWorld | null;
  players: PlayerFile[];
  rules: RoomRules;
}

/** A generated world the server runs as a room (`GET /api/worlds/:id` once it has `room_key`). */
export interface GeneratedWorld {
  id: string;
  seed: string;
  roomKey: string;
  /** `{host}:{port}/{room_key}`: what a player types into their client. */
  address: string;
}

export interface Slot {
  slot: SlotNo;
  name: string;
  game: string;
  status: SlotStatus;
  checksDone: number;
  checksTotal: number;
  itemsReceived: number;
  /** Null where the server does not track them (the real room view). */
  hintPoints: number | null;
  /** ISO time of the last packet from this slot, null if it never connected, undefined where not tracked. */
  lastSeen?: string | null;
  clients: number;
}

/**
 * A room's phase: `generating` (the world file is being built), `lobby` (players can connect,
 * nothing is sent until the host starts), `running`, `paused`, `closed`.
 */
export type RoomState = 'generating' | 'lobby' | 'running' | 'paused' | 'closed';

export interface RoomSummary {
  id: string;
  name: string;
  state: RoomState;
  createdAt: string;
  slots: number;
  slotsOnline: number;
  slotsDone: number;
  games: string[];
  /** Short join code; freed for reuse when the room closes, so null once closed. */
  code: string | null;
  /** Present only for rooms this browser created (the stub hands it to everyone). */
  adminKey?: string;
}

export interface Room extends RoomSummary {
  /** `{host}:{port}/{room_key}`: what a player types into their client. */
  address: string;
  seed: string;
  rules: RoomRules;
  slotList: Slot[];
}

export interface FeedItem {
  /** Room log sequence number; strictly increasing. */
  seq: number;
  at: string;
  kind: 'item' | 'hint' | 'chat' | 'join' | 'leave' | 'goal' | 'release' | 'collect' | 'admin';
  text: string;
  /** Slots this entry concerns, for filtering ("only mine"). */
  slots: SlotNo[];
  item?: { name: string; flag: ItemFlag; from: SlotNo; to: SlotNo; location: string };
}

export interface Hint {
  id: string;
  finding: SlotNo;
  receiving: SlotNo;
  item: string;
  flag: ItemFlag;
  location: string;
  entrance?: string;
  status: HintStatus;
}

export interface GameCatalog {
  game: string;
  items: { name: string; flag: ItemFlag }[];
  locations: string[];
}

export interface AuditEntry {
  at: string;
  action: string;
  detail: string;
}

export interface ReadinessCheck {
  id: string;
  level: 'ok' | 'warning' | 'error';
  message: string;
}

export interface ApiError {
  error: string;
  checks?: ReadinessCheck[];
}
