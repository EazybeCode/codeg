import { beforeEach, describe, expect, it } from "vitest"
import {
  DEFAULT_DOC_WIDTH,
  DEFAULT_RIGHT_PANEL_OPEN,
  DEFAULT_SECTION,
  getActiveSection,
  getDocWidth,
  getOpenResource,
  getRightPanelOpen,
  setActiveSection,
  setDocWidth,
  setOpenResource,
  setRightPanelOpen,
} from "./neev-workspace-prefs"

const RESOURCE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"

/**
 * These are per-user viewer preferences read at mount; a stale/absent/corrupt
 * value must always degrade to the documented default rather than throw or hand
 * back a shape the shell can't use. Width in particular is per-reader — it never
 * touches document state.
 */
describe("neev workspace prefs", () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it("defaults to a full-canvas document width", () => {
    expect(getDocWidth()).toBe(DEFAULT_DOC_WIDTH)
    expect(DEFAULT_DOC_WIDTH).toBe("full")
  })

  it("round-trips the document width preference", () => {
    setDocWidth("focus")
    expect(getDocWidth()).toBe("focus")
    setDocWidth("full")
    expect(getDocWidth()).toBe("full")
  })

  it("treats a damaged width value as the default", () => {
    localStorage.setItem("neev.ws.doc-width", "wide")
    expect(getDocWidth()).toBe("full")
    localStorage.setItem("neev.ws.doc-width", "{not json")
    expect(getDocWidth()).toBe("full")
  })

  it("defaults the right context panel to collapsed and round-trips it", () => {
    expect(getRightPanelOpen()).toBe(DEFAULT_RIGHT_PANEL_OPEN)
    expect(DEFAULT_RIGHT_PANEL_OPEN).toBe(false)
    setRightPanelOpen(true)
    expect(getRightPanelOpen()).toBe(true)
    setRightPanelOpen(false)
    expect(getRightPanelOpen()).toBe(false)
    localStorage.setItem("neev.ws.right-panel-open", "yes")
    expect(getRightPanelOpen()).toBe(false)
  })

  it("defaults the section to documents and round-trips a valid section", () => {
    expect(getActiveSection()).toBe(DEFAULT_SECTION)
    expect(DEFAULT_SECTION).toBe("docs")
    setActiveSection("agents")
    expect(getActiveSection()).toBe("agents")
    setActiveSection("team")
    expect(getActiveSection()).toBe("team")
  })

  it("treats an unknown section as the default", () => {
    localStorage.setItem("neev.ws.section", "inbox")
    expect(getActiveSection()).toBe("docs")
  })

  it("scopes the open-resource pointer to one user in one organization", () => {
    const ref = { resourceId: RESOURCE, nodeId: 4 }
    expect(getOpenResource(1, 3)).toBeNull()
    setOpenResource(1, 3, ref)
    expect(getOpenResource(1, 3)).toEqual(ref)
    // A different account, or the same account in another tenant, must not
    // inherit a selection from a shared browser profile.
    expect(getOpenResource(2, 3)).toBeNull()
    expect(getOpenResource(1, 9)).toBeNull()
    setOpenResource(1, 3, null)
    expect(getOpenResource(1, 3)).toBeNull()
  })

  it("discards a pointer that could not have been written by this app", () => {
    for (const bad of [
      "{",
      JSON.stringify({ resourceId: "not-a-uuid", nodeId: 4 }),
      JSON.stringify({ resourceId: RESOURCE }),
      JSON.stringify({ resourceId: RESOURCE, nodeId: 0 }),
      JSON.stringify({ resourceId: RESOURCE, nodeId: 1.5 }),
      JSON.stringify({ resourceId: RESOURCE, nodeId: "4" }),
    ]) {
      localStorage.setItem("neev.ws.open.1:3", bad)
      expect(getOpenResource(1, 3)).toBeNull()
    }
  })
})
