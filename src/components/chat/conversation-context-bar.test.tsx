import { render, screen, cleanup } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import { ConversationHeaderFolderPicker } from "./conversation-context-bar"
import type { FolderDetail } from "@/lib/types"
import { getFolderRepoContext } from "@/lib/api"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"

// ---------------------------------------------------------------------------
// Mocks. The header folder picker reads the tab store + tab actions and renders
// the shared FolderPicker (cmdk); the folder-display helpers run for real.
// ---------------------------------------------------------------------------

const openNewConversationTab = vi.fn()

vi.mock("@/lib/api", () => ({
  getFolderRepoContext: vi.fn(),
  getGitHead: vi.fn(async () => ({
    is_repo: true,
    branch: "main",
    detached: false,
    short_sha: "abc123",
  })),
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}))

vi.mock("@/components/layout/branch-dropdown", () => ({
  BranchDropdown: ({ folder }: { folder: FolderDetail | null }) =>
    folder ? (
      <button aria-label={`Branch for ${folder.id}`}>main</button>
    ) : null,
}))

// Tab state, mutated per test before render. Workspace state (folders /
// branches) is seeded into the real zustand store in beforeEach.
let tabs: Array<{
  id: string
  folderId: number
  conversationId: number | null
  isChat?: boolean
}> = []
let activeTabId: string | null = null

vi.mock("@/contexts/tab-context", () => ({
  useTabStore: (
    selector: (s: {
      tabs: typeof tabs
      activeTabId: typeof activeTabId
    }) => unknown
  ) => selector({ tabs, activeTabId }),
  useTabActions: () => ({
    openNewConversationTab,
    openChatModeTab: vi.fn(),
  }),
}))

function mkFolder(p: Partial<FolderDetail> & { id: number }): FolderDetail {
  return {
    name: `folder-${p.id}`,
    path: `/repo/folder-${p.id}`,
    git_branch: null,
    default_agent_type: null,
    last_opened_at: "2026-01-01T00:00:00Z",
    sort_order: p.id,
    color: "blue",
    parent_id: null,
    kind: "regular",
    alias: null,
    group_id: null,
    repo_full_name: null,
    ...p,
  }
}

const repo = mkFolder({
  id: 1,
  name: "repo",
  path: "/repo",
  git_branch: "main",
})

beforeEach(() => {
  openNewConversationTab.mockClear()
  vi.mocked(getFolderRepoContext).mockResolvedValue({
    folder_id: 1,
    project_folder_id: 1,
    repo_folder_id: 1,
    repo_full_name: "acme/repo",
    branch: "main",
    relative_path: "",
    is_inherited: false,
    git_root: "/repo",
  })
  resetAppWorkspaceStore()
  useAppWorkspaceStore.setState({
    folders: [repo],
    allFolders: [repo],
    branches: new Map([[1, "main"]]),
  })
})

afterEach(() => cleanup())

// The conversation header renders the owning folder as a STATIC breadcrumb —
// folder (and chat-mode) switching moved to the below-composer picker row, so
// the header never opens a popover, even for a draft. `next-intl` is mocked to
// echo keys, so a translated label like the chat-mode item reads back as its key.
describe("ConversationHeaderFolderPicker", () => {
  it("shows nested ancestry while targeting the enclosing repo for Git", async () => {
    vi.mocked(getFolderRepoContext).mockResolvedValue({
      folder_id: 3,
      project_folder_id: 1,
      repo_folder_id: 1,
      repo_full_name: "acme/repo",
      branch: "main",
      relative_path: "docs/api",
      is_inherited: true,
      git_root: "/repo",
    })
    const docs = mkFolder({
      id: 2,
      name: "docs",
      parent_id: 1,
      kind: "subfolder",
    })
    const api = mkFolder({
      id: 3,
      name: "api",
      alias: "API",
      parent_id: 2,
      kind: "subfolder",
    })
    useAppWorkspaceStore.setState({
      folders: [repo, docs, api],
      allFolders: [repo, docs, api],
    })
    tabs = [{ id: "nested", folderId: 3, conversationId: null }]
    activeTabId = "nested"
    render(<ConversationHeaderFolderPicker tabId="nested" />)
    const breadcrumb = screen.getByRole("navigation", { name: "Folder path" })
    expect(breadcrumb.textContent).toContain("repo")
    expect(breadcrumb.textContent).toContain("docs")
    expect(screen.getByRole("button", { name: /API/ })).toBeTruthy()
    expect(
      await screen.findByRole("button", { name: "Branch for 1" })
    ).toBeTruthy()
    expect(openNewConversationTab).not.toHaveBeenCalled()
  })

  it("targets a nested repository override instead of its logical parent", async () => {
    vi.mocked(getFolderRepoContext).mockResolvedValue({
      folder_id: 2,
      project_folder_id: 1,
      repo_folder_id: 2,
      repo_full_name: "acme/wiki",
      branch: "main",
      relative_path: "",
      is_inherited: false,
      git_root: "/repo/folder-2",
    })
    const nested = mkFolder({
      id: 2,
      name: "wiki",
      parent_id: 1,
      kind: "subfolder",
      repo_full_name: "acme/wiki",
    })
    useAppWorkspaceStore.setState({
      folders: [repo, nested],
      allFolders: [repo, nested],
    })
    tabs = [{ id: "nested", folderId: 2, conversationId: null }]
    activeTabId = "nested"
    render(<ConversationHeaderFolderPicker tabId="nested" />)
    expect(
      await screen.findByRole("button", { name: "Branch for 2" })
    ).toBeTruthy()
  })
  it("renders a draft's folder name as a static (non-switchable) breadcrumb", async () => {
    const other = mkFolder({ id: 2, name: "other-repo", path: "/repo/other" })
    useAppWorkspaceStore.setState({
      folders: [repo, other],
      allFolders: [repo, other],
    })
    tabs = [{ id: "tab-draft", folderId: 1, conversationId: null }]
    activeTabId = "tab-draft"

    const user = userEvent.setup()
    render(<ConversationHeaderFolderPicker tabId="tab-draft" />)
    // Even a draft is static now: clicking the label opens no folder list, so
    // the other repo is unreachable and no switch fires.
    await user.click(screen.getByRole("button", { name: /repo/ }))
    expect(screen.queryByText("other-repo")).toBeNull()
    expect(openNewConversationTab).not.toHaveBeenCalled()
  })

  it("renders a static (non-switchable) chip for an existing conversation", async () => {
    const other = mkFolder({ id: 2, name: "other-repo", path: "/repo/other" })
    useAppWorkspaceStore.setState({
      folders: [repo, other],
      allFolders: [repo, other],
    })
    tabs = [{ id: "tab-1", folderId: 1, conversationId: 42 }]
    activeTabId = "tab-1"

    const user = userEvent.setup()
    render(<ConversationHeaderFolderPicker tabId="tab-1" />)
    await user.click(screen.getByRole("button", { name: /repo/ }))
    // Non-editable: clicking opens no folder list, so the other repo is
    // unreachable and no switch fires.
    expect(screen.queryByText("other-repo")).toBeNull()
    expect(openNewConversationTab).not.toHaveBeenCalled()
  })

  it("shows the chat-mode label for a folderless chat tab", () => {
    tabs = [
      { id: "tab-chat", folderId: 999, conversationId: null, isChat: true },
    ]
    activeTabId = "tab-chat"

    render(<ConversationHeaderFolderPicker tabId="tab-chat" />)
    expect(screen.getByRole("button", { name: /chatModeLabel/ })).toBeTruthy()
  })
})
