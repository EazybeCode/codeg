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
}: {
  chatId: string | null
  enabled: boolean
  signal?: AbortSignal
  onNotify: () => void
  /** Backoff schedule; shortened by tests, never overridden in the app. */
  retryDelays?: readonly number[]
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
    let timer = 0
    // The last sequence we have already asked the caller to reconcile, so an
    // idle heartbeat does not trigger a pointless refetch every interval.
    let seen: string | null = null
    const done = () => stopped || controller.signal.aborted
    setStatus("connecting")
    const receive = (event: NativeMessageStreamEvent) => {
      if (done()) return
      if (event.type === "revoked") {
        // Access ended mid-stream. The refetch is what surfaces the denial;
        // this hook never decides on its own what the user may see.
        stopped = true
        setStatus("off")
        controller.abort()
        notify.current()
        return
      }
      failures = 0
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
    const loop = async () => {
      for (;;) {
        if (done()) return
        try {
          await openNativeMessageStream(chatId, receive, controller.signal)
        } catch (error) {
          if (done()) return
          // 404 means this deployment does not push (rolled back) or the chat
          // is no longer readable; either way, stop trying and let polling say so.
          if (
            error instanceof NativeContentNotFoundError ||
            (error instanceof NativeClientError &&
              (error.kind === "unauthorized" || error.kind === "forbidden"))
          ) {
            setStatus("polling")
            return
          }
          failures += 1
        }
        if (done()) return
        if (failures > retryDelays.length) {
          setStatus("polling")
          return
        }
        const delay = failures === 0 ? 0 : retryDelays[failures - 1]
        // A stream that simply reached its bounded lifetime reconnects at once;
        // only a real failure is worth telling the user about.
        if (failures > 0) setStatus("reconnecting")
        if (delay > 0)
          await new Promise((resolve) => {
            timer = window.setTimeout(resolve, delay)
          })
      }
    }
    void loop()
    return () => {
      stopped = true
      window.clearTimeout(timer)
      signal?.removeEventListener("abort", abort)
      controller.abort()
    }
  }, [chatId, enabled, signal, retryDelays])
  return { status }
}
