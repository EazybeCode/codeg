import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { ReactNode } from "react"
import en from "@/i18n/messages/en.json"
import { GitLogTab } from "./aux-panel-git-log-tab"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import type { FolderDetail } from "@/lib/types"

const { call, openCommitDiff, openFilePreview } = vi.hoisted(() => ({
  call: vi.fn(),
  openCommitDiff: vi.fn(),
  openFilePreview: vi.fn(),
}))
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call }),
  isDesktop: () => false,
}))
vi.mock("@/lib/platform", () => ({
  subscribe: async () => () => {},
  isDesktop: () => false,
}))
vi.mock("@/contexts/workspace-context", () => ({
  useWorkspaceActions: () => ({ openCommitDiff, openFilePreview }),
}))
vi.mock("@/contexts/alert-context", () => ({
  useAlertContext: () => ({ pushAlert: vi.fn() }),
}))
vi.mock("@/contexts/task-context", () => ({
  useTaskContext: () => ({
    addTask: vi.fn(),
    updateTask: vi.fn(),
    removeTask: vi.fn(),
  }),
}))
vi.mock("@/contexts/git-credential-context", () => ({
  useGitCredential: () => ({
    withCredentialRetry: (action: () => unknown) => action(),
  }),
}))
vi.mock("@/hooks/use-workspace-state-store", () => ({
  useWorkspaceStateStore: () => ({ isGitRepo: true }),
}))

// Only scroll geometry is substituted: the real Log, Git quick-actions hook,
// active-folder store, Radix menus/dialogs and commit/file controls are rendered.
vi.mock("virtua", () => ({
  Virtualizer: ({
    data,
    children,
  }: {
    data: unknown[]
    children: (item: unknown, index: number) => ReactNode
  }) => <>{data.map(children)}</>,
}))
vi.mock("@/components/ui/scroll-area", () => ({
  ScrollArea: ({
    children,
    onViewportRef,
  }: {
    children: ReactNode
    onViewportRef?: (element: HTMLElement | null) => void
  }) => <div ref={onViewportRef}>{children}</div>,
}))

const commit = {
  hash: "abc1234",
  full_hash: "abc12340000000000000000000000000000000000",
  author: "Maya",
  date: "2026-09-07T00:00:00Z",
  message: "Improve launch copy",
  files: [],
  pushed: false,
}
const logLabels = en.Folder.gitLogTab
const branchLabels = en.Folder.branchDropdown

function mount(kind: FolderDetail["kind"]) {
  const folder = {
    id: 70,
    name: "Website",
    path: "/workspace/site",
    kind,
    is_git: true,
  } as FolderDetail
  useAppWorkspaceStore.setState({
    allFolders: [folder],
    folders: [folder],
    activeFolderId: 70,
  })
  return render(
    <NextIntlClientProvider locale="en" messages={en}>
      <GitLogTab />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  resetAppWorkspaceStore()
  localStorage.clear()
  vi.clearAllMocks()
  call.mockImplementation(async (command) => {
    if (command === "git_log") return { entries: [commit], has_upstream: true }
    if (command === "git_list_all_branches")
      return {
        local: ["main"],
        remote: [],
        worktree_branches: [],
        main_worktree_branch: "main",
      }
    if (command === "get_git_branch") return "main"
    if (command === "git_current_user") return "Maya"
    if (command === "git_commit_branches") return ["main"]
    if (command === "git_commit_files")
      return [{ path: "README.md", status: "M", additions: 2, deletions: 1 }]
    return null
  })
})
afterEach(cleanup)

describe("Git Log managed context", () => {
  it("hides managed mutations while retaining refresh and lazy file diff viewing", async () => {
    mount("workspace_chat")
    const row = await screen.findByText(commit.message)
    expect(
      screen.queryByRole("button", { name: logLabels.moreActions })
    ).toBeNull()
    expect(
      screen.getByRole("button", {
        name: logLabels.branchSelector.refreshCommitHistory,
      })
    ).toBeVisible()
    fireEvent.contextMenu(row)
    for (const name of [
      logLabels.newBranch,
      logLabels.resetToHere,
      branchLabels.pushCode,
      branchLabels.pullCode,
      branchLabels.manageRemotes,
    ]) {
      expect(screen.queryByRole("menuitem", { name })).toBeNull()
    }
    expect(
      call.mock.calls.some(([command]) => command === "git_commit_files")
    ).toBe(false)
    await userEvent.click(row)
    await userEvent.click(await screen.findByText("README.md"))
    expect(openCommitDiff).toHaveBeenCalledWith(commit.full_hash, "README.md")
    expect(call).toHaveBeenCalledWith("git_commit_files", {
      path: "/workspace/site",
      commit: commit.full_hash,
    })
    await userEvent.click(
      screen.getByRole("button", {
        name: logLabels.branchSelector.refreshCommitHistory,
      })
    )
    await waitFor(() =>
      expect(
        call.mock.calls.filter(([command]) => command === "git_log").length
      ).toBeGreaterThan(1)
    )
    expect(
      call.mock.calls.some(([command]) =>
        ["git_new_branch", "git_reset", "git_pull", "git_fetch"].includes(
          command
        )
      )
    ).toBe(false)
  })

  it("retains ordinary folder mutation menus and commit diff viewing", async () => {
    mount("regular")
    const row = await screen.findByText(commit.message)
    await userEvent.click(
      screen.getByRole("button", { name: logLabels.moreActions })
    )
    for (const name of [
      branchLabels.pushCode,
      branchLabels.pullCode,
      branchLabels.fetchRemoteBranches,
      branchLabels.manageRemotes,
    ]) {
      expect(screen.getByRole("menuitem", { name })).toBeVisible()
    }
    await userEvent.keyboard("{Escape}")
    fireEvent.contextMenu(row)
    expect(
      screen.getByRole("menuitem", { name: logLabels.newBranch })
    ).toBeVisible()
    expect(
      screen.getByRole("menuitem", { name: logLabels.resetToHere })
    ).toBeVisible()
    await userEvent.click(
      screen.getByRole("menuitem", { name: en.Folder.common.viewDiff })
    )
    expect(openCommitDiff).toHaveBeenCalledWith(
      commit.full_hash,
      undefined,
      commit.message
    )
  })
})
