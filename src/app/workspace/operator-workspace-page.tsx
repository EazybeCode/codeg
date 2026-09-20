"use client"

// The operator workspace surface. Split out of `page.tsx` for the same
// reason the operator layout is: Next ships a route's page module in that
// route's initial script whether or not the layout ever renders `children`,
// so native `/workspace` was downloading the conversation panel and its
// operator graph to render `NativeWorkspace` instead.

import { ConversationDetailPanel } from "@/components/conversations/conversation-detail-panel"
import { useWorkspaceCatalogContext } from "@/components/workspace/workspace-catalog-provider"
import { WorkspaceChatLauncher } from "@/components/workspace/workspace-chat-launcher"
import { WorkspaceCatalogWelcome } from "@/components/workspace/workspace-catalog-welcome"
import { useTabStore } from "@/stores/tab-store"
import { useWorkspacePresentationTabs } from "@/hooks/use-workspace-presentation-tabs"

export default function OperatorWorkspacePage() {
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
