"use client"

import { useCallback, useState } from "react"

/** Expansion is scoped to the current user/org; loading lives in the child. */
export function useExpandedItems(scopeKey: string | null) {
  const [state, setState] = useState<{
    scope: string | null
    ids: Set<string>
  }>(() => ({ scope: scopeKey, ids: new Set() }))
  const toggle = useCallback(
    (id: string) => {
      setState((previous) => {
        const ids = new Set(previous.scope === scopeKey ? previous.ids : [])
        if (ids.has(id)) ids.delete(id)
        else ids.add(id)
        return { scope: scopeKey, ids }
      })
    },
    [scopeKey]
  )
  const isExpanded = (id: string) =>
    state.scope === scopeKey && state.ids.has(id)
  return { toggle, isExpanded }
}
