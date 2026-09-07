"use client"
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react"
import { useTabStore } from "@/stores/tab-store"
import { useWorkspaceCatalog } from "@/hooks/use-workspace-catalog"
import { useWorkspaceIdentity } from "@/components/layout/workspace-identity"
import { useOptionalWorkbenchRoute } from "@/contexts/workbench-route-context"
import type { WorkspaceNodeKind } from "@/lib/workspace-catalog"
import type { WorkspaceChatRuntime } from "@/lib/workspace-execution"
import { WorkspaceNodeDialog } from "./workspace-node-dialog"
import { WorkspaceChatDialog } from "./workspace-chat-dialog"

type Creation = { kind: WorkspaceNodeKind | "chat"; parentId: number | null }
type CatalogContext = ReturnType<typeof useWorkspaceCatalog> & {
  identity: ReturnType<typeof useWorkspaceIdentity>
  enabled: boolean
  scope: string | null
  selectedNodeId: number | null
  selectNode: (id: number) => void
  create: (kind: Creation["kind"], parentId?: number | null) => void
  chatId: number | null
  openChat: (id: number, push?: boolean) => void
  ready: Map<number, WorkspaceChatRuntime>
  rememberReady: (runtime: WorkspaceChatRuntime) => void
  nodeForConversation: (conversationId?: number | null) => number | null
}
const Context = createContext<CatalogContext | null>(null)
export function useWorkspaceCatalogContext() {
  return useContext(Context)
}

export function WorkspaceCatalogProvider({
  children,
}: {
  children: ReactNode
}) {
  const identity = useWorkspaceIdentity()
  const openConversations = useOptionalWorkbenchRoute()?.openConversations
  useLayoutEffect(() => {
    useTabStore.setState({ catalogMode: identity.status !== "local" })
    return () => {
      useTabStore.setState({ catalogMode: false })
    }
  }, [identity.status])
  const scope = identity.value
    ? `${identity.value.organization.id}:${identity.value.user.id}`
    : null
  const catalog = useWorkspaceCatalog(scope)
  const [selection, setSelection] = useState<{
    scope: string | null
    id: number
  } | null>(null)
  const [creation, setCreation] = useState<
    (Creation & { scope: string }) | null
  >(null)
  const [chatId, setChatId] = useState<number | null>(null)
  const activeTabId = useTabStore((state) => state.activeTabId)
  const localFocusRevision = useTabStore((state) => state.localFocusRevision)
  const previousActive = useRef(activeTabId)
  const previousFocus = useRef(localFocusRevision)
  const [readyState, setReadyState] = useState<{
    scope: string | null
    items: Map<number, WorkspaceChatRuntime>
  }>({ scope: null, items: new Map() })
  useLayoutEffect(() => {
    // Destroy identity-owned caches, including when the user later returns to
    // this organization. Masking by scope alone would resurrect old entries.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setReadyState({ scope, items: new Map() })
    setSelection(null)
    setCreation(null)
  }, [scope])
  const readyRef = useRef({ readyState, scope })
  useLayoutEffect(() => {
    readyRef.current = { readyState, scope }
  }, [readyState, scope])
  const rememberReady = useCallback(
    (runtime: WorkspaceChatRuntime) => {
      setReadyState((old) => ({
        scope,
        items: new Map([
          ...(old.scope === scope ? old.items : []),
          [runtime.draft.id, runtime],
        ]),
      }))
    },
    [scope]
  )
  const openChat = useCallback(
    (id: number, push = true) => {
      openConversations?.()
      // A new canonical destination supersedes any preceding tab focus in the
      // same event. Cache/hydration updates must not replay that old selection.
      previousActive.current = useTabStore.getState().activeTabId
      previousFocus.current = useTabStore.getState().localFocusRevision
      if (push && window.location.search !== `?chatId=${id}`)
        window.history.pushState({}, "", `/workspace?chatId=${id}`)
      setChatId(id)
      const current = readyRef.current
      const runtime =
        current.readyState.scope === current.scope
          ? current.readyState.items.get(id)
          : null
      if (runtime?.folder && runtime.conversation)
        useTabStore
          .getState()
          .openTab(
            runtime.folder.id,
            runtime.conversation.id,
            runtime.conversation.agent_type,
            true,
            runtime.draft.title ?? undefined
          )
    },
    [openConversations]
  )
  useEffect(() => {
    const sync = () => {
      const raw = new URLSearchParams(window.location.search).get("chatId")
      const id = Number(raw)
      if (raw && Number.isSafeInteger(id) && id > 0) openChat(id, false)
      else setChatId(null)
    }
    sync()
    window.addEventListener("popstate", sync)
    return () => window.removeEventListener("popstate", sync)
  }, [openChat])
  useEffect(() => {
    const explicitFocus = previousFocus.current !== localFocusRevision
    const closedLast = previousActive.current != null && activeTabId == null
    const changed = previousActive.current !== activeTabId || explicitFocus
    previousActive.current = activeTabId
    previousFocus.current = localFocusRevision
    if (readyState.scope !== scope) return
    // An inbound tab snapshot is not an explicit request to leave a launcher.
    if (
      !explicitFocus &&
      !closedLast &&
      chatId != null &&
      !readyState.items.has(chatId)
    )
      return
    const tab = useTabStore
      .getState()
      .tabs.find((item) => item.id === activeTabId)
    const runtime = [...readyState.items.values()].find(
      (item) => item.conversation?.id === tab?.conversationId
    )
    if (runtime) {
      const url = `/workspace?chatId=${runtime.draft.id}`
      if (`${window.location.pathname}${window.location.search}` !== url)
        window.history.replaceState({}, "", url)
      // Bridge the external persisted-tab store to the canonical browser URL.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setChatId(runtime.draft.id)
    } else if (changed) {
      window.history.replaceState({}, "", "/workspace")
      setChatId(null)
    }
  }, [activeTabId, localFocusRevision, readyState, scope, chatId])
  const selectedNodeId = selection?.scope === scope ? selection.id : null
  const create = (
    kind: Creation["kind"],
    parentId: number | null = selectedNodeId
  ) => {
    if (!scope || (kind !== "project" && parentId == null)) return
    setCreation({ kind, parentId: kind === "project" ? null : parentId, scope })
  }
  const nodeForConversation = (conversationId?: number | null) => {
    if (conversationId != null && readyState.scope === scope) {
      const runtime = [...readyState.items.values()].find(
        (item) => item.conversation?.id === conversationId
      )
      return runtime?.draft.node_id ?? null
    }
    return selectedNodeId
  }
  return (
    <Context.Provider
      value={{
        ...catalog,
        identity,
        scope,
        enabled: identity.status !== "local",
        selectedNodeId,
        selectNode: (id) => setSelection({ scope, id }),
        create,
        chatId,
        openChat,
        ready: readyState.scope === scope ? readyState.items : new Map(),
        rememberReady,
        nodeForConversation,
      }}
    >
      {children}
      {creation &&
        creation.scope === scope &&
        (creation.kind === "chat" ? (
          <WorkspaceChatDialog
            key={`${scope}:chat:${creation.parentId}`}
            nodeId={creation.parentId!}
            onClose={() => setCreation(null)}
          />
        ) : (
          <WorkspaceNodeDialog
            key={`${scope}:${creation.kind}:${creation.parentId}`}
            kind={creation.kind}
            parentId={creation.parentId}
            onClose={() => setCreation(null)}
          />
        ))}
    </Context.Provider>
  )
}
