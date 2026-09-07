import { act, render, screen, waitFor, cleanup } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, afterEach, it, expect, vi } from "vitest"
import en from "@/i18n/messages/en.json"
import { WorkspaceCatalogProvider } from "./workspace-catalog-provider"
import { WorkspaceChatLauncher } from "./workspace-chat-launcher"
import { useTabStore } from "@/stores/tab-store"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
const { call } = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call }),
  isDesktop: () => false,
}))
vi.mock("@/lib/platform", () => ({ isDesktop: () => false }))
const draft = {
  id: 20,
  org_id: 4,
  creator_user_id: 7,
  node_id: 1,
  repository_id: 8,
  github_repository_id: "900",
  full_name: "acme/site",
  starting_branch: "main",
  title: "Launch",
  execution_state: "unprovisioned",
}
const ready = {
  draft,
  state: "ready",
  folder: {
    id: 70,
    kind: "workspace_chat",
    name: "Launch",
    path: "/managed/20/worktree",
    is_git: true,
  },
  conversation: {
    id: 90,
    folder_id: 70,
    agent_type: "codex",
    title: "Launch",
    status: "pending",
  },
  branch_name: "workos/chat/20",
  starting_commit: "abc",
  error_code: null,
}
const pending = {
  ...ready,
  state: "unprovisioned",
  folder: null,
  conversation: null,
  branch_name: null,
  starting_commit: null,
}
function mount() {
  return render(
    <NextIntlClientProvider locale="en" messages={en}>
      <WorkspaceCatalogProvider>
        <WorkspaceChatLauncher draftId={20} />
      </WorkspaceCatalogProvider>
    </NextIntlClientProvider>
  )
}
beforeEach(() => {
  useTabStore.setState({ rawTabs: [], tabs: [], activeTabId: null })
  useAppWorkspaceStore.setState({
    folders: [],
    allFolders: [],
    conversations: [],
  })
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          user: { id: 7, login: "maya", name: "Maya", role: "member" },
          organization: { id: 4, login: "acme", name: "Acme" },
        })
      )
    )
  )
  call.mockReset().mockImplementation(async (cmd) => {
    if (cmd === "get_workspace_chat_runtime") return pending
    if (cmd === "get_workspace_runtime_capabilities")
      return { execution_enabled: true }
    if (cmd === "provision_workspace_chat") return ready
    if (cmd === "acp_list_agents")
      return [
        {
          agent_type: "codex",
          name: "Codex",
          enabled: true,
          available: true,
          installed: true,
        },
      ]
    return []
  })
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})
it("opens canonical ready IDs without provisioning on view and never duplicates tabs", async () => {
  const normal = call.getMockImplementation()!
  call.mockImplementation(async (cmd, body) =>
    cmd === "get_workspace_chat_runtime" ? ready : normal(cmd, body)
  )
  const view = mount()
  await waitFor(() => expect(useTabStore.getState().tabs).toHaveLength(1))
  expect(useTabStore.getState().tabs[0]).toMatchObject({
    folderId: 70,
    conversationId: 90,
    agentType: "codex",
  })
  expect(
    call.mock.calls.some(
      ([cmd]) => cmd === "provision_workspace_chat" || cmd === "acp_connect"
    )
  ).toBe(false)
  view.unmount()
  mount()
  await waitFor(() => expect(useTabStore.getState().tabs).toHaveLength(1))
  expect(useAppWorkspaceStore.getState().allFolders[0].id).toBe(70)
})
it("keeps disabled execution informative with no impossible action", async () => {
  const normal = call.getMockImplementation()!
  call.mockImplementation(async (cmd, body) =>
    cmd === "get_workspace_runtime_capabilities"
      ? { execution_enabled: false }
      : normal(cmd, body)
  )
  mount()
  expect(
    await screen.findByText("Chat execution is not enabled on this server.")
  ).toBeVisible()
  expect(screen.queryByRole("button", { name: "Prepare chat" })).toBeNull()
  expect(useTabStore.getState().tabs).toHaveLength(0)
})
it("retains a retryable runtime error without mounting a conversation", async () => {
  const normal = call.getMockImplementation()!
  let failed = false
  call.mockImplementation(async (cmd, body) => {
    if (cmd === "get_workspace_chat_runtime" && !failed) {
      failed = true
      throw { error: "upstream_unavailable" }
    }
    return normal(cmd, body)
  })
  mount()
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }))
  expect(
    await screen.findByRole("button", { name: "Prepare chat" })
  ).toBeVisible()
  expect(useTabStore.getState().tabs).toHaveLength(0)
})
it("ignores a preparation response after the launcher is unmounted", async () => {
  let resolve!: (value: typeof ready) => void
  const response = new Promise<typeof ready>((done) => {
    resolve = done
  })
  const normal = call.getMockImplementation()!
  call.mockImplementation((cmd, body) =>
    cmd === "provision_workspace_chat" ? response : normal(cmd, body)
  )
  const view = mount()
  await userEvent.click(
    await screen.findByRole("button", { name: "Prepare chat" })
  )
  view.unmount()
  await act(async () => {
    resolve(ready)
    await response
  })
  expect(useTabStore.getState().tabs).toHaveLength(0)
  expect(useAppWorkspaceStore.getState().allFolders).toHaveLength(0)
})
it("provisions only on explicit preparation and upserts returned IDs", async () => {
  mount()
  await userEvent.click(
    await screen.findByRole("button", { name: "Prepare chat" })
  )
  await waitFor(() => expect(useTabStore.getState().tabs).toHaveLength(1))
  expect(call).toHaveBeenCalledWith("provision_workspace_chat", {
    draftId: 20,
    agentType: "codex",
  })
})
it("resumes an interrupted reservation with its original agent after reload", async () => {
  const normal = call.getMockImplementation()!
  call.mockImplementation(async (cmd, body) =>
    cmd === "get_workspace_chat_runtime"
      ? {
          ...pending,
          state: "provisioning",
          reserved_agent_type: "claude_code",
          provisioning_retryable: true,
        }
      : normal(cmd, body)
  )
  mount()
  await userEvent.click(
    await screen.findByRole("button", { name: "Prepare chat" })
  )
  await waitFor(() => expect(useTabStore.getState().tabs).toHaveLength(1))
  expect(call).toHaveBeenCalledWith("provision_workspace_chat", {
    draftId: 20,
    agentType: "claude_code",
  })
})
it("retries failed preparation rather than just reloading metadata", async () => {
  const normal = call.getMockImplementation()!
  let attempts = 0
  call.mockImplementation(async (cmd, body) => {
    if (cmd === "provision_workspace_chat" && attempts++ === 0)
      throw { error: "busy" }
    return normal(cmd, body)
  })
  mount()
  await userEvent.click(
    await screen.findByRole("button", { name: "Prepare chat" })
  )
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }))
  await waitFor(() => expect(useTabStore.getState().tabs).toHaveLength(1))
})
