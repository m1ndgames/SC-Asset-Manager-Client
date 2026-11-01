/**
 * Remembering which location a kiosk turned out to be.
 *
 * **Within this session only, and never auto-filled.** Two reasons, one
 * measured and one a design rule:
 *
 * - `shopId` is a per-session entity id. Measured across 11 real sessions: 24
 *   distinct shopIds, exactly one of which ever appears in two sessions. So a
 *   remembered mapping is worthless after a relaunch and persisting it would
 *   only lend it false authority.
 * - A wrong auto-filled location is worse than an empty one. The log genuinely
 *   does not record where you were — nothing in it names the station, verified
 *   on a full log — so a guess the user doesn't notice becomes a wrong record
 *   in a shared database. Suggestions are offered as a labelled button the user
 *   presses.
 *
 * Module-level state so it survives switching tabs, and deliberately not on
 * disk so it dies with the process, like the ids it is keyed on.
 */

const byShop = $state<Record<string, string>>({});

/** Record what the user chose, so sibling kiosks can offer it. */
export function remember(shopId: string | null | undefined, location: string) {
  const id = shopId?.trim();
  const loc = location.trim();
  if (!id || !loc) return;
  byShop[id] = loc;
}

/** What this kiosk was called last time, this session. */
export function suggestionFor(shopId: string | null | undefined): string | null {
  const id = shopId?.trim();
  return (id && byShop[id]) || null;
}

/**
 * Every location used this session, most recent first.
 *
 * Useful even when the shopId does not match: a trade run usually revisits a
 * handful of places, and these are far likelier than the other 219 entries in
 * `locations.json`.
 */
export function recentLocations(): string[] {
  const seen: string[] = [];
  for (const loc of Object.values(byShop)) {
    if (!seen.includes(loc)) seen.push(loc);
  }
  return seen.reverse();
}
