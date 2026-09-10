import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const client = vi.hoisted(() => ({
  listTeamNodes: vi.fn(),
  listTeamRepositories: vi.fn(),
  listTeamChats: vi.fn(),
  loadTeamChat: vi.fn(),
  createTeamNode: vi.fn(),
  updateTeamNode: vi.fn(),
  connectTeamRepository: vi.fn(),
  createTeamChat: vi.fn(),
  changeTeamChatVisibility: vi.fn(),
}))
const auth = vi.hoisted(() => ({ value: {} as Record<string, unknown> }))

vi.mock("@/lib/team-catalog-client", () => client)
vi.mock("./team-auth-provider", () => ({ useTeamAuth: () => auth.value }))

import { TeamClientError } from "@/lib/team-client"
import { parseTeamCatalogSelection, useTeamCatalog } from "./use-team-catalog"

const project = {
  id: 1,
  parentId: null,
  kind: "project" as const,
  name: "Atlas",
  repositoryDefault: null,
  revision: "0",
}
const repository = {
  id: 2,
  githubRepositoryId: "8080",
  fullName: "atlas/repo",
}
const visibleChat = {
  id: 9,
  nodeId: 1,
  title: "Shared planning",
  repositoryId: 2,
  githubRepositoryId: "8080",
  fullName: "atlas/repo",
  startingBranch: "main",
  visibility: "organization" as const,
  accessRevision: "0",
  canShare: false,
  executionState: "unprovisioned",
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

function setAuth(scope = "scope-a", userId = 11) {
  auth.value = {
    status: "authenticated",
    scopeKey: `${scope}:21:${userId}`,
    session: {
      csrf: `csrf-${scope}`,
      scope,
      user: { id: userId },
      organization: { id: 21 },
    },
    refresh: vi.fn().mockResolvedValue(undefined),
  }
}

beforeEach(() => {
  vi.resetAllMocks()
  window.history.replaceState({}, "", "/workspace")
  setAuth()
  client.listTeamNodes.mockResolvedValue([project])
  client.listTeamRepositories.mockResolvedValue([repository])
  client.listTeamChats.mockResolvedValue([visibleChat])
})

describe("useTeamCatalog", () => {
  it("parses only positive integer query selections", () => {
    expect(parseTeamCatalogSelection("?node=7&chat=9")).toEqual({
      nodeId: 7,
      chatId: 9,
    })
    expect(parseTeamCatalogSelection("?node=0&chat=1.5")).toEqual({
      nodeId: null,
      chatId: null,
    })
  })

  it("aborts old scope reads, synchronously gates content, and ignores a late mutation", async () => {
    const mutation = deferred<typeof project>()
    client.createTeamNode.mockReturnValueOnce(mutation.promise)
    const { result, rerender } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.nodes).toEqual([project]))
    const oldSignal = client.listTeamNodes.mock.calls[0][0] as AbortSignal
    let pending!: Promise<unknown>
    act(() => {
      pending = result.current
        .createNode({
          parentId: null,
          kind: "project",
          name: "Late",
          repositoryDefault: null,
        })
        .catch((error) => error)
    })

    setAuth("scope-b", 12)
    rerender()
    expect(result.current.nodes).toEqual([])
    expect(result.current.chats).toEqual([])
    expect(oldSignal.aborted).toBe(true)
    await act(async () => mutation.resolve({ ...project, id: 4, name: "Late" }))
    await expect(pending).resolves.toMatchObject({ kind: "aborted" })
    expect(result.current.nodes).not.toContainEqual(
      expect.objectContaining({ name: "Late" })
    )
  })

  it("clears protected content and refreshes authentication on 401", async () => {
    const refresh = vi.fn().mockResolvedValue(undefined)
    auth.value = { ...auth.value, refresh }
    client.listTeamNodes.mockRejectedValueOnce(
      new TeamClientError("unauthorized", 401)
    )
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(refresh).toHaveBeenCalledOnce())
    expect(result.current.nodes).toEqual([])
    expect(result.current.chats).toEqual([])
  })

  it("prioritizes a parallel 401 over an earlier non-authentication failure", async () => {
    const refreshAuth = auth.value.refresh as ReturnType<typeof vi.fn>
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.nodes).toEqual([project]))
    client.listTeamNodes.mockRejectedValueOnce(
      new TeamClientError("request_failed", 500)
    )
    client.listTeamRepositories.mockRejectedValueOnce(
      new TeamClientError("unauthorized", 401)
    )
    client.listTeamChats.mockResolvedValueOnce([visibleChat])

    await act(async () => result.current.refresh())

    await waitFor(() => expect(refreshAuth).toHaveBeenCalledOnce())
    expect(result.current.nodes).toEqual([])
    expect(result.current.chats).toEqual([])
  })

  it("clears loaded chat detail when a node is selected", async () => {
    window.history.replaceState({}, "", "/workspace?node=1&chat=9")
    client.loadTeamChat.mockResolvedValueOnce(visibleChat)
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() =>
      expect(result.current.selectedChat).toEqual(visibleChat)
    )

    act(() => result.current.selectNode(1))

    expect(result.current.selectedNodeId).toBe(1)
    expect(result.current.selectedChatId).toBeNull()
    expect(result.current.selectedChat).toBeNull()
  })

  it("ignores a delayed chat detail success after later node navigation", async () => {
    const detail = deferred<typeof visibleChat>()
    client.loadTeamChat.mockReturnValueOnce(detail.promise)
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.chats).toEqual([visibleChat]))

    act(() => result.current.selectChat(9))
    expect(result.current.selectedChatId).toBe(9)
    act(() => result.current.selectNode(1))
    await act(async () => detail.resolve(visibleChat))

    expect(result.current.selectedNodeId).toBe(1)
    expect(result.current.selectedChatId).toBeNull()
    expect(result.current.selectedChat).toBeNull()
  })

  it("loads history chat selections and ignores a delayed 404 after later node navigation", async () => {
    const detail = deferred<typeof visibleChat>()
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.chats).toEqual([visibleChat]))
    client.loadTeamChat.mockReturnValueOnce(detail.promise)

    window.history.pushState({}, "", "/workspace?node=1&chat=9")
    act(() => window.dispatchEvent(new PopStateEvent("popstate")))
    await waitFor(() =>
      expect(client.loadTeamChat).toHaveBeenCalledWith(
        9,
        expect.any(AbortSignal)
      )
    )
    window.history.pushState({}, "", "/workspace?node=1")
    act(() => window.dispatchEvent(new PopStateEvent("popstate")))
    await act(async () =>
      detail.reject(new TeamClientError("request_failed", 404))
    )

    expect(result.current.selectedNodeId).toBe(1)
    expect(result.current.selectedChatId).toBeNull()
    expect(result.current.selectedChat).toBeNull()
    expect(result.current.chats).toEqual([visibleChat])
    expect(result.current.error).toBeNull()
  })

  it("does not reinsert a revoked selected chat from an older delayed list", async () => {
    window.history.replaceState({}, "", "/workspace?chat=9")
    const list = deferred<(typeof visibleChat)[]>()
    client.listTeamChats.mockReturnValueOnce(list.promise)
    client.loadTeamChat.mockRejectedValueOnce(
      new TeamClientError("request_failed", 404)
    )
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() =>
      expect(client.loadTeamChat).toHaveBeenCalledWith(
        9,
        expect.any(AbortSignal)
      )
    )
    await act(async () => list.resolve([visibleChat]))
    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.selectedChat).toBeNull()
    expect(result.current.chats).toEqual([])
  })

  it("restores a reshared chat from a newer list while rejecting the pre-404 list", async () => {
    window.history.replaceState({}, "", "/workspace?chat=9")
    const olderList = deferred<(typeof visibleChat)[]>()
    const detail = deferred<typeof visibleChat>()
    client.listTeamChats
      .mockReturnValueOnce(olderList.promise)
      .mockResolvedValueOnce([visibleChat])
    client.loadTeamChat.mockReturnValueOnce(detail.promise)
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.selectedChatId).toBe(9))
    await act(async () =>
      detail.reject(new TeamClientError("request_failed", 404))
    )
    await waitFor(() => expect(result.current.selectedChatId).toBeNull())

    await act(async () => result.current.refresh())
    expect(result.current.chats).toEqual([visibleChat])
    await act(async () => olderList.resolve([visibleChat]))
    expect(result.current.chats).toEqual([visibleChat])
  })

  it("loads another page only after 200 visible rows using the last visible id", async () => {
    const page = Array.from({ length: 200 }, (_, index) => ({
      ...visibleChat,
      id: index + 1,
      title: `Chat ${index + 1}`,
    }))
    client.listTeamChats
      .mockResolvedValueOnce(page)
      .mockResolvedValueOnce([{ ...visibleChat, id: 201 }])
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.chats).toHaveLength(200))
    expect(result.current.hasMoreChats).toBe(true)
    await act(async () => result.current.loadMoreChats())
    expect(client.listTeamChats).toHaveBeenLastCalledWith(
      { afterId: 200 },
      expect.any(AbortSignal)
    )
    expect(result.current.chats).toHaveLength(201)
    expect(result.current.hasMoreChats).toBe(false)
  })

  it("does not merge a stale load-more page after refresh replaces the list", async () => {
    const page = Array.from({ length: 200 }, (_, index) => ({
      ...visibleChat,
      id: index + 1,
      title: `Chat ${index + 1}`,
    }))
    const stalePage = deferred<(typeof visibleChat)[]>()
    const refreshed = { ...visibleChat, id: 1, title: "Refreshed page one" }
    client.listTeamChats
      .mockResolvedValueOnce(page)
      .mockReturnValueOnce(stalePage.promise)
      .mockResolvedValueOnce([refreshed])
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.chats).toHaveLength(200))
    let pending!: Promise<void>
    act(() => {
      pending = result.current.loadMoreChats()
    })
    await waitFor(() => expect(client.listTeamChats).toHaveBeenCalledTimes(2))

    await act(async () => result.current.refresh())
    expect(result.current.chats).toEqual([refreshed])
    await act(async () => stalePage.resolve([{ ...visibleChat, id: 201 }]))
    await pending
    expect(result.current.chats).toEqual([refreshed])
  })

  it("refreshes the node revision after a conflict so a retained edit can retry", async () => {
    client.updateTeamNode.mockRejectedValueOnce(
      new TeamClientError("conflict", 409)
    )
    const refreshed = { ...project, revision: "1", name: "Server name" }
    client.listTeamNodes
      .mockResolvedValueOnce([project])
      .mockResolvedValueOnce([refreshed])
    const { result } = renderHook(() => useTeamCatalog())
    await waitFor(() => expect(result.current.nodes).toEqual([project]))

    await act(async () => {
      await expect(
        result.current.updateNode(project, {
          expectedRevision: "0",
          parentId: null,
          name: "Retained name",
          repositoryDefault: null,
        })
      ).rejects.toMatchObject({ kind: "conflict" })
    })
    await waitFor(() => expect(result.current.nodes).toEqual([refreshed]))
    expect(client.updateTeamNode).toHaveBeenCalledWith(
      1,
      expect.objectContaining({ expectedRevision: "0", parentId: null }),
      "csrf-scope-a",
      expect.any(AbortSignal)
    )
  })

  it("stops polling while hidden and refreshes immediately when visible", async () => {
    vi.useFakeTimers()
    let visibility: DocumentVisibilityState = "hidden"
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => visibility,
    })
    renderHook(() => useTeamCatalog())
    await act(async () => Promise.resolve())
    const before = client.listTeamNodes.mock.calls.length
    await act(async () => vi.advanceTimersByTimeAsync(30_000))
    expect(client.listTeamNodes).toHaveBeenCalledTimes(before)

    visibility = "visible"
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    expect(client.listTeamNodes.mock.calls.length).toBeGreaterThan(before)
    vi.useRealTimers()
  })
})
