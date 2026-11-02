/**
 * Remembering which location a kiosk turned out to be.
 *
 * **Within this session only, and never auto-filled.** Two reasons, one
 * measured and one a design rule:
 *
 * - `shopId` is a runtime entity id (a GEID), minted by the server when the
 *   entity spawns. `kioskId` is the next ticket from the same counter: across
 *   the corpus it is `shopId` plus a small per-shop constant (+1, +4, -8, +2,
 *   -36, -60). The values track wall-clock time across builds — 8.517e12 in
 *   Dec 2025 rising monotonically to 9.956e12 in Apr 2026, then 8.6e11 after a
 *   server-side scheme change — so they identify an allocation, not a place.
 *
 *   The id lives for the *shard's* lifetime, not the session's. 906792501776
 *   appears in two sessions 24 minutes apart and is gone 1h40 later, because
 *   the station entity stays spawned under Persistent Entity Streaming. That
 *   is why this must not reach disk: a persisted mapping is right while the
 *   shard lives and silently wrong once the GEID is recycled onto a different
 *   entity, and nothing on the line distinguishes the two cases.
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

/**
 * Locations in the order they were last used, most recent last.
 *
 * Kept separately because `byShop` cannot express recency: its key order is
 * first-insertion order by `shopId`, and re-recording an existing shopId
 * leaves that key where it was.
 */
const order = $state<string[]>([]);

/** Record what the user chose, so sibling kiosks can offer it. */
export function remember(shopId: string | null | undefined, location: string) {
  const id = shopId?.trim();
  const loc = location.trim();
  if (!id || !loc) return;
  byShop[id] = loc;

  // Move to the end rather than appending blindly, so using a place again
  // makes it the most recent instead of leaving it where it first appeared.
  const at = order.indexOf(loc);
  if (at !== -1) order.splice(at, 1);
  order.push(loc);
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
  return order.slice().reverse();
}
