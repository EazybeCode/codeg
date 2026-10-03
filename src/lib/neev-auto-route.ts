/**
 * Auto model routing, client side.
 *
 * The server owns every decision: it builds the list of the caller's own
 * connected agents and models, asks the configured router (TypeSafe Jev), and
 * falls back to the manual selection whenever that cannot be trusted. This file
 * only reads whether Auto is available, asks for a new chat's decision, and
 * remembers two small things on this device: whether Auto is switched on, and
 * the last decision per chat so the composer can say what ran.
 */
import { NativeClientError } from "./neev-client"
import { contentGet, contentMutation } from "./neev-content-client"
import {
  isSupportedEffort,
  isSupportedModel,
  NEEV_MAX_PROMPT_CHARS,
  parseRoute,
  type NeevAgentProvider,
  type NeevEffort,
  type NeevRoute,
} from "./neev-agent-sessions-client"

/**
 * `off`: no router configured, Auto is not offered.
 * `missing_credential`: configured but its key is absent — shown, never used.
 * `ready`: decisions are made.
 */
export type NeevRoutingState = "off" | "missing_credential" | "ready"

export interface NeevRoutingStatus {
  state: NeevRoutingState
  router: string | null
  /** How much of a prompt the router is sent. */
  maxTaskChars: number
  /** How much of the previous exchange may be sent with a follow-up. */
  maxPreviousChars: number
  /** Whether complex Auto work is supervised: `off`, `review`, `plan_review`. */
  supervision: "off" | "review" | "plan_review"
  /** The caller's OWN usage per connected provider, as far as reported. */
  usage: NeevUsageSummary[]
}

export interface NeevUsageSummary {
  provider: string
  /** `available`, `rate_limited`, `exhausted`, `exhausted_unknown_reset`. */
  availability: string
  until: number | null
  /** `plenty`, `limited`, `low` or `unknown` — never a number. */
  remaining: string
  nextResetAt: number | null
  observedAt: number | null
}

const SUPERVISION_MODES = ["off", "review", "plan_review"] as const
const CODE = /^[a-z_]{1,40}$/

function nullableSeconds(value: unknown): number | null {
  if (value === null) return null
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0)
    return invalid()
  return value
}

function usageSummary(value: unknown): NeevUsageSummary {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return invalid()
  const row = value as Record<string, unknown>
  if (
    Object.keys(row).sort().join(",") !==
    "availability,nextResetAt,observedAt,provider,remaining,until"
  )
    return invalid()
  for (const key of ["provider", "availability", "remaining"])
    if (typeof row[key] !== "string" || !CODE.test(row[key] as string))
      return invalid()
  return {
    provider: row.provider as string,
    availability: row.availability as string,
    until: nullableSeconds(row.until),
    remaining: row.remaining as string,
    nextResetAt: nullableSeconds(row.nextResetAt),
    observedAt: nullableSeconds(row.observedAt),
  }
}

const STATES: readonly NeevRoutingState[] = [
  "off",
  "missing_credential",
  "ready",
]

function invalid(): never {
  throw new NativeClientError("invalid_response")
}

export function parseRoutingStatus(value: unknown): NeevRoutingStatus {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return invalid()
  const object = value as Record<string, unknown>
  const keys = Object.keys(object).sort()
  if (
    keys.join(",") !==
    "maxPreviousChars,maxTaskChars,router,state,supervision,usage"
  )
    return invalid()
  const state = object.state as NeevRoutingState
  if (!STATES.includes(state)) return invalid()
  const router = object.router
  if (
    router !== null &&
    (typeof router !== "string" || !/^[a-z_]{1,40}$/.test(router))
  )
    return invalid()
  const previous = object.maxPreviousChars
  if (
    typeof previous !== "number" ||
    !Number.isSafeInteger(previous) ||
    previous < 0
  )
    return invalid()
  const max = object.maxTaskChars
  if (typeof max !== "number" || !Number.isSafeInteger(max) || max <= 0)
    return invalid()
  // "off" names no router; anything else must say which one.
  if ((state === "off") !== (router === null)) return invalid()
  const supervision = object.supervision as NeevRoutingStatus["supervision"]
  if (!SUPERVISION_MODES.includes(supervision)) return invalid()
  if (!Array.isArray(object.usage) || object.usage.length > 8) return invalid()
  return {
    state,
    router,
    maxTaskChars: max,
    maxPreviousChars: previous,
    supervision,
    usage: object.usage.map(usageSummary),
  }
}

export async function loadRoutingStatus(
  signal?: AbortSignal
): Promise<NeevRoutingStatus> {
  return parseRoutingStatus(await contentGet("/api/neev/agent/routing", signal))
}

export interface RouteNewChatInput {
  prompt: string
  /** The draft's manual selection: what runs if routing cannot decide. */
  provider: NeevAgentProvider
  model: string
  effort: NeevEffort | null
  fast: boolean
  attachedFiles: number
}

/**
 * Decide a NEW chat's agent, model and effort. Creates nothing: the caller
 * creates the session with the answer, so a draft abandoned while this is in
 * flight leaves nothing behind. A `409` means no provider is connected.
 */
export async function routeNewAgentChat(
  input: RouteNewChatInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevRoute> {
  if (
    typeof input.prompt !== "string" ||
    [...input.prompt].length > NEEV_MAX_PROMPT_CHARS ||
    !isSupportedModel(input.model) ||
    (input.effort !== null && !isSupportedEffort(input.effort)) ||
    !Number.isSafeInteger(input.attachedFiles) ||
    input.attachedFiles < 0
  )
    throw new NativeClientError("invalid_request")
  return parseRoute(
    await contentMutation(
      "/api/neev/agent/route",
      {
        prompt: input.prompt,
        provider: input.provider,
        model: input.model,
        effort: input.effort,
        fast: input.fast,
        attachedFiles: input.attachedFiles,
      },
      csrf,
      signal
    )
  )
}

// ---- Device-local state ----

const PREFERENCE_KEY = "neev.agent.auto"

/**
 * Whether this device sends with Auto. ON by default — Auto is the default
 * once the server's router is ready — and off only after someone switched it
 * off here. The switch alone never makes a turn "Auto": that also needs the
 * router to be ready, so a missing key always means the manual selection.
 */
export function readAutoPreference(): boolean {
  try {
    return globalThis.localStorage?.getItem(PREFERENCE_KEY) !== "0"
  } catch {
    return true
  }
}

export function writeAutoPreference(on: boolean): void {
  try {
    globalThis.localStorage?.setItem(PREFERENCE_KEY, on ? "1" : "0")
  } catch {
    // Storage refused (private mode): the switch still works for this page.
  }
}

/**
 * The last decision per chat, in memory only. Lets a chat created from a routed
 * draft show what its first turn ran on, and a reload simply shows nothing
 * rather than a stale claim.
 */
const lastRoutes = new Map<string, NeevRoute>()
const MAX_REMEMBERED = 50

export function rememberRoute(sessionId: string, route: NeevRoute): void {
  lastRoutes.delete(sessionId)
  lastRoutes.set(sessionId, route)
  while (lastRoutes.size > MAX_REMEMBERED) {
    const oldest = lastRoutes.keys().next().value
    if (oldest === undefined) break
    lastRoutes.delete(oldest)
  }
}

export function recalledRoute(sessionId: string): NeevRoute | null {
  return lastRoutes.get(sessionId) ?? null
}
