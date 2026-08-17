"use client"

import { ConversationDetailPanel } from "@/components/conversations/conversation-detail-panel"
import { PkArenaHost } from "@/components/pk/pk-arena-host"

export default function WorkspacePage() {
  return (
    <>
      <ConversationDetailPanel />
      {/* Inside the workspace layout's AcpConnectionsProvider — the arena
          orchestrator needs the connection actions and the acp://event
          subscription. */}
      <PkArenaHost />
    </>
  )
}
