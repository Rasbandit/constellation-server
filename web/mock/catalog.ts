import type { GameCatalog, ItemFlag } from '../src/api/types';

// Made-up games for the seeded rooms. None of these names come from a real world.
const defs: Record<string, { items: [string, ItemFlag][]; locations: string[] }> = {
  'Starfall Keep': {
    items: [
      ['Grappling Vine', 'progression'],
      ['Moonlit Key', 'progression'],
      ['Ember Lantern', 'progression'],
      ['Silver Greaves', 'useful'],
      ['Heart Shard', 'useful'],
      ['Pouch of Stardust', 'filler'],
      ['Rusty Bolt', 'filler'],
      ['Gloom Curse', 'trap'],
    ],
    locations: [
      'Gatehouse Chest',
      'Bell Tower Summit',
      'Crypt of Echoes - Boss',
      'Moat Bottom',
      'Library Hidden Shelf',
      'Throne Room Altar',
      'Kitchen Pantry',
      'Observatory Lens',
    ],
  },
  'Tidewater Farm': {
    items: [
      ['Copper Watering Can', 'progression'],
      ['Barn Blueprint', 'progression'],
      ['Ferry Pass', 'progression'],
      ['Golden Seed Bag', 'useful'],
      ['Stamina Tonic', 'useful'],
      ['Turnip Seeds', 'filler'],
      ['Driftwood', 'filler'],
      ['Crow Swarm', 'trap'],
    ],
    locations: [
      'Harvest Festival Prize',
      'Fishing Pier - Legendary Catch',
      'Mine Floor 20',
      'Community Notice Board',
      'Old Well',
      'Lighthouse Keeper Gift',
      'Orchard Stump',
      'Tidepool Shell',
    ],
  },
  'Clockwork Depths': {
    items: [
      ['Gear Drill', 'progression'],
      ['Pressure Valve', 'progression'],
      ['Spring Boots', 'progression'],
      ['Oil Can', 'useful'],
      ['Armor Plating', 'useful'],
      ['Copper Scrap', 'filler'],
      ['Spare Cog', 'filler'],
      ['Short Circuit', 'trap'],
    ],
    locations: [
      'Boiler Room Locker',
      'Pendulum Hall',
      'Foundry Overseer - Boss',
      'Steam Vent Ledge',
      'Assembly Line End',
      'Clock Face Interior',
      'Rail Yard Crate',
      'Furnace Core',
    ],
  },
  'Lantern Run': {
    items: [
      ['Double Jump', 'progression'],
      ['Wall Cling', 'progression'],
      ['Blue Flame', 'progression'],
      ['Extra Wick', 'useful'],
      ['Speed Charm', 'useful'],
      ['Wax Drop', 'filler'],
      ['Moth Wing', 'filler'],
      ['Snuffer', 'trap'],
    ],
    locations: [
      'Level 1-1 Secret',
      'Level 2-3 Goal',
      'Shadow King - Boss',
      'Candle Shop',
      'Level 3-2 Hidden Wall',
      'Rooftop Run',
      'Bell Chamber',
      'Final Ascent',
    ],
  },
  'Archipela-Go': {
    items: [
      ['Route Unlock: North', 'progression'],
      ['Route Unlock: Harbor', 'progression'],
      ['Compass', 'progression'],
      ['Step Booster', 'useful'],
      ['Map Fragment', 'useful'],
      ['Postcard', 'filler'],
      ['Sticker', 'filler'],
      ['Detour', 'trap'],
    ],
    locations: [
      'Park Fountain',
      'Harbor Pier',
      'Old Town Square',
      'Hilltop Bench',
      'Library Steps',
      'Market Stall',
      'Train Station',
      'Riverside Path',
    ],
  },
};

/** Games the server can generate without an uploaded APWorld (stub: one). */
export const BUILTIN_GAMES = ['Archipela-Go'];

/** The catalog of a known game, or a generic one for any other (uploaded) game. */
export function catalogFor(game: string): GameCatalog {
  const def = defs[game];
  if (def) {
    return {
      game,
      items: def.items.map(([name, flag]) => ({ name, flag })),
      locations: def.locations,
    };
  }
  const flags: ItemFlag[] = ['progression', 'progression', 'useful', 'filler', 'filler', 'trap'];
  return {
    game,
    items: flags.map((flag, i) => ({ name: `${game} Item ${i + 1}`, flag })),
    locations: Array.from({ length: 8 }, (_, i) => `${game} Location ${i + 1}`),
  };
}

export function knownGames(): string[] {
  return Object.keys(defs).sort();
}
