import { act, renderHook, waitFor } from "@testing-library/react"
import { expect, it, vi } from "vitest"
import { useWorkspaceCatalog } from "./use-workspace-catalog"
const call = vi.hoisted(() => vi.fn())
vi.mock("@/lib/transport", () => ({ getTransport: () => ({ call }) }))
it.each([true, false])(
  "only retains old tree metadata for a same-session refresh (identity reset: %s)",
  async (resetIdentity) => {
    const nodes = [{ id: 1, name: "Previous private project" }]
    const repositories = [{ id: 8, full_name: "acme/private" }]
    call.mockImplementation(async (cmd) =>
      cmd === "list_workspace_nodes" ? nodes : repositories
    )
    const hook = renderHook(({ scope }) => useWorkspaceCatalog(scope), {
      initialProps: { scope: "1:7" as string | null },
    })
    await waitFor(() => expect(hook.result.current.nodes).toEqual(nodes))
    call.mockRejectedValue({ error: "upstream_unavailable" })
    if (resetIdentity) {
      hook.rerender({ scope: null })
      hook.rerender({ scope: "1:7" })
    } else {
      await act(() => hook.result.current.refresh())
    }
    await waitFor(() =>
      expect(hook.result.current.error).toEqual({
        error: "upstream_unavailable",
      })
    )
    expect(hook.result.current.nodes).toEqual(resetIdentity ? [] : nodes)
    expect(hook.result.current.repositories).toEqual(
      resetIdentity ? [] : repositories
    )
  }
)
it("clears organization caches and ignores a stale tree and draft response", async () => {
  let resolveTree!: (value: unknown) => void
  let resolveDraft!: (value: unknown) => void
  let first = true
  call.mockImplementation(async (cmd) => {
    if (cmd === "list_workspace_repositories") return []
    if (cmd === "list_workspace_chat_drafts")
      return new Promise((resolve) => {
        resolveDraft = resolve
      })
    if (first) {
      first = false
      return new Promise((resolve) => {
        resolveTree = resolve
      })
    }
    return [
      {
        id: 2,
        org_id: 2,
        parent_id: null,
        name: "Second organization",
        kind: "project",
        repository_default: null,
        revision: 1,
      },
    ]
  })
  const hook = renderHook(({ scope }) => useWorkspaceCatalog(scope), {
    initialProps: { scope: "1:7" as string | null },
  })
  act(() => hook.result.current.expand(1))
  hook.rerender({ scope: "2:8" })
  await waitFor(() =>
    expect(hook.result.current.nodes[0]?.name).toBe("Second organization")
  )
  await act(async () => {
    resolveTree([{ id: 1, name: "Old organization" }])
    resolveDraft([{ id: 10, title: "Old chat" }])
  })
  expect(hook.result.current.nodes.map((node) => node.name)).toEqual([
    "Second organization",
  ])
  expect(hook.result.current.drafts).toEqual({})
  expect(hook.result.current.expanded.size).toBe(0)
  hook.rerender({ scope: null })
  expect(hook.result.current.nodes).toEqual([])
})

it("does not resurrect an old draft response when an identity returns to the same scope", async () => {
  let resolveDraft!: (value: unknown) => void
  call.mockImplementation(async (cmd) =>
    cmd === "list_workspace_chat_drafts"
      ? new Promise((resolve) => {
          resolveDraft = resolve
        })
      : []
  )
  const hook = renderHook(({ scope }) => useWorkspaceCatalog(scope), {
    initialProps: { scope: "1:7" },
  })
  act(() => hook.result.current.expand(1))
  hook.rerender({ scope: "2:8" })
  hook.rerender({ scope: "1:7" })
  await act(async () => {
    resolveDraft([{ id: 20, title: "Stale private chat" }])
  })
  expect(hook.result.current.drafts).toEqual({})
})
