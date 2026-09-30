// The Neev Browser extension's sign-in relay, from the page's side
// (`direct-signin-ui-contract-v2.md` §1–§2).
//
// Discovery is the PAGE BRIDGE: the extension's content script, injected into
// the Neev app origin only, answers `window.postMessage` requests on the
// `neev-signin` channel. That works for any extension id — unpacked on any
// path, the existing install, a store build — so nothing here depends on one.
// The legacy `externally_connectable` channel to the one known id is tried
// only when the bridge gives no answer.
//
// The extension holds no Neev credential and is told nothing secret: an
// attempt id and the provider's own authorize URL, from which it derives the
// loopback port, path and exact `state` itself. Everything is best-effort — no
// answer simply means the user pastes the address instead.

import type { NeevAgentProvider } from "./neev-agent-sessions-client"

/**
 * The legacy channel's one known id (`externally_connectable`). A FALLBACK
 * only: the bridge is what finds the extension, whatever its id.
 */
export const NEEV_SIGNIN_EXTENSION_ID = "ohjngcnmibbjbbmcneiacfhnpldijcan"

export const NEEV_SIGNIN_CHANNEL = "neev-signin"
/** No answer within these is "no extension" (contract §1). */
export const NEEV_SIGNIN_PING_MS = 1_500
export const NEEV_SIGNIN_ARM_MS = 3_000

/** Which way the extension answered; later messages go the same way. */
export type SigninChannel = "bridge" | "legacy"

export interface SigninArmResult {
  /** The extension installed this attempt's relay rule. */
  armed: boolean
  /** The extension opened the provider page itself (no popup blocker). */
  opened: boolean
}

/** A request id the content script accepts: `^[A-Za-z0-9_-]{1,64}$`. */
function requestId(): string {
  return `p${crypto.randomUUID().replace(/-/g, "")}`
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value)
}

/**
 * One request over the page bridge. `undefined` = no answer in time (or no
 * bridge at all). Only an answer from THIS window, on THIS origin, on the
 * channel, marked as the extension's and carrying our id, is read — anything
 * else on `message` (another script, another frame, a stale answer) is
 * ignored rather than trusted.
 */
function viaBridge(message: unknown, timeoutMs: number): Promise<unknown> {
  if (typeof window === "undefined") return Promise.resolve(undefined)
  const origin = window.location.origin
  // An opaque origin cannot address the bridge, and "*" is never used.
  if (!origin || origin === "null") return Promise.resolve(undefined)
  const id = requestId()
  return new Promise((resolve) => {
    const done = (value: unknown) => {
      window.clearTimeout(timer)
      window.removeEventListener("message", listen)
      resolve(value)
    }
    const listen = (event: MessageEvent) => {
      if (event.origin !== origin || event.source !== window) return
      const data: unknown = event.data
      if (
        !isRecord(data) ||
        data.channel !== NEEV_SIGNIN_CHANNEL ||
        data.direction !== "extension" ||
        data.id !== id ||
        !("response" in data)
      )
        return
      done(data.response)
    }
    const timer = window.setTimeout(() => done(undefined), timeoutMs)
    window.addEventListener("message", listen)
    window.postMessage(
      { channel: NEEV_SIGNIN_CHANNEL, direction: "page", id, message },
      origin
    )
  })
}

type SendMessage = (
  extensionId: string,
  message: unknown,
  callback: (response: unknown) => void
) => void

interface ChromeGlobal {
  runtime?: { sendMessage?: SendMessage; lastError?: unknown }
}

/** One request over the legacy channel. `undefined` = no answer. */
function viaLegacy(message: unknown, timeoutMs: number): Promise<unknown> {
  const api = (globalThis as { chrome?: ChromeGlobal }).chrome?.runtime
  if (!api?.sendMessage) return Promise.resolve(undefined)
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(undefined), timeoutMs)
    try {
      api.sendMessage!(NEEV_SIGNIN_EXTENSION_ID, message, (response) => {
        clearTimeout(timer)
        // Reading `lastError` is what stops Chrome logging an unchecked one.
        resolve(api.lastError ? undefined : response)
      })
    } catch {
      clearTimeout(timer)
      resolve(undefined)
    }
  })
}

function via(
  channel: SigninChannel,
  message: unknown,
  timeoutMs: number
): Promise<unknown> {
  return channel === "bridge"
    ? viaBridge(message, timeoutMs)
    : viaLegacy(message, timeoutMs)
}

/** `{ok:true, version:"…"}` and nothing looser. */
export function isPingAnswer(value: unknown): boolean {
  return (
    isRecord(value) &&
    value.ok === true &&
    typeof value.version === "string" &&
    value.version.length > 0 &&
    value.version.length <= 32
  )
}

/**
 * An arm answer, read strictly. Anything that is not exactly an `ok:true`
 * answer — a refusal, a malformed or hostile payload — is "not armed", which
 * the page treats as "no relay": open the page itself, show the paste box.
 */
export function readArmAnswer(value: unknown): SigninArmResult {
  if (!isRecord(value) || value.ok !== true)
    return { armed: false, opened: false }
  return { armed: true, opened: value.opened === true }
}

/**
 * Find the extension: the bridge first (any id), then the legacy id. `null`
 * means no answer either way.
 */
export async function discoverSigninExtension(): Promise<SigninChannel | null> {
  const ping = { type: "neev.signin.ping" }
  if (isPingAnswer(await viaBridge(ping, NEEV_SIGNIN_PING_MS))) return "bridge"
  if (isPingAnswer(await viaLegacy(ping, NEEV_SIGNIN_PING_MS))) return "legacy"
  return null
}

/**
 * Ask the extension to relay exactly this attempt's loopback redirect.
 *
 * `expiresAtSec` is the API's `expiresAt` verbatim — unix SECONDS, the unit the
 * extension's `expiresAtSec` takes (contract §4, N1). `verificationUrl` is the
 * provider URL verbatim; the extension derives port, path and the exact
 * `state` from it, so none of those is sent (they would be refused). With
 * `open`, the extension opens the page itself, outside the popup blocker.
 */
export async function armSigninExtension(
  channel: SigninChannel,
  target: {
    attemptId: string
    provider: NeevAgentProvider
    verificationUrl: string
    expiresAtSec: number
    open: boolean
  }
): Promise<SigninArmResult> {
  return readArmAnswer(
    await via(
      channel,
      {
        type: "neev.signin.arm",
        attemptId: target.attemptId,
        provider: target.provider,
        verificationUrl: target.verificationUrl,
        expiresAtSec: target.expiresAtSec,
        open: target.open,
      },
      NEEV_SIGNIN_ARM_MS
    )
  )
}

/**
 * Drop this attempt's relay rule, and only it. Fire-and-forget: the rule also
 * lapses at the attempt's expiry. With no known channel (the return page,
 * which never pinged) it goes both ways; each is harmless when unanswered.
 */
export function disarmSigninExtension(
  attemptId: string,
  channel?: SigninChannel | null
): void {
  const message = { type: "neev.signin.disarm", attemptId }
  if (channel !== "legacy") void viaBridge(message, NEEV_SIGNIN_PING_MS)
  if (channel !== "bridge") void viaLegacy(message, NEEV_SIGNIN_PING_MS)
}
