import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  APPEARANCE_INIT_SCRIPT,
  STORAGE_KEY_CUSTOM_CSS,
  STORAGE_KEY_CUSTOM_CSS_ENABLED,
  STORAGE_KEY_CUSTOM_THEME,
  STORAGE_KEY_THEME_COLOR,
  STORAGE_KEY_UI_FONT,
  STORAGE_KEY_UI_FONT_STACK,
  STORAGE_KEY_WORKSPACE_BG_ENABLED,
  STORAGE_KEY_WORKSPACE_BG_PANEL_OPACITY,
  STORAGE_KEY_ZOOM_LEVEL,
} from "./appearance-script"
import { CUSTOM_CSS_ELEMENT_ID } from "./custom-style"

function runInitScript() {
  ;(0, eval)(APPEARANCE_INIT_SCRIPT)
}

function resetDocument() {
  const root = document.documentElement
  root.removeAttribute("style")
  root.removeAttribute("data-theme")
  root.removeAttribute("data-workspace-bg")
  root.classList.remove("dark")
  document.getElementById(CUSTOM_CSS_ELEMENT_ID)?.remove()
}

beforeEach(() => {
  localStorage.clear()
  resetDocument()
  vi.stubGlobal(
    "matchMedia",
    vi.fn().mockReturnValue({ matches: false }) as unknown as typeof matchMedia
  )
})

describe("APPEARANCE_INIT_SCRIPT safe pre-hydration subset", () => {
  it("applies allowlisted theme color, zoom and dark mode", () => {
    localStorage.setItem(STORAGE_KEY_THEME_COLOR, "blue")
    localStorage.setItem(STORAGE_KEY_ZOOM_LEVEL, "125")
    localStorage.setItem("theme", "dark")

    runInitScript()

    expect(document.documentElement).toHaveAttribute("data-theme", "blue")
    expect(document.documentElement.style.fontSize).toBe("20px")
    expect(document.documentElement).toHaveClass("dark")
    expect(document.documentElement.style.colorScheme).toBe("dark")
  })

  it("falls back when color and zoom storage is malformed", () => {
    localStorage.setItem(STORAGE_KEY_THEME_COLOR, "url(https://remote.invalid)")
    localStorage.setItem(STORAGE_KEY_ZOOM_LEVEL, "999")

    runInitScript()

    expect(document.documentElement).toHaveAttribute("data-theme", "neutral")
    expect(document.documentElement.style.fontSize).toBe("16px")
  })

  it.each(["unknown browser mode", "team browser mode"])(
    "does not inject remote stored CSS in %s",
    () => {
      localStorage.setItem(STORAGE_KEY_CUSTOM_CSS_ENABLED, "1")
      localStorage.setItem(
        STORAGE_KEY_CUSTOM_CSS,
        '.foo { background-image: url("https://remote.invalid/pixel") }'
      )

      runInitScript()

      expect(document.getElementById(CUSTOM_CSS_ELEMENT_ID)).toBeNull()
      expect(document.documentElement.innerHTML).not.toContain("remote.invalid")
    }
  )

  it("does not apply stored custom tokens, fonts or workspace background", () => {
    localStorage.setItem(
      STORAGE_KEY_CUSTOM_THEME,
      JSON.stringify({ light: { primary: "#111111" }, dark: {} })
    )
    localStorage.setItem(STORAGE_KEY_UI_FONT, "custom")
    localStorage.setItem(
      STORAGE_KEY_UI_FONT_STACK,
      'url("https://remote.invalid/font")'
    )
    localStorage.setItem(STORAGE_KEY_WORKSPACE_BG_ENABLED, "1")
    localStorage.setItem(STORAGE_KEY_WORKSPACE_BG_PANEL_OPACITY, "0.5")

    runInitScript()

    expect(document.documentElement.style.getPropertyValue("--primary")).toBe(
      ""
    )
    expect(document.documentElement.style.getPropertyValue("--font-sans")).toBe(
      ""
    )
    expect(document.documentElement).not.toHaveAttribute("data-workspace-bg")
    expect(
      document.documentElement.style.getPropertyValue("--ws-surface-alpha")
    ).toBe("")
  })

  it("preserves deferred operator preferences in local storage", () => {
    const css = ".operator-preference { color: red }"
    localStorage.setItem(STORAGE_KEY_CUSTOM_CSS_ENABLED, "1")
    localStorage.setItem(STORAGE_KEY_CUSTOM_CSS, css)

    runInitScript()

    expect(localStorage.getItem(STORAGE_KEY_CUSTOM_CSS_ENABLED)).toBe("1")
    expect(localStorage.getItem(STORAGE_KEY_CUSTOM_CSS)).toBe(css)
  })
})
