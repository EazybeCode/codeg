import { renderHook, waitFor, cleanup, act } from "@testing-library/react"
import { afterEach, describe, expect, it, vi } from "vitest"
import { getFolderRepoContext, getGitHead } from "@/lib/api"
import {
  useAppWorkspaceStore,
  resetAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import { useFolderRepoContext } from "./use-folder-repo-context"
import type { FolderDetail } from "@/lib/types"

vi.mock("@/lib/api", () => ({
  getFolderRepoContext: vi.fn(),
  getGitHead: vi.fn(),
}))
afterEach(() => {
  cleanup()
  vi.resetAllMocks()
  vi.useRealTimers()
  resetAppWorkspaceStore()
})

const repo: FolderDetail = {
  id: 1,
  name: "docs",
  path: "/repo",
  parent_id: null,
  kind: "regular",
  git_branch: "main",
  default_agent_type: null,
  last_opened_at: "2026-09-06T00:00:00Z",
  sort_order: 0,
  color: "blue",
  alias: null,
  group_id: null,
  repo_full_name: "acme/docs",
}
const child: FolderDetail = {
  ...repo,
  id: 2,
  name: "API",
  path: "/repo/API",
  parent_id: 1,
  kind: "subfolder",
  repo_full_name: null,
}

describe("repository context failures", () => {
  it("disables Git actions when the backend confirms missing project files", async () => {
    vi.mocked(getFolderRepoContext).mockRejectedValue({
      code: "io_error",
      message: "Project directory is missing",
    })
    const { result } = renderHook(() =>
      useFolderRepoContext(child, [repo, child])
    )
    await waitFor(() =>
      expect(result.current.unavailable).toBe("Project directory is missing")
    )
    expect(result.current.folder).toBeNull()
  })

  it("does not enable heuristic Git actions on an older server", async () => {
    vi.mocked(getFolderRepoContext).mockRejectedValue({
      code: "network_error",
      message: "HTTP 404",
    })
    const { result } = renderHook(() =>
      useFolderRepoContext(child, [repo, child])
    )
    await waitFor(() => expect(result.current.unavailable).toBeTruthy())
    expect(result.current.folder).toBeNull()
  })

  it("keeps Git actions disabled until the physical checkout is resolved", () => {
    vi.mocked(getFolderRepoContext).mockReturnValue(new Promise(() => {}))
    const { result } = renderHook(() =>
      useFolderRepoContext(child, [repo, child])
    )
    expect(result.current.folder).toBeNull()
    expect(result.current.loading).toBe(true)
  })

  it("refreshes the resolved repository HEAD while a nested folder stays active", async () => {
    vi.useFakeTimers()
    vi.mocked(getFolderRepoContext).mockResolvedValue({
      folder_id: 2,
      project_folder_id: 1,
      repo_folder_id: 1,
      repo_full_name: "acme/docs",
      branch: "main",
      relative_path: "API",
      is_inherited: true,
      git_root: "/repo",
    })
    vi.mocked(getGitHead)
      .mockResolvedValueOnce({
        is_repo: true,
        branch: "main",
        detached: false,
        short_sha: "abc123",
      })
      .mockResolvedValue({
        is_repo: true,
        branch: "feature",
        detached: false,
        short_sha: "def456",
      })
    await act(async () => {
      renderHook(() => useFolderRepoContext(child, [repo, child]))
    })
    expect(useAppWorkspaceStore.getState().branches.get(1)).toBe("main")
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000)
    })
    expect(useAppWorkspaceStore.getState().branches.get(1)).toBe("feature")
  })
})
