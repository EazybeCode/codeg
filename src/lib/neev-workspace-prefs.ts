"use client"

/**
 * Per-user, device-local presentation preferences for the Neev native workspace
 * shell. These are VIEW preferences only — never document or server state — so
 * they live in localStorage and a stale/absent/corrupt value always degrades to
 * the documented default. SSR-safe: every accessor no-ops (returns the default)
 * when `window` is undefined.
 *
 * Deliberately NOT stored here: anything shared or authoritative (document
 * content, chat membership, agent connection state). Width in particular is a
 * per-reader viewer setting — the same document must render identically for
 * everyone except each reader's own chosen width (see the UX design spec §3).
 */

const DOC_WIDTH_KEY = "neev.ws.doc-width"
const RIGHT_PANEL_KEY = "neev.ws.right-panel-open"
const SECTION_KEY = "neev.ws.section"

/** Document canvas width. `full` = edge-to-edge canvas (prose keeps a readable
 *  measure; tables/images/code expand); `focus` = the narrow ~768px column. */
export type DocWidth = "focus" | "full"

/** The three primary workspace sections in the left rail. */
export type Section = "agents" | "team" | "docs"

/** Default new documents to the full canvas (the approved direction). */
export const DEFAULT_DOC_WIDTH: DocWidth = "full"
/** The right context panel (agent/details) starts collapsed. */
export const DEFAULT_RIGHT_PANEL_OPEN = false
/** Documents is the default landing section. */
export const DEFAULT_SECTION: Section = "docs"

export function getDocWidth(): DocWidth {
  if (typeof window === "undefined") return DEFAULT_DOC_WIDTH
  try {
    const raw = localStorage.getItem(DOC_WIDTH_KEY)
    if (raw === "focus" || raw === "full") return raw
  } catch {
    /* ignore */
  }
  return DEFAULT_DOC_WIDTH
}

export function setDocWidth(value: DocWidth): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(DOC_WIDTH_KEY, value)
  } catch {
    /* ignore */
  }
}

export function getRightPanelOpen(): boolean {
  if (typeof window === "undefined") return DEFAULT_RIGHT_PANEL_OPEN
  try {
    const raw = localStorage.getItem(RIGHT_PANEL_KEY)
    if (raw === "true") return true
    if (raw === "false") return false
  } catch {
    /* ignore */
  }
  return DEFAULT_RIGHT_PANEL_OPEN
}

export function setRightPanelOpen(value: boolean): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(RIGHT_PANEL_KEY, String(value))
  } catch {
    /* ignore */
  }
}

function isSection(value: unknown): value is Section {
  return value === "agents" || value === "team" || value === "docs"
}

export function getActiveSection(): Section {
  if (typeof window === "undefined") return DEFAULT_SECTION
  try {
    const raw = localStorage.getItem(SECTION_KEY)
    if (isSection(raw)) return raw
  } catch {
    /* ignore */
  }
  return DEFAULT_SECTION
}

export function setActiveSection(value: Section): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(SECTION_KEY, value)
  } catch {
    /* ignore */
  }
}
