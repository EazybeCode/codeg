import type { UseConnectionLifecycleReturn } from "./use-connection-lifecycle"
import { useCallback, useRef } from "react"
import type { PromptDraft, MessageTurn, ContentBlock } from "@/lib/types"
import { useConversationRuntimeActions } from "@/stores/conversation-runtime-store"
import {
  getPromptDraftDisplayText,
  extractUserImagesFromDraft,
} from "@/lib/prompt-draft"
import { randomUUID } from "@/lib/utils"

interface Options {
  workspaceChatId?: number
  conversationId: number | null
  folderId: number
  send: UseConnectionLifecycleReturn["handleSend"]
  attachedResourcesFallback: string
}
export function buildOptimisticUserTurnFromDraft(
  draft: PromptDraft,
  fallback: string
): MessageTurn {
  const blocks: ContentBlock[] = extractUserImagesFromDraft(draft).map(
    (image) => ({
      type: "image",
      data: image.data,
      mime_type: image.mime_type,
      uri: image.uri ?? null,
    })
  )
  blocks.push({
    type: "text",
    text: getPromptDraftDisplayText(draft, fallback),
  })
  return {
    id: `optimistic-${randomUUID()}`,
    role: "user",
    blocks,
    timestamp: new Date().toISOString(),
  }
}

/** Both the panel's send gate and its disconnected composer use eligibility
 * here. Managed chats already have persisted IDs before the first Send. */
export function useWorkspaceChatSend({
  workspaceChatId,
  conversationId,
  folderId,
  send,
  attachedResourcesFallback,
}: Options) {
  const pending = useRef(false)
  const eligible =
    workspaceChatId != null &&
    workspaceChatId > 0 &&
    conversationId != null &&
    conversationId > 0 &&
    folderId > 0
  const { appendOptimisticTurn, removeOptimisticTurn, setSyncState } =
    useConversationRuntimeActions()
  const sendDraft = useCallback(
    async (
      draft: PromptDraft,
      mode?: string | null,
      onBusy?: () => void
    ): Promise<boolean> => {
      if (!eligible || conversationId == null || pending.current) return false
      pending.current = true
      const turn = buildOptimisticUserTurnFromDraft(
        draft,
        attachedResourcesFallback
      )
      appendOptimisticTurn(conversationId, turn, turn.id)
      setSyncState(conversationId, "awaiting_persist")
      const rollback = () => removeOptimisticTurn(conversationId, turn.id)
      try {
        const sent = await send(draft, mode, {
          folderId,
          conversationId,
          clientMessageId: turn.id,
          onSendFailed: rollback,
          onTurnInProgress: onBusy
            ? () => {
                rollback()
                onBusy()
              }
            : undefined,
        })
        if (!sent) rollback()
        return sent
      } catch {
        rollback()
        return false
      } finally {
        pending.current = false
      }
    },
    [
      eligible,
      conversationId,
      folderId,
      send,
      attachedResourcesFallback,
      appendOptimisticTurn,
      removeOptimisticTurn,
      setSyncState,
    ]
  )
  return { eligible, sendDraft }
}
