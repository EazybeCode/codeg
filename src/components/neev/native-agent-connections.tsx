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
import { BrowserLink } from "@/components/ui/browser-link"
import {
  cancelProviderConnect,
  disconnectProvider,
  isValidRequestKey,
  loadAgentConnections,
  loadConnectAttempt,
  newRequestKey,
  startProviderConnect,
  testProviderConnection,
  submitConnectCode,
  NEEV_MAX_AUTH_CODE_CHARS,
  NEEV_AGENT_PROVIDERS,
  type NeevApproval,
  type NeevAgentConnection,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAttemptState,
} from "@/lib/neev-agent-sessions-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"

/** How often a pending sign-in is polled while the tab is visible. */
const ATTEMPT_POLL_MS = 3_000
/** Transient poll failures tolerated before the wait is reported as failed. */
const POLL_RETRIES = 3
const RUN_ID = /^[0-9a-f]{32}$/

type Attempt = {
  provider: NeevAgentProvider
  attemptId: string
  expiresAt: number
  /** Kept so a retried cancel replays instead of queueing a second one. */
  cancelKey: string
  /** The connect key that produced it, so a resumed wait can still clear it. */
  connectKey: string
}

/**
 * A sign-in outlives the page that started it, so the ids needed to *resume
 * watching* it are remembered per user. These are ids and an idempotency key —
 * never a credential, never connection state, which always comes from the
 * server. A stale or malformed entry degrades to "no attempt in flight".
 */
function attemptStorageKey(userId: number, orgId: number): string {
  return `neev.agent.attempt.${userId}:${orgId}`
}

function readStoredAttempt(key: string, now: number): Attempt | null {
  if (typeof window === "undefined") return null
  try {
    const raw = window.localStorage.getItem(key)
    if (!raw) return null
    const value = JSON.parse(raw) as Partial<Attempt>
    if (
      !NEEV_AGENT_PROVIDERS.includes(value.provider as NeevAgentProvider) ||
      typeof value.attemptId !== "string" ||
      !RUN_ID.test(value.attemptId) ||
      typeof value.expiresAt !== "number" ||
      !Number.isSafeInteger(value.expiresAt) ||
      typeof value.cancelKey !== "string" ||
      !isValidRequestKey(value.cancelKey) ||
      typeof value.connectKey !== "string" ||
      !isValidRequestKey(value.connectKey) ||
      // Already lapsed: there is nothing left to watch or cancel.
      value.expiresAt * 1000 <= now
    ) {
      window.localStorage.removeItem(key)
      return null
    }
    return value as Attempt
  } catch {
    return null
  }
}

function writeStoredAttempt(key: string, attempt: Attempt | null): void {
  if (typeof window === "undefined") return
  try {
    if (attempt === null) window.localStorage.removeItem(key)
    else window.localStorage.setItem(key, JSON.stringify(attempt))
  } catch {
    /* ignore */
  }
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
  // What the user must do to approve, once the supervisor has produced it.
  // Never persisted: it is re-read from the attempt on every poll.
  const [approval, setApproval] = useState<NeevApproval | null>(null)
  // The provider whose check is in flight, and the row the server returned.
  const [testing, setTesting] = useState<NeevAgentProvider | null>(null)
  const [tested, setTested] = useState<NeevAgentConnection | null>(null)
  const [code, setCode] = useState("")
  const [submitting, setSubmitting] = useState(false)
  // Per-provider connect keys for the current page. A resumed attempt carries
  // its own key, so a reload never loses the ability to replay rather than
  // start a second sign-in.
  const pendingKeys = useRef(new Map<NeevAgentProvider, string>())

  const user = auth.session?.user.id
  const org = auth.session?.organization?.id
  const storageKey =
    user !== undefined && org !== undefined
      ? attemptStorageKey(user, org)
      : null

  const refresh = useCallback(() => {
    setRefreshToken((value) => value + 1)
    onRefresh?.()
  }, [onRefresh])

  // A sign-in survives a reload: pick the attempt back up so the wait is still
  // visible and — crucially — still cancellable.
  useEffect(() => {
    if (!storageKey) return
    const resumed = readStoredAttempt(storageKey, Date.now())
    if (!resumed) return
    pendingKeys.current.set(resumed.provider, resumed.connectKey)
    setAttempt(resumed)
  }, [storageKey])

  const remember = useCallback(
    (next: Attempt | null) => {
      setAttempt(next)
      if (storageKey) writeStoredAttempt(storageKey, next)
    },
    [storageKey]
  )

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
    if (!attempt || !auth.session) return
    const controller = new AbortController()
    let timer = 0
    let failures = 0
    const settle = (state: NeevAttemptState) => {
      setOutcome(state)
      setApproval(null)
      setCode("")
      remember(null)
      // Whatever happened, the connection row is the truth now.
      refresh()
    }
    const poll = async () => {
      if (controller.signal.aborted) return
      // The server reclaims a lapsed attempt; stop waiting on it either way
      // rather than polling something that can no longer settle.
      if (attempt.expiresAt * 1000 <= Date.now()) {
        settle("expired")
        return
      }
      // A hidden tab has nobody watching: keep the loop alive, skip the work.
      if (document.visibilityState !== "visible") {
        timer = window.setTimeout(() => void poll(), pollMs)
        return
      }
      try {
        const state = await loadConnectAttempt(
          attempt.provider,
          attempt.attemptId,
          controller.signal
        )
        if (controller.signal.aborted) return
        failures = 0
        if (state.state !== "pending") {
          settle(state.state)
          return
        }
        setApproval(state.approval)
      } catch (cause) {
        if (controller.signal.aborted) return
        auth.handleAuthorityError(cause)
        failures += 1
        // A blip should not abandon a live sign-in, but an endpoint that keeps
        // failing must not be hammered forever either.
        if (failures > POLL_RETRIES) {
          setError(cause)
          settle("failed")
          return
        }
        timer = window.setTimeout(() => void poll(), pollMs * 2 ** failures)
        return
      }
      timer = window.setTimeout(() => void poll(), pollMs)
    }
    void poll()
    return () => {
      controller.abort()
      window.clearTimeout(timer)
    }
  }, [attempt, pollMs, refresh, remember, auth])

  async function connect(provider: NeevAgentProvider) {
    if (busy || !auth.session) return
    setBusy(provider)
    setError(null)
    setOutcome(null)
    setApproval(null)
    setCode("")
    // One key per in-flight sign-in: a retry after a lost response replays the
    // SAME attempt instead of starting a second one.
    const connectKey = pendingKeys.current.get(provider) ?? newRequestKey()
    pendingKeys.current.set(provider, connectKey)
    try {
      const started = await startProviderConnect(
        provider,
        connectKey,
        auth.session.csrf,
        auth.signal
      )
      if (started.outcome === "pending")
        remember({
          provider,
          attemptId: started.attemptId,
          expiresAt: started.expiresAt,
          cancelKey: newRequestKey(),
          connectKey,
        })
      else {
        // Already in its final state; the returned row is the truth.
        pendingKeys.current.delete(provider)
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
      pendingKeys.current.delete(attempt.provider)
      remember(null)
      setApproval(null)
      setCode("")
      setOutcome("canceled")
      refresh()
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setBusy(null)
    }
  }

  async function submitCode() {
    if (!attempt || !approval || submitting || !auth.session) return
    const trimmed = code.trim()
    if (!trimmed) return
    setSubmitting(true)
    setError(null)
    try {
      const result = await submitConnectCode(
        attempt.provider,
        attempt.attemptId,
        trimmed,
        auth.session.csrf,
        auth.signal
      )
      // The code is forwarded, never kept — here or on the server.
      setCode("")
      if (result.state !== "pending") {
        setOutcome(result.state)
        setApproval(null)
        remember(null)
        refresh()
      }
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setSubmitting(false)
    }
  }

  async function test(provider: NeevAgentProvider) {
    if (busy || testing || !auth.session) return
    setTesting(provider)
    setError(null)
    setTested(null)
    try {
      // What comes back IS the outcome; nothing here decides the state.
      setTested(
        await testProviderConnection(
          provider,
          newRequestKey(),
          auth.session.csrf,
          auth.signal
        )
      )
      refresh()
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setTesting(null)
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
      pendingKeys.current.delete(provider)
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
      <div className="flex flex-wrap items-center justify-between gap-3">
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
              <li key={connection.provider} className="space-y-2">
                <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
                  <span className="font-medium">{connection.provider}</span>
                  <span className="text-muted-foreground">
                    {attempt?.provider === connection.provider
                      ? t("awaitingSignIn")
                      : testing === connection.provider
                        ? t("testingConnection")
                        : t(
                            `connectionState_${
                              tested?.provider === connection.provider
                                ? tested.state
                                : connection.state
                            }`
                          )}
                  </span>
                  {connection.accountLabel && (
                    <span className="min-w-0 truncate text-xs text-muted-foreground">
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
                {attempt?.provider === connection.provider &&
                  approval !== null && (
                    <ApprovalPrompt
                      approval={approval}
                      code={code}
                      submitting={submitting}
                      onCode={setCode}
                      onSubmit={() => void submitCode()}
                    />
                  )}
                <div className="flex flex-wrap gap-2">
                  {actionsFor(connection)}
                </div>
              </li>
            ))}
          </ul>
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
    // Every provider renders the same verbs, so the accessible name has to say
    // which one — otherwise two identical "Connect" buttons are ambiguous to a
    // screen reader and to keyboard navigation.
    const named = (
      key:
        | "connectProvider"
        | "reconnectProvider"
        | "disconnectProvider"
        | "cancelSignIn"
        | "testConnection"
    ) => t(`${key}Named`, { name: provider })
    if (attempt?.provider === provider)
      return (
        <Button
          size="sm"
          variant="outline"
          disabled={working}
          aria-label={named("cancelSignIn")}
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
    const reconnecting = connection.state === "reconnect_required"
    return (
      <>
        {/* Only a connection that exists can be checked; `not_signed_in` has
            nothing to test yet. */}
        {connection.state !== "not_signed_in" && (
          <Button
            size="sm"
            variant="outline"
            disabled={working || testing !== null}
            aria-label={named("testConnection")}
            onClick={() => void test(provider)}
          >
            {testing === provider
              ? t("testingConnection")
              : t("testConnection")}
          </Button>
        )}
        {connection.state !== "connected" && (
          <Button
            size="sm"
            disabled={working}
            aria-label={named(
              reconnecting ? "reconnectProvider" : "connectProvider"
            )}
            onClick={() => void connect(provider)}
          >
            {t(reconnecting ? "reconnectProvider" : "connectProvider")}
          </Button>
        )}
        {connection.state !== "not_signed_in" && (
          <Button
            size="sm"
            variant="outline"
            disabled={working}
            aria-label={named("disconnectProvider")}
            onClick={() => void disconnect(provider)}
          >
            {t("disconnectProvider")}
          </Button>
        )}
      </>
    )
  }
}

/**
 * What the user actually does to approve. The verification URL is opened in
 * their own browser — Neev never drives the provider's sign-in page and never
 * sees the credential, only the code the user chooses to paste back.
 */
function ApprovalPrompt({
  approval,
  code,
  submitting,
  onCode,
  onSubmit,
}: {
  approval: NeevApproval
  code: string
  submitting: boolean
  onCode: (value: string) => void
  onSubmit: () => void
}) {
  const t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  return (
    <div className="space-y-2 rounded-lg border bg-muted/40 p-3">
      <p className="text-xs text-muted-foreground">
        {t(approval.kind === "device" ? "signInDeviceHint" : "signInCodeHint")}
      </p>
      {/* The user approves in their OWN browser; Neev never drives the
          provider's sign-in page. `BrowserLink` is the only link form that
          actually opens in the desktop webview. */}
      <BrowserLink
        href={approval.verificationUrl}
        className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
      >
        {t("openSignInPage")}
      </BrowserLink>
      {approval.kind === "device" ? (
        <p className="text-sm">
          <span className="text-muted-foreground">{t("signInCodeLabel")} </span>
          <code className="rounded bg-background px-1.5 py-0.5 font-mono tracking-widest">
            {approval.userCode}
          </code>
        </p>
      ) : (
        approval.needsCode && (
          <form
            className="flex flex-wrap items-end gap-2"
            onSubmit={(event) => {
              event.preventDefault()
              onSubmit()
            }}
          >
            <label className="min-w-0 flex-1 space-y-1">
              <span className="block text-xs font-medium">
                {t("pasteCodeLabel")}
              </span>
              <input
                value={code}
                autoComplete="off"
                spellCheck={false}
                maxLength={NEEV_MAX_AUTH_CODE_CHARS}
                disabled={submitting}
                onChange={(event) => onCode(event.target.value)}
                className="w-full rounded-md border bg-background px-3 py-2 font-mono text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
              />
            </label>
            <Button
              type="submit"
              size="sm"
              disabled={submitting || !code.trim()}
            >
              {submitting ? n("working") : t("submitCode")}
            </Button>
          </form>
        )
      )}
    </div>
  )
}
