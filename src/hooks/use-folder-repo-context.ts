import { useEffect, useState } from "react"
import { getFolderRepoContext, getGitHead } from "@/lib/api"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import type { FolderDetail, FolderRepoContext } from "@/lib/types"

type Resolution = {
  key: string
  context: FolderRepoContext | null
  unavailable: string | null
}

/** Git mutations require a validated physical checkout, never guessed ancestry. */
export function useFolderRepoContext(
  folder: FolderDetail | null,
  folders: readonly FolderDetail[],
  active = true
) {
  const [resolution, setResolution] = useState<Resolution | null>(null)
  const folderId = folder?.id
  const key = folder ? `${folder.id}:${folder.path}` : ""
  useEffect(() => {
    if (folderId == null) return
    let current = true
    getFolderRepoContext(folderId)
      .then((context) => {
        if (!current) return
        setResolution({
          key,
          context: context ?? null,
          unavailable: context ? null : "Repository context unavailable",
        })
      })
      .catch((error: unknown) => {
        if (!current) return
        const message =
          error &&
          typeof error === "object" &&
          "message" in error &&
          typeof error.message === "string"
            ? error.message
            : "Repository context unavailable"
        setResolution({ key, context: null, unavailable: message })
      })
    return () => {
      current = false
    }
  }, [folderId, key])

  const current = resolution?.key === key ? resolution : null
  const context = current?.context
  const root = context
    ? folders.find((entry) => entry.id === context.repo_folder_id)
    : null
  const resolved =
    folder && root && context
      ? {
          ...root,
          path: context.git_root ?? root.path,
          repo_full_name: context.repo_full_name,
        }
      : null
  const rootId = resolved?.id
  const rootPath = context?.git_root

  // The workspace's existing poll owns the active logical folder's cache entry.
  // When Git operations use an ancestor ID, refresh that separate entry too.
  useEffect(() => {
    if (!active || rootId == null || !rootPath || rootId === folderId) return
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const poll = async () => {
      try {
        const head = await getGitHead(rootPath)
        if (!cancelled)
          useAppWorkspaceStore.getState().applyGitHead(rootId, head)
      } catch {
        // Keep last-known HEAD through a transient failure; retry while active.
      } finally {
        if (!cancelled) timer = setTimeout(poll, 10_000)
      }
    }
    void poll()
    return () => {
      cancelled = true
      if (timer) clearTimeout(timer)
    }
  }, [active, folderId, rootId, rootPath])

  return {
    folder: resolved,
    unavailable:
      current?.unavailable ??
      (context && !root ? "Repository root is not in this workspace" : null),
    loading: !!folder && !current,
  }
}
