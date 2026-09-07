"use client"

import { ConversationDetailPanel } from "@/components/conversations/conversation-detail-panel"
import { useWorkspaceCatalogContext } from "@/components/workspace/workspace-catalog-provider"
import { WorkspaceChatLauncher } from "@/components/workspace/workspace-chat-launcher"
import { WorkspaceCatalogWelcome } from "@/components/workspace/workspace-catalog-welcome"
import { useTabStore } from "@/stores/tab-store"

export default function WorkspacePage() {
  const catalog = useWorkspaceCatalogContext()
  const tabs = useTabStore((state) => state.tabs)
  if (catalog?.enabled) {
    if (!catalog.scope) return <WorkspaceCatalogWelcome />
    if (catalog.chatId != null && !catalog.ready.has(catalog.chatId))
      return (
        <WorkspaceChatLauncher
          key={`${catalog.scope}:${catalog.chatId}`}
          draftId={catalog.chatId}
        />
      )
    if (!tabs.length) return <WorkspaceCatalogWelcome />
  }
  return <ConversationDetailPanel />
}
