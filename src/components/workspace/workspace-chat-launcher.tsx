"use client"
import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { GitBranch, Loader2 } from "lucide-react"
import { getTransport } from "@/lib/transport"
import type { AcpAgentInfo, AgentType } from "@/lib/types"
import type { WorkspaceChatRuntime } from "@/lib/workspace-execution"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabStore } from "@/stores/tab-store"
import { Button } from "@/components/ui/button"
import { useWorkspaceCatalogContext } from "./workspace-catalog-provider"
import { WorkspaceCatalogError } from "./workspace-catalog-error"

export function WorkspaceChatLauncher({ draftId }: { draftId: number }) {
  const t = useTranslations("WorkspaceCatalog")
  const catalog = useWorkspaceCatalogContext()
  const scope = catalog?.scope
  const rememberReady = catalog?.rememberReady
  const [attempt, setAttempt] = useState(0)
  const [result, setResult] = useState<{
    key: string
    runtime?: WorkspaceChatRuntime
    enabled?: boolean
    agents?: AcpAgentInfo[]
    error?: unknown
  } | null>(null)
  const [selected, setSelected] = useState<AgentType | null>(null)
  const [busy, setBusy] = useState(false)
  const currentKey = `${scope}:${draftId}`
  const keyRef = useRef(currentKey)
  keyRef.current = currentKey
  const inFlight = useRef(false)
  const alive = useRef(true)
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])
  const accept = useCallback(
    (runtime: WorkspaceChatRuntime) => {
      if (runtime.draft.id !== draftId) throw new Error("runtime mismatch")
      if (runtime.state !== "ready") return
      const { folder, conversation } = runtime
      if (
        !folder ||
        !conversation ||
        conversation.folder_id !== folder.id ||
        folder.kind !== "workspace_chat"
      )
        throw new Error("runtime mapping unavailable")
      const workspace = useAppWorkspaceStore.getState()
      workspace.upsertFolder(folder)
      workspace.applyConversationUpsert(conversation)
      rememberReady?.(runtime)
      useTabStore
        .getState()
        .openTab(
          folder.id,
          conversation.id,
          conversation.agent_type,
          true,
          runtime.draft.title ?? undefined
        )
    },
    [draftId, rememberReady]
  )
  useEffect(() => {
    if (!scope) return
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const load = async () => {
      try {
        const [runtime, capabilities] = await Promise.all([
          getTransport().call<WorkspaceChatRuntime>(
            "get_workspace_chat_runtime",
            { draftId }
          ),
          getTransport().call<{ execution_enabled: boolean }>(
            "get_workspace_runtime_capabilities",
            {}
          ),
        ])
        if (cancelled) return
        accept(runtime)
        const agents =
          capabilities.execution_enabled && runtime.state !== "ready"
            ? await getTransport().call<AcpAgentInfo[]>("acp_list_agents", {})
            : []
        if (cancelled) return
        setResult({
          key: currentKey,
          runtime,
          enabled: capabilities.execution_enabled,
          agents,
        })
        if (runtime.state === "provisioning")
          timer = setTimeout(() => void load(), 1500)
      } catch (error) {
        if (!cancelled) setResult({ key: currentKey, error })
      }
    }
    void load()
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [scope, draftId, attempt, currentKey, accept])
  const current = result?.key === currentKey ? result : null
  const agents = current?.agents?.filter((agent) => agent.enabled) ?? []
  const agentType =
    agents.find((agent) => agent.agent_type === selected && agent.available)
      ?.agent_type ?? agents.find((agent) => agent.available)?.agent_type
  async function prepare() {
    if (!agentType || inFlight.current || !current?.enabled) return
    inFlight.current = true
    setBusy(true)
    try {
      const runtime = await getTransport().call<WorkspaceChatRuntime>(
        "provision_workspace_chat",
        { draftId, agentType }
      )
      if (!alive.current || keyRef.current !== currentKey) return
      accept(runtime)
      setResult((old) => ({ ...old!, runtime, error: undefined }))
      if (runtime.state === "provisioning") setAttempt((old) => old + 1)
    } catch (error) {
      if (alive.current && keyRef.current === currentKey)
        setResult((old) => ({ ...old!, error }))
    } finally {
      inFlight.current = false
      if (alive.current) setBusy(false)
    }
  }
  return (
    <section className="mx-auto flex w-full max-w-lg flex-col gap-5 px-6 py-12 sm:py-20">
      <GitBranch aria-hidden className="size-7 text-muted-foreground" />
      <div>
        <h1 className="text-xl font-semibold tracking-tight">
          {current?.runtime?.draft.title ?? t("untitledChat")}
        </h1>
        <p className="mt-2 break-words text-sm text-muted-foreground">
          {current?.runtime
            ? `${current.runtime.draft.full_name} · ${current.runtime.draft.starting_branch}`
            : t("loadingChat")}
        </p>
      </div>
      {current?.error != null && (
        <WorkspaceCatalogError
          error={current.error}
          retry={() => setAttempt((old) => old + 1)}
        />
      )}
      {current?.runtime?.error_code && (
        <WorkspaceCatalogError error={{ error: current.runtime.error_code }} />
      )}
      {current?.enabled === false ? (
        <p role="status" className="text-sm text-muted-foreground">
          {t("disabledError")}
        </p>
      ) : busy || current?.runtime?.state === "provisioning" ? (
        <p role="status" className="flex items-center gap-2 text-sm">
          <Loader2 className="size-4 animate-spin" />
          {t("provisioning")}
        </p>
      ) : (
        current?.runtime &&
        current.runtime.state !== "ready" && (
          <>
            <label className="space-y-2 text-sm">
              <span>{t("chooseAgent")}</span>
              <select
                className="mt-2 h-9 w-full rounded-md border border-input bg-background px-2"
                value={agentType ?? ""}
                onChange={(event) =>
                  setSelected(event.target.value as AgentType)
                }
              >
                {!agentType && <option value="">{t("chooseAgent")}</option>}
                {agents.map((agent) => (
                  <option
                    key={agent.agent_type}
                    value={agent.agent_type}
                    disabled={!agent.available}
                  >
                    {agent.name}
                  </option>
                ))}
              </select>
            </label>
            <Button disabled={!agentType} onClick={() => void prepare()}>
              {t("prepare")}
            </Button>
          </>
        )
      )}
      <p className="text-xs leading-relaxed text-muted-foreground">
        {t("chatHint")} {t("startHint")}
      </p>
      <p className="text-xs text-muted-foreground">{t("trustedPreview")}</p>
    </section>
  )
}
