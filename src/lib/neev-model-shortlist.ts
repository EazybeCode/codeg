// Which models a person wants on the front of the composer menu.
//
// This is a PREFERENCE, and the distinction matters everywhere below: it says
// what to show first, never what may be run. The catalogue from
// `GET /api/neev/agent/capabilities` remains the only authority on which models
// exist, so every id read back from storage is checked against it before it is
// offered. A shortlist entry for a model the server no longer lists is dropped,
// not shown as a choice that would fail.
//
// It lives in `localStorage` rather than on the server because it is about this
// browser's composer, not about the organization: two people sharing a chat
// should not be reordering each other's menus, and a shortlist is not worth a
// write to a session that a run may be reading.

/** The stored shape, one list per provider. */
export type NeevShortlists = Record<string, string[]>

const STORAGE_KEY = "neev.model-shortlist.v1"

/**
 * How many models the menu shows before "More models".
 *
 * Three is what fits above the Effort and Fast rows without the menu becoming
 * a scrolling list — the point of a shortlist is that the whole thing is
 * readable at a glance.
 */
export const NEEV_SHORTLIST_SIZE = 3

/** An upper bound on what may be pinned, so a saved list cannot grow forever. */
export const NEEV_SHORTLIST_MAX = 8

/**
 * Storage, where there is any.
 *
 * Server-rendered passes, a locked-down browser and a private window that
 * refuses storage all land here, and all three mean "no saved preference" —
 * never an error. The menu then falls back to the catalogue's own order, which
 * is a working menu.
 */
function store(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage
  } catch {
    return null
  }
}

/** Every saved list, ignoring anything that is not the shape written here. */
export function readShortlists(): NeevShortlists {
  const raw = (() => {
    try {
      return store()?.getItem(STORAGE_KEY) ?? null
    } catch {
      return null
    }
  })()
  if (!raw) return {}
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return {}
    const out: NeevShortlists = {}
    for (const [provider, value] of Object.entries(parsed))
      if (Array.isArray(value))
        out[provider] = value
          .filter((id): id is string => typeof id === "string" && id.length > 0)
          .slice(0, NEEV_SHORTLIST_MAX)
    return out
  } catch {
    // Someone else's key, a truncated write, a hand-edited value: a corrupt
    // preference is a missing preference. It is not worth a message.
    return {}
  }
}

/** One provider's saved list. */
export function readShortlist(provider: string): string[] {
  return readShortlists()[provider] ?? []
}

/**
 * Save one provider's list, leaving every other provider's alone.
 *
 * Returns whether it was written: a browser that refuses storage is not a
 * failure worth interrupting anyone over, but the caller still keeps the choice
 * in memory for this session rather than pretending it was persisted.
 */
export function writeShortlist(provider: string, ids: readonly string[]) {
  const next = {
    ...shortlistSnapshot(),
    [provider]: [...ids].slice(0, NEEV_SHORTLIST_MAX),
  }
  // The snapshot moves first, so the menu that was just clicked redraws even
  // when storage refuses the write — the choice holds for this session, and is
  // simply not there next time.
  snapshot = next
  for (const listener of listeners) listener()
  const storage = store()
  if (!storage) return false
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(next))
    return true
  } catch {
    return false
  }
}

// Every mounted composer reads the same shortlist, so a pin added in one is a
// pin in all of them without a reload. A cached snapshot is what makes that
// safe to subscribe to: `readShortlists` parses and builds a NEW object every
// call, and handing React a fresh identity every render is an infinite loop.
let snapshot: NeevShortlists | null = null
const listeners = new Set<() => void>()

/** The current lists, parsed at most once until something writes. */
export function shortlistSnapshot(): NeevShortlists {
  return (snapshot ??= readShortlists())
}

/** Nothing is saved server-side, so a server render has no lists at all. */
export const NO_SHORTLISTS: NeevShortlists = Object.freeze({})

/** Told when a list changes, so every open composer redraws together. */
export function subscribeShortlists(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/** Forget the cache. Tests that swap `localStorage` need this; nothing else does. */
export function resetShortlistCache() {
  snapshot = null
  for (const listener of listeners) listener()
}

/**
 * The ids to show at the front of the menu, in the order to show them.
 *
 * Three rules, and all three exist so the menu cannot lie:
 *
 *  - Only ids the catalogue actually lists survive. A saved pin for a model
 *    that has since been removed is silently dropped, because offering it
 *    would offer a run that fails.
 *  - An explicit pin is ALWAYS shown. `size` truncates the default only — the
 *    catalogue's first few, used when nothing has been pinned — and never a
 *    list someone chose. Applying it to both is what would make pinning a
 *    fourth model appear to do nothing: the pin is saved, the editor shows it
 *    ticked, and the menu it was pinned to never mentions it. `NEEV_SHORTLIST_MAX`
 *    is the real ceiling, and it is enforced where pinning happens.
 *  - The model this chat is CURRENTLY set to is always present, even when it
 *    is not pinned and even when it is not in the catalogue's first few. A
 *    menu whose top section does not contain the thing it is describing sends
 *    the reader to a submenu to find out what they already chose.
 *
 * With nothing saved, the catalogue's own order is the shortlist: the server
 * listed its models in an order, and that is a better default than alphabetical.
 */
export function resolveShortlist({
  available,
  saved,
  selected,
  size = NEEV_SHORTLIST_SIZE,
}: {
  /** Model ids the catalogue lists for this provider, in its order. */
  available: readonly string[]
  /**
   * What was pinned, from storage, or `undefined` for "never pinned anything".
   *
   * The two are different states and must not be merged: an EMPTY array is a
   * deliberate "nothing pinned", and falling back to the catalogue default
   * there would tick three boxes the person had just cleared.
   */
  saved: readonly string[] | undefined
  /** The model in force right now, pinned or not. */
  selected: string | null
  /** How many to show when nothing has been pinned. Never caps real pins. */
  size?: number
}): string[] {
  const exists = new Set(available)
  const shown =
    saved === undefined
      ? available.slice(0, Math.max(size, 0))
      : // Stale ids are dropped BEFORE the cap, so a list half full of models
        // the catalogue no longer has does not crowd out real ones.
        saved.filter((id) => exists.has(id)).slice(0, NEEV_SHORTLIST_MAX)
  if (selected && exists.has(selected) && !shown.includes(selected))
    // Appended rather than substituted: dropping a pinned model to make room
    // would quietly un-pin it the moment someone tried a different one.
    shown.push(selected)
  return shown
}

/**
 * The pins the editor ticks, and the list a toggle starts from.
 *
 * The same value for both, which is the whole point. When these were computed
 * separately — ticks from storage, toggles from what the menu happened to show
 * — the first click with nothing saved did the opposite of what it looked
 * like: three unticked rows, and ticking one wrote a list that REMOVED it.
 */
export function effectivePins({
  available,
  saved,
  size = NEEV_SHORTLIST_SIZE,
}: {
  available: readonly string[]
  saved: readonly string[] | undefined
  size?: number
}): string[] {
  const exists = new Set(available)
  return saved === undefined
    ? available.slice(0, Math.max(size, 0))
    : saved.filter((id) => exists.has(id)).slice(0, NEEV_SHORTLIST_MAX)
}
