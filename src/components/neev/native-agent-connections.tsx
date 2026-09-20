"use client"

// The caller's own per-user, per-provider agent connections (spec §5.4).
//
// Everything shown is the server's own answer: connection state and account
// label from `GET /api/neev/agent/connections`, sign-in progress from the
// attempt the server hands back. Credentials never reach the browser.
//
// Honesty rules that shape this file:
//  - `connect` records an intent; it does NOT mean signed in. While an attempt
//    is pending the row says "waiting for sign-in" and offers a working Cancel.
//    Only the server flipping the connection to `connected` says connected.
//  - An attempt that lapses is reported as expired, not quietly dropped.
//  - With no runtime bound every provider reads `disabled` and no action is
//    offered, because none could be exercised.
//  - There is no "Test connection": the server has no route that could settle
//    one, and a control whose state can never resolve would be a lie.

import { useCallback, useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  cancelProviderConnect,
  disconnectProvider,
  loadAgentConnections,
  loadConnectAttempt,
  newRequestKey,
  startProviderConnect,
  type NeevAgentConnection,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAttemptState,
} from "@/lib/neev-agent-sessions-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"

/** How often a pending sign-in is polled. */
const ATTEMPT_POLL_MS = 3_000

type Attempt = {
  provider: NeevAgentProvider
  attemptId: string
  expiresAt: number
  /** Kept so a retried cancel replays instead of queueing a second one. */
  cancelKey: string
}

export function NativeAgentConnectionsPanel({
  connections,
  onRefresh,
  onRequestSettings,
  showSettingsLink = true,
  heading,
  pollMs = ATTEMPT_POLL_MS,
}: {
  /** Supplied when a parent already loaded them; otherwise this loads its own. */
  connections?: NeevAgentConnections | null
  onRefresh?: () => void
  onRequestSettings?: () => void
  showSettingsLink?: boolean
  heading?: string
  /** Attempt poll interval; shortened by tests, never overridden in the app. */
  pollMs?: number
}) {
  const t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const auth = useNativeAuth()
  const owned = connections === undefined
  const [loaded, setLoaded] = useState<NeevAgentConnections | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [refreshToken, setRefreshToken] = useState(0)
  const [busy, setBusy] = useState<NeevAgentProvider | null>(null)
  const [attempt, setAttempt] = useState<Attempt | null>(null)
  const [outcome, setOutcome] = useState<NeevAttemptState | null>(null)
  // Per-provider connect keys: a retry after a lost response replays the same
  // attempt rather than starting a second sign-in.
  const connectKeys = useRef(new Map<NeevAgentProvider, string>())

  const refresh = useCallback(() => {
    setRefreshToken((value) => value + 1)
    onRefresh?.()
  }, [onRefresh])

  useEffect(() => {
    if (!owned) return
    const controller = new AbortController()
    const load = async () => {
      try {
        const wiring = await loadAgentConnections(controller.signal)
        if (controller.signal.aborted) return
        setLoaded(wiring)
        setError(null)
      } catch (cause) {
        if (controller.signal.aborted) return
        setError(cause)
        auth.handleAuthorityError(cause)
      }
    }
    void load()
    return () => controller.abort()
  }, [owned, refreshToken, auth])

  // Poll the pending sign-in until the server settles it. Nothing here decides
  // the outcome; it only reports the state the server returns.
  useEffect(() => {
    if (!attempt) return
    const controller = new AbortController()
    let timer = 0
    const poll = async () => {
      if (controller.signal.aborted) return
      try {
        const state = await loadConnectAttempt(
          attempt.provider,
          attempt.attemptId,
          controller.signal
        )
        if (controller.signal.aborted) return
        if (state.state !== "pending") {
          setOutcome(state.state)
          setAttempt(null)
          // Whatever happened, the connection row is the truth now.
          refresh()
          return
        }
      } catch (cause) {
        if (controller.signal.aborted) return
        setError(cause)
        setAttempt(null)
        auth.handleAuthorityError(cause)
        return
      }
      timer = window.setTimeout(() => void poll(), pollMs)
    }
    void poll()
    return () => {
      controller.abort()
      window.clearTimeout(timer)
    }
  }, [attempt, pollMs, refresh, auth])

  async function connect(provider: NeevAgentProvider) {
    if (busy || !auth.session) return
    setBusy(provider)
    setError(null)
    setOutcome(null)
    const keys = connectKeys.current
    if (!keys.has(provider)) keys.set(provider, newRequestKey())
    try {
      const started = await startProviderConnect(
        provider,
        keys.get(provider) as string,
        auth.session.csrf,
        auth.signal
      )
      if (started.outcome === "pending")
        setAttempt({
          provider,
          attemptId: started.attemptId,
          expiresAt: started.expiresAt,
          cancelKey: newRequestKey(),
        })
      else {
        // Already in its final state; the returned row is the truth.
        keys.delete(provider)
        refresh()
      }
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setBusy(null)
    }
  }

  async function cancel() {
    if (!attempt || busy || !auth.session) return
    setBusy(attempt.provider)
    setError(null)
    try {
      await cancelProviderConnect(
        attempt.provider,
        attempt.attemptId,
        attempt.cancelKey,
        auth.session.csrf,
        auth.signal
      )
      connectKeys.current.delete(attempt.provider)
      setAttempt(null)
      setOutcome("canceled")
      refresh()
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setBusy(null)
    }
  }

  async function disconnect(provider: NeevAgentProvider) {
    if (busy || !auth.session) return
    setBusy(provider)
    setError(null)
    setOutcome(null)
    try {
      await disconnectProvider(
        provider,
        newRequestKey(),
        auth.session.csrf,
        auth.signal
      )
      connectKeys.current.delete(provider)
      // The teardown is queued: the row moves to `disconnecting` and only the
      // server moves it on from there.
      refresh()
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setBusy(null)
    }
  }

  const state = owned ? loaded : connections
  return (
    <section className="space-y-3 rounded-xl border p-5">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-medium">
          {heading ?? t("agentConnections")}
        </h2>
        <Button variant="outline" size="sm" onClick={refresh}>
          {t("refreshConnections")}
        </Button>
      </div>
      <NativeError error={error} />
      {outcome !== null && outcome !== "completed" && (
        <p role="status" className="text-sm text-muted-foreground">
          {t(`attemptState_${outcome}`)}
        </p>
      )}
      {state === null || state === undefined ? (
        <p role="status">{n("loading")}</p>
      ) : (
        <>
          {!state.runtimeEnabled && (
            <p role="status" className="text-sm text-muted-foreground">
              {t("agentsOff")}
            </p>
          )}
          <ul className="space-y-3">
            {state.connections.map((connection) => (
              <li key={connection.provider} className="space-y-1">
                <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
                  <span className="font-medium">{connection.provider}</span>
                  <span className="text-muted-foreground">
                    {attempt?.provider === connection.provider
                      ? t("awaitingSignIn")
                      : t(`connectionState_${connection.state}`)}
                  </span>
                  {connection.accountLabel && (
                    <span className="truncate text-xs text-muted-foreground">
                      {connection.accountLabel}
                    </span>
                  )}
                  {attempt?.provider === connection.provider ? (
                    <span className="text-xs text-muted-foreground">
                      {t("signInExpires")}{" "}
                      <time
                        dateTime={new Date(
                          attempt.expiresAt * 1000
                        ).toISOString()}
                      >
                        {new Date(attempt.expiresAt * 1000).toLocaleString()}
                      </time>
                    </span>
                  ) : (
                    connection.checkedAt !== null && (
                      <span className="text-xs text-muted-foreground">
                        {t("lastChecked")}{" "}
                        <time
                          dateTime={new Date(
                            connection.checkedAt * 1000
                          ).toISOString()}
                        >
                          {new Date(
                            connection.checkedAt * 1000
                          ).toLocaleString()}
                        </time>
                      </span>
                    )
                  )}
                </div>
                <div className="flex flex-wrap gap-2">
                  {actionsFor(connection)}
                </div>
              </li>
            ))}
          </ul>
          {/* The server ships no route that could settle a connection test, so
              no such control is offered (§A of the published contract). */}
          <p className="text-xs text-muted-foreground">
            {t("agentTestUnavailable")}
          </p>
        </>
      )}
      {showSettingsLink && (
        <Link
          href="/settings"
          onClick={(event) => {
            if (onRequestSettings) {
              event.preventDefault()
              onRequestSettings()
            }
          }}
          className="inline-block rounded text-xs underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
        >
          {n("personalAgents")}
        </Link>
      )}
    </section>
  )

  function actionsFor(connection: NeevAgentConnection) {
    const provider = connection.provider
    const working = busy === provider
    if (attempt?.provider === provider)
      return (
        <Button
          size="sm"
          variant="outline"
          disabled={working}
          onClick={() => void cancel()}
        >
          {t("cancelSignIn")}
        </Button>
      )
    // No runtime, or a state the server is already moving through: nothing here
    // could be acted on, so nothing is offered.
    if (
      connection.state === "disabled" ||
      connection.state === "connecting" ||
      connection.state === "disconnecting" ||
      connection.state === "unavailable"
    )
      return null
    return (
      <>
        {connection.state !== "connected" && (
          <Button
            size="sm"
            disabled={working}
            onClick={() => void connect(provider)}
          >
            {t(
              connection.state === "reconnect_required"
                ? "reconnectProvider"
                : "connectProvider"
            )}
          </Button>
        )}
        {connection.state !== "not_signed_in" && (
          <Button
            size="sm"
            variant="outline"
            disabled={working}
            onClick={() => void disconnect(provider)}
          >
            {t("disconnectProvider")}
          </Button>
        )}
      </>
    )
  }
}
