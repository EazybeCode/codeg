import { beforeEach, describe, expect, it } from "vitest"
import {
  DEFAULT_DOC_WIDTH,
  DEFAULT_RIGHT_PANEL_OPEN,
  DEFAULT_SECTION,
  getActiveSection,
  getDocWidth,
  getRightPanelOpen,
  setActiveSection,
  setDocWidth,
  setRightPanelOpen,
} from "./neev-workspace-prefs"

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
})
