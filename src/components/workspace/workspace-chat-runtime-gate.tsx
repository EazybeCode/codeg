"use client"
import { useEffect, useState, type ReactNode } from "react"
import { getTransport } from "@/lib/transport"
import type { WorkspaceChatRuntime } from "@/lib/workspace-execution"
import { Button } from "@/components/ui/button"

/** Persisted tabs carry legacy IDs. Never mount ACP until their scoped
 * inverse mapping has identified the canonical ready workspace chat. */
export function WorkspaceChatRuntimeGate({
  conversationId,
  folderId,
  children,
}: {
  conversationId: number
  folderId: number
  children: (draftId: number) => ReactNode
}) {
  const [attempt, setAttempt] = useState(0)
  const [result, setResult] = useState<{
    conversationId: number
    folderId: number
    attempt: number
    draftId?: number
    error?: string
  } | null>(null)
  useEffect(() => {
    let cancelled = false
    void getTransport()
      .call<WorkspaceChatRuntime>(
        "get_workspace_chat_runtime_for_conversation",
        { conversationId }
      )
      .then((runtime) => {
        if (
          runtime.state !== "ready" ||
          runtime.conversation?.id !== conversationId ||
          runtime.folder?.id !== folderId ||
          !(runtime.draft.id > 0)
        ) {
          throw new Error("Workspace chat mapping is not ready")
        }
        if (!cancelled)
          setResult({
            conversationId,
            folderId,
            attempt,
            draftId: runtime.draft.id,
          })
      })
      .catch((error: unknown) => {
        const code =
          error && typeof error === "object" && "error" in error
            ? error.error
            : null
        const message =
          code === "unauthorized" || code === "reauth_required"
            ? "Sign in again to load this workspace chat."
            : "Unable to load this workspace chat."
        if (!cancelled)
          setResult({ conversationId, folderId, attempt, error: message })
      })
    return () => {
      cancelled = true
    }
  }, [conversationId, folderId, attempt])
  const current =
    result?.conversationId === conversationId &&
    result.folderId === folderId &&
    result.attempt === attempt
      ? result
      : null
  if (current?.draftId != null) return <>{children(current.draftId)}</>
  return (
    <div role="status" className="p-4 text-sm">
      <p>{current?.error ?? "Loading workspace chat…"}</p>
      {current?.error && (
        <Button
          variant="outline"
          className="mt-2"
          onClick={() => setAttempt((value) => value + 1)}
        >
          Retry
        </Button>
      )}
    </div>
  )
}
