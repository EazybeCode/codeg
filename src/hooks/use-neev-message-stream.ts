"use client"

import { useEffect, useRef, useState } from "react"
import { NativeClientError } from "@/lib/neev-client"
import {
  openNativeMessageStream,
  NativeContentNotFoundError,
  type NativeMessageStreamEvent,
} from "@/lib/neev-content-client"

/**
 * How team messages are actually arriving right now. Reported verbatim to the
 * user: "live" is claimed only while a server stream is open and has synced.
 */
export type NeevStreamStatus =
  | "off"
  | "connecting"
  | "live"
  | "reconnecting"
  | "polling"

/** Backoff between reconnect attempts; exhausting it falls back to polling. */
const RETRY_DELAYS = [500, 1000, 2000, 4000] as const

/**
 * How long a connection must survive before reconnecting it counts as routine
 * rather than as a failure. The server's own stream lifetime is minutes, so a
 * connection that ends far sooner — even one that accepted us and sent a valid
 * `sync` before hanging up — is a fault: without this floor, "it didn't throw"
 * would reset the backoff and the reconnect loop would spin at full speed
 * against a server that is failing in exactly that way.
 */
const HEALTHY_AFTER_MS = 5_000

/**
 * Subscribes to a chat's content-free push notices and asks the caller to
 * re-read the conversation whenever it may have advanced.
 *
 * The stream never carries message content, so this hook cannot render anything
 * the server would not also hand back from the authorized list endpoint — a
 * revoked reader simply gets denied on the refetch.
 *
 * Push is best-effort by design: an unavailable, rolled-back, or repeatedly
 * failing stream lands on `"polling"`, which the caller uses both to re-enable
 * its interval and to tell the user what is really happening.
 */
export function useNeevMessageStream({
  chatId,
  enabled,
  signal,
  onNotify,
  retryDelays = RETRY_DELAYS,
  healthyAfterMs = HEALTHY_AFTER_MS,
}: {
  chatId: string | null
  enabled: boolean
  signal?: AbortSignal
  onNotify: () => void
  /** Backoff schedule; shortened by tests, never overridden in the app. */
  retryDelays?: readonly number[]
  /** Healthy-connection floor; shortened by tests, never overridden in the app. */
  healthyAfterMs?: number
}): { status: NeevStreamStatus } {
  const [status, setStatus] = useState<NeevStreamStatus>("off")
  const notify = useRef(onNotify)
  useEffect(() => {
    notify.current = onNotify
  }, [onNotify])
  useEffect(() => {
    if (!enabled || !chatId) {
      setStatus("off")
      return
    }
    const controller = new AbortController()
    const abort = () => controller.abort()
    if (signal?.aborted) abort()
    signal?.addEventListener("abort", abort)
    let stopped = false
    let failures = 0
    // The last sequence we have already asked the caller to reconcile, so an
    // idle heartbeat does not trigger a pointless refetch every interval.
    let seen: string | null = null
    const done = () => stopped || controller.signal.aborted
    // Always settles: aborting clears the timer AND resolves, so teardown can
    // never strand this promise (and the loop task with it).
    const sleep = (ms: number) =>
      new Promise<void>((resolve) => {
        if (controller.signal.aborted) return resolve()
        const finish = () => {
          window.clearTimeout(handle)
          controller.signal.removeEventListener("abort", finish)
          resolve()
        }
        const handle = window.setTimeout(finish, ms)
        controller.signal.addEventListener("abort", finish)
      })
    setStatus("connecting")
    const terminal = (error: unknown) =>
      // 404 means this deployment does not push (rolled back) or the chat is no
      // longer readable; either way, stop trying and let polling say so.
      error instanceof NativeContentNotFoundError ||
      (error instanceof NativeClientError &&
        (error.kind === "unauthorized" || error.kind === "forbidden"))
    const loop = async () => {
      for (;;) {
        if (done()) return
        let delivered = false
        const startedAt = Date.now()
        const receive = (event: NativeMessageStreamEvent) => {
          if (done()) return
          delivered = true
          if (event.type === "revoked") {
            // Access ended mid-stream. The refetch is what surfaces the denial;
            // this hook never decides on its own what the user may see.
            stopped = true
            setStatus("off")
            controller.abort()
            notify.current()
            return
          }
          setStatus("live")
          if (event.type === "resync") {
            // We fell behind the bounded buffer: re-read from the caller's cursor.
            seen = null
            notify.current()
            return
          }
          if (event.type === "message" || seen !== event.sequence) {
            seen = event.sequence
            notify.current()
          }
        }
        try {
          await openNativeMessageStream(chatId, receive, controller.signal)
        } catch (error) {
          if (done()) return
          if (terminal(error)) {
            setStatus("polling")
            return
          }
        }
        if (done()) return
        // An attempt counts as healthy only if it actually delivered something
        // AND stayed open. A server that accepts the request and hangs up at
        // once — with or without a valid frame first — must back off and end on
        // polling, never reconnect in a tight loop.
        const healthy = delivered && Date.now() - startedAt >= healthyAfterMs
        failures = healthy ? 0 : failures + 1
        if (failures > retryDelays.length) {
          setStatus("polling")
          return
        }
        if (failures > 0) {
          // Only a real fault is worth telling the user about; a connection that
          // simply reached its bounded lifetime reconnects silently.
          setStatus("reconnecting")
          await sleep(retryDelays[failures - 1])
        }
      }
    }
    void loop()
    return () => {
      stopped = true
      signal?.removeEventListener("abort", abort)
      controller.abort()
    }
  }, [chatId, enabled, signal, retryDelays, healthyAfterMs])
  return { status }
}
