import type { ReactNode } from "react"
import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"

const context = vi.hoisted(() => ({ mobile: false, search: "" }))
vi.mock("next/navigation", () => ({
  usePathname: () => "/settings/appearance",
  useSearchParams: () => new URLSearchParams(context.search),
  useRouter: () => ({ push: vi.fn() }),
}))
vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => context.mobile }))
// Native window controls are outside SettingsShell's navigation contract.
vi.mock("@/components/layout/app-title-bar", () => ({
  AppTitleBar: ({ left, center }: { left?: ReactNode; center?: ReactNode }) => (
    <header>
      {left}
      {center}
    </header>
  ),
}))

function showSettings() {
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <SettingsShell>
        <p>Settings content</p>
      </SettingsShell>
    </NextIntlClientProvider>
  )
}

import { SettingsShell, settingsNavItemsFor } from "./settings-shell"

describe("Settings workspace return", () => {
  beforeEach(() => {
    context.mobile = false
    context.search = ""
  })

  it("offers a workspace destination even when settings was opened directly", () => {
    showSettings()
    expect(
      screen.getByRole("link", { name: "Back to workspace" })
    ).toHaveAttribute("href", "/workspace")
    expect(screen.getByRole("button", { name: "Organization" })).toBeVisible()
  })

  it("does not show organization settings in the desktop runtime", () => {
    Object.assign(window, { __TAURI_INTERNALS__: {} })
    showSettings()
    expect(screen.queryByRole("button", { name: "Organization" })).toBeNull()
    delete (window as Window & { __TAURI_INTERNALS__?: unknown })
      .__TAURI_INTERNALS__
  })

  it("keeps the return action outside the closed mobile navigation drawer", () => {
    context.mobile = true
    showSettings()
    expect(
      screen.getByRole("link", { name: "Back to workspace" })
    ).toBeVisible()
  })

  it("returns to the recorded chat without carrying settings-only parameters", () => {
    context.search = "?returnChatId=42&agentType=claude_code&section=appearance"
    showSettings()
    expect(
      screen.getByRole("link", { name: "Back to workspace" })
    ).toHaveAttribute("href", "/workspace?chatId=42")
  })

  it.each([
    "-1",
    "0",
    "1e3",
    "9007199254740992",
    "https://evil.example",
    "42&returnChatId=43",
  ])(
    "does not treat invalid or ambiguous chat context %s as a return destination",
    (id) => {
      context.search = `?returnChatId=${id}&returnTo=https://evil.example`
      showSettings()
      expect(
        screen.getByRole("link", { name: "Back to workspace" })
      ).toHaveAttribute("href", "/workspace")
    }
  )

  it("preserves the remote workspace identity without trusting arbitrary return URLs", () => {
    context.search =
      "?remoteConnectionId=7&remoteWindowId=rw-a&returnChatId=42&returnTo=//evil.example"
    showSettings()
    expect(
      screen.getByRole("link", { name: "Back to workspace" })
    ).toHaveAttribute(
      "href",
      "/workspace?chatId=42&remoteConnectionId=7&remoteWindowId=rw-a"
    )
  })
})

describe("settingsNavItemsFor", () => {
  it("drops the Web service page in a browser session and nothing else", () => {
    const labels = settingsNavItemsFor("web").map((item) => item.labelKey)
    // There is no second server to configure from inside the one you are
    // already talking to.
    expect(labels).not.toContain("web_service")
    // The Browser page is mostly a desktop browser engine, but site rules,
    // what a server starting in a terminal does and the terminal's link menu
    // all still decide something here — and this page is the only place they
    // can be set, so the entry has to lead there.
    expect(labels).toContain("browser")
    expect(labels.filter((label) => label !== "organization")).toEqual(
      settingsNavItemsFor("tauri")
        .map((item) => item.labelKey)
        .filter((label) => label !== "web_service")
    )
  })

  it("keeps collaboration under General and the two Use pages together", () => {
    const labels = settingsNavItemsFor("tauri").map((item) => item.labelKey)
    const after = (label: (typeof labels)[number]) =>
      labels[labels.indexOf(label) + 1]
    expect(after("general")).toBe("collaboration")
    expect(after("quick_messages")).toBe("browser")
    expect(after("browser")).toBe("computer_use")
  })

  it("keeps every page on the desktop", () => {
    const labels = settingsNavItemsFor("tauri").map((item) => item.labelKey)
    expect(labels).toContain("web_service")
    expect(labels).toContain("browser")
  })
})
