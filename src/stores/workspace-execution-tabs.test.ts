import { beforeEach, expect, it, vi } from "vitest"
import { excludeChatFolders } from "@/lib/folder-display"
import type { FolderDetail } from "@/lib/types"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "./app-workspace-store"
import { resetTabStore, useTabStore } from "./tab-store"

vi.mock("@/lib/api", () => ({
  listOpenedTabs: vi.fn(),
  saveOpenedTabs: vi.fn(),
  getFolderConversation: vi.fn(),
}))
vi.mock("@/lib/platform", () => ({
  subscribe: vi.fn(),
  onTransportReconnect: vi.fn(),
}))

const regular: FolderDetail = {
  id: 1,
  name: "repo",
  path: "/repo",
  git_branch: "main",
  default_agent_type: null,
  last_opened_at: "",
  sort_order: 0,
  color: "inherit",
  parent_id: null,
  kind: "regular",
  alias: null,
  group_id: null,
  repo_full_name: null,
}
const workspaceChat = {
  ...regular,
  id: 2,
  path: "/isolated/worktree",
  kind: "workspace_chat",
} as FolderDetail

beforeEach(() => {
  resetTabStore()
  resetAppWorkspaceStore()
})

it("hides execution folders while preserving regular folders", () => {
  expect(excludeChatFolders([regular, workspaceChat])).toEqual([regular])
})

it("upserts a ready execution into allFolders without exposing a legacy folder", () => {
  useAppWorkspaceStore.getState().upsertFolder(regular)
  useAppWorkspaceStore.getState().upsertFolder(workspaceChat)
  expect(useAppWorkspaceStore.getState().allFolders).toEqual([
    regular,
    workspaceChat,
  ])
  expect(useAppWorkspaceStore.getState().folders).toEqual([regular])
})

it.each([true, false])(
  "closing the last managed tab never seeds its worktree (fallback %s)",
  (withRegular) => {
    useAppWorkspaceStore.setState({
      folders: withRegular ? [regular] : [],
      allFolders: [regular, workspaceChat],
    })
    useTabStore.setState({
      rawTabs: [
        {
          id: "managed",
          kind: "conversation",
          folderId: 2,
          conversationId: 42,
          agentType: "claude_code",
          title: "Managed",
          isPinned: true,
          workingDir: workspaceChat.path,
        },
      ],
      activeTabId: "managed",
    })
    useTabStore.getState().closeTab("managed")
    if (!withRegular) {
      expect(useTabStore.getState().rawTabs).toEqual([])
      return
    }
    const [replacement] = useTabStore.getState().rawTabs
    expect(replacement.conversationId).toBeNull()
    expect(replacement.folderId).toBe(withRegular ? 1 : 0)
    expect(replacement.workingDir).toBe(withRegular ? "/repo" : "")
  }
)
