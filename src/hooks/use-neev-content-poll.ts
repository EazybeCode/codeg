"use client"

import { useEffect, useMemo, useRef, useState } from "react"

/** A component-local request lifetime. Even transports ignoring abort cannot publish. */
export function useNeevContentPoll({
  signal,
  scopeKey,
  resourceScope,
  enabled = true,
  onPoll,
}: {
  signal?: AbortSignal
  scopeKey: string | null
  resourceScope: string
  enabled?: boolean
  onPoll: () => Promise<void>
}) {
  const [restart, setRestart] = useState(0)
  const lifetime = useMemo(
    () => ({
      controller: new AbortController(),
      parent: signal,
      scopeKey,
      resourceScope,
      restart,
    }),
    [signal, scopeKey, resourceScope, restart]
  )
  const callback = useRef(onPoll)
  useEffect(() => {
    callback.current = onPoll
  }, [onPoll])
  useEffect(() => {
    // Strict Mode replays mount effects after cleanup. Allocate a NEW lifetime
    // instead of reviving the old signal, so its late responses remain rejected.
    if (lifetime.controller.signal.aborted && !signal?.aborted) {
      // This one-time transport replacement requires a fresh request scope.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setRestart((value) => value + 1)
      return
    }
    const abort = () => lifetime.controller.abort()
    if (signal?.aborted) abort()
    signal?.addEventListener("abort", abort)
    return () => {
      signal?.removeEventListener("abort", abort)
      abort()
    }
  }, [lifetime, signal])
  useEffect(() => {
    if (!enabled || !scopeKey) return
    let busy = false
    const timer = window.setInterval(() => {
      if (
        document.visibilityState !== "visible" ||
        lifetime.controller.signal.aborted ||
        busy
      )
        return
      busy = true
      void callback.current().finally(() => {
        busy = false
      })
    }, 5000)
    return () => window.clearInterval(timer)
  }, [enabled, lifetime, scopeKey])
  return useMemo(
    () => ({
      signal: lifetime.controller.signal,
      isCurrent: () =>
        !lifetime.controller.signal.aborted && !lifetime.parent?.aborted,
      stop: () => lifetime.controller.abort(),
    }),
    [lifetime]
  )
}
