import { cleanup, render as rtlRender, screen } from "@testing-library/react"
import { afterEach, describe, expect, it, vi } from "vitest"
import { NextIntlClientProvider } from "next-intl"
import enMessages from "@/i18n/messages/en.json"
import type { ReactNode } from "react"
const render = (ui: ReactNode) =>
  rtlRender(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
import { WorkspaceIdentity } from "./workspace-identity"
const { desktop } = vi.hoisted(() => ({ desktop: vi.fn(() => false) }))
vi.mock("@/lib/platform", () => ({ isDesktop: desktop }))

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  desktop.mockReturnValue(false)
})

describe("workspace account identity", () => {
  it("keeps desktop local without relative auth fetches or login links", () => {
    desktop.mockReturnValue(true)
    const fetch = vi.fn().mockResolvedValue(new Response("null"))
    vi.stubGlobal("fetch", fetch)
    render(<WorkspaceIdentity />)
    expect(screen.getByText("Local workspace")).toBeVisible()
    expect(screen.queryByRole("link")).toBeNull()
    expect(fetch).not.toHaveBeenCalled()
  })
  it("shows the authenticated user's actual name and organization", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            user: { id: 7, login: "maya", name: "Maya Rao", role: "member" },
            organization: { id: 4, login: "acme", name: "Acme Product" },
          })
        )
      )
    )
    render(<WorkspaceIdentity />)
    expect(await screen.findByText("Acme Product")).toBeVisible()
    expect(screen.getByText("Maya Rao")).toBeVisible()
    expect(screen.getByText("@maya")).toBeVisible()
  })

  it("asks a token-only visitor to sign in instead of inventing a name", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("null")))
    render(<WorkspaceIdentity />)
    expect(
      await screen.findByRole("link", { name: "Sign in with GitHub" })
    ).toHaveAttribute("href", "/auth/github/login")
    expect(screen.getByText("Sign in to your workspace")).toBeVisible()
  })

  it("shows an unavailable state on a failed identity request", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("unavailable", { status: 503 }))
    )
    render(<WorkspaceIdentity />)
    expect(await screen.findByText("Account details unavailable")).toBeVisible()
    expect(screen.queryByText("Local workspace")).toBeNull()
  })
})
