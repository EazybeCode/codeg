"use client"

import { useEffect, useSyncExternalStore } from "react"
import type { NativeSession } from "./neev-client"

type Kind = "chat" | "agent"
const owners = new Map<string, Set<symbol>>()
const listeners = new Set<() => void>()
const empty: ReadonlySet<string> = new Set()
let snapshot: ReadonlySet<string> = empty

export function runningChatKey(
  session: NativeSession | null,
  kind: Kind,
  id: string
): string | null {
  return session?.scope === "organization"
    ? `${session.user.id}:${session.organization.id}:${kind}:${id}`
    : null
}
function publish() {
  const next = new Set(owners.keys())
  if (
    next.size === snapshot.size &&
    [...next].every((key) => snapshot.has(key))
  )
    return
  snapshot = next
  listeners.forEach((listener) => listener())
}
function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
export function useRunningChats() {
  return useSyncExternalStore(
    subscribe,
    () => snapshot,
    () => empty
  )
}

/** Mirrors live client run state; no per-row requests or persisted stale flags. */
export function usePublishRunningChat(
  session: NativeSession | null,
  kind: Kind,
  id: string,
  running: boolean
) {
  const key = runningChatKey(session, kind, id)
  useEffect(() => {
    if (!key || !running) return
    const owner = Symbol()
    const active = owners.get(key) ?? new Set<symbol>()
    active.add(owner)
    owners.set(key, active)
    publish()
    return () => {
      active.delete(owner)
      if (active.size === 0) owners.delete(key)
      publish()
    }
  }, [key, running])
}
