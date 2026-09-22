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
  /**
   * A revalidation that could not be answered, with a session still held.
   *
   * NOT a weaker "authenticated": nothing may be read or written under it, and
   * the surface it belongs to is hidden and inert until the check succeeds.
   * What it does preserve is the subtree, because the alternative is worse than
   * an unanswered check — dropping a laptop's wifi for a moment would unmount
   * every open panel and take unsaved writing with it. Authority is re-proved
   * on Retry; a definitive refusal still clears everything.
   */
  | "unverified"
  | "unavailable"
interface NativeAuth {
  status: Status
  /**
   * Whether this session has been proved, right now.
   *
   * `session` outlives a failed check on purpose, so it alone is no longer the
   * question to ask before writing. Anything that mutates — including work a
   * timer would start on its own, with no one there to be stopped by an inert
   * surface — asks this instead.
   */
  verified: boolean
  session: NativeSession | null
  capabilities: NativeCapabilities | null
  scopeKey: string | null
  signal: AbortSignal | undefined
  logoutError: boolean
  refresh: () => Promise<void>
  /**
   * Prove the session that is still held, without discarding it first.
   *
   * `refresh` starts from nothing — it clears the session, and everything
   * scoped to it, before asking. That is right when there is nothing worth
   * keeping, and exactly wrong after a check that failed to be answered: it
   * would destroy the drafts the hold exists to protect. This re-proves what is
   * already there, and falls back to `refresh` when nothing is held. An answer
   * that refuses, or that comes back as a different identity, still clears.
   */
  retry: () => Promise<void>
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
    // Asked for together, because neither depends on the other and running
    // them nose-to-tail put a whole round trip in front of everything else.
    //
    // Settled rather than all, because they are not the same KIND of answer.
    // The capabilities are public and say how to sign in; the session is
    // private and says who you are. Requiring both meant a signed-out
    // visitor's 401 discarded the capabilities alongside it, and the sign-in
    // page — the one page where having no session is the normal case — went
    // on to offer no method at all, every one of them labelled unavailable.
    //
    // Whichever answer arrives first, both are read here, once, after a single
    // staleness check: an aborted or superseded load writes nothing.
    const [capsRead, sessionRead] = await Promise.allSettled([
      loadServerCapabilities(next.controller.signal),
      loadNativeSession(next.controller.signal),
    ])
    if (next.controller.signal.aborted || next.generation !== current.current)
      return
    // Fail closed on the capabilities themselves. Unreadable, or an answer
    // from something that is not this server, and nothing is published or
    // believed — including a session that happened to come back beside it.
    if (capsRead.status !== "fulfilled" || capsRead.value.mode !== "native") {
      setStatus("unavailable")
      return
    }
    setCapabilities(capsRead.value)
    if (sessionRead.status === "fulfilled") {
      recoveryCsrf.current = sessionRead.value.csrf
      verified.current = sessionRead.value
      setSession(sessionRead.value)
      setStatus("authenticated")
      setLogoutError(false)
      return
    }
    // Having no session is an answer, not a failure, and it restores nothing:
    // `clear` above already emptied the private half and nothing here puts
    // anything back. Only what the server says publicly has been published.
    const error = sessionRead.reason
    if (
      error instanceof NativeClientError &&
      (error.kind === "unauthorized" || error.kind === "forbidden")
    ) {
      recoveryCsrf.current = null
      setStatus("unauthenticated")
    } else setStatus("unavailable")
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
      // Same pairing as the first load, and the same fail-closed order: a
      // non-native capabilities answer throws before the session is used for
      // anything, and a superseded revalidation is dropped.
      const [caps, value] = await Promise.all([
        loadServerCapabilities(controller.signal),
        loadNativeSession(controller.signal),
      ])
      if (!isCurrent()) return
      if (caps.mode !== "native")
        throw new NativeClientError("invalid_response")
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
      // A refusal is an answer: the reader is not entitled to this any more,
      // and everything scoped to it goes, drafts included.
      if (
        error instanceof NativeClientError &&
        (error.kind === "unauthorized" || error.kind === "forbidden")
      ) {
        clear()
        recoveryCsrf.current = null
        setStatus("unauthenticated")
        return
      }
      // Anything else is not an answer at all — an unreachable server, a
      // dropped connection, a reply that made no sense. Destroying the session
      // here treated silence as a refusal, and a moment of bad wifi took every
      // open draft with it. The session is kept, unproved and unusable, until
      // the check can actually be made.
      setStatus("unverified")
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
  // Proved, and still proved: a check that has not come back leaves the
  // session in place but not in force.
  const proved = status === "authenticated"
  const value = useMemo(
    () => ({
      status,
      verified: proved,
      session,
      capabilities,
      scopeKey,
      signal,
      logoutError,
      refresh,
      retry: revalidateVisibility,
      logout,
      changed,
      handleError,
      handleAuthorityError,
    }),
    [
      status,
      proved,
      session,
      capabilities,
      scopeKey,
      signal,
      logoutError,
      refresh,
      revalidateVisibility,
      logout,
      changed,
      handleError,
      handleAuthorityError,
    ]
  )
  return <Context.Provider value={value}>{children}</Context.Provider>
}
