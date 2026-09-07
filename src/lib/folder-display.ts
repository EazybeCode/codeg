import type { FolderDetail } from "@/lib/types"

/**
 * The name to show for a folder in the branch selector and the input-box folder
 * chip. For a worktree folder (`parent_id != null`) we surface the *root repo*
 * folder's name instead of the worktree directory's own name — the worktree dir
 * is typically named after its branch (e.g. `myproject-feature-x`), so showing
 * the repo name (`myproject`) alongside the branch label reads cleaner.
 *
 * Display-only: callers keep using the folder's real `path`/`id` for every
 * filesystem and git operation. Falls back to the folder's own name when the
 * parent is not present in `folders` (e.g. closed/removed), so the label never
 * blanks out.
 */
export function resolveFolderDisplayName(
  folder: Pick<FolderDetail, "name" | "parent_id" | "kind">,
  folders: readonly Pick<FolderDetail, "id" | "name">[]
): string {
  // `parent_id` carries two different relationships in legacy data: runtime
  // worktrees point at their repository, while organizational subfolders point
  // at their logical parent. Only the former should borrow the parent's label.
  if (folder.parent_id == null || folder.kind === "subfolder")
    return folder.name
  const parent = folders.find((f) => f.id === folder.parent_id)
  return parent?.name ?? folder.name
}

export interface FolderBreadcrumbItem {
  id: number
  /** Alias when present, otherwise the real folder name. */
  label: string
  /** Retained for an informative tooltip when an alias masks the real name. */
  name: string
}

/**
 * Resolve a folder's logical ancestry without touching its runtime path/id.
 * Missing parents produce the useful known suffix; cycles and corrupt chains
 * are bounded so workspace chrome can never hang on malformed legacy data.
 */
export function resolveFolderBreadcrumb(
  folderId: number,
  folders: readonly Pick<FolderDetail, "id" | "name" | "alias" | "parent_id">[],
  maxDepth = 64
): FolderBreadcrumbItem[] {
  const byId = new Map(folders.map((folder) => [folder.id, folder]))
  const seen = new Set<number>()
  const reversed: FolderBreadcrumbItem[] = []
  let currentId: number | null = folderId

  while (
    currentId != null &&
    reversed.length < maxDepth &&
    !seen.has(currentId)
  ) {
    const folder = byId.get(currentId)
    if (!folder) break
    seen.add(currentId)
    const alias = folder.alias?.trim()
    reversed.push({
      id: folder.id,
      label: alias || folder.name,
      name: folder.name,
    })
    currentId = folder.parent_id
  }

  return reversed.reverse()
}

/**
 * Best-effort Git root while the authoritative repo-context endpoint is
 * loading or unavailable. Runtime worktrees are real Git roots in their own
 * right. Organizational subfolders inherit upward unless they carry an
 * explicit repository override (`repo_full_name`).
 */
export function resolveKnownRepoFolder<
  T extends Pick<FolderDetail, "id" | "parent_id" | "kind" | "repo_full_name">,
>(folder: T, folders: readonly T[], maxDepth = 64): T | null {
  if (folder.kind !== "subfolder" || folder.repo_full_name) return folder

  const byId = new Map(folders.map((entry) => [entry.id, entry]))
  const seen = new Set<number>([folder.id])
  let currentId = folder.parent_id
  let depth = 0

  while (currentId != null && depth < maxDepth && !seen.has(currentId)) {
    const current = byId.get(currentId)
    if (!current) return null
    if (current.kind !== "subfolder" || current.repo_full_name) return current
    seen.add(current.id)
    currentId = current.parent_id
    depth++
  }

  return null
}

/**
 * Plain-string folder label for `title` tooltips and other string-only contexts:
 * `alias [ name ]` (e.g. `My Project [ codeg ]`) when an alias is set, else the
 * bare `name`. The rendered (color-distinguished) equivalent is the
 * `FolderAliasLabel` component — keep the `alias [ name ]` spacing in sync.
 */
export function formatFolderLabelWithAlias(
  folder: Pick<FolderDetail, "name" | "alias">
): string {
  const alias = folder.alias?.trim()
  return alias ? `${alias} [ ${folder.name} ]` : folder.name
}

/**
 * The folders the input-box folder picker should list: top-level repos only.
 * Worktree folders (`parent_id != null`) are reached via the branch picker, so
 * listing them here too would be redundant and confusing. Display-only — the
 * full folder list still backs every actual switch/lookup.
 */
export function filterTopLevelFolders<
  T extends Pick<FolderDetail, "parent_id">,
>(folders: readonly T[]): T[] {
  return folders.filter((f) => f.parent_id == null)
}

/**
 * Drop hidden scratch and workspace-chat folders from a folder list. These
 * back folderless "chat mode" conversations and must never appear in
 * user-facing folder surfaces (the sidebar "文件夹" group, the input-box folder
 * picker). They stay in the full `allFolders` set so by-id lookups (cwd,
 * active-folder, theme color) keep resolving — only list rendering excludes
 * them.
 */
export function excludeChatFolders<T extends Pick<FolderDetail, "kind">>(
  folders: readonly T[]
): T[] {
  return folders.filter((f) => f.kind !== "chat" && f.kind !== "workspace_chat")
}

/**
 * The folder id the input-box picker highlights for a conversation's folder:
 * the parent repo for a worktree (since the worktree itself isn't listed), or
 * the folder itself for a top-level repo. Display-only — never used for the
 * conversation's real working directory.
 */
export function resolvePickerSelectedFolderId(
  folder: Pick<FolderDetail, "id" | "parent_id"> &
    Partial<Pick<FolderDetail, "kind">>
): number {
  return folder.kind === "subfolder"
    ? folder.id
    : (folder.parent_id ?? folder.id)
}
