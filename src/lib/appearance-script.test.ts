import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  APPEARANCE_INIT_SCRIPT,
  STORAGE_KEY_CHAT_ANIMATIONS,
  STORAGE_KEY_CHAT_CONTENT_WIDTH,
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
import {
  THEME_COLOR_DARK,
  THEME_COLOR_LIGHT,
  THEME_COLOR_MEDIA_DARK,
  THEME_COLOR_MEDIA_LIGHT,
} from "./theme-color"

function runInitScript() {
  ;(0, eval)(APPEARANCE_INIT_SCRIPT)
}

function resetDocument() {
  const root = document.documentElement
  root.removeAttribute("style")
  root.removeAttribute("data-theme")
  root.removeAttribute("data-workspace-bg")
  root.removeAttribute("data-chat-animations")
  root.classList.remove("dark")
  document.getElementById(CUSTOM_CSS_ELEMENT_ID)?.remove()
  document
    .querySelectorAll('meta[name="theme-color"]')
    .forEach((tag) => tag.remove())
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

/**
 * 静态 HTML 里按 prefers-color-scheme 各有一个 theme-color 标签（layout 的
 * viewport.themeColor）。应用内显式选了明/暗时，首帧前就要让两个都换成该模式的
 * 颜色，否则安卓已安装应用的状态栏按系统偏好着色，和界面反着来。
 */
describe("APPEARANCE_INIT_SCRIPT — theme-color", () => {
  function addSchemeTags() {
    for (const [media, color] of [
      [THEME_COLOR_MEDIA_LIGHT, THEME_COLOR_LIGHT],
      [THEME_COLOR_MEDIA_DARK, THEME_COLOR_DARK],
    ]) {
      const tag = document.createElement("meta")
      tag.setAttribute("name", "theme-color")
      tag.setAttribute("media", media)
      tag.setAttribute("content", color)
      document.head.appendChild(tag)
    }
  }

  function contents() {
    return Array.from(
      document.querySelectorAll('meta[name="theme-color"]'),
      (tag) => tag.getAttribute("content")
    )
  }

  it("paints both tags dark for an explicit dark choice on a light OS", () => {
    addSchemeTags()
    localStorage.setItem("theme", "dark")

    runInitScript()

    expect(contents()).toEqual([THEME_COLOR_DARK, THEME_COLOR_DARK])
  })

  it("paints both tags light for an explicit light choice on a dark OS", () => {
    addSchemeTags()
    vi.stubGlobal(
      "matchMedia",
      vi.fn().mockReturnValue({ matches: true }) as unknown as typeof matchMedia
    )
    localStorage.setItem("theme", "light")

    runInitScript()

    expect(contents()).toEqual([THEME_COLOR_LIGHT, THEME_COLOR_LIGHT])
  })

  it("leaves the per-scheme tags to the browser when following the system", () => {
    addSchemeTags()
    vi.stubGlobal(
      "matchMedia",
      vi.fn().mockReturnValue({ matches: true }) as unknown as typeof matchMedia
    )
    localStorage.setItem("theme", "system")

    runInitScript()

    expect(document.documentElement.classList.contains("dark")).toBe(true)
    expect(contents()).toEqual([THEME_COLOR_LIGHT, THEME_COLOR_DARK])
  })
})

describe("APPEARANCE_INIT_SCRIPT — chat area", () => {
  const root = () => document.documentElement
  const chatWidth = () => root().style.getPropertyValue("--chat-content-width")

  it("defers stored animation choices until the operator provider mounts", () => {
    runInitScript()
    expect(root().hasAttribute("data-chat-animations")).toBe(false)

    localStorage.setItem(STORAGE_KEY_CHAT_ANIMATIONS, "1")
    runInitScript()
    expect(root().hasAttribute("data-chat-animations")).toBe(false)

    localStorage.setItem(STORAGE_KEY_CHAT_ANIMATIONS, "0")
    runInitScript()
    expect(root()).not.toHaveAttribute("data-chat-animations")
    expect(localStorage.getItem(STORAGE_KEY_CHAT_ANIMATIONS)).toBe("0")
  })

  it("defers stored chat width while applying the allowlisted zoom", () => {
    localStorage.setItem(STORAGE_KEY_CHAT_CONTENT_WIDTH, "900")
    localStorage.setItem(STORAGE_KEY_ZOOM_LEVEL, "200")
    runInitScript()
    expect(chatWidth()).toBe("")
    expect(localStorage.getItem(STORAGE_KEY_CHAT_CONTENT_WIDTH)).toBe("900")
    // Chat preferences survive untouched for the confirmed operator branch.
    expect(root().style.fontSize).toBe("32px")
  })

  it.each(["", "abc", "0", "-5", "99999"])(
    "leaves the default width for a stored %j",
    (raw) => {
      localStorage.setItem(STORAGE_KEY_CHAT_CONTENT_WIDTH, raw)
      runInitScript()
      expect(chatWidth()).toBe("")
    }
  )
})
