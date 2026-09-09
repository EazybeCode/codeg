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
  TeamClientError,
  loadTeamSession,
  logoutTeamSession,
  type TeamSession,
} from "@/lib/team-client"

type TeamAuthStatus =
  | "idle"
  | "loading"
  | "authenticated"
  | "unauthenticated"
  | "unavailable"

export interface TeamAuthContextValue {
  status: TeamAuthStatus
  session: TeamSession | null
  scopeKey: string | null
  logoutError: boolean
  activate: () => void
  deactivate: () => void
  refresh: () => Promise<void>
  logout: () => Promise<void>
}

const TeamAuthContext = createContext<TeamAuthContextValue | null>(null)
const TEAM_AUTH_CHANNEL = "neev-team-auth"

export function notifyTeamAuthChanged(): void {
  if (typeof BroadcastChannel === "undefined") return
  const channel = new BroadcastChannel(TEAM_AUTH_CHANNEL)
  channel.postMessage({ type: "invalidate" })
  channel.close()
}

export function useTeamAuth(): TeamAuthContextValue {
  const value = useContext(TeamAuthContext)
  if (!value)
    throw new Error("useTeamAuth must be used within TeamAuthProvider")
  return value
}

export function TeamAuthProvider({ children }: { children: React.ReactNode }) {
  const router = useRouter()
  const [status, setStatus] = useState<TeamAuthStatus>("idle")
  const [session, setSession] = useState<TeamSession | null>(null)
  const [logoutError, setLogoutError] = useState(false)
  const activeRef = useRef(false)
  const generationRef = useRef(0)
  const bootstrapRef = useRef<AbortController | null>(null)
  const recoveryCsrfRef = useRef<string | null>(null)

  const clearCurrent = useCallback(() => {
    bootstrapRef.current?.abort()
    bootstrapRef.current = null
    generationRef.current += 1
    setSession(null)
  }, [])

  const refresh = useCallback(async () => {
    if (!activeRef.current) return
    clearCurrent()
    const generation = generationRef.current
    const controller = new AbortController()
    bootstrapRef.current = controller
    setStatus("loading")
    setLogoutError(false)
    try {
      const next = await loadTeamSession(controller.signal)
      if (controller.signal.aborted || generation !== generationRef.current)
        return
      recoveryCsrfRef.current = next.csrf
      setSession(next)
      setStatus("authenticated")
    } catch (error) {
      if (controller.signal.aborted || generation !== generationRef.current)
        return
      setSession(null)
      if (error instanceof TeamClientError && error.kind === "unauthorized") {
        recoveryCsrfRef.current = null
        setStatus("unauthenticated")
      } else if (error instanceof TeamClientError && error.kind === "aborted") {
        return
      } else {
        setStatus("unavailable")
      }
    } finally {
      if (bootstrapRef.current === controller) bootstrapRef.current = null
    }
  }, [clearCurrent])

  const activate = useCallback(() => {
    if (activeRef.current) return
    activeRef.current = true
    void refresh()
  }, [refresh])

  const deactivate = useCallback(() => {
    activeRef.current = false
    clearCurrent()
    setStatus("idle")
    setLogoutError(false)
  }, [clearCurrent])

  const logout = useCallback(async () => {
    const csrf = session?.csrf ?? recoveryCsrfRef.current
    if (!csrf) {
      router.replace("/login")
      return
    }
    setLogoutError(false)
    try {
      await logoutTeamSession(csrf)
      activeRef.current = false
      clearCurrent()
      recoveryCsrfRef.current = null
      setStatus("unauthenticated")
      notifyTeamAuthChanged()
      router.replace("/login")
    } catch {
      setLogoutError(true)
    }
  }, [clearCurrent, router, session?.csrf])

  useEffect(() => {
    const onResume = () => {
      if (activeRef.current) void refresh()
    }
    const onVisibility = () => {
      if (document.visibilityState === "visible") onResume()
    }
    window.addEventListener("focus", onResume)
    document.addEventListener("visibilitychange", onVisibility)

    let channel: BroadcastChannel | null = null
    if (typeof BroadcastChannel !== "undefined") {
      channel = new BroadcastChannel(TEAM_AUTH_CHANNEL)
      channel.addEventListener("message", onResume)
    }
    return () => {
      activeRef.current = false
      clearCurrent()
      window.removeEventListener("focus", onResume)
      document.removeEventListener("visibilitychange", onVisibility)
      channel?.removeEventListener("message", onResume)
      channel?.close()
    }
  }, [clearCurrent, refresh])

  const scopeKey = session
    ? `${session.scope}:${session.organization.id}:${session.user.id}`
    : null
  const value = useMemo<TeamAuthContextValue>(
    () => ({
      status,
      session,
      scopeKey,
      logoutError,
      activate,
      deactivate,
      refresh,
      logout,
    }),
    [
      activate,
      deactivate,
      logout,
      logoutError,
      refresh,
      scopeKey,
      session,
      status,
    ]
  )

  return (
    <TeamAuthContext.Provider value={value}>
      {children}
    </TeamAuthContext.Provider>
  )
}
