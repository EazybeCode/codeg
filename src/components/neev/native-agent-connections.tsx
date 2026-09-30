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
//  - "Test connection" is offered only because the server can actually settle
//    one. `POST /api/neev/agent/connections/{provider}/test` adopts the status
//    epoch, admits a check against a specific generation and storage handle,
//    has the supervisor verify the stored credential against the provider, and
//    re-proves the row has not moved before recording the observation. A check
//    it cannot perform is a 503, never a verified-because-we-asked. The row
//    this returns IS the outcome; nothing here infers one.
//    (Live 2026-09-23: two checks on a connected Codex row advanced the
//    server's `checked_at` to 18:38:13Z and 18:38:30Z. Note the check writes no
//    operation row, so an absent operation is not evidence it did not run.)

import { useCallback, useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { BrowserLink } from "@/components/ui/browser-link"
import {
  callbackFailure,
  callbackQueryFromAddress,
  cancelProviderConnect,
  disconnectProvider,
  isValidRequestKey,
  loadAgentConnections,
  loadConnectAttempt,
  newRequestKey,
  startProviderConnect,
  testProviderConnection,
  submitConnectCallback,
  submitConnectCode,
  NEEV_MAX_AUTH_CODE_CHARS,
  NEEV_AGENT_PROVIDERS,
  NEEV_CORE_AGENT_PROVIDERS,
  type NeevApproval,
  type NeevAgentConnection,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAttemptState,
  type NeevConnectMethod,
} from "@/lib/neev-agent-sessions-client"
import {
  armSigninExtension,
  disarmSigninExtension,
  discoverSigninExtension,
  type SigninChannel,
} from "@/lib/neev-signin-extension"
import { openUrl } from "@/lib/platform"
import { NativeClientError } from "@/lib/neev-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"

/** How often a pending sign-in is polled while the tab is visible. */
const ATTEMPT_POLL_MS = 3_000
/** Transient poll failures tolerated before the wait is reported as failed. */
const POLL_RETRIES = 3
/** How long a reconnect waits for its own teardown before giving the row back. */
const TEARDOWN_ROUNDS = 10
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

/**
 * The prerequisite Codex does not mention until after a failed sign-in.
 *
 * Stated up front, beside the button that starts it. It does NOT claim to know
 * whether the setting is on — Neev can neither read nor change it.
 */
function CodexSetupHelp() {
  const t = useTranslations("NativeContent")
  const link =
    "inline-block rounded text-xs underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
  return (
    <div className="space-y-1.5 rounded-lg border bg-muted/30 px-3 py-2">
      <p className="text-xs text-muted-foreground">{t("codexSetupHint")}</p>
      {/* BrowserLink is the only link form that opens outside the webview. */}
      <BrowserLink
        href="https://chatgpt.com/#settings/Security"
        className={link}
      >
        {t("codexSetupOpen")}
      </BrowserLink>
      <details>
        <summary className="cursor-pointer text-xs font-medium">
          {t("codexSetupTitle")}
        </summary>
        <div className="mt-2 space-y-2">
          <p className="text-xs text-muted-foreground">{t("codexSetupBody")}</p>
          <p className="text-xs text-muted-foreground">
            {t("codexSetupAdmin")}
          </p>
          <BrowserLink
            href="https://learn.chatgpt.com/docs/auth#login-on-headless-devices"
            className={link}
          >
            {t("codexSetupDocs")}
          </BrowserLink>
        </div>
      </details>
    </div>
  )
}

export function NativeAgentConnectionsPanel({
  connections,
  providerFilter,
  onRefresh,
  onRequestSettings,
  showSettingsLink = true,
  heading,
  pollMs = ATTEMPT_POLL_MS,
}: {
  /** Supplied when a parent already loaded them; otherwise this loads its own. */
  providerFilter?: NeevAgentProvider
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
  // Why the last pasted callback did not go through, when the attempt is
  // still alive (contract v2 §3). Said beside the paste box, never thrown.
  const [callbackIssue, setCallbackIssue] = useState<CallbackIssue | null>(null)
  // A delivery the server could not take right now (503): the SAME query under
  // the SAME request key, so "Try again" replays it rather than delivering a
  // second time. In memory only, and dropped with the attempt.
  const retryCallback = useRef<{ query: string; key: string } | null>(null)
  // The pasted address carried the provider's `error=`: the user declined on
  // the provider's page, so a failed attempt is said as a cancellation.
  const declined = useRef(false)
  const [cancelledByUser, setCancelledByUser] = useState(false)
  // Polling gave up, but the sign-in did NOT fail: the server was never heard
  // from. The attempt stays, so Cancel stays, and "Refresh status" resumes.
  const [stalled, setStalled] = useState(false)
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
    // Also the explicit way out of a stalled poll: asking for status is asking
    // for the wait to be picked back up.
    setStalled(false)
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
      setStalled(false)
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
    if (!attempt || !auth.session || stalled) return
    const controller = new AbortController()
    let timer = 0
    let failures = 0
    const settle = (state: NeevAttemptState) => {
      setOutcome(state)
      setApproval(null)
      setCode("")
      setCallbackIssue(null)
      retryCallback.current = null
      setCancelledByUser(state === "failed" && declined.current)
      declined.current = false
      disarmSigninExtension(attempt.attemptId)
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
        //
        // Stop polling — do NOT declare the sign-in failed. Only the server
        // decides that, and it has said nothing. Calling this "failed" cleared
        // the attempt and its stored copy, so the row went to "connecting" with
        // no Cancel and a reload had nothing to restore, while the server still
        // held a live attempt. The attempt is kept, so Cancel stays available
        // and a reload resumes; "Refresh status" restarts the wait.
        if (failures > POLL_RETRIES) {
          setError(cause)
          setStalled(true)
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
  }, [attempt, pollMs, refresh, remember, auth, stalled])

  async function connect(provider: NeevAgentProvider) {
    // Only one sign-in at a time, and not only because this panel holds one
    // attempt: the supervisor's login slot is shared, so a second provider
    // started now would overwrite the attempt — and its stored copy — while the
    // first is still waiting on the user's authorization, stranding it with
    // nothing left to cancel it by.
    if (busy || attempt || !auth.session) return
    setBusy(provider)
    setError(null)
    setOutcome(null)
    setCancelledByUser(false)
    setApproval(null)
    setCode("")
    try {
      await beginSignIn(provider)
    } catch (cause) {
      forgetSpentKey(provider, cause)
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setBusy(null)
    }
  }

  /**
   * Drop this provider's connect key on the ONE answer that proves it is spent.
   *
   * The key is an idempotency key, so it may only be retired when the previous
   * attempt's fate is known. `409` is that proof and nothing else is: it is the
   * application's own reply, and it means the server matched this key to a recorded
   * connect operation whose attempt is no longer replayable — which is exactly what a
   * sign-in the server started and then cancelled itself leaves behind. Without this,
   * one such failure poisons the key permanently: every later click replays it,
   * earns 409 again, and the panel reports "This information changed, or the sign-in
   * method is linked to another account" for what is not an account problem at all.
   *
   * Everything else keeps the key, deliberately. A 502/503/504 may come from the
   * reverse proxy AFTER the backend admitted the request, so it is no evidence the
   * attempt died; a timeout, abort or transport failure carries no answer at all.
   * In each of those the attempt may be live, and replaying the same key resumes it
   * instead of stranding it with nothing left to cancel it by.
   */
  function forgetSpentKey(provider: NeevAgentProvider, cause: unknown): void {
    if (
      cause instanceof NativeClientError &&
      cause.status === 409 &&
      cause.kind === "conflict"
    )
      pendingKeys.current.delete(provider)
  }

  /** Start a sign-in. The caller owns `busy` and the error reporting. */
  async function beginSignIn(
    provider: NeevAgentProvider,
    method?: NeevConnectMethod
  ) {
    if (!auth.session) return
    // One key per in-flight sign-in: a retry after a lost response replays the
    // SAME attempt instead of starting a second one.
    const connectKey = pendingKeys.current.get(provider) ?? newRequestKey()
    pendingKeys.current.set(provider, connectKey)
    const started = await startProviderConnect(
      provider,
      connectKey,
      auth.session.csrf,
      auth.signal,
      // Claude and Codex sign in through the CLI's own browser login first;
      // the server reads an absent method as the legacy flow.
      method ??
        (provider === "claude" || provider === "codex" ? "browser" : undefined)
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
  }

  /**
   * Reconnect is a teardown followed by a sign-in, never a sign-in on its own.
   *
   * A `reconnect_required` row still holds the old credential, and the server
   * rightly refuses to start a second sign-in over it — connecting straight
   * away returns 409 and the user is left doing Disconnect and Connect by hand.
   * So: disconnect, wait for the server to actually reach `not_signed_in`, and
   * only then start. The wait is what keeps this from racing the queued
   * teardown; the backend guard is respected rather than worked around.
   */
  async function reconnect(provider: NeevAgentProvider) {
    if (busy || attempt || !auth.session) return
    setBusy(provider)
    setError(null)
    setOutcome(null)
    setCancelledByUser(false)
    setApproval(null)
    setCode("")
    try {
      const receipt = await disconnectProvider(
        provider,
        newRequestKey(),
        auth.session.csrf,
        auth.signal
      )
      pendingKeys.current.delete(provider)
      // The receipt carries the row as it stands, so a teardown that already
      // completed needs no poll at all.
      const cleared =
        receipt.connection.state === "not_signed_in" ||
        (await waitForSignedOut(provider))
      if (cleared) await beginSignIn(provider)
      // Otherwise the teardown is still running. The row says `disconnecting`,
      // which is the truth; starting anyway would just earn the 409 this exists
      // to avoid. The next refresh offers Connect.
      else refresh()
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
      refresh()
    } finally {
      setBusy(null)
    }
  }

  /**
   * Poll until the server says the credential is gone. Bounded: the teardown is
   * queued work, and this holds the row busy while it runs.
   */
  async function waitForSignedOut(
    provider: NeevAgentProvider,
    passing: readonly string[] = ["disconnecting"]
  ) {
    for (let round = 0; round < TEARDOWN_ROUNDS; round += 1) {
      const wiring = await loadAgentConnections(auth.signal)
      // Keep the row honest while we wait rather than freezing the old state.
      if (owned) setLoaded(wiring)
      const row = wiring.connections.find(
        (candidate) => candidate.provider === provider
      )
      if (!row || row.state === "not_signed_in") return true
      if (!passing.includes(row.state)) return false
      await new Promise((resolve) => setTimeout(resolve, pollMs))
    }
    return false
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
      disarmSigninExtension(attempt.attemptId)
      remember(null)
      setApproval(null)
      setCode("")
      setCallbackIssue(null)
      retryCallback.current = null
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
        return
      }
      // Still pending, but the server may already have taken the code and
      // dropped the challenge. Follow it: leaving the paste prompt up would
      // invite the user to submit a code nothing is waiting for.
      setApproval(result.approval)
      if (!result.approval) setOutcome("pending")
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setSubmitting(false)
    }
  }

  /**
   * Hand over the provider's loopback redirect the user pasted. The server
   * answers with the attempt, exactly like a pasted code: still pending (the
   * CLI is finishing) or settled. Only the poll — never this — says Connected.
   */
  async function submitCallback(query: string, replay = false) {
    if (!attempt || !approval || submitting || !auth.session) return
    // One key per delivery; a retry after "unavailable" reuses it so the server
    // treats it as the same delivery (contract v2 §3).
    const key =
      replay && retryCallback.current?.query === query
        ? retryCallback.current.key
        : newRequestKey()
    retryCallback.current = null
    declined.current = new URLSearchParams(query).has("error")
    setSubmitting(true)
    setError(null)
    setCallbackIssue(null)
    try {
      const result = await submitConnectCallback(
        attempt.provider,
        attempt.attemptId,
        key,
        query,
        auth.session.csrf,
        auth.signal
      )
      if (result.state !== "pending") {
        setOutcome(result.state)
        setCancelledByUser(result.state === "failed" && declined.current)
        setApproval(null)
        disarmSigninExtension(attempt.attemptId)
        remember(null)
        refresh()
        return
      }
      setApproval(result.approval)
    } catch (cause) {
      const failure = callbackFailure(cause)
      // 409: this attempt is no longer taking a callback — most often because
      // the extension's relay already delivered one. Not an error to show:
      // re-read the attempt, and the poll settles what actually happened.
      if (failure === "conflict") refresh()
      else if (failure === "unavailable") {
        retryCallback.current = { query, key }
        setCallbackIssue("unavailable")
      } else if (
        failure === "rejected" ||
        failure === "gone" ||
        failure === "limited"
      )
        setCallbackIssue(failure)
      else {
        setError(cause)
        auth.handleAuthorityError(cause)
      }
    } finally {
      setSubmitting(false)
    }
  }

  /**
   * Codex's fallback: give up the browser sign-in and start a device-code one.
   * Cancel first — the supervisor's login slot is shared, so a second sign-in
   * over a live one would strand it — then wait for the row to clear.
   */
  async function switchToDevice() {
    if (!attempt || busy || !auth.session) return
    const provider = attempt.provider
    setBusy(provider)
    setError(null)
    setCallbackIssue(null)
    retryCallback.current = null
    try {
      await cancelProviderConnect(
        provider,
        attempt.attemptId,
        attempt.cancelKey,
        auth.session.csrf,
        auth.signal
      )
      disarmSigninExtension(attempt.attemptId)
      pendingKeys.current.delete(provider)
      remember(null)
      setApproval(null)
      setCode("")
      if (await waitForSignedOut(provider, ["connecting", "disconnecting"]))
        await beginSignIn(provider, "device")
      else refresh()
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
      refresh()
    } finally {
      setBusy(null)
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

  /**
   * When the last check settled, by the server's clock.
   *
   * A settled check returns the row as it now stands, so its time is fresher
   * than the list's until the refresh lands.
   */
  function checkedAt(connection: NeevAgentConnection) {
    return (tested?.provider === connection.provider ? tested : connection)
      .checkedAt
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
          {cancelledByUser
            ? t("signinCancelled")
            : t(`attemptState_${outcome}`)}
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
            {state.connections
              .filter(
                (connection) =>
                  !providerFilter || connection.provider === providerFilter
              )
              .map((connection) => (
                <li key={connection.provider} className="space-y-2">
                  <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
                    <span className="font-medium">
                      {label(connection.provider)}
                    </span>
                    {/* Only rows the server lists reach here, so this marks an
                        offered provider — never advertises one. */}
                    {!NEEV_CORE_AGENT_PROVIDERS.includes(
                      connection.provider
                    ) && (
                      <Badge
                        variant="secondary"
                        className="self-center"
                        title={t("providerBetaHint")}
                      >
                        {t("providerBeta")}
                      </Badge>
                    )}
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
                    {/* A check that finds nothing wrong leaves every other word on
                      this row identical, so without the time it reads as a
                      button that does nothing. The server's own `checkedAt` is
                      what moves; `role="status"` announces it. */}
                    {checkedAt(connection) !== null && (
                      <span
                        role="status"
                        className="text-xs text-muted-foreground"
                      >
                        {n("connectionCheckedAt", {
                          time: new Date(
                            checkedAt(connection)! * 1000
                          ).toLocaleTimeString(),
                        })}
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
                    ) : null}
                  </div>
                  {/* The device-code prerequisite, beside the button that may
                      need it — but not over a browser sign-in, which does not. */}
                  {connection.provider === "codex" &&
                    connection.state !== "connected" &&
                    connection.state !== "disabled" &&
                    connection.state !== "unavailable" &&
                    !(
                      attempt?.provider === "codex" &&
                      approval?.kind === "browser_callback"
                    ) && <CodexSetupHelp />}
                  {attempt?.provider === connection.provider &&
                    approval?.kind === "browser_callback" && (
                      <CallbackApprovalPrompt
                        approval={approval}
                        attempt={attempt}
                        issue={callbackIssue}
                        onRetry={
                          retryCallback.current
                            ? () =>
                                void submitCallback(
                                  retryCallback.current!.query,
                                  true
                                )
                            : undefined
                        }
                        submitting={submitting}
                        busy={busy !== null}
                        onCallback={(query) => void submitCallback(query)}
                        onDevice={
                          attempt.provider === "codex"
                            ? () => void switchToDevice()
                            : undefined
                        }
                        code={code}
                        onCode={setCode}
                        onSubmitCode={() => void submitCode()}
                      />
                    )}
                  {attempt?.provider === connection.provider &&
                    approval !== null &&
                    approval.kind !== "browser_callback" && (
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

  /** Friendly name: the rows and accessible names used the raw key. */
  function label(provider: NeevAgentProvider) {
    return t(`${provider}Label`)
  }
  function actionsFor(connection: NeevAgentConnection) {
    const provider = connection.provider
    const working = busy === provider
    // Another provider's sign-in is still waiting for the user. Starting one
    // here would replace it, so the verb that would do that is offered but not
    // usable; cancelling or cleaning up stays available on every row.
    const otherSignIn = attempt !== null && attempt.provider !== provider
    // Every provider renders the same verbs, so the accessible name has to say
    // which one — otherwise two identical "Connect" buttons are ambiguous to a
    // screen reader and to keyboard navigation.
    const named = (
      key:
        | "connectProvider"
        | "reconnectProvider"
        | "disconnectProvider"
        | "cancelSignIn"
        | "testConnection" // The accessible name keeps the provider key the tests and
    ) =>
      // existing semantics use; only the visible row name is friendlier.
      t(`${key}Named`, { name: provider })
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
    // A sign-in the server still holds but this browser knows nothing about:
    // started elsewhere, or stranded here by a page that lost its attempt. With
    // no attempt id there is nothing to cancel, so the row would otherwise be
    // permanently actionless. Disconnect is the way out — `intents::disconnect`
    // accepts `connecting` and cancels the pending attempt with it. Nothing
    // else is offered: there is no connection yet to test, and starting a
    // second sign-in over a live one is not a recovery.
    if (connection.state === "connecting")
      return (
        <Button
          size="sm"
          variant="outline"
          disabled={working}
          aria-label={named("disconnectProvider")}
          onClick={() => void disconnect(provider)}
        >
          {t("disconnectProvider")}
        </Button>
      )
    // No runtime, or a state the server is already moving through: nothing here
    // could be acted on, so nothing is offered.
    if (
      connection.state === "disabled" ||
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
            disabled={working || otherSignIn}
            aria-label={named(
              reconnecting ? "reconnectProvider" : "connectProvider"
            )}
            onClick={() =>
              void (reconnecting ? reconnect(provider) : connect(provider))
            }
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
  const t = useTranslations("NativeContent")
  // The outcome is explicit, and it is tagged with the code it was for: a
  // clipboard promise that settles after the attempt has produced a FRESH code
  // must not report "Copied" against that new one.
  const [copy, setCopy] = useState<{
    code: string
    status: "copied" | "failed"
  } | null>(null)
  const userCode = approval.kind === "device" ? approval.userCode : null
  const copyStatus = copy && copy.code === userCode ? copy.status : null
  return (
    <div className="space-y-2 rounded-lg border bg-muted/40 p-3">
      <p className="text-xs text-muted-foreground">
        {t(
          approval.kind === "device"
            ? "signInDeviceHint"
            : approval.kind === "browser"
              ? "signInBrowserHint"
              : "signInCodeHint"
        )}
      </p>
      {/* The user approves in their OWN browser; Neev never drives the
          provider's sign-in page. `BrowserLink` is the only link form that
          actually opens in the desktop webview. */}
      {approval.kind !== "device" && (
        <BrowserLink
          href={approval.verificationUrl}
          className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("openSignInPage")}
        </BrowserLink>
      )}
      {approval.kind === "device" ? (
        <div className="space-y-1">
          <p className="text-xs font-medium">{t("stepCopyCode")}</p>
          <p className="text-sm">
            <span className="text-muted-foreground">
              {t("signInCodeLabel")}{" "}
            </span>
            <code className="rounded bg-background px-1.5 py-0.5 font-mono tracking-widest">
              {approval.userCode}
            </code>
            <Button
              variant="ghost"
              size="sm"
              className="ms-2 h-7 px-2"
              onClick={() => {
                const target = approval.userCode
                setCopy(null)
                // Inside the try, because `navigator.clipboard` is undefined
                // in a non-secure context and reading `.writeText` off it
                // throws before any `.catch` could see it. A refused clipboard
                // is said out loud rather than leaving a button that looked
                // like it worked.
                void (async () => {
                  try {
                    await navigator.clipboard.writeText(target)
                    setCopy({ code: target, status: "copied" })
                  } catch {
                    setCopy({ code: target, status: "failed" })
                  }
                })()
              }}
            >
              {copyStatus === "copied" ? t("codeCopied") : t("copyCode")}
            </Button>
          </p>
          {copyStatus === "failed" && (
            <p role="alert" className="text-xs text-destructive">
              {t("copyCodeFailed")}
            </p>
          )}
          <p className="pt-1 text-xs font-medium">{t("stepApprove")}</p>
          <BrowserLink
            href={approval.verificationUrl}
            className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("openSignInPage")}
          </BrowserLink>
          {/* Retrying is the existing Connect: this only says so. Nothing here
              renews an attempt or touches its lifecycle. */}
          <p className="text-xs text-muted-foreground">
            {t("codeExpiredHint")}
          </p>
        </div>
      ) : (
        // A browser approval has nothing to paste: the CLI notices the approval
        // itself, and the poll above reports it.
        approval.kind === "authorization_code" &&
        approval.needsCode && (
          <PasteCodeForm
            code={code}
            submitting={submitting}
            onCode={onCode}
            onSubmit={onSubmit}
          />
        )
      )}
    </div>
  )
}

/** Today's paste-a-code form (Claude, Gemini CLI), shared by both prompts. */
function PasteCodeForm({
  code,
  submitting,
  onCode,
  onSubmit,
}: {
  code: string
  submitting: boolean
  onCode: (value: string) => void
  onSubmit: () => void
}) {
  const t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  return (
    <form
      className="flex flex-wrap items-end gap-2"
      onSubmit={(event) => {
        event.preventDefault()
        onSubmit()
      }}
    >
      <label className="min-w-0 flex-1 space-y-1">
        <span className="block text-xs font-medium">{t("pasteCodeLabel")}</span>
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
      <Button type="submit" size="sm" disabled={submitting || !code.trim()}>
        {submitting ? n("working") : t("submitCode")}
      </Button>
    </form>
  )
}

/**
 * The primary Claude/Codex sign-in: the CLI's own browser login.
 *
 * One button does the whole thing. With the Neev extension answering, it is
 * armed for exactly this attempt's loopback address and the provider's page
 * opens; the browser comes back through `/agents/signin` and the poll says
 * Connected. Without the extension the provider's redirect lands on a tab that
 * cannot connect, so the paste box for that address is shown up front. With it,
 * every fallback waits behind one disclosure rather than crowding the prompt.
 */
type CallbackIssue = "rejected" | "gone" | "limited" | "unavailable"

const CALLBACK_ISSUE_KEYS = {
  rejected: "callbackRejected",
  gone: "signinReturnNotAvailable",
  limited: "callbackLimited",
  unavailable: "callbackUnavailable",
} as const

function CallbackApprovalPrompt({
  approval,
  attempt,
  issue,
  onRetry,
  submitting,
  busy,
  onCallback,
  onDevice,
  code,
  onCode,
  onSubmitCode,
}: {
  approval: Extract<NeevApproval, { kind: "browser_callback" }>
  attempt: Attempt
  issue: CallbackIssue | null
  /** Replay the last delivery under its own key (after "unavailable"). */
  onRetry?: () => void
  submitting: boolean
  busy: boolean
  onCallback: (query: string) => void
  /** Codex only: restart as a device-code sign-in. */
  onDevice?: () => void
  code: string
  onCode: (value: string) => void
  onSubmitCode: () => void
}) {
  const t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  // Which way the extension answered — the page bridge (any extension id) or
  // the legacy id — or that nothing did.
  const [extension, setExtension] = useState<
    "checking" | SigninChannel | "absent"
  >("checking")
  // The extension armed the relay but did not open the page (or could not be
  // asked), and our own `window.open` ran after an await, where a popup
  // blocker may have eaten it: offer the page as a plain link too.
  const [openYourself, setOpenYourself] = useState(false)
  // The pasted address lives only in this field, and only until it is sent.
  const [address, setAddress] = useState("")
  const [badAddress, setBadAddress] = useState(false)
  const [codeInstead, setCodeInstead] = useState(false)

  useEffect(() => {
    let live = true
    void discoverSigninExtension().then((channel) => {
      if (live) setExtension(channel ?? "absent")
    })
    return () => {
      live = false
    }
  }, [])

  async function continueInBrowser() {
    if (extension === "checking") return
    // No extension: open the page right here, inside the click, where no
    // popup blocker objects. The paste box is already in front.
    if (extension === "absent") {
      void openUrl(approval.verificationUrl)
      return
    }
    // The extension opens the page itself (`open: true`, `chrome.tabs.create`),
    // which the popup blocker does not see — the click's gesture would not
    // survive the await below.
    const result = await armSigninExtension(extension, {
      attemptId: attempt.attemptId,
      provider: attempt.provider,
      verificationUrl: approval.verificationUrl,
      // The API's unix seconds, verbatim: the unit `expiresAtSec` takes.
      expiresAtSec: attempt.expiresAt,
      open: true,
    })
    // Not armed means the browser will not come back on its own: say so and
    // put the paste box in front rather than leave a silent dead end.
    if (!result.armed) setExtension("absent")
    if (!result.opened) {
      void openUrl(approval.verificationUrl)
      setOpenYourself(true)
    }
  }

  function sendAddress() {
    const query = callbackQueryFromAddress(address, approval)
    // Cleared either way: nothing pasted is kept around or shown back.
    setAddress("")
    if (query === null) {
      setBadAddress(true)
      return
    }
    setBadAddress(false)
    onCallback(query)
  }

  const pasteAddress = (
    <form
      className="flex flex-wrap items-end gap-2"
      onSubmit={(event) => {
        event.preventDefault()
        sendAddress()
      }}
    >
      <label className="min-w-0 flex-1 space-y-1">
        <span className="block text-xs font-medium">
          {t("pasteCallbackLabel")}
        </span>
        <input
          value={address}
          autoComplete="off"
          spellCheck={false}
          maxLength={8192}
          disabled={submitting}
          onChange={(event) => {
            setAddress(event.target.value)
            setBadAddress(false)
          }}
          className="w-full rounded-md border bg-background px-3 py-2 font-mono text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
        />
      </label>
      <Button type="submit" size="sm" disabled={submitting || !address.trim()}>
        {submitting ? n("working") : t("submitCallback")}
      </Button>
    </form>
  )

  const alternatives = (
    <>
      {approval.fallbackUrl !== null &&
        (codeInstead ? (
          <div className="space-y-2">
            <BrowserLink
              href={approval.fallbackUrl}
              className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("openCodePage")}
            </BrowserLink>
            <PasteCodeForm
              code={code}
              submitting={submitting}
              onCode={onCode}
              onSubmit={onSubmitCode}
            />
          </div>
        ) : (
          <Button
            type="button"
            variant="link"
            size="sm"
            className="h-auto px-0 text-xs"
            onClick={() => setCodeInstead(true)}
          >
            {t("pasteCodeInstead")}
          </Button>
        ))}
      {onDevice && (
        <Button
          type="button"
          variant="link"
          size="sm"
          className="h-auto px-0 text-xs"
          disabled={busy}
          onClick={onDevice}
        >
          {t("useDeviceCodeInstead")}
        </Button>
      )}
    </>
  )

  return (
    <div className="space-y-2 rounded-lg border bg-muted/40 p-3">
      <p className="text-xs text-muted-foreground">
        {t("signInBrowserCallbackHint")}
      </p>
      <Button
        type="button"
        size="sm"
        disabled={extension === "checking" || busy}
        onClick={() => void continueInBrowser()}
      >
        {t("continueInBrowser")}
      </Button>
      {openYourself && (
        <BrowserLink
          href={approval.verificationUrl}
          className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("openSignInPage")}
        </BrowserLink>
      )}
      {issue && (
        <div className="flex flex-wrap items-center gap-2">
          <p role="alert" className="text-xs text-destructive">
            {t(CALLBACK_ISSUE_KEYS[issue])}
          </p>
          {issue === "unavailable" && onRetry && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-7 px-2 text-xs"
              disabled={submitting}
              onClick={onRetry}
            >
              {t("callbackRetry")}
            </Button>
          )}
        </div>
      )}
      {badAddress && (
        <p role="alert" className="text-xs text-destructive">
          {t("callbackAddressInvalid")}
        </p>
      )}
      {extension === "absent" && (
        <p className="text-xs text-muted-foreground">
          {t("signInExtensionMissing")}
        </p>
      )}
      {/* One element in every state, so a check that settles while someone is
          typing does not remount the field under them. Closed while the
          browser can come back by itself; opened when it cannot. */}
      <details className="text-xs" open={extension === "absent"}>
        <summary className="cursor-pointer text-muted-foreground">
          {t("signInFallbackSummary")}
        </summary>
        <div className="mt-2 space-y-2">
          <p className="text-muted-foreground">{t("pasteCallbackHint")}</p>
          {pasteAddress}
          <div className="flex flex-wrap gap-x-4">{alternatives}</div>
        </div>
      </details>
    </div>
  )
}
