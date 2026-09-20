import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"

const auth = vi.hoisted(() => ({
  value: {} as Record<string, unknown>,
}))
vi.mock("./team-auth-provider", () => ({ useTeamAuth: () => auth.value }))
vi.mock("./team-workspace", () => ({
  TeamWorkspace: () => <p>team catalog workspace</p>,
}))

import { TeamSignedInShell } from "./team-signed-in-shell"

function showShell(children?: React.ReactNode) {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamSignedInShell>{children}</TeamSignedInShell>
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  auth.value = {
    status: "authenticated",
    session: {
      user: {
        id: 1,
        login: "person-login",
        name: "Person Name",
        avatarUrl: null,
      },
      organization: {
        id: 2,
        login: "org-login",
        name: "Organization Display",
        avatarUrl: null,
      },
      role: "admin",
      scope: "scope",
      csrf: "csrf",
    },
    scopeKey: "scope:2:1",
    activate: vi.fn(),
    deactivate: vi.fn(),
    refresh: vi.fn(),
    logout: vi.fn(),
    logoutError: false,
  }
})

describe("TeamSignedInShell", () => {
  it("mounts the team catalog workspace by default without legacy workspace data", async () => {
    showShell()
    // The workspace is loaded on demand now, so await it rather than asserting
    // on the frame before its chunk resolves.
    expect(await screen.findByText("team catalog workspace")).toBeVisible()
  })

  it("keeps a fixed browser return to workspace", () => {
    window.history.replaceState({}, "", "/settings")
    showShell(<h1>Organization settings</h1>)
    expect(
      screen.getByRole("link", { name: /back to workspace/i })
    ).toHaveAttribute("href", "/workspace")
    expect(screen.getByRole("link", { name: /^settings$/i })).toHaveAttribute(
      "href",
      "/settings"
    )
  })

  it("renders authenticated children instead of the migration explanation", () => {
    showShell(<h1>Organization settings</h1>)
    expect(
      screen.getByRole("heading", { name: "Organization settings" })
    ).toBeVisible()
    expect(
      screen.queryByText(/workspace routes are not enabled yet/i)
    ).toBeNull()
  })

  it("pauses identity content on a 503 while retaining safe retry and sign out", () => {
    auth.value = {
      ...auth.value,
      status: "unavailable",
      session: null,
      scopeKey: null,
    }
    showShell()
    expect(screen.queryByText("Person Name")).toBeNull()
    expect(screen.getByRole("button", { name: "Retry" })).toBeVisible()
    expect(screen.getByRole("button", { name: "Sign out" })).toBeVisible()
  })

  it("preserves a normalized stored organization logo", () => {
    const storedLogo = "data:image/png;base64,aW52ZW50ZWQ="
    const session = auth.value.session as {
      organization: { avatarUrl: string | null }
    }
    session.organization.avatarUrl = storedLogo
    const { container } = showShell(<h1>Organization settings</h1>)
    expect(container.querySelector(`img[src="${storedLogo}"]`)).not.toBeNull()
  })
})
