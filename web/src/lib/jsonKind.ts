export type JsonKind = 'manifest' | 'player' | 'other';

/**
 * What a dropped `.json` file looks like, so a manifest dropped in the player zone (or the reverse)
 * is refused before it is uploaded. Anything that is not clearly one or the other is `other` and
 * left to the server's checks.
 */
export async function jsonKind(f: File): Promise<JsonKind> {
  if (!f.name.toLowerCase().endsWith('.json')) return 'other';
  try {
    const doc: unknown = JSON.parse(await f.text());
    if (typeof doc !== 'object' || doc === null || Array.isArray(doc)) return 'other';
    if ('regions' in doc) return 'manifest';
    if ('name' in doc) return 'player';
  } catch {
    // Not JSON: the server's answer says so.
  }
  return 'other';
}
