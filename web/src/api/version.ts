// Set by vite.config.ts from package.json; the consistency script keeps it equal to the server's.
declare const __WEB_VERSION__: string | undefined;
export const webVersion = typeof __WEB_VERSION__ !== 'undefined' ? __WEB_VERSION__ : 'dev';

export type VersionBadge = { text: string; mismatch: boolean };

/** The server's version from `GET /version`, or undefined when it cannot be read. */
export async function fetchServerVersion(): Promise<string | undefined> {
  try {
    const res = await fetch('/version');
    if (!res.ok) return undefined;
    const body: unknown = await res.json();
    const v = (body as { version?: unknown } | null)?.version;
    return typeof v === 'string' ? v : undefined;
  } catch {
    return undefined;
  }
}

/** The server may carry a `+<sha>` build suffix; the pair matches when the versions before it are equal. */
export function versionBadge(server: string | undefined, web: string): VersionBadge {
  if (server === undefined) return { text: `server ? / web ${web}`, mismatch: true };
  return { text: `server ${server} / web ${web}`, mismatch: server.split('+')[0] !== web.split('+')[0] };
}
