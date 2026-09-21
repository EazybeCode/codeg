/**
 * Which project a saved agent session belongs to, remembered locally.
 *
 * Agent sessions are private to their owner and the server does not file them
 * under a node — there is no column for it and this adds none. Neev's shape is
 * that everything lives in a project, so the association is kept here instead:
 * a pointer from session id to node id, per signed-in user AND organization.
 *
 * What this deliberately is NOT: it does not move, copy, delete or re-share
 * anything, and it never decides who may read a session. The server's list is
 * still the only source of which sessions exist and who may see them; this only
 * decides which project a session the caller already has is shown under. A
 * pointer to a node the reader cannot see shows them nothing extra, because the
 * session was theirs either way.
 *
 * Sessions with no pointer are UNFILED, not hidden: they are the ones that
 * existed before this, and the project view offers to file them rather than
 * quietly adopting them.
 */

const KEY_PREFIX = "neev.agent.project."
/** Per scope. Far above any real session count, and bounds a corrupt file. */
const MAX_ENTRIES = 500
const SESSION_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

export type SessionProjects = Record<string, number>

function key(scopeKey: string): string {
  return `${KEY_PREFIX}${scopeKey}`
}

/**
 * The stored map, with anything unrecognisable dropped.
 *
 * Parsed strictly rather than trusted: this is localStorage, which another tab,
 * an extension or an older build may have written.
 */
export function readSessionProjects(scopeKey: string | null): SessionProjects {
  if (!scopeKey || typeof window === "undefined") return {}
  let raw: string | null
  try {
    raw = window.localStorage.getItem(key(scopeKey))
  } catch {
    return {}
  }
  if (!raw) return {}
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return {}
    const result: SessionProjects = {}
    let count = 0
    for (const [sessionId, nodeId] of Object.entries(parsed)) {
      if (count >= MAX_ENTRIES) break
      if (!SESSION_ID.test(sessionId)) continue
      if (typeof nodeId !== "number" || !Number.isSafeInteger(nodeId)) continue
      if (nodeId <= 0) continue
      result[sessionId] = nodeId
      count += 1
    }
    return result
  } catch {
    return {}
  }
}

function write(scopeKey: string, value: SessionProjects): void {
  try {
    window.localStorage.setItem(key(scopeKey), JSON.stringify(value))
  } catch {
    /* A full or blocked store must not break the surface it decorates. */
  }
}

/** File a session under a project. Replaces any previous pointer for it. */
export function assignSessionProject(
  scopeKey: string | null,
  sessionId: string,
  nodeId: number
): SessionProjects {
  const current = readSessionProjects(scopeKey)
  if (!scopeKey || typeof window === "undefined") return current
  if (!SESSION_ID.test(sessionId)) return current
  if (!Number.isSafeInteger(nodeId) || nodeId <= 0) return current
  // Bounded: drop the oldest insertions rather than growing without limit.
  const entries = Object.entries(current).filter(([id]) => id !== sessionId)
  const trimmed = entries.slice(Math.max(0, entries.length - (MAX_ENTRIES - 1)))
  const next: SessionProjects = Object.fromEntries(trimmed)
  next[sessionId] = nodeId
  write(scopeKey, next)
  return next
}

/**
 * Forget a session's project. Used when a session is gone from the server's
 * list — the pointer would otherwise outlive what it points at.
 */
export function forgetSessionProjects(
  scopeKey: string | null,
  keepSessionIds: readonly string[]
): SessionProjects {
  const current = readSessionProjects(scopeKey)
  if (!scopeKey || typeof window === "undefined") return current
  const keep = new Set(keepSessionIds)
  const next: SessionProjects = {}
  let changed = false
  for (const [sessionId, nodeId] of Object.entries(current))
    if (keep.has(sessionId)) next[sessionId] = nodeId
    else changed = true
  if (changed) write(scopeKey, next)
  return changed ? next : current
}
