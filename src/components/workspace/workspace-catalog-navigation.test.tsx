import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  cleanup,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, afterEach, expect, it, vi } from "vitest"
import { useEffect, type ReactNode } from "react"
import en from "@/i18n/messages/en.json"
import {
  WorkspaceCatalogProvider,
  useWorkspaceCatalogContext,
} from "./workspace-catalog-provider"
import { WorkbenchRouteProvider } from "@/contexts/workbench-route-context"
import { TabBar } from "@/components/tabs/tab-bar"
import { resetTabStore, useTabStore } from "@/stores/tab-store"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import type { WorkspaceChatRuntime } from "@/lib/workspace-execution"
import { WorkspaceChatHeader } from "./workspace-chat-header"
import {
  AuxPanelProvider,
  useAuxPanelContext,
} from "@/contexts/aux-panel-context"
import { BranchDropdown } from "@/components/layout/branch-dropdown"
const call = vi.hoisted(() => vi.fn())
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call }),
  isDesktop: () => false,
}))
vi.mock("@/lib/platform", () => ({ isDesktop: () => false }))
const runtime = (id: number) =>
  ({
    draft: {
      id,
      node_id: id,
      title: `Chat ${id}`,
      org_id: 4,
      creator_user_id: 7,
      full_name: "acme/site",
      starting_branch: "main",
      repository_id: 8,
      github_repository_id: "900",
      execution_state: "unprovisioned",
    },
    state: "ready",
    folder: {
      id: id + 10,
      kind: "workspace_chat",
      name: `Chat ${id}`,
      path: `/managed/${id}`,
    },
    conversation: {
      id: id + 20,
      folder_id: id + 10,
      agent_type: "codex",
      status: "pending",
      title: `Chat ${id}`,
    },
    branch_name: `workos/chat/${id}`,
    starting_commit: "abc",
    error_code: null,
  }) as WorkspaceChatRuntime
function Controls() {
  const catalog = useWorkspaceCatalogContext()!
  const remember = catalog.rememberReady
  useEffect(() => {
    if (catalog.scope) {
      remember(runtime(1))
      remember(runtime(2))
    }
  }, [catalog.scope, remember])
  return (
    <>
      <button onClick={() => catalog.openChat(1)}>Open first</button>
      <button onClick={() => catalog.openChat(2)}>Open second</button>
      <button onClick={() => remember(runtime(1))}>
        Refresh first metadata
      </button>
      <p>{catalog.chatId}</p>
    </>
  )
}
function mount(children: ReactNode) {
  return render(
    <NextIntlClientProvider locale="en" messages={en}>
      <WorkspaceCatalogProvider>
        <WorkbenchRouteProvider>{children}</WorkbenchRouteProvider>
      </WorkspaceCatalogProvider>
    </NextIntlClientProvider>
  )
}
beforeEach(() => {
  resetTabStore()
  useAppWorkspaceStore.setState({
    folders: [],
    allFolders: [],
    conversations: [],
  })
  window.history.replaceState({}, "", "/workspace")
  call.mockReset().mockImplementation(async (cmd) =>
    cmd === "list_workspace_nodes"
      ? [1, 2].map((id) => ({
          id,
          org_id: 4,
          parent_id: null,
          kind: "project",
          name: `Project ${id}`,
          repository_default: { repository_id: 8, starting_branch: "main" },
          revision: 1,
        }))
      : []
  )
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation(
      async () =>
        new Response(
          JSON.stringify({
            user: { id: 7, login: "maya", name: "Maya", role: "member" },
            organization: { id: 4, login: "acme", name: "Acme" },
          })
        )
    )
  )
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})
it("does not offer root New chat and never seeds a scratch tab", async () => {
  mount(<TabBar />)
  await waitFor(() =>
    expect(call).toHaveBeenCalledWith("list_workspace_nodes", {})
  )
  expect(screen.queryByRole("button", { name: "New chat" })).toBeNull()
  expect(useTabStore.getState().tabs).toHaveLength(0)
})
it("reopens canonical tabs with back/forward without provisioning or metadata-driven refocus", async () => {
  mount(<Controls />)
  await waitFor(() =>
    expect(call).toHaveBeenCalledWith("list_workspace_nodes", {})
  )
  await userEvent.click(screen.getByText("Open first"))
  await userEvent.click(screen.getByText("Open second"))
  const second = useTabStore.getState().activeTabId
  await userEvent.click(screen.getByText("Refresh first metadata"))
  expect(useTabStore.getState().activeTabId).toBe(second)
  await act(async () => {
    window.history.back()
    await new Promise((resolve) => setTimeout(resolve, 30))
  })
  expect(window.location.search).toBe("?chatId=1")
  expect(
    useTabStore
      .getState()
      .tabs.find((tab) => tab.id === useTabStore.getState().activeTabId)
      ?.conversationId
  ).toBe(21)
  await act(async () => {
    window.history.forward()
    await new Promise((resolve) => setTimeout(resolve, 30))
  })
  expect(useTabStore.getState().activeTabId).toBe(second)
  await userEvent.click(screen.getByText("Open second"))
  expect(useTabStore.getState().tabs).toHaveLength(2)
  expect(
    call.mock.calls.some(([cmd]) => cmd === "provision_workspace_chat")
  ).toBe(false)
})
it("routes an active managed tab's New chat through its logical project", async () => {
  mount(
    <>
      <Controls />
      <TabBar />
    </>
  )
  await waitFor(() =>
    expect(call).toHaveBeenCalledWith("list_workspace_nodes", {})
  )
  await userEvent.click(screen.getByText("Open second"))
  await userEvent.click(screen.getByRole("button", { name: "New chat" }))
  expect(await screen.findByRole("dialog")).toHaveTextContent("Project 2")
  expect(screen.getByLabelText("Chat title")).toBeVisible()
  expect(useTabStore.getState().tabs).toHaveLength(1)
})

it("disables draft-seeding split actions while preserving persisted-tab split moves", async () => {
  mount(
    <>
      <Controls />
      <TabBar />
    </>
  )
  await waitFor(() =>
    expect(call).toHaveBeenCalledWith("list_workspace_nodes", {})
  )
  await userEvent.click(screen.getByText("Open first"))
  await userEvent.click(screen.getByText("Open second"))
  const active = useTabStore.getState().activeTabId!
  fireEvent.contextMenu(screen.getByText("Chat 2"))
  expect(screen.getByRole("menuitem", { name: "Split Right" })).toHaveAttribute(
    "data-disabled"
  )
  expect(
    screen.getByRole("menuitem", { name: "Split and Move Right" })
  ).not.toHaveAttribute("data-disabled")
  act(() => useTabStore.getState().splitTab(active, "right", { move: false }))
  expect(useTabStore.getState().tabs).toHaveLength(2)
  expect(
    useTabStore.getState().tabs.every((tab) => tab.conversationId != null)
  ).toBe(true)
  act(() => useTabStore.getState().splitTab(active, "right", { move: true }))
  expect(new Set(Object.values(useTabStore.getState().groupOf)).size).toBe(2)
})

it("opens the managed worktree in Files with logical identity and no legacy branch switcher", async () => {
  function FilesState() {
    const panel = useAuxPanelContext()
    return (
      <output>{`${panel.isOpen}:${panel.activeTab}:${panel.pendingRevealPath}`}</output>
    )
  }
  mount(
    <AuxPanelProvider>
      <WorkspaceChatHeader runtime={runtime(2)} />
      <BranchDropdown folder={runtime(2).folder!} isChatMode={false} />
      <FilesState />
    </AuxPanelProvider>
  )
  expect(await screen.findByText("Project 2")).toBeVisible()
  expect(screen.getByText("acme/site")).toBeVisible()
  expect(screen.getByText("workos/chat/2")).toBeVisible()
  expect(screen.getByText("Unpublished")).toBeVisible()
  expect(screen.getAllByRole("button")).toHaveLength(1)
  await userEvent.click(screen.getByRole("button", { name: "Files" }))
  expect(screen.getByText("true:file_tree:/managed/2")).toBeVisible()
})

it("hydrates two saved tabs without chatId and preserves the selected second tab", async () => {
  useTabStore.getState().openTab(11, 21, "codex", true, "Chat 1")
  useTabStore.getState().openTab(12, 22, "codex", true, "Chat 2")
  const selected = useTabStore.getState().activeTabId
  mount(<Controls />)
  await waitFor(() => expect(window.location.search).toBe("?chatId=2"))
  expect(useTabStore.getState().activeTabId).toBe(selected)
  await userEvent.click(screen.getByText("Refresh first metadata"))
  expect(window.location.search).toBe("?chatId=2")
  expect(useTabStore.getState().activeTabId).toBe(selected)
  expect(useTabStore.getState().tabs).toHaveLength(2)
})

it("discards ready caches and selection even when identity later returns to its previous org", async () => {
  let orgId = 4
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation(
      async () =>
        new Response(
          JSON.stringify({
            user: { id: 7, login: "maya", name: "Maya", role: "member" },
            organization: { id: orgId, login: "org", name: "Organization" },
          })
        )
    )
  )
  function CacheProbe() {
    const catalog = useWorkspaceCatalogContext()!
    return (
      <>
        <button
          onClick={() => {
            catalog.rememberReady(runtime(1))
            catalog.selectNode(1)
          }}
        >
          Remember
        </button>
        <output>{`${catalog.scope}:${catalog.ready.size}:${catalog.selectedNodeId}`}</output>
      </>
    )
  }
  mount(<CacheProbe />)
  await screen.findByText("4:7:0:null")
  await userEvent.click(screen.getByText("Remember"))
  expect(screen.getByText("4:7:1:1")).toBeVisible()
  orgId = 5
  fireEvent(window, new Event("focus"))
  await screen.findByText("5:7:0:null")
  orgId = 4
  fireEvent(window, new Event("focus"))
  expect(await screen.findByText("4:7:0:null")).toBeVisible()
})
