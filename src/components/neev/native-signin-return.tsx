"use client"

// `/agents/signin`: where the Neev extension sends the browser back after the
// provider's own sign-in page (`direct-signin-contract.md` §2 step 4, §6.4).
//
// What arrives is the provider's redirect to the CLI's loopback listener, moved
// into the FRAGMENT so no server ever sees or logs it. Rules this file keeps:
//  - The fragment is scrubbed from the address bar the moment it is read, and
//    held only in memory until it is sent. Never stored, never logged, never
//    rendered.
//  - It is sent only for an attempt the SERVER says is this user's, still
//    pending, still in time, and waiting on a browser callback.
//  - "Connected" is said only once the attempt has completed AND the
//    connection row reads connected. Delivering the callback is not success:
//    the CLI still has to finish.

import { useEffect, useLayoutEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  callbackFailure,
  loadAgentConnections,
  loadConnectAttempt,
  newRequestKey,
  parseSigninFragment,
  submitConnectCallback,
  type NeevAgentProvider,
  type NeevConnectAttempt,
} from "@/lib/neev-agent-sessions-client"
import { NativeClientError } from "@/lib/neev-client"
import { NativeContentNotFoundError } from "@/lib/neev-content-client"
import { disarmSigninExtension } from "@/lib/neev-signin-extension"
import { useOptionalNativeAuth } from "./native-auth-provider"

type Captured = ReturnType<typeof parseSigninFragment>

/** In memory only, between the page module loading and the page mounting. */
let pending: Captured | undefined

/**
 * Read the fragment and remove it from the address bar, at once.
 *
 * Called when the page module loads — before the app has decided what to
 * render, which takes a request — and again on mount. Whatever it finds is
 * handed over exactly once.
 */
export function captureSigninFragment(): void {
  if (typeof window === "undefined") return
  const { hash, pathname } = window.location
  if (!hash) return
  // Replace, not push: the fragment must not survive in history either.
  window.history.replaceState(window.history.state, "", pathname)
  pending = parseSigninFragment(hash)
}

function takeCaptured(): Captured {
  captureSigninFragment()
  const taken = pending ?? null
  pending = undefined
  return taken
}

type Phase =
  | "reading"
  | "missing"
  | "signedOut"
  | "working"
  | "finishing"
  | "connected"
  | "notConnected"
  | "rejected"
  | "expired"
  | "notWaiting"
  | "failed"
  | "cancelled"
  | "gone"
  | "limited"
  | "unavailable"

const PHASE_KEYS = {
  reading: "signinReturnWorking",
  working: "signinReturnWorking",
  finishing: "signinReturnFinishing",
  missing: "signinReturnMissing",
  signedOut: "signinReturnSignedOut",
  connected: "signinReturnConnected",
  notConnected: "signinReturnNotConnected",
  rejected: "callbackRejected",
  expired: "signinReturnExpired",
  notWaiting: "signinReturnNotWaiting",
  failed: "signinReturnFailed",
  cancelled: "signinCancelled",
  gone: "signinReturnNotAvailable",
  limited: "callbackLimited",
  unavailable: "signinReturnUnavailable",
} as const

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

/** Automatic replays of a delivery the server could not take (503). */
const DELIVERY_RETRIES = 2

export function NativeSigninReturn({
  pollMs = 2_000,
  maxPolls = 90,
}: {
  /** Shortened by tests only. */
  pollMs?: number
  maxPolls?: number
}) {
  const t = useTranslations("NativeContent")
  const auth = useOptionalNativeAuth()
  const captured = useRef<Captured | undefined>(undefined)
  const started = useRef(false)
  // ONE request key for this page's delivery (contract v2 §3): every replay
  // after "unavailable" is the same delivery, never a second one.
  const deliveryKey = useRef(newRequestKey())
  const [phase, setPhase] = useState<Phase>("reading")
  // Still held (in memory) only while a replay is possible.
  const [retryable, setRetryable] = useState(false)

  // Layout effect: the earliest point a mounted component can scrub the hash.
  useLayoutEffect(() => {
    if (captured.current === undefined) captured.current = takeCaptured()
  }, [])

  const status = auth?.status
  const session = auth?.session ?? null
  useEffect(() => {
    if (started.current || captured.current === undefined) return
    const target = captured.current
    if (target === null) {
      setPhase("missing")
      return
    }
    if (status === "loading" || status === "revalidating") return
    if (!auth || status !== "authenticated" || !session?.organization) {
      // Not signed in here: the callback is dropped, never parked for later.
      captured.current = null
      setPhase("signedOut")
      return
    }
    started.current = true
    void deliver(target, session.csrf, auth.signal)
    // `auth` itself changes identity every render; status and session are
    // what this depends on.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status, session])

  async function deliver(
    target: NonNullable<Captured>,
    csrf: string,
    signal: AbortSignal | undefined
  ) {
    const { provider, attemptId } = target
    const declined = new URLSearchParams(target.query).has("error")
    setRetryable(false)
    setPhase("working")
    try {
      // Is this attempt really this user's, pending, in time, and waiting on
      // a browser? Asked of the server before anything is sent.
      const before = await loadConnectAttempt(provider, attemptId, signal)
      if (before.state !== "pending") {
        captured.current = null
        return settle(provider, before, signal, declined)
      }
      if (before.expiresAt * 1000 <= Date.now()) {
        captured.current = null
        return setPhase("expired")
      }
      if (before.approval?.kind !== "browser_callback") {
        captured.current = null
        return setPhase("notWaiting")
      }
      let after: NeevConnectAttempt | null = null
      for (let tries = 0; after === null; tries += 1) {
        try {
          after = await submitConnectCallback(
            provider,
            attemptId,
            deliveryKey.current,
            target.query,
            csrf,
            signal
          )
        } catch (cause) {
          const failure = callbackFailure(cause)
          // 503 / no answer: the gate was NOT used. Replay under the same key
          // a couple of times, then hand the choice to the reader.
          if (failure === "unavailable") {
            if (tries < DELIVERY_RETRIES) {
              await sleep(pollMs)
              continue
            }
            setRetryable(true)
            return setPhase("unavailable")
          }
          // Anything else is final for this callback: drop it.
          captured.current = null
          disarmSigninExtension(attemptId)
          // 409: the attempt stopped taking callbacks — most often because
          // this same callback already arrived (the relay and a paste racing).
          // Not a failure to report: read what the attempt did and follow it.
          if (failure === "conflict") {
            after = await loadConnectAttempt(provider, attemptId, signal)
            break
          }
          if (failure === "rejected") return setPhase("rejected")
          if (failure === "gone") return setPhase("gone")
          if (failure === "limited") return setPhase("limited")
          throw cause
        }
      }
      // Delivered (or already delivered): nothing is kept to send again.
      captured.current = null
      disarmSigninExtension(attemptId)
      // Delivered is not connected: the CLI still has to finish. Follow the
      // attempt until the server settles it.
      setPhase("finishing")
      for (let round = 0; after.state === "pending"; round += 1) {
        if (round >= maxPolls || after.expiresAt * 1000 <= Date.now())
          return setPhase("expired")
        await sleep(pollMs)
        after = await loadConnectAttempt(provider, attemptId, signal)
      }
      await settle(provider, after, signal, declined)
    } catch (cause) {
      captured.current = null
      setRetryable(false)
      if (cause instanceof NativeContentNotFoundError) setPhase("gone")
      else if (cause instanceof NativeClientError && cause.kind === "conflict")
        setPhase("notWaiting")
      else {
        auth?.handleAuthorityError(cause)
        setPhase("unavailable")
      }
    }
  }

  /** "Try again" after the server stayed unreachable: the SAME delivery. */
  function replay() {
    const target = captured.current
    if (!target || !auth?.session) return
    void deliver(target, auth.session.csrf, auth.signal)
  }

  async function settle(
    provider: NeevAgentProvider,
    attempt: NeevConnectAttempt,
    signal: AbortSignal | undefined,
    declined: boolean
  ) {
    if (attempt.state === "expired") return setPhase("expired")
    // The provider sent `error=` (the user declined on its page) and the CLI
    // gave up: that is a cancellation, not a fault.
    if (attempt.state === "failed" && declined) return setPhase("cancelled")
    if (attempt.state !== "completed") return setPhase("failed")
    // A completed attempt is the sign-in; the connection row is whether it
    // took. Only both together are "Connected".
    const wiring = await loadAgentConnections(signal)
    const row = wiring.connections.find((item) => item.provider === provider)
    setPhase(row?.state === "connected" ? "connected" : "notConnected")
  }

  const busy =
    phase === "reading" || phase === "working" || phase === "finishing"
  return (
    <main className="flex min-h-screen items-center justify-center bg-background px-4 text-foreground">
      <div className="w-full max-w-sm space-y-4 text-center">
        <h1 className="text-xl font-semibold">{t("signinReturnTitle")}</h1>
        <p
          role={busy || phase === "connected" ? "status" : "alert"}
          className="text-sm text-muted-foreground"
        >
          {t(PHASE_KEYS[phase])}
        </p>
        {retryable && phase === "unavailable" && (
          <Button type="button" variant="outline" size="sm" onClick={replay}>
            {t("callbackRetry")}
          </Button>
        )}
        {!busy && phase !== "connected" && (
          <Link
            href="/"
            className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("signinReturnBack")}
          </Link>
        )}
      </div>
    </main>
  )
}
