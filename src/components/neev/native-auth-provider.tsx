"use client"

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react"
import { useRouter } from "next/navigation"
import {
  loadNativeSession,
  loadServerCapabilities,
  nativeMutation,
  NativeClientError,
  type NativeCapabilities,
  type NativeSession,
} from "@/lib/neev-client"

const CHANNEL = "neev-native-auth"
type Status =
  | "loading"
  | "revalidating"
  | "authenticated"
  | "unauthenticated"
  | "unavailable"
interface NativeAuth {
  status: Status
  session: NativeSession | null
  capabilities: NativeCapabilities | null
  scopeKey: string | null
  signal: AbortSignal | undefined
  logoutError: boolean
  refresh: () => Promise<void>
  logout: () => Promise<void>
  changed: () => Promise<void>
  handleError: (error: unknown) => void
  handleAuthorityError: (error: unknown) => void
}
const Context = createContext<NativeAuth | null>(null)
export function useNativeAuth() {
  const value = useContext(Context)
  if (!value) throw new Error("NativeAuthProvider required")
  return value
}
export function notifyNativeAuthChanged(source: string) {
  if (typeof BroadcastChannel === "undefined") return
  const channel = new BroadcastChannel(CHANNEL)
  channel.postMessage({ type: "invalidate", source })
  channel.close()
}
export function NativeAuthProvider({
  children,
}: {
  children: React.ReactNode
}) {
  const router = useRouter()
  const [status, setStatus] = useState<Status>("loading")
  const [session, setSession] = useState<NativeSession | null>(null)
  const [capabilities, setCapabilities] = useState<NativeCapabilities | null>(
    null
  )
  const [logoutError, setLogoutError] = useState(false)
  const [generation, setGeneration] = useState(0)
  const [signal, setSignal] = useState<AbortSignal>()
  const current = useRef(0),
    verified = useRef<NativeSession | null>(null),
    request = useRef<AbortController | null>(null),
    recoveryCsrf = useRef<string | null>(null),
    mounted = useRef(false),
    logoutPending = useRef(false)
  const source = useRef<string>(crypto.randomUUID())
  const clear = useCallback(() => {
    request.current?.abort()
    request.current = new AbortController()
    current.current += 1
    setSignal(request.current.signal)
    setGeneration(current.current)
    verified.current = null
    setSession(null)
    return { generation: current.current, controller: request.current }
  }, [])
  const refresh = useCallback(async () => {
    if (!mounted.current || logoutPending.current) return
    const next = clear()
    setStatus("loading")
    try {
      const caps = await loadServerCapabilities(next.controller.signal)
      if (caps.mode !== "native")
        throw new NativeClientError("invalid_response")
      if (next.controller.signal.aborted || next.generation !== current.current)
        return
      setCapabilities(caps)
      const value = await loadNativeSession(next.controller.signal)
      if (next.controller.signal.aborted || next.generation !== current.current)
        return
      recoveryCsrf.current = value.csrf
      verified.current = value
      setSession(value)
      setStatus("authenticated")
      setLogoutError(false)
    } catch (error) {
      if (next.controller.signal.aborted || next.generation !== current.current)
        return
      if (
        error instanceof NativeClientError &&
        (error.kind === "unauthorized" || error.kind === "forbidden")
      ) {
        recoveryCsrf.current = null
        setStatus("unauthenticated")
      } else setStatus("unavailable")
    }
  }, [clear])
  const revalidateVisibility = useCallback(async () => {
    if (!mounted.current || logoutPending.current) return
    const previous = verified.current
    if (!previous) return refresh()
    request.current?.abort()
    const controller = new AbortController()
    request.current = controller
    const epoch = ++current.current
    setSignal(controller.signal)
    setStatus("revalidating")
    const isCurrent = () =>
      !controller.signal.aborted && epoch === current.current
    try {
      const caps = await loadServerCapabilities(controller.signal)
      if (!isCurrent()) return
      if (caps.mode !== "native")
        throw new NativeClientError("invalid_response")
      const value = await loadNativeSession(controller.signal)
      if (!isCurrent()) return
      // CSRF is validated against the cookie session by bootstrap. Compare only
      // in memory: it must never become a React key, DOM value, or stored draft key.
      const sameSession =
        value.user.id === previous.user.id &&
        value.scope === previous.scope &&
        value.organization?.id === previous.organization?.id &&
        value.role === previous.role &&
        value.csrf === previous.csrf
      if (!sameSession) setGeneration(epoch)
      verified.current = value
      recoveryCsrf.current = value.csrf
      setCapabilities(caps)
      setSession(value)
      setStatus("authenticated")
    } catch (error) {
      if (!isCurrent()) return
      clear()
      recoveryCsrf.current = null
      setStatus(
        error instanceof NativeClientError &&
          (error.kind === "unauthorized" || error.kind === "forbidden")
          ? "unauthenticated"
          : "unavailable"
      )
    }
  }, [clear, refresh])
  const changed = useCallback(async () => {
    notifyNativeAuthChanged(source.current)
    await refresh()
  }, [refresh])
  const logout = useCallback(async () => {
    logoutPending.current = true
    const csrf = recoveryCsrf.current,
      next = clear()
    setStatus("loading")
    setLogoutError(false)
    // Clear all in-memory scoped UI at the start, including failed logout attempts.
    notifyNativeAuthChanged(source.current)
    try {
      if (csrf)
        await nativeMutation(
          "/api/neev/logout",
          {},
          csrf,
          next.controller.signal
        )
      if (next.controller.signal.aborted || next.generation !== current.current)
        return
      recoveryCsrf.current = null
      logoutPending.current = false
      setStatus("unauthenticated")
      notifyNativeAuthChanged(source.current)
      router.replace("/login")
    } catch (error) {
      if (next.controller.signal.aborted || next.generation !== current.current)
        return
      if (error instanceof NativeClientError && error.kind === "unauthorized") {
        // A401 may mean another tab rotated the cookies, not that logout
        // succeeded. Confirm current authority without exposing its workspace.
        try {
          const currentSession = await loadNativeSession(next.controller.signal)
          if (
            next.controller.signal.aborted ||
            next.generation !== current.current
          )
            return
          recoveryCsrf.current = currentSession.csrf
        } catch (checkError) {
          if (
            next.controller.signal.aborted ||
            next.generation !== current.current
          )
            return
          if (
            checkError instanceof NativeClientError &&
            (checkError.kind === "unauthorized" ||
              checkError.kind === "forbidden")
          ) {
            recoveryCsrf.current = null
            logoutPending.current = false
            setStatus("unauthenticated")
            notifyNativeAuthChanged(source.current)
            router.replace("/login")
            return
          }
        }
      }
      setLogoutError(true)
      setStatus("unavailable")
    }
  }, [clear, router])
  const handleError = useCallback(
    (error: unknown) => {
      if (error instanceof NativeClientError && error.kind === "unauthorized")
        void refresh()
    },
    [refresh]
  )
  const handleAuthorityError = useCallback(
    (error: unknown) => {
      if (
        error instanceof NativeClientError &&
        (error.kind === "unauthorized" || error.kind === "forbidden")
      )
        void refresh()
    },
    [refresh]
  )
  useEffect(() => {
    mounted.current = true
    void refresh()
    const resume = () => {
      void refresh()
    }
    const visible = () => {
      if (document.visibilityState === "visible") void revalidateVisibility()
    }
    const message = (event: MessageEvent) => {
      if (
        event.data?.type === "invalidate" &&
        event.data.source !== source.current
      )
        resume()
    }
    const channel =
      typeof BroadcastChannel === "undefined"
        ? null
        : new BroadcastChannel(CHANNEL)
    channel?.addEventListener("message", message)
    // A native file picker returns window focus without changing visibility.
    // Revalidating that event would erase the just-selected branding draft.
    document.addEventListener("visibilitychange", visible)
    return () => {
      mounted.current = false
      current.current += 1
      request.current?.abort()
      recoveryCsrf.current = null
      verified.current = null
      channel?.removeEventListener("message", message)
      channel?.close()
      document.removeEventListener("visibilitychange", visible)
    }
  }, [refresh, revalidateVisibility])
  const scopeKey = session
    ? `${session.user.id}:${session.organization?.id ?? "identity"}:${generation}`
    : null
  const value = useMemo(
    () => ({
      status,
      session,
      capabilities,
      scopeKey,
      signal,
      logoutError,
      refresh,
      logout,
      changed,
      handleError,
      handleAuthorityError,
    }),
    [
      status,
      session,
      capabilities,
      scopeKey,
      signal,
      logoutError,
      refresh,
      logout,
      changed,
      handleError,
      handleAuthorityError,
    ]
  )
  return <Context.Provider value={value}>{children}</Context.Provider>
}
