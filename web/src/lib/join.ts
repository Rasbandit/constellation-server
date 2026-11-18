/** Where a pasted value leads: a short link or code (`/c/K7Q2XM`), a room link, or a room id. */
export function joinPath(text: string): string | null {
  const t = text.trim();
  if (!t) return null;
  const short = /\/c\/([A-Za-z0-9]+)/.exec(t);
  if (short) return `/c/${short[1].toUpperCase()}`;
  const room = /rooms\/([^/?#\s]+)/.exec(t);
  if (room) return `/rooms/${room[1]}`;
  if (/^[A-Za-z0-9]{6}$/.test(t)) return `/c/${t.toUpperCase()}`;
  const id = t.replace(/[^\w-]/g, '');
  return id ? `/rooms/${id}` : null;
}
