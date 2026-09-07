/** Typed mode check shared by presentation-only catalog guards. */
export function isCatalogWorkspaceMode(
  catalog: { enabled: boolean } | null | undefined
): boolean {
  return catalog?.enabled === true
}

export function canRestoreCatalogConversation(
  catalogMode: boolean,
  managedFolder: boolean,
  conversationId: number | null
): boolean {
  return !catalogMode || (managedFolder && conversationId != null)
}
