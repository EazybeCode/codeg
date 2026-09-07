"use client"

import { ConversationDetailPanel } from "@/components/conversations/conversation-detail-panel"
import { useWorkspaceCatalogContext } from "@/components/workspace/workspace-catalog-provider"
import { WorkspaceChatLauncher } from "@/components/workspace/workspace-chat-launcher"
import { WorkspaceCatalogWelcome } from "@/components/workspace/workspace-catalog-welcome"
import { useTabStore } from "@/stores/tab-store"
import { useWorkspacePresentationTabs } from "@/hooks/use-workspace-presentation-tabs"

export default function WorkspacePage() {
  const catalog = useWorkspaceCatalogContext()
  const tabs = useWorkspacePresentationTabs()
  const activeTabId = useTabStore((state) => state.activeTabId)
  if (catalog?.enabled) {
    if (!catalog.scope) return <WorkspaceCatalogWelcome />
    if (catalog.chatId != null && !catalog.ready.has(catalog.chatId))
      return (
        <WorkspaceChatLauncher
          key={`${catalog.scope}:${catalog.chatId}`}
          draftId={catalog.chatId}
        />
      )
    if (!tabs.some((tab) => tab.id === activeTabId))
      return <WorkspaceCatalogWelcome />
  }
  return <ConversationDetailPanel />
}
