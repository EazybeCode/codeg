"use client"

import { useMemo } from "react"
import { useWorkspaceCatalogContext } from "@/components/workspace/workspace-catalog-provider"
import { isCatalogWorkspaceMode } from "@/lib/workspace-catalog-mode"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { groupOfTab, useTabStore, type TabItem } from "@/stores/tab-store"
import type { FolderDetail } from "@/lib/types"
import { leafIds, normalizeTree, type LayoutNode } from "@/lib/tab-group-layout"

export function selectWorkspacePresentationTabs(
  tabs: TabItem[],
  folders: FolderDetail[],
  catalogMode: boolean,
  readyFolderIds: Iterable<number> = []
): TabItem[] {
  if (!catalogMode) return tabs
  const managedFolderIds = new Set(
    folders
      .filter((folder) => folder.kind === "workspace_chat")
      .map((folder) => folder.id)
  )
  for (const folderId of readyFolderIds) managedFolderIds.add(folderId)
  return tabs.filter((tab) => managedFolderIds.has(tab.folderId))
}

export function projectWorkspacePresentationGroups(
  tabs: TabItem[],
  groupLayout: LayoutNode,
  groupOf: Record<string, string>,
  groupSelection: Record<string, string>
): {
  layout: LayoutNode
  selection: Record<string, string>
  sourceLayoutUnchanged: boolean
} {
  const liveGroups = new Set(
    tabs.map((tab) => groupOfTab(groupOf, groupLayout, tab.id))
  )
  const layout = normalizeTree(groupLayout, liveGroups)
  const selection: Record<string, string> = {}
  for (const groupId of leafIds(layout)) {
    const members = tabs.filter(
      (tab) => groupOfTab(groupOf, groupLayout, tab.id) === groupId
    )
    const selected = groupSelection[groupId]
    const visibleSelected = members.find((tab) => tab.id === selected)
    if (visibleSelected) selection[groupId] = visibleSelected.id
    else if (members[0]) selection[groupId] = members[0].id
  }
  return {
    layout,
    selection,
    sourceLayoutUnchanged: layout === groupLayout,
  }
}

/**
 * UI-only tab view. Catalog mode keeps persisted legacy tabs intact while
 * excluding them from rendering and activation surfaces.
 */
export function useWorkspacePresentationTabs(): TabItem[] {
  const catalog = useWorkspaceCatalogContext()
  const tabs = useTabStore((state) => state.tabs)
  const folders = useAppWorkspaceStore((state) => state.allFolders)
  const catalogMode = isCatalogWorkspaceMode(catalog)
  const ready = catalog?.ready
  const readyFolderIds = useMemo(
    () =>
      ready
        ? [...ready.values()].flatMap((runtime) =>
            runtime.folder ? [runtime.folder.id] : []
          )
        : [],
    [ready]
  )
  return useMemo(
    () =>
      selectWorkspacePresentationTabs(
        tabs,
        folders,
        catalogMode,
        readyFolderIds
      ),
    [tabs, folders, catalogMode, readyFolderIds]
  )
}

export function useWorkspacePresentationGroups(tabs: TabItem[]): {
  layout: LayoutNode
  selection: Record<string, string>
  sourceLayoutUnchanged: boolean
} {
  const groupLayout = useTabStore((state) => state.groupLayout)
  const groupOf = useTabStore((state) => state.groupOf)
  const groupSelection = useTabStore((state) => state.groupSelection)
  return useMemo(
    () =>
      projectWorkspacePresentationGroups(
        tabs,
        groupLayout,
        groupOf,
        groupSelection
      ),
    [tabs, groupLayout, groupOf, groupSelection]
  )
}
