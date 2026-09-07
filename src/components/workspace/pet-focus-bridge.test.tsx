import { act, render, waitFor, cleanup } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"

// The workspace half lives in the real zustand store: tests seed it via
// setState in beforeEach and flip hydration with act(setState). The tab half
// is still a mutable hook mock — the mock reads this module-level var, so
// reassigning + rerendering simulates the provider state changing.
let tabs: { tabsHydrated: boolean; openTab: ReturnType<typeof vi.fn> }
let addFolderToWorkspaceById: ReturnType<typeof vi.fn>
let capturedHandler: ((p: unknown) => void) | null = null
let catalogEnabled = false

vi.mock("./workspace-catalog-provider", () => ({
  useWorkspaceCatalogContext: () => ({ enabled: catalogEnabled }),
}))

vi.mock("@/contexts/tab-context", () => ({
  useTabStore: (selector: (s: typeof tabs) => unknown) => selector(tabs),
  useTabActions: () => tabs,
}))
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({
    subscribe: async (_event: string, cb: (p: unknown) => void) => {
      capturedHandler = cb
      return () => {}
    },
  }),
}))

import { DeepLinkBootstrap, PetFocusBridge } from "./deep-link-bootstrap"

describe("DeepLinkBootstrap", () => {
  beforeEach(() => {
    catalogEnabled = true
    addFolderToWorkspaceById = vi.fn()
    resetAppWorkspaceStore()
    useAppWorkspaceStore.setState({
      foldersHydrated: true,
      folders: [{ id: 7 }] as never,
      conversations: [{ id: 9, folder_id: 7, agent_type: "codex" }] as never,
      addFolderToWorkspaceById,
    })
    tabs = { tabsHydrated: true, openTab: vi.fn() }
    window.history.replaceState(
      {},
      "",
      "/workspace?folderId=7&conversationId=9&agent=codex"
    )
  })
  afterEach(() => cleanup())

  it("clears a legacy conversation deep link without activating it in catalog mode", async () => {
    render(<DeepLinkBootstrap />)

    await waitFor(() => expect(window.location.search).toBe(""))
    expect(tabs.openTab).not.toHaveBeenCalled()
    expect(addFolderToWorkspaceById).not.toHaveBeenCalled()
  })
})

describe("PetFocusBridge", () => {
  beforeEach(() => {
    capturedHandler = null
    addFolderToWorkspaceById = vi.fn()
    resetAppWorkspaceStore()
    useAppWorkspaceStore.setState({
      foldersHydrated: false,
      folders: [{ id: 7 }] as never,
      addFolderToWorkspaceById,
    })
    tabs = { tabsHydrated: false, openTab: vi.fn() }
    catalogEnabled = false
  })
  afterEach(() => cleanup())

  it("queues a request that arrives before hydration and replays it", async () => {
    const { rerender } = render(<PetFocusBridge />)
    await waitFor(() => expect(capturedHandler).toBeTruthy())

    // Arrives before folders/tabs (and the independently-loading conversations
    // snapshot) are ready — must not be dropped.
    capturedHandler!({ folderId: 7, conversationId: 42, agent: "claude_code" })
    expect(tabs.openTab).not.toHaveBeenCalled()

    // Hydration completes → queued request replays.
    tabs = { ...tabs, tabsHydrated: true }
    rerender(<PetFocusBridge />)
    act(() => {
      useAppWorkspaceStore.setState({ foldersHydrated: true })
    })

    await waitFor(() =>
      expect(tabs.openTab).toHaveBeenCalledWith(7, 42, "claude_code", true)
    )
  })

  it("opens immediately when already hydrated, without re-adding an open folder", async () => {
    useAppWorkspaceStore.setState({ foldersHydrated: true })
    tabs = { ...tabs, tabsHydrated: true }
    render(<PetFocusBridge />)
    await waitFor(() => expect(capturedHandler).toBeTruthy())

    capturedHandler!({ folderId: 7, conversationId: 9, agent: "codex" })
    await waitFor(() =>
      expect(tabs.openTab).toHaveBeenCalledWith(7, 9, "codex", true)
    )
    expect(addFolderToWorkspaceById).not.toHaveBeenCalled()
  })

  it("ignores malformed payloads", async () => {
    useAppWorkspaceStore.setState({ foldersHydrated: true })
    tabs = { ...tabs, tabsHydrated: true }
    render(<PetFocusBridge />)
    await waitFor(() => expect(capturedHandler).toBeTruthy())

    capturedHandler!({ folderId: "x", conversationId: 1, agent: "codex" })
    capturedHandler!({ folderId: 7, conversationId: 1 }) // missing agent
    await Promise.resolve()
    expect(tabs.openTab).not.toHaveBeenCalled()
  })

  it("ignores legacy focus requests in catalog mode", async () => {
    catalogEnabled = true
    useAppWorkspaceStore.setState({ foldersHydrated: true })
    tabs = { ...tabs, tabsHydrated: true }
    render(<PetFocusBridge />)
    await waitFor(() => expect(capturedHandler).toBeTruthy())

    capturedHandler!({ folderId: 7, conversationId: 9, agent: "codex" })
    await Promise.resolve()

    expect(tabs.openTab).not.toHaveBeenCalled()
  })
})
