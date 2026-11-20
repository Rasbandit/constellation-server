/**
 * Whether `/api` answers on this origin. The server never serves the admin API on its public port
 * (docs/container.md); without an operator proxy in front, every call 404s and the create and
 * upload pages would be dead screens.
 */
export async function adminApiReachable(): Promise<boolean> {
  try {
    return (await fetch('/api/games')).status !== 404;
  } catch {
    return false;
  }
}
