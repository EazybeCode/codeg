"use client"
import { useEffect, useState, type ReactNode } from "react"
import { getTransport } from "@/lib/transport"
import type { WorkspaceChatRuntime } from "@/lib/workspace-execution"
import { Button } from "@/components/ui/button"
import { useTranslations } from "next-intl"
import { useWorkspaceCatalogContext } from "./workspace-catalog-provider"
import { WorkspaceCatalogError } from "./workspace-catalog-error"

/** Persisted tabs carry legacy IDs. Never mount ACP until their scoped
 * inverse mapping has identified the canonical ready workspace chat. */
export function WorkspaceChatRuntimeGate({
  conversationId,
  folderId,
  children,
}: {
  conversationId: number
  folderId: number
  children: (draftId: number, runtime: WorkspaceChatRuntime) => ReactNode
}) {
  const t = useTranslations("WorkspaceCatalog")
  const catalog = useWorkspaceCatalogContext()
  const scope = catalog?.scope
  const rememberReady = catalog?.rememberReady
  const [attempt, setAttempt] = useState(0)
  const [result, setResult] = useState<{
    scope: string | null | undefined
    conversationId: number
    folderId: number
    attempt: number
    draftId?: number
    runtime?: WorkspaceChatRuntime
    error?: unknown
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
        if (!cancelled) {
          rememberReady?.(runtime)
          setResult({
            scope,
            conversationId,
            folderId,
            attempt,
            draftId: runtime.draft.id,
            runtime,
          })
        }
      })
      .catch((error: unknown) => {
        if (!cancelled)
          setResult({ conversationId, folderId, attempt, error, scope })
      })
    return () => {
      cancelled = true
    }
  }, [conversationId, folderId, attempt, scope, rememberReady])
  const current =
    result?.scope === scope &&
    result?.conversationId === conversationId &&
    result.folderId === folderId &&
    result.attempt === attempt
      ? result
      : null
  if (current?.draftId != null && current.runtime)
    return <>{children(current.draftId, current.runtime)}</>
  return (
    <div role="status" className="p-4 text-sm">
      {current?.error != null ? (
        <WorkspaceCatalogError error={current.error} />
      ) : (
        <p>{t("loadingChat")}</p>
      )}
      {current?.error != null && (
        <Button
          variant="outline"
          className="mt-2"
          onClick={() => setAttempt((value) => value + 1)}
        >
          {t("retry")}
        </Button>
      )}
    </div>
  )
}
