/**
 * The open tabs of one workspace.
 *
 * A tab is a REFERENCE, never content: a project, a document or team chat, a
 * saved agent session, or a draft of a new agent chat. What each one shows is
 * fetched and authorized exactly as it was before — a tab cannot make something
 * visible that the server would not have shown anyway, and restoring one is
 * only a request the reader could have made themselves.
 *
 * Drafts are deliberately NOT persisted. A draft has no server-side identity,
 * so a restored one would be a promise of work that does not exist.
 */

export type WorkspaceTab =
  | { kind: "project"; nodeId: number }
  | { kind: "resource"; resourceId: string; nodeId: number }
  | { kind: "agent"; sessionId: string }
  | { kind: "draft"; provider: string; nodeId: number | null }

const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
const PREFIX = "neev.ws.tabs"
/** More than anyone opens, and a bound on a corrupt or hostile store. */
export const MAX_TABS = 12

/**
 * A tab's identity.
 *
 * Two tabs for the same thing are the same tab: opening a document twice moves
 * to the one already open rather than stacking duplicates.
 */
export function tabKey(tab: WorkspaceTab): string {
  switch (tab.kind) {
    case "project":
      return `project:${tab.nodeId}`
    case "resource":
      return `resource:${tab.resourceId}`
    case "agent":
      return `agent:${tab.sessionId}`
    case "draft":
      // One draft per provider per project: a second click on New Claude chat
      // returns to the draft already being written, rather than discarding it.
      return `draft:${tab.provider}:${tab.nodeId ?? "none"}`
  }
}

export function sameTab(left: WorkspaceTab, right: WorkspaceTab): boolean {
  return tabKey(left) === tabKey(right)
}

/**
 * Open `tab`, or move to it if it is already open.
 *
 * At the limit this REFUSES rather than evicting. An open tab may hold a
 * half-written document or an unsent message, and closing one is a decision
 * with a guard attached — silently dropping the oldest to make room for the
 * newest would throw that work away without ever asking.
 */
export function openTab(
  tabs: readonly WorkspaceTab[],
  tab: WorkspaceTab
): { tabs: WorkspaceTab[]; active: number; refused: boolean } {
  const existing = tabs.findIndex((item) => sameTab(item, tab))
  if (existing >= 0)
    return { tabs: [...tabs], active: existing, refused: false }
  if (tabs.length >= MAX_TABS)
    return { tabs: [...tabs], active: -1, refused: true }
  return { tabs: [...tabs, tab], active: tabs.length, refused: false }
}

/**
 * Close the tab at `index`.
 *
 * The neighbour is selected — the one to the right, or the left when the last
 * tab closes — because landing on nothing after closing one of several open
 * things is a worse answer than landing on the next.
 */
export function closeTab(
  tabs: readonly WorkspaceTab[],
  index: number,
  active: number
): { tabs: WorkspaceTab[]; active: number } {
  if (index < 0 || index >= tabs.length) return { tabs: [...tabs], active }
  const next = tabs.filter((_, position) => position !== index)
  if (next.length === 0) return { tabs: next, active: -1 }
  if (index > active) return { tabs: next, active }
  if (index < active) return { tabs: next, active: active - 1 }
  // The active tab closed: take its place, or the new last one.
  return { tabs: next, active: Math.min(index, next.length - 1) }
}

/**
 * Close the tab holding `key`, wherever it is.
 *
 * Used when a panel's OWN authorized load reports its subject gone — a 404 or
 * a 403 from the thing itself. Deliberately not driven by the sidebar's
 * catalogs: those are lazy and paginated, so a resource missing from them is
 * unloaded, not revoked, and closing on that basis would shut tabs the reader
 * can still perfectly well open.
 */
export function closeTabByKey(
  tabs: readonly WorkspaceTab[],
  key: string,
  active: number
): { tabs: WorkspaceTab[]; active: number } {
  const index = tabs.findIndex((tab) => tabKey(tab) === key)
  if (index < 0) return { tabs: [...tabs], active }
  return closeTab(tabs, index, active)
}

function key(userId: number, orgId: number): string {
  return `${PREFIX}.${userId}:${orgId}`
}

/**
 * The saved tabs for this user in this organization.
 *
 * Scoped to both, so switching organization never shows the previous one's
 * tabs, and parsed as hostile input: another tab, an extension or an older
 * build can write this.
 */
export function readTabs(
  userId: number,
  orgId: number
): { tabs: WorkspaceTab[]; active: number } {
  const empty = { tabs: [] as WorkspaceTab[], active: -1 }
  if (typeof window === "undefined") return empty
  let raw: string | null
  try {
    raw = window.localStorage.getItem(key(userId, orgId))
  } catch {
    return empty
  }
  if (!raw) return empty
  try {
    const parsed: unknown = JSON.parse(raw)
    // The older shape was a bare array, with no record of what was in front.
    const list = Array.isArray(parsed)
      ? parsed
      : Array.isArray((parsed as { tabs?: unknown })?.tabs)
        ? (parsed as { tabs: unknown[] }).tabs
        : null
    if (!list) return empty
    const selected = Array.isArray(parsed)
      ? null
      : ((parsed as { active?: unknown }).active ?? null)
    const seen = new Set<string>()
    const tabs: WorkspaceTab[] = []
    for (const value of list.slice(0, MAX_TABS)) {
      const tab = parseTab(value)
      if (!tab) continue
      const identity = tabKey(tab)
      if (seen.has(identity)) continue
      seen.add(identity)
      tabs.push(tab)
    }
    if (tabs.length === 0) return empty
    // The tab that was in FRONT, not the one opened last. If it did not
    // survive — unparsable, or a draft, which is never saved — the nearest
    // surviving neighbour takes its place.
    const wanted =
      typeof selected === "string"
        ? tabs.findIndex((tab) => tabKey(tab) === selected)
        : -1
    return {
      tabs,
      active: wanted >= 0 ? wanted : Math.min(tabs.length - 1, 0),
    }
  } catch {
    return empty
  }
}

function parseTab(value: unknown): WorkspaceTab | null {
  if (!value || typeof value !== "object") return null
  const tab = value as Record<string, unknown>
  const node = tab.nodeId
  const positive = (input: unknown): input is number =>
    typeof input === "number" && Number.isSafeInteger(input) && input > 0
  if (tab.kind === "project" && positive(node))
    return { kind: "project", nodeId: node }
  if (
    tab.kind === "resource" &&
    typeof tab.resourceId === "string" &&
    UUID.test(tab.resourceId) &&
    positive(node)
  )
    return { kind: "resource", resourceId: tab.resourceId, nodeId: node }
  if (
    tab.kind === "agent" &&
    typeof tab.sessionId === "string" &&
    UUID.test(tab.sessionId)
  )
    return { kind: "agent", sessionId: tab.sessionId }
  // Drafts are never read back: see the note at the top of this file.
  return null
}

export function writeTabs(
  userId: number,
  orgId: number,
  tabs: readonly WorkspaceTab[],
  active: number
): void {
  if (typeof window === "undefined") return
  try {
    const saved = tabs.filter((tab) => tab.kind !== "draft").slice(0, MAX_TABS)
    const front = tabs[active]
    // The identity rather than the index: the saved list omits drafts, so a
    // position in one list is not a position in the other.
    const selected = front && front.kind !== "draft" ? tabKey(front) : null
    window.localStorage.setItem(
      key(userId, orgId),
      JSON.stringify({ tabs: saved, active: selected })
    )
  } catch {
    /* A full or blocked store must not break the workspace it decorates. */
  }
}
