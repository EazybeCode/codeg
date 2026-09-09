import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"

const auth = vi.hoisted(() => ({
  value: {} as Record<string, unknown>,
}))
vi.mock("./team-auth-provider", () => ({ useTeamAuth: () => auth.value }))

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
  it("shows selected person and distinct organization branding without legacy workspace data", () => {
    showShell()
    expect(screen.getByText("Person Name")).toBeVisible()
    expect(screen.getByText("@person-login")).toBeVisible()
    expect(screen.getByText("Organization Display")).toBeVisible()
    expect(screen.getByText("@org-login")).toBeVisible()
    expect(
      screen.getByText(/workspace routes are not enabled yet/i)
    ).toBeVisible()
    expect(screen.getByRole("button", { name: /sign out/i })).toBeVisible()
    expect(
      screen.queryByRole("button", { name: /change organization/i })
    ).toBeNull()
  })

  it("keeps a fixed browser return to workspace", () => {
    window.history.replaceState({}, "", "/settings")
    showShell()
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
    const { container } = showShell()
    expect(container.querySelector(`img[src="${storedLogo}"]`)).not.toBeNull()
  })
})
