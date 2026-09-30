/**
 * Why a run failed, and what the reader can do about it.
 *
 * A failed turn used to reach the team chat as "This service is temporarily
 * unavailable" whatever had actually happened — a Codex sign-in problem looked
 * exactly like a server outage. This reads the turn's own record instead: the
 * provider's `error` step (class + redacted message) when the server kept one,
 * otherwise the stored `errorCode`, which older turns and runtime refusals
 * carry alone.
 */
import {
  NEEV_RUN_ERROR_CATEGORIES,
  type NeevActivityItem,
  type NeevRunErrorCategory,
  type NeevTurnStatus,
} from "./neev-agent-sessions-client"

export interface NeevRunFailure {
  category: NeevRunErrorCategory
  /** The provider's own words, already redacted and bounded by the server. */
  message: string | null
  /** A second attempt can plausibly succeed, so Retry is offered. */
  retryable: boolean
  /** Signing in again is the fix, so Reconnect is offered (and not Retry). */
  reconnect: boolean
  /**
   * The stored code, when the class was read from it alone (no provider
   * step). Its own wording ("Agents are turned off on this server") is more
   * precise than the class's, so the card uses it.
   */
  errorCode: string | null
}

/** Classes a plain second attempt can fix. */
const RETRYABLE: ReadonlySet<NeevRunErrorCategory> = new Set([
  "rate_limit",
  "network",
  "timeout",
  "runtime_unavailable",
  "provider",
])

const RECONNECT: ReadonlySet<NeevRunErrorCategory> = new Set([
  "auth_expired",
  "signin_required",
])

/**
 * A stored `errorCode` read as a class, for a turn with no `error` step.
 * `disabled` and `unsupported` are not retryable: the server said no, and will
 * say it again.
 */
function fromErrorCode(code: string | null): {
  category: NeevRunErrorCategory
  retryable?: boolean
} {
  switch (code) {
    case "unauthorized":
      return { category: "signin_required" }
    case "unavailable":
    case "isolation":
    case "interrupted":
    case "io":
      return { category: "runtime_unavailable" }
    case "disabled":
      return { category: "runtime_unavailable", retryable: false }
    case "unsupported":
    case "too_large":
      return { category: "provider_refusal" }
    default:
      return { category: "provider" }
  }
}

/** The failure a turn records, or `null` when it did not fail. */
export function runFailure(turn: {
  status: NeevTurnStatus
  errorCode: string | null
  activity: readonly NeevActivityItem[]
}): NeevRunFailure | null {
  if (turn.status !== "failed") return null
  // The LAST error step: a turn fails once, but a replay may carry an earlier
  // server-written one before the provider's own.
  let step: Extract<NeevActivityItem, { kind: "status" }> | null = null
  for (const item of turn.activity)
    if (item.kind === "status" && item.state === "error") step = item
  const stepCategory =
    step?.category &&
    (NEEV_RUN_ERROR_CATEGORIES as readonly string[]).includes(step.category)
      ? step.category
      : null
  const fallback = fromErrorCode(turn.errorCode)
  const category = stepCategory ?? fallback.category
  const reconnect = RECONNECT.has(category)
  return {
    category,
    errorCode: stepCategory === null ? turn.errorCode : null,
    message: step?.detail?.trim() ? step.detail.trim() : null,
    reconnect,
    retryable:
      !reconnect &&
      RETRYABLE.has(category) &&
      (stepCategory !== null || fallback.retryable !== false),
  }
}

const PROVIDER_NAMES: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  cursor: "Cursor",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
}

/** A stored provider id as a reader knows it; unknown ids are shown as-is. */
export function providerName(provider: string): string {
  return PROVIDER_NAMES[provider] ?? provider
}
