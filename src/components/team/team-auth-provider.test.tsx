import { useEffect } from "react"
import { act, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { beforeEach, describe, expect, it, vi } from "vitest"

const client = vi.hoisted(() => ({
  loadTeamSession: vi.fn(),
  logoutTeamSession: vi.fn(),
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

import {
  TeamAuthProvider,
  useTeamAuth,
  type TeamAuthContextValue,
} from "./team-auth-provider"

const firstSession = {
  user: { id: 11, login: "person-a", name: "Person A", avatarUrl: null },
  organization: {
    id: 21,
    login: "org-a",
    name: "Organization A",
    avatarUrl: null,
  },
  role: "member" as const,
  scope: "scope-a",
  csrf: "csrf-a",
}
const secondSession = {
  user: { id: 12, login: "person-b", name: "Person B", avatarUrl: null },
  organization: {
    id: 22,
    login: "org-b",
    name: "Organization B",
    avatarUrl: null,
  },
  role: "admin" as const,
  scope: "scope-b",
  csrf: "csrf-b",
}

function Probe() {
  const auth = useTeamAuth()
  const { activate, deactivate } = auth
  useEffect(() => {
    activate()
    return deactivate
  }, [activate, deactivate])
  return (
    <div>
      <span>{auth.status}</span>
      <span>{auth.scopeKey ?? "no-scope"}</span>
      <span>{auth.session?.user.name ?? "no-user"}</span>
      <span>{auth.logoutError ? "logout-error" : "no-logout-error"}</span>
      <button onClick={() => void auth.refresh()}>Refresh</button>
      <button onClick={() => void auth.logout()}>Logout</button>
    </div>
  )
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe("TeamAuthProvider", () => {
  it("derives scope from fingerprint, organization and user", async () => {
    client.loadTeamSession.mockResolvedValueOnce(firstSession)
    render(
      <TeamAuthProvider>
        <Probe />
      </TeamAuthProvider>
    )
    expect(await screen.findByText("scope-a:21:11")).toBeVisible()
    expect(screen.getByText("Person A")).toBeVisible()
  })

  it("clears prior-scope content synchronously and ignores a stale bootstrap", async () => {
    const firstRefresh = deferred<TeamAuthContextValue["session"]>()
    const secondRefresh = deferred<TeamAuthContextValue["session"]>()
    client.loadTeamSession
      .mockResolvedValueOnce(firstSession)
      .mockReturnValueOnce(firstRefresh.promise)
      .mockReturnValueOnce(secondRefresh.promise)
    const user = userEvent.setup()
    render(
      <TeamAuthProvider>
        <Probe />
      </TeamAuthProvider>
    )
    expect(await screen.findByText("Person A")).toBeVisible()

    await user.click(screen.getByRole("button", { name: "Refresh" }))
    expect(screen.getByText("no-user")).toBeVisible()
    await user.click(screen.getByRole("button", { name: "Refresh" }))

    await act(async () => firstRefresh.resolve(firstSession))
    expect(screen.getByText("no-user")).toBeVisible()
    await act(async () => secondRefresh.resolve(secondSession))
    expect(screen.getByText("Person B")).toBeVisible()
    expect(screen.getByText("scope-b:22:12")).toBeVisible()
  })

  it("clears authority on a 503 refresh instead of keeping it interactive", async () => {
    const { TeamClientError } = await import("@/lib/team-client")
    client.loadTeamSession
      .mockResolvedValueOnce(firstSession)
      .mockRejectedValueOnce(new TeamClientError("unavailable", 503))
    const user = userEvent.setup()
    render(
      <TeamAuthProvider>
        <Probe />
      </TeamAuthProvider>
    )
    expect(await screen.findByText("Person A")).toBeVisible()
    await user.click(screen.getByRole("button", { name: "Refresh" }))
    expect(await screen.findByText("unavailable")).toBeVisible()
    expect(screen.getByText("no-user")).toBeVisible()
  })

  it("does not claim logout success when the server is unavailable", async () => {
    const { TeamClientError } = await import("@/lib/team-client")
    client.loadTeamSession.mockResolvedValueOnce(firstSession)
    client.logoutTeamSession.mockRejectedValueOnce(
      new TeamClientError("unavailable", 503)
    )
    const user = userEvent.setup()
    render(
      <TeamAuthProvider>
        <Probe />
      </TeamAuthProvider>
    )
    expect(await screen.findByText("Person A")).toBeVisible()
    await user.click(screen.getByRole("button", { name: "Logout" }))
    expect(await screen.findByText("logout-error")).toBeVisible()
    expect(screen.getByText("Person A")).toBeVisible()
  })

  it("broadcasts only a fixed invalidation after successful logout", async () => {
    const messages: unknown[] = []
    class TestChannel {
      postMessage(value: unknown) {
        messages.push(value)
      }
      addEventListener() {}
      removeEventListener() {}
      close() {}
    }
    vi.stubGlobal("BroadcastChannel", TestChannel)
    client.loadTeamSession.mockResolvedValueOnce(firstSession)
    client.logoutTeamSession.mockResolvedValueOnce(undefined)
    const user = userEvent.setup()
    render(
      <TeamAuthProvider>
        <Probe />
      </TeamAuthProvider>
    )
    expect(await screen.findByText("Person A")).toBeVisible()
    await user.click(screen.getByRole("button", { name: "Logout" }))

    expect(messages).toEqual([{ type: "invalidate" }])
    expect(JSON.stringify(messages)).not.toContain("csrf-a")
    expect(JSON.stringify(messages)).not.toContain("scope-a")
    expect(router.replace).toHaveBeenCalledWith("/login")
    vi.unstubAllGlobals()
  })
})
