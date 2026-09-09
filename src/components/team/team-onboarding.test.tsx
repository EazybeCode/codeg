import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"

const client = vi.hoisted(() => ({
  loadPendingChoices: vi.fn(),
  selectOrganization: vi.fn(),
}))
const router = vi.hoisted(() => ({ replace: vi.fn() }))
vi.mock("next/navigation", () => ({ useRouter: () => router }))
vi.mock("@/lib/team-client", async () => {
  class TeamClientError extends Error {
    constructor(
      public readonly kind: string,
      public readonly status?: number
    ) {
      super("Team request failed")
    }
  }
  return { ...client, TeamClientError }
})

import { TeamOnboarding } from "./team-onboarding"

const identity = {
  githubUserId: "9007199254740992",
  login: "octo-person",
  name: "Octo Person",
  avatarUrl: null,
}
const organization = {
  githubOrgId: "9223372036854775807",
  login: "octo-org",
  avatarUrl: null,
  role: "admin" as const,
}

function showOnboarding() {
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamOnboarding />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  vi.clearAllMocks()
  client.selectOrganization.mockResolvedValue({ redirect: "/workspace" })
})

describe("TeamOnboarding", () => {
  it("requires explicit choice even when only one organization is eligible", async () => {
    client.loadPendingChoices.mockResolvedValueOnce({
      identity,
      organizations: [organization],
      csrf: "pending-csrf",
    })
    const user = userEvent.setup()
    showOnboarding()

    expect(await screen.findByText("Octo Person")).toBeVisible()
    expect(screen.getByText("@octo-person")).toBeVisible()
    const continueButton = screen.getByRole("button", { name: "Continue" })
    expect(continueButton).toBeDisabled()
    await user.click(screen.getByRole("radio", { name: /octo-org/i }))
    expect(continueButton).toBeEnabled()
    await user.click(continueButton)
    expect(client.selectOrganization).toHaveBeenCalledWith(
      "9223372036854775807",
      "pending-csrf",
      expect.any(AbortSignal)
    )
  })

  it("offers search for a long keyboard-accessible organization list", async () => {
    client.loadPendingChoices.mockResolvedValueOnce({
      identity,
      organizations: Array.from({ length: 7 }, (_, index) => ({
        ...organization,
        githubOrgId: String(1000 + index),
        login: `organization-${index}`,
      })),
      csrf: "pending-csrf",
    })
    const user = userEvent.setup()
    showOnboarding()

    const search = await screen.findByRole("searchbox", {
      name: /search organizations/i,
    })
    await user.type(search, "organization-6")
    expect(screen.getByRole("radio", { name: /organization-6/i })).toBeVisible()
    expect(screen.queryByRole("radio", { name: /organization-0/i })).toBeNull()
    await user.tab()
    expect(screen.getByRole("radio", { name: /organization-6/i })).toHaveFocus()
  })

  it("shows zero-choice membership and GitHub App recovery without auto-installing", async () => {
    client.loadPendingChoices.mockResolvedValueOnce({
      identity,
      organizations: [],
      csrf: "pending-csrf",
    })
    showOnboarding()

    expect(await screen.findByText(/no eligible organizations/i)).toBeVisible()
    expect(screen.getByText(/active organization membership/i)).toBeVisible()
    expect(screen.getByText(/pending invitations/i)).toBeVisible()
    expect(
      screen.getByRole("link", { name: /open github app installation/i })
    ).toHaveAttribute(
      "href",
      "https://github.com/apps/eazyworkos/installations/new"
    )
    expect(screen.getByRole("button", { name: "Retry" })).toBeVisible()
    expect(
      screen.getByRole("link", { name: /restart sign-in/i })
    ).toHaveAttribute("href", "/auth/github/login")
  })

  it("keeps the unsent choice and focuses a retryable selection error", async () => {
    const { TeamClientError } = await import("@/lib/team-client")
    client.loadPendingChoices.mockResolvedValueOnce({
      identity,
      organizations: [organization],
      csrf: "pending-csrf",
    })
    client.selectOrganization.mockRejectedValueOnce(
      new TeamClientError("unavailable", 503)
    )
    const user = userEvent.setup()
    showOnboarding()
    const radio = await screen.findByRole("radio", { name: /octo-org/i })
    await user.click(radio)
    await user.click(screen.getByRole("button", { name: "Continue" }))

    const error = await screen.findByRole("alert")
    expect(error).toHaveFocus()
    expect(radio).toBeChecked()
    expect(screen.getByRole("button", { name: "Continue" })).toBeEnabled()
  })

  it("offers recovery when the selected membership is no longer eligible", async () => {
    const { TeamClientError } = await import("@/lib/team-client")
    client.loadPendingChoices.mockResolvedValueOnce({
      identity,
      organizations: [organization],
      csrf: "pending-csrf",
    })
    client.selectOrganization.mockRejectedValueOnce(
      new TeamClientError("forbidden", 403)
    )
    const user = userEvent.setup()
    showOnboarding()
    await user.click(await screen.findByRole("radio", { name: /octo-org/i }))
    await user.click(screen.getByRole("button", { name: "Continue" }))

    expect(await screen.findByRole("alert")).toHaveTextContent(
      /no longer available/i
    )
    expect(router.replace).not.toHaveBeenCalled()
  })

  it("turns an expired pending flow into restart recovery without inventing an organization", async () => {
    const { TeamClientError } = await import("@/lib/team-client")
    client.loadPendingChoices.mockRejectedValueOnce(
      new TeamClientError("unauthorized", 401)
    )
    showOnboarding()
    expect(await screen.findByText(/sign-in has expired/i)).toBeVisible()
    expect(screen.queryByRole("radio")).toBeNull()
    expect(
      screen.getByRole("link", { name: /restart sign-in/i })
    ).toHaveAttribute("href", "/auth/github/login")
  })

  it("marks async work busy and uses a narrow-screen-safe container", async () => {
    let resolve!: (value: unknown) => void
    client.loadPendingChoices.mockReturnValueOnce(
      new Promise((next) => {
        resolve = next
      })
    )
    showOnboarding()
    const region = screen.getByRole("main")
    expect(region).toHaveAttribute("aria-busy", "true")
    expect(region.firstElementChild).toHaveClass("w-full", "max-w-lg", "px-4")
    resolve({ identity, organizations: [organization], csrf: "pending-csrf" })
    await waitFor(() => expect(region).toHaveAttribute("aria-busy", "false"))
  })
})
