"use client"

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react"
import {
  changeTeamChatVisibility,
  connectTeamRepository,
  createTeamChat,
  createTeamNode,
  listTeamChats,
  listTeamNodes,
  listTeamRepositories,
  loadTeamChat,
  updateTeamNode,
  type CreateTeamChatInput,
  type CreateTeamNodeInput,
  type TeamCatalogChat,
  type TeamCatalogNode,
  type TeamCatalogRepository,
  type TeamChatVisibility,
  type UpdateTeamNodeInput,
} from "@/lib/team-catalog-client"
import { TeamClientError } from "@/lib/team-client"
import { useTeamAuth } from "./team-auth-provider"

const CHAT_PAGE_SIZE = 200
const POLL_INTERVAL_MS = 15_000

type CatalogState = {
  scope: string | null
  nodes: TeamCatalogNode[]
  repositories: TeamCatalogRepository[]
  chats: TeamCatalogChat[]
  selectedChat: TeamCatalogChat | null
  loading: boolean
  loadingMoreChats: boolean
  hasMoreChats: boolean
  error: unknown
}

const EMPTY_STATE: CatalogState = {
  scope: null,
  nodes: [],
  repositories: [],
  chats: [],
  selectedChat: null,
  loading: false,
  loadingMoreChats: false,
  hasMoreChats: false,
  error: null,
}

type Selection = {
  scope: string | null
  nodeId: number | null
  chatId: number | null
}

function positiveQueryId(value: string | null): number | null {
  if (!value || !/^[1-9]\d*$/.test(value)) return null
  const id = Number(value)
  return Number.isSafeInteger(id) && id <= 2_147_483_647 ? id : null
}

export function parseTeamCatalogSelection(search: string): {
  nodeId: number | null
  chatId: number | null
} {
  const params = new URLSearchParams(search)
  return {
    nodeId: positiveQueryId(params.get("node")),
    chatId: positiveQueryId(params.get("chat")),
  }
}

function isUnauthorized(error: unknown): boolean {
  return error instanceof TeamClientError && error.kind === "unauthorized"
}

function isNotFound(error: unknown): boolean {
  return error instanceof TeamClientError && error.status === 404
}

function isAborted(error: unknown): boolean {
  return error instanceof TeamClientError && error.kind === "aborted"
}

export function useTeamCatalog() {
  const auth = useTeamAuth()
  const scope = auth.status === "authenticated" ? auth.scopeKey : null
  const csrf =
    auth.status === "authenticated" ? (auth.session?.csrf ?? null) : null
  const scopeRef = useRef(scope)
  const epochRef = useRef(0)
  const controllerRef = useRef<AbortController | null>(null)
  const listSequenceRef = useRef(0)
  const detailSequenceRef = useRef(0)
  const removedChatIdsRef = useRef(new Set<number>())
  const [state, setState] = useState<CatalogState>(EMPTY_STATE)
  const [selection, setSelection] = useState<Selection>({
    scope: null,
    nodeId: null,
    chatId: null,
  })
  const [expandedState, setExpandedState] = useState<{
    scope: string | null
    ids: Set<number>
  }>({ scope: null, ids: new Set() })

  scopeRef.current = scope

  const clearProtected = useCallback(() => {
    listSequenceRef.current += 1
    detailSequenceRef.current += 1
    removedChatIdsRef.current.clear()
    setState(EMPTY_STATE)
    setSelection({ scope: null, nodeId: null, chatId: null })
    setExpandedState({ scope: null, ids: new Set() })
  }, [])

  const recoverUnauthorized = useCallback(
    (error: unknown) => {
      if (!isUnauthorized(error)) return false
      controllerRef.current?.abort()
      clearProtected()
      void auth.refresh()
      return true
    },
    [auth, clearProtected]
  )

  const isCurrent = useCallback(
    (expectedScope: string, epoch: number, controller: AbortController) =>
      scopeRef.current === expectedScope &&
      epochRef.current === epoch &&
      controllerRef.current === controller &&
      !controller.signal.aborted,
    []
  )

  const refreshSelectedChat = useCallback(
    async (
      chatId: number,
      expectedScope: string,
      epoch: number,
      controller: AbortController
    ) => {
      const sequence = ++detailSequenceRef.current
      try {
        const chat = await loadTeamChat(chatId, controller.signal)
        if (!isCurrent(expectedScope, epoch, controller)) return
        if (detailSequenceRef.current !== sequence) return
        removedChatIdsRef.current.delete(chatId)
        setState((old) =>
          old.scope === expectedScope ? { ...old, selectedChat: chat } : old
        )
      } catch (error) {
        if (!isCurrent(expectedScope, epoch, controller) || isAborted(error))
          return
        if (recoverUnauthorized(error)) return
        if (isNotFound(error)) {
          removedChatIdsRef.current.add(chatId)
          setState((old) =>
            old.scope === expectedScope
              ? {
                  ...old,
                  chats: old.chats.filter((item) => item.id !== chatId),
                  selectedChat: null,
                }
              : old
          )
          setSelection((old) =>
            old.scope === expectedScope && old.chatId === chatId
              ? { ...old, chatId: null }
              : old
          )
        } else {
          setState((old) =>
            old.scope === expectedScope ? { ...old, error } : old
          )
        }
      }
    },
    [isCurrent, recoverUnauthorized]
  )

  const refresh = useCallback(async () => {
    if (!scope || !csrf) return
    const controller = controllerRef.current
    if (!controller || controller.signal.aborted) return
    const epoch = epochRef.current
    const sequence = ++listSequenceRef.current
    const selected =
      selection.scope === scope
        ? selection
        : {
            scope,
            ...parseTeamCatalogSelection(window.location.search),
          }

    setState((old) => ({
      ...(old.scope === scope ? old : EMPTY_STATE),
      scope,
      loading: true,
      loadingMoreChats: false,
      error: null,
    }))
    if (selected.chatId != null) {
      void refreshSelectedChat(selected.chatId, scope, epoch, controller)
    }
    const [nodesResult, repositoriesResult, chatsResult] =
      await Promise.allSettled([
        listTeamNodes(controller.signal),
        listTeamRepositories(controller.signal),
        listTeamChats({}, controller.signal),
      ])
    if (
      !isCurrent(scope, epoch, controller) ||
      sequence !== listSequenceRef.current
    )
      return
    if (
      nodesResult.status === "rejected" ||
      repositoriesResult.status === "rejected" ||
      chatsResult.status === "rejected"
    ) {
      const failure = [nodesResult, repositoriesResult, chatsResult].find(
        (result): result is PromiseRejectedResult =>
          result.status === "rejected"
      )!
      if (isAborted(failure.reason) || recoverUnauthorized(failure.reason))
        return
      setState((old) =>
        old.scope === scope
          ? { ...old, loading: false, error: failure.reason }
          : old
      )
      return
    }
    const chats = chatsResult.value.filter(
      (chat) => !removedChatIdsRef.current.has(chat.id)
    )
    setState((old) =>
      old.scope === scope
        ? {
            ...old,
            nodes: nodesResult.value,
            repositories: repositoriesResult.value,
            chats,
            selectedChat:
              old.selectedChat &&
              !removedChatIdsRef.current.has(old.selectedChat.id)
                ? old.selectedChat
                : null,
            loading: false,
            hasMoreChats: chatsResult.value.length === CHAT_PAGE_SIZE,
            error: null,
          }
        : old
    )
  }, [
    csrf,
    isCurrent,
    recoverUnauthorized,
    refreshSelectedChat,
    scope,
    selection,
  ])

  useLayoutEffect(() => {
    controllerRef.current?.abort()
    epochRef.current += 1
    const epoch = epochRef.current
    clearProtected()
    if (!scope || !csrf) {
      controllerRef.current = null
      return
    }
    const controller = new AbortController()
    controllerRef.current = controller
    const parsed = parseTeamCatalogSelection(window.location.search)
    setSelection({ scope, ...parsed })
    setExpandedState({ scope, ids: new Set() })
    setState({ ...EMPTY_STATE, scope, loading: true })
    void refresh()
    return () => {
      if (epochRef.current === epoch) epochRef.current += 1
      controller.abort()
    }
    // refresh intentionally follows scope ownership; selection changes do not
    // restart the scope lifetime.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope, csrf, clearProtected])

  useEffect(() => {
    if (!scope || !csrf) return
    let interval: number | null = null
    const stop = () => {
      if (interval != null) window.clearInterval(interval)
      interval = null
    }
    const start = () => {
      stop()
      if (document.visibilityState !== "visible") return
      interval = window.setInterval(() => void refresh(), POLL_INTERVAL_MS)
    }
    const onVisibility = () => {
      if (document.visibilityState === "visible") {
        void refresh()
        start()
      } else {
        stop()
      }
    }
    start()
    document.addEventListener("visibilitychange", onVisibility)
    return () => {
      stop()
      document.removeEventListener("visibilitychange", onVisibility)
    }
  }, [csrf, refresh, scope])

  useEffect(() => {
    if (!scope) return
    const sync = () => {
      setSelection({
        scope,
        ...parseTeamCatalogSelection(window.location.search),
      })
    }
    window.addEventListener("popstate", sync)
    return () => window.removeEventListener("popstate", sync)
  }, [scope])

  const current = state.scope === scope && scope ? state : EMPTY_STATE
  const currentSelection = useMemo(
    () =>
      selection.scope === scope && scope
        ? selection
        : { scope: null, nodeId: null, chatId: null },
    [scope, selection]
  )
  const expanded = useMemo(
    () =>
      expandedState.scope === scope && scope
        ? expandedState.ids
        : new Set<number>(),
    [expandedState, scope]
  )

  const loadMoreChats = useCallback(async () => {
    if (!scope || !csrf || !current.hasMoreChats || current.loadingMoreChats)
      return
    const afterId = current.chats[current.chats.length - 1]?.id
    const controller = controllerRef.current
    if (!afterId || !controller) return
    const epoch = epochRef.current
    setState((old) =>
      old.scope === scope ? { ...old, loadingMoreChats: true } : old
    )
    try {
      const page = await listTeamChats({ afterId }, controller.signal)
      if (!isCurrent(scope, epoch, controller)) return
      const allowed = page.filter(
        (chat) => !removedChatIdsRef.current.has(chat.id)
      )
      setState((old) => {
        if (old.scope !== scope) return old
        const merged = new Map(old.chats.map((chat) => [chat.id, chat]))
        allowed.forEach((chat) => merged.set(chat.id, chat))
        return {
          ...old,
          chats: [...merged.values()].sort((a, b) => a.id - b.id),
          loadingMoreChats: false,
          hasMoreChats: page.length === CHAT_PAGE_SIZE,
        }
      })
    } catch (error) {
      if (!isCurrent(scope, epoch, controller) || isAborted(error)) return
      if (recoverUnauthorized(error)) return
      setState((old) =>
        old.scope === scope ? { ...old, loadingMoreChats: false, error } : old
      )
    }
  }, [csrf, current, isCurrent, recoverUnauthorized, scope])

  const runMutation = useCallback(
    async <T>(operation: (csrf: string, signal: AbortSignal) => Promise<T>) => {
      if (!scope || !csrf) throw new TeamClientError("unauthorized", 401)
      const controller = controllerRef.current
      if (!controller) throw new TeamClientError("aborted")
      const epoch = epochRef.current
      try {
        const result = await operation(csrf, controller.signal)
        if (!isCurrent(scope, epoch, controller))
          throw new TeamClientError("aborted")
        return result
      } catch (error) {
        if (recoverUnauthorized(error)) throw error
        throw error
      }
    },
    [csrf, isCurrent, recoverUnauthorized, scope]
  )

  const createNodeMutation = useCallback(
    async (input: CreateTeamNodeInput) => {
      const node = await runMutation((token, signal) =>
        createTeamNode(input, token, signal)
      )
      setState((old) =>
        old.scope === scope ? { ...old, nodes: [...old.nodes, node] } : old
      )
      setSelection({ scope, nodeId: node.id, chatId: null })
      return node
    },
    [runMutation, scope]
  )

  const updateNodeMutation = useCallback(
    async (node: TeamCatalogNode, input: UpdateTeamNodeInput) => {
      try {
        const updated = await runMutation((token, signal) =>
          updateTeamNode(node.id, input, token, signal)
        )
        setState((old) =>
          old.scope === scope
            ? {
                ...old,
                nodes: old.nodes.map((item) =>
                  item.id === updated.id ? updated : item
                ),
              }
            : old
        )
        return updated
      } catch (error) {
        if (error instanceof TeamClientError && error.kind === "conflict") {
          await refresh()
        }
        throw error
      }
    },
    [refresh, runMutation, scope]
  )

  const connectRepositoryMutation = useCallback(
    (fullName: string, startingBranch: string) =>
      runMutation((token, signal) =>
        connectTeamRepository({ fullName, startingBranch }, token, signal)
      ),
    [runMutation]
  )

  const createChatMutation = useCallback(
    async (input: CreateTeamChatInput) => {
      const chat = await runMutation((token, signal) =>
        createTeamChat(input, token, signal)
      )
      setState((old) =>
        old.scope === scope
          ? { ...old, chats: [...old.chats, chat], selectedChat: chat }
          : old
      )
      setSelection({ scope, nodeId: chat.nodeId, chatId: chat.id })
      return chat
    },
    [runMutation, scope]
  )

  const shareChat = useCallback(
    async (chat: TeamCatalogChat, visibility: TeamChatVisibility) => {
      const updated = await runMutation((token, signal) =>
        changeTeamChatVisibility(
          chat.id,
          { expectedRevision: chat.accessRevision, visibility },
          token,
          signal
        )
      )
      setState((old) =>
        old.scope === scope
          ? {
              ...old,
              chats: old.chats.map((item) =>
                item.id === updated.id ? updated : item
              ),
              selectedChat:
                old.selectedChat?.id === updated.id
                  ? updated
                  : old.selectedChat,
            }
          : old
      )
      return updated
    },
    [runMutation, scope]
  )

  const selectNode = useCallback(
    (nodeId: number) => {
      if (!scope) return
      window.history.pushState({}, "", `/workspace?node=${nodeId}`)
      setSelection({ scope, nodeId, chatId: null })
    },
    [scope]
  )

  const selectChat = useCallback(
    (chatId: number) => {
      if (!scope) return
      const chat = current.chats.find((item) => item.id === chatId)
      const nodeId = chat?.nodeId ?? null
      const query = nodeId
        ? `/workspace?node=${nodeId}&chat=${chatId}`
        : `/workspace?chat=${chatId}`
      window.history.pushState({}, "", query)
      setSelection({ scope, nodeId, chatId })
      const controller = controllerRef.current
      if (controller)
        void refreshSelectedChat(chatId, scope, epochRef.current, controller)
    },
    [current.chats, refreshSelectedChat, scope]
  )

  const toggleNode = useCallback(
    (nodeId: number) => {
      if (!scope) return
      setExpandedState((old) => {
        const ids = new Set(old.scope === scope ? old.ids : [])
        if (ids.has(nodeId)) ids.delete(nodeId)
        else ids.add(nodeId)
        return { scope, ids }
      })
    },
    [scope]
  )

  const pathFor = useCallback(
    (nodeId: number) => {
      const path: TeamCatalogNode[] = []
      const seen = new Set<number>()
      let node = current.nodes.find((item) => item.id === nodeId)
      while (node && !seen.has(node.id)) {
        seen.add(node.id)
        path.unshift(node)
        node = current.nodes.find((item) => item.id === node?.parentId)
      }
      return path
    },
    [current.nodes]
  )

  return useMemo(
    () => ({
      ...current,
      selectedNodeId: currentSelection.nodeId,
      selectedChatId: currentSelection.chatId,
      expanded,
      refresh,
      loadMoreChats,
      toggleNode,
      selectNode,
      selectChat,
      createNode: createNodeMutation,
      updateNode: updateNodeMutation,
      connectRepository: connectRepositoryMutation,
      createChat: createChatMutation,
      shareChat,
      pathFor,
    }),
    [
      connectRepositoryMutation,
      createChatMutation,
      createNodeMutation,
      current,
      currentSelection,
      expanded,
      loadMoreChats,
      pathFor,
      refresh,
      selectChat,
      selectNode,
      shareChat,
      toggleNode,
      updateNodeMutation,
    ]
  )
}
