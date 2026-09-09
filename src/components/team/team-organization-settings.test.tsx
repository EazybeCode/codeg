import { act, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import messages from "@/i18n/messages/en.json"

const client = vi.hoisted(() => ({
  loadTeamOrganizationSettings: vi.fn(),
  updateTeamOrganizationSettings: vi.fn(),
}))
const auth = vi.hoisted(() => ({
  value: {} as Record<string, unknown>,
  notifyTeamAuthChanged: vi.fn(),
}))
vi.mock("@/lib/team-client", () => {
  class TeamClientError extends Error {
    constructor(public readonly kind: string) {
      super("Team request failed")
    }
  }
  return { ...client, TeamClientError }
})
vi.mock("./team-auth-provider", () => ({
  useTeamAuth: () => auth.value,
  notifyTeamAuthChanged: auth.notifyTeamAuthChanged,
}))

import { TeamOrganizationSettingsPage } from "./team-organization-settings"

const organization = {
  id: 21,
  login: "acme-github",
  name: "Acme Product",
  avatarUrl: "data:image/png;base64,b2xk",
  canEdit: true,
}
const session = {
  user: { id: 11, login: "person-a", name: "Person A", avatarUrl: null },
  organization: {
    id: 21,
    login: "acme-github",
    name: "Acme Product",
    avatarUrl: null,
  },
  role: "admin" as const,
  scope: "checked-session",
  csrf: "checked-csrf",
}

function showPage() {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamOrganizationSettingsPage />
    </NextIntlClientProvider>
  )
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((res) => {
    resolve = res
  })
  return { promise, resolve }
}

beforeEach(() => {
  vi.clearAllMocks()
  auth.value = {
    status: "authenticated",
    session,
    scopeKey: "checked-session:21:11",
    activate: vi.fn(),
    deactivate: vi.fn(),
    refresh: vi.fn().mockResolvedValue(undefined),
    logout: vi.fn(),
    logoutError: false,
  }
  client.loadTeamOrganizationSettings.mockResolvedValue(organization)
})

describe("TeamOrganizationSettingsPage", () => {
  it("renders a member-safe read-only organization form", async () => {
    client.loadTeamOrganizationSettings.mockResolvedValueOnce({
      ...organization,
      canEdit: false,
    })
    showPage()

    expect(
      await screen.findByLabelText(/GitHub organization/i)
    ).toHaveAttribute("readonly")
    expect(screen.getByLabelText(/display name/i)).toHaveAttribute("readonly")
    expect(
      screen.getByText(/only organization owners and admins/i)
    ).toBeVisible()
    expect(screen.queryByLabelText(/upload organization logo/i)).toBeNull()
    expect(screen.queryByRole("button", { name: /save/i })).toBeNull()
  })

  it("saves one controlled draft with exact CSRF and announces normalized success", async () => {
    const saved = {
      ...organization,
      name: "Atlas Team",
      avatarUrl: "data:image/png;base64,c2VydmVy",
    }
    client.updateTeamOrganizationSettings.mockResolvedValueOnce(saved)
    const identityChanged = vi.fn()
    window.addEventListener("workos:identity-changed", identityChanged)
    const user = userEvent.setup()
    showPage()

    const name = await screen.findByLabelText(/display name/i)
    await user.clear(name)
    await user.type(name, "Atlas Team")
    const save = screen.getByRole("button", { name: /save/i })
    await user.click(save)
    await user.click(save)

    expect(client.updateTeamOrganizationSettings).toHaveBeenCalledOnce()
    expect(client.updateTeamOrganizationSettings).toHaveBeenCalledWith(
      { name: "Atlas Team", logo: { action: "keep" } },
      "checked-csrf",
      expect.any(AbortSignal)
    )
    expect(await screen.findByRole("status")).toHaveTextContent(/saved/i)
    expect(name).toHaveValue("Atlas Team")
    expect(auth.notifyTeamAuthChanged).toHaveBeenCalledOnce()
    expect(auth.value.refresh).toHaveBeenCalledOnce()
    expect(identityChanged).not.toHaveBeenCalled()
    window.removeEventListener("workos:identity-changed", identityChanged)
  })

  it("previews valid uploads, rejects oversized files before reading, and saves removal explicitly", async () => {
    client.updateTeamOrganizationSettings.mockResolvedValueOnce({
      ...organization,
      avatarUrl: null,
    })
    const user = userEvent.setup()
    showPage()
    const upload = await screen.findByLabelText(/upload organization logo/i)

    await user.upload(
      upload,
      new File([new Uint8Array(1024 * 1024 + 1)], "too-large.png", {
        type: "image/png",
      })
    )
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /no larger than 1 MB/i
    )

    await user.upload(
      upload,
      new File(["logo"], "logo.webp", { type: "image/webp" })
    )
    expect(
      await screen.findByRole("img", { name: /organization logo preview/i })
    ).toHaveAttribute("src", expect.stringMatching(/^data:image\/webp;base64,/))

    await user.click(screen.getByRole("button", { name: /remove logo/i }))
    expect(
      screen.queryByRole("img", { name: /organization logo preview/i })
    ).toBeNull()
    await user.click(screen.getByRole("button", { name: /save/i }))
    expect(client.updateTeamOrganizationSettings).toHaveBeenCalledWith(
      { name: "Acme Product", logo: { action: "remove" } },
      "checked-csrf",
      expect.any(AbortSignal)
    )
  })

  it("retains a failed save draft, focuses its translated error, and permits retry", async () => {
    const { TeamClientError } = await import("@/lib/team-client")
    client.updateTeamOrganizationSettings.mockRejectedValueOnce(
      new TeamClientError("unavailable")
    )
    const user = userEvent.setup()
    showPage()
    const name = await screen.findByLabelText(/display name/i)
    await user.clear(name)
    await user.type(name, "Retry Atlas")
    await user.click(screen.getByRole("button", { name: /save/i }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent(/could not save/i)
    expect(alert.parentElement).toHaveFocus()
    expect(name).toHaveValue("Retry Atlas")
    expect(screen.getByRole("button", { name: /save/i })).toBeEnabled()
  })

  it("hides a retained draft while authority is revalidating", async () => {
    const user = userEvent.setup()
    const view = showPage()
    const name = await screen.findByLabelText(/display name/i)
    await user.clear(name)
    await user.type(name, "Hidden draft")

    auth.value = {
      ...auth.value,
      status: "loading",
      session: null,
      scopeKey: null,
    }
    view.rerender(
      <NextIntlClientProvider locale="en" messages={messages}>
        <TeamOrganizationSettingsPage />
      </NextIntlClientProvider>
    )
    expect(screen.queryByDisplayValue("Hidden draft")).toBeNull()
    expect(screen.getByRole("main")).toHaveAttribute("aria-busy", "true")

    const reload = deferred<typeof organization>()
    client.loadTeamOrganizationSettings.mockReturnValueOnce(reload.promise)
    auth.value = {
      ...auth.value,
      status: "authenticated",
      session,
      scopeKey: "checked-session:21:11",
    }
    view.rerender(
      <NextIntlClientProvider locale="en" messages={messages}>
        <TeamOrganizationSettingsPage />
      </NextIntlClientProvider>
    )
    expect(screen.queryByDisplayValue("Hidden draft")).toBeNull()
    await act(async () => reload.resolve(organization))
    expect(await screen.findByDisplayValue("Hidden draft")).toBeVisible()
  })

  it("does not render untrusted remote or SVG logo URLs and remains operable at mobile width", async () => {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 375,
    })
    client.loadTeamOrganizationSettings.mockResolvedValueOnce({
      ...organization,
      avatarUrl: "https://uploads.example/untrusted.svg",
    })
    showPage()

    expect(await screen.findByLabelText(/display name/i)).toBeVisible()
    expect(
      screen.queryByRole("img", { name: /organization logo preview/i })
    ).toBeNull()
    expect(screen.getByLabelText(/upload organization logo/i)).toBeVisible()
    expect(screen.getByRole("button", { name: /remove logo/i })).toBeVisible()
    expect(
      screen.getByRole("link", { name: /back to workspace/i })
    ).toHaveAttribute("href", "/workspace")
  })

  it("exposes busy state and native keyboard-operable controls while a save is pending", async () => {
    const save = deferred<typeof organization>()
    client.updateTeamOrganizationSettings.mockReturnValueOnce(save.promise)
    const user = userEvent.setup()
    showPage()
    const name = await screen.findByLabelText(/display name/i)
    await user.clear(name)
    await user.type(name, "Pending Atlas")
    await user.tab()
    expect(document.activeElement).not.toBe(document.body)
    await user.click(screen.getByRole("button", { name: /save/i }))

    expect(screen.getByRole("form")).toHaveAttribute("aria-busy", "true")
    expect(screen.getByRole("button", { name: /saving/i })).toBeDisabled()
    expect(name).toBeDisabled()
    await act(async () =>
      save.resolve({ ...organization, name: "Pending Atlas" })
    )
  })
})
