"use client"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import {
  listWorkspaceNodes,
  listWorkspaceRepositories,
  listWorkspaceChatDrafts,
  type WorkspaceNode,
  type WorkspaceRepository,
  type WorkspaceChatDraft,
} from "@/lib/workspace-catalog"

type Tree = {
  scope: string | null
  nodes: WorkspaceNode[]
  repositories: WorkspaceRepository[]
  loading: boolean
  error: unknown
}
type DraftList = {
  loading: boolean
  error: unknown
  items: WorkspaceChatDraft[]
}
const EMPTY: Tree = {
  scope: null,
  nodes: [],
  repositories: [],
  loading: false,
  error: null,
}

/** One scoped tree/metadata read, with lazy draft lists and stale-response guards. */
export function useWorkspaceCatalog(scope: string | null) {
  const scopeRef = useRef(scope)
  const scopeGeneration = useRef(0)
  if (scopeRef.current !== scope) scopeGeneration.current++
  scopeRef.current = scope
  const sequence = useRef(0)
  const [tree, setTree] = useState<Tree>(EMPTY)
  const [expanded, setExpanded] = useState<Set<number>>(new Set())
  const [drafts, setDrafts] = useState<Record<number, DraftList>>({})
  const draftSequence = useRef(new Map<number, number>())
  const refresh = useCallback(async () => {
    if (!scope) return
    const request = ++sequence.current
    setTree((old) => ({
      ...(old.scope === scope ? old : EMPTY),
      scope,
      loading: true,
      error: null,
    }))
    try {
      const [nodes, repositories] = await Promise.all([
        listWorkspaceNodes(),
        listWorkspaceRepositories(),
      ])
      if (scopeRef.current === scope && sequence.current === request)
        setTree({ scope, nodes, repositories, loading: false, error: null })
    } catch (error) {
      if (scopeRef.current === scope && sequence.current === request)
        setTree((old) => ({ ...old, loading: false, error }))
    }
  }, [scope])
  const refreshDrafts = useCallback(
    async (nodeId: number) => {
      if (!scope) return
      const generation = scopeGeneration.current
      const request = (draftSequence.current.get(nodeId) ?? 0) + 1
      draftSequence.current.set(nodeId, request)
      setDrafts((old) => ({
        ...old,
        [nodeId]: {
          items: old[nodeId]?.items ?? [],
          loading: true,
          error: null,
        },
      }))
      try {
        const items = await listWorkspaceChatDrafts(nodeId)
        if (
          scopeRef.current === scope &&
          scopeGeneration.current === generation &&
          draftSequence.current.get(nodeId) === request
        )
          setDrafts((old) => ({
            ...old,
            [nodeId]: { items, loading: false, error: null },
          }))
      } catch (error) {
        if (
          scopeRef.current === scope &&
          scopeGeneration.current === generation &&
          draftSequence.current.get(nodeId) === request
        )
          setDrafts((old) => ({
            ...old,
            [nodeId]: { ...old[nodeId], loading: false, error },
          }))
      }
    },
    [scope]
  )
  const invalidate = useCallback(() => {
    sequence.current++
    scopeGeneration.current++
  }, [])
  useEffect(() => {
    // Scope changes discard local caches before new organization data can render.

    setExpanded(new Set())
    setDrafts({})
    void refresh()
    return invalidate
  }, [refresh, invalidate])
  const current = tree.scope === scope && scope ? tree : EMPTY
  const toggle = useCallback(
    (id: number) => {
      if (!expanded.has(id)) void refreshDrafts(id)
      setExpanded((old) => {
        const next = new Set(old)
        if (next.has(id)) next.delete(id)
        else next.add(id)
        return next
      })
    },
    [expanded, refreshDrafts]
  )
  const expand = useCallback(
    (id: number) => {
      setExpanded((old) => new Set([...old, id]))
      void refreshDrafts(id)
    },
    [refreshDrafts]
  )
  const pathFor = useCallback(
    (nodeId: number) => {
      const path: WorkspaceNode[] = []
      const visited = new Set<number>()
      let node = current.nodes.find((item) => item.id === nodeId)
      while (node && !visited.has(node.id)) {
        visited.add(node.id)
        path.unshift(node)
        node = current.nodes.find((item) => item.id === node?.parent_id)
      }
      return path
    },
    [current.nodes]
  )
  return useMemo(
    () => ({
      ...current,
      expanded: current.scope ? expanded : new Set<number>(),
      drafts: current.scope ? drafts : {},
      refresh,
      refreshDrafts,
      toggle,
      expand,
      pathFor,
    }),
    [current, expanded, drafts, refresh, refreshDrafts, toggle, expand, pathFor]
  )
}
