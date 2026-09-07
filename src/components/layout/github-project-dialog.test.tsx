import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react"
import { afterEach, describe, expect, it, vi } from "vitest"
import { GithubProjectDialog } from "./github-project-dialog"

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
    render(<GithubProjectDialog open onOpenChange={() => {}} />)
    expect(
      await screen.findByRole("link", { name: "Connect GitHub" })
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
    render(<GithubProjectDialog open onOpenChange={() => {}} />)
    fireEvent.click(await screen.findByRole("button", { name: "Try again" }))
    expect(await screen.findByText("acme/docs")).toBeTruthy()
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull())
  })
})
