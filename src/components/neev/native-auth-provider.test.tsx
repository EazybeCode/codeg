import { act, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeBoundary } from "./native-boundary"
import { useNativeAuth } from "./native-auth-provider"
import { NativeWorkspace } from "./native-workspace"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
const router = vi.hoisted(() => ({ replace: vi.fn(), push: vi.fn() }))
vi.mock("next/navigation", () => ({ useRouter: () => router }))
afterEach(() => {
  vi.unstubAllGlobals()
  vi.clearAllMocks()
})
function Controls() {
  const auth = useNativeAuth()
  return (
    <>
      <button onClick={() => void auth.refresh()}>Refresh fixture</button>
      <NativeBoundary>
        <p>{auth.session?.organization?.name}</p>
        <button onClick={() => void auth.logout()}>Logout fixture</button>
      </NativeBoundary>
    </>
  )
}
it("hides the old organization while refreshing and rejects an aborted old response", async () => {
  let request = 0,
    oldReply: ((response: Response) => void) | undefined,
    oldSignal: AbortSignal | null | undefined
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": (init) => {
      request += 1
      if (request === 2) {
        oldSignal = init.signal
        return new Promise((resolve) => {
          oldReply = resolve
        })
      }
      return json(
        request === 1
          ? orgSession
          : {
              ...orgSession,
              organization: { id: 4, name: "Second company", avatarUrl: null },
            }
      )
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await screen.findByText("Neev demo")
  await user.click(screen.getByText("Refresh fixture"))
  expect(screen.queryByText("Neev demo")).toBeNull()
  await user.click(screen.getByText("Refresh fixture"))
  await screen.findByText("Second company")
  expect(oldSignal?.aborted).toBe(true)
  await act(async () => oldReply?.(json(orgSession)))
  expect(screen.queryByText("Neev demo")).toBeNull()
  expect(mock.unknown).toEqual([])
})
it("clears scoped workspace immediately even if logout fails, then permits retry", async () => {
  let calls = 0
  const mock = mockNativeSession(orgSession, {
    "POST /api/neev/logout": () =>
      ++calls === 1
        ? json({ code: "unavailable" }, 503)
        : new Response(null, { status: 204 }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await user.click(await screen.findByText("Logout fixture"))
  expect(await screen.findByRole("alert")).toHaveTextContent(
    /sign-out could not/i
  )
  expect(screen.queryByText("Neev demo")).toBeNull()
  await act(async () => window.dispatchEvent(new Event("focus")))
  expect(screen.queryByText("Neev demo")).toBeNull()
  expect(screen.getByRole("button", { name: "Retry sign-out" })).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Retry sign-out" }))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(mock.unknown).toEqual([])
})
it("invalidates another tab's scope and redirects a lost session", async () => {
  let valid = true,
    receive: ((event: MessageEvent) => void) | undefined
  class Channel {
    addEventListener(_name: string, callback: (event: MessageEvent) => void) {
      receive = callback
    }
    removeEventListener() {}
    close() {}
    postMessage() {}
  }
  vi.stubGlobal("BroadcastChannel", Channel)
  mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      json(valid ? orgSession : { code: "unauthorized" }, valid ? 200 : 401),
  })
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await screen.findByText("Neev demo")
  valid = false
  await act(async () =>
    receive?.({ data: { type: "invalidate" } } as MessageEvent)
  )
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(screen.queryByText("Neev demo")).toBeNull()
})

it("hides the current scope on visibility return and waits for fresh authority", async () => {
  let request = 0,
    nextReply: ((response: Response) => void) | undefined
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      ++request === 1
        ? json(orgSession)
        : new Promise((resolve) => {
            nextReply = resolve
          }),
  })
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await screen.findByText("Neev demo")
  const visibility = vi
    .spyOn(document, "visibilityState", "get")
    .mockReturnValue("visible")
  await act(async () => document.dispatchEvent(new Event("visibilitychange")))
  expect(screen.getByText("Neev demo")).not.toBeVisible()
  expect(screen.getByText("Neev demo").closest("[inert]")).not.toBeNull()
  await act(async () => nextReply?.(json({ code: "unauthorized" }, 401)))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  visibility.mockRestore()
  expect(mock.unknown).toEqual([])
})

it("does not claim logout when401 is caused by a still-valid rotated session", async () => {
  let bootstraps = 0,
    logouts = 0
  const headers: unknown[] = []
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      json({
        ...orgSession,
        csrf: ++bootstraps === 1 ? "synthetic-csrf" : "rotated-csrf",
      }),
    "POST /api/neev/logout": (init) => {
      headers.push((init.headers as Record<string, string>)["x-neev-csrf"])
      return ++logouts === 1
        ? json({ code: "unauthorized" }, 401)
        : new Response(null, { status: 204 })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await user.click(await screen.findByText("Logout fixture"))
  expect(
    await screen.findByRole("button", { name: "Retry sign-out" })
  ).toBeVisible()
  expect(router.replace).not.toHaveBeenCalled()
  expect(screen.queryByText("Neev demo")).toBeNull()
  await user.click(screen.getByRole("button", { name: "Retry sign-out" }))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(headers).toEqual(["synthetic-csrf", "rotated-csrf"])
  expect(mock.unknown).toEqual([])
})

it("confirms an already-ended session after logout401 using a fresh bootstrap", async () => {
  let bootstraps = 0
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      ++bootstraps === 1
        ? json(orgSession)
        : json({ code: "unauthorized" }, 401),
    "POST /api/neev/logout": () => json({ code: "unauthorized" }, 401),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await user.click(await screen.findByText("Logout fixture"))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(bootstraps).toBe(2)
  expect(mock.unknown).toEqual([])
})
it("never paints an old catalog response after a tab changes organization", async () => {
  let receive: ((event: MessageEvent) => void) | undefined,
    oldReply: ((response: Response) => void) | undefined,
    currentOrg = 3,
    nodeSignal: AbortSignal | null | undefined
  class Channel {
    addEventListener(_name: string, callback: (event: MessageEvent) => void) {
      receive = callback
    }
    removeEventListener() {}
    close() {}
    postMessage() {}
  }
  vi.stubGlobal("BroadcastChannel", Channel)
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      json({
        ...orgSession,
        organization: {
          id: currentOrg,
          name: currentOrg === 3 ? "Old company" : "New company",
          avatarUrl: null,
        },
      }),
    "GET /api/neev/nodes": (init) =>
      currentOrg === 3
        ? new Promise((resolve) => {
            oldReply = resolve
            nodeSignal = init.signal
          })
        : json({ nodes: [], nextAfter: null }),
  })
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await screen.findByText("Old company")
  currentOrg = 4
  await act(async () =>
    receive?.({ data: { type: "invalidate" } } as MessageEvent)
  )
  await screen.findByText("New company")
  expect(nodeSignal?.aborted).toBe(true)
  await act(async () =>
    oldReply?.(
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            parentId: null,
            kind: "project",
            name: "Private old project",
            revision: "1",
          },
        ],
        nextAfter: null,
      })
    )
  )
  expect(screen.queryByText("Private old project")).toBeNull()
  expect(mock.unknown).toEqual([])
})
