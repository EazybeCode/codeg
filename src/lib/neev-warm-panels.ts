/**
 * Engine documents kept mounted, out of sight, after the reader browsed away.
 *
 * Browsing replaces the front tab, and a replaced panel unmounts — for an
 * engine document that throws away the whole booted editor, and coming back
 * costs the full boot again (about six round trips). The last few documents
 * left that way are kept here instead, so going back to one is a visibility
 * change rather than a reload.
 *
 * This is NOT a tab list. Nothing here is in the strip, counts towards the tab
 * limit, or is ever persisted: it holds only documents this reader opened in
 * this scope a moment ago, and it is emptied by anything that would have
 * destroyed the panel for a reason other than browsing.
 */

/** How many documents are kept. Each one is a live editor and a socket. */
export const WARM_LIMIT = 2
/** How long one may sit unseen before it is let go. */
export const WARM_TTL_MS = 10 * 60 * 1000

export type WarmEntry<Tab> = {
  key: string
  tab: Tab
  /** The auth scope it was opened in. Never revealed in any other. */
  scope: string
  /** When it was hidden. */
  hiddenAt: number
}

/**
 * Put `entry` at the front, most recent first, and say what fell off the end.
 *
 * An entry for the same key is replaced, not duplicated.
 */
export function keepWarm<Tab>(
  list: readonly WarmEntry<Tab>[],
  entry: WarmEntry<Tab>,
  limit = WARM_LIMIT
): { list: WarmEntry<Tab>[]; evicted: string[] } {
  const rest = list.filter((item) => item.key !== entry.key)
  const next = [entry, ...rest]
  return {
    list: next.slice(0, limit),
    evicted: next.slice(limit).map((item) => item.key),
  }
}

/** Whether `entry` may still be revealed at `now`. */
export function warmUsable<Tab>(
  entry: WarmEntry<Tab>,
  scope: string | null,
  now: number
): boolean {
  // No scope means no session: nothing is usable then, whatever it was tagged.
  return (
    scope !== null &&
    entry.scope === scope &&
    now - entry.hiddenAt <= WARM_TTL_MS
  )
}

/** The keys that should go, because they are stale or from another scope. */
export function warmExpired<Tab>(
  list: readonly WarmEntry<Tab>[],
  scope: string | null,
  now: number
): string[] {
  return list
    .filter((item) => !warmUsable(item, scope, now))
    .map((item) => item.key)
}
