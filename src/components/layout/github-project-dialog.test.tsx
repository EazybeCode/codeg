import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react"
import { afterEach, describe, expect, it, vi } from "vitest"
import { GithubProjectDialog } from "./github-project-dialog"
import { NextIntlClientProvider } from "next-intl"
import en from "@/i18n/messages/en.json"

vi.mock("@/stores/tab-store", () => ({
  useTabActions: () => ({ openNewConversationTab: vi.fn() }),
}))
vi.mock("@/lib/transport/web-auth", () => ({ getCodegToken: () => "test" }))

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe("GitHub project recovery", () => {
  it("offers reconnection for an expired session, not an empty repository list", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ error: "Not signed in" }), {
          status: 401,
        })
      )
    )
    render(
      <NextIntlClientProvider locale="en" messages={en}>
        <GithubProjectDialog open onOpenChange={() => {}} />
      </NextIntlClientProvider>
    )
    expect(
      await screen.findByRole("link", { name: "Reconnect GitHub" })
    ).toHaveAttribute("href", "/auth/github/login")
    expect(screen.queryByText("No repositories found.")).toBeNull()
  })

  it("retries a failed load and renders the real repositories", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response("{}", { status: 503 }))
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify([
            {
              full_name: "acme/docs",
              name: "docs",
              owner: "acme",
              default_branch: "main",
              private: true,
            },
          ])
        )
      )
    vi.stubGlobal("fetch", fetch)
    render(
      <NextIntlClientProvider locale="en" messages={en}>
        <GithubProjectDialog open onOpenChange={() => {}} />
      </NextIntlClientProvider>
    )
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }))
    expect(await screen.findByText("acme/docs")).toBeTruthy()
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull())
  })
})
