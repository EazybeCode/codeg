/**
 * The extension half of the wire contract with `com.neev.browser`.
 *
 * Every message that leaves the service worker is built here, so the shapes
 * the host validates exist in exactly one place. Nothing in this file touches
 * a Chrome API — it is pure, which is what lets the Node tests run it.
 */

export const HOST_NAME = "com.neev.browser"

export const PROVIDERS = ["claude", "codex"]

export const MAX_TASK_CHARS = 8000
export const MAX_MODEL_CHARS = 100
export const MAX_URL_CHARS = 2048

/** The complete tool allowlist. The host never sends anything else. */
export const ALLOWED_TOOLS = [
  "browser_snapshot",
  "browser_click",
  "browser_type",
  "browser_scroll",
  "browser_screenshot",
  "browser_request_user_action",
]

/** Tools that address one element and therefore must quote a live ref. */
export const REF_TOOLS = ["browser_click", "browser_type"]

/**
 * The failure vocabulary the host understands. Anything else we might come up
 * with is mapped onto `action_failed` before it goes out.
 */
export const ERROR_SLUGS = [
  "stale_ref",
  "not_found",
  "not_visible",
  "not_actionable",
  "detached",
  "wrong_tab",
  "wrong_origin",
  "screenshot_unavailable",
  "action_failed",
]

/** Revocation reasons the host accepts on `target.revoked`. */
export const REVOKE_REASONS = [
  "tab_closed",
  "tab_replaced",
  "permission_revoked",
  "cross_origin_navigation",
  "panel_closed",
]

const RUN_ID_RE = /^[A-Za-z0-9_-]{1,64}$/
const REF_RE = /^e(\d+):(\d+)$/

export function newRunId(random) {
  const uuid = (random || globalThis.crypto).randomUUID()
  return uuid.replace(/-/g, "")
}

export function makeRef(epoch, index) {
  return "e" + epoch + ":" + index
}

/** The epoch a ref was minted in, or `null` if it is not a ref at all. */
export function refEpoch(ref) {
  if (typeof ref !== "string") return null
  const match = REF_RE.exec(ref)
  return match ? Number(match[1]) : null
}

/**
 * `scheme://host[:port]` for http(s) URLs only, or `null`.
 *
 * Userinfo is refused outright: `https://app.test@evil.test/` must never
 * compare equal to the attached origin, and `URL.origin` alone would happily
 * report `https://evil.test` for it — correct, but only if you remember that
 * the host part is the one after the `@`. Refusing is the readable answer.
 */
export function originOf(url) {
  if (typeof url !== "string" || !url || url.length > MAX_URL_CHARS) return null
  let parsed
  try {
    parsed = new URL(url)
  } catch {
    return null
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null
  if (parsed.username || parsed.password) return null
  if (!parsed.hostname) return null
  return parsed.origin
}

/** The `chrome.permissions.request` pattern for one origin, and nothing more. */
export function originPattern(origin) {
  return origin.replace(/\/$/, "") + "/*"
}

export function isRunId(value) {
  return typeof value === "string" && RUN_ID_RE.test(value)
}

export function normalizeErrorSlug(slug) {
  return ERROR_SLUGS.includes(slug) ? slug : "action_failed"
}

function requireRunId(runId) {
  if (!isRunId(runId)) throw new Error("unusable run id")
  return runId
}

export function runStart({ runId, provider, task, target, model }) {
  requireRunId(runId)
  if (!PROVIDERS.includes(provider)) throw new Error("unknown provider")
  if (typeof task !== "string" || !task.trim()) throw new Error("empty task")
  if (task.length > MAX_TASK_CHARS) throw new Error("task is too long")
  if (!target || !Number.isInteger(target.tabId)) throw new Error("no tab")
  if (!originOf(target.url)) throw new Error("target must be an http(s) page")
  const message = {
    type: "run.start",
    runId,
    provider,
    task,
    target: { tabId: target.tabId, url: target.url },
  }
  if (model) {
    if (typeof model !== "string" || model.length > MAX_MODEL_CHARS) {
      throw new Error("model must be a short string")
    }
    message.model = model
  }
  return message
}

export function runStop(runId) {
  return { type: "run.stop", runId: requireRunId(runId) }
}

export function targetRevoked(runId, reason) {
  return {
    type: "target.revoked",
    runId: requireRunId(runId),
    reason: String(reason || "revoked").slice(0, 64),
  }
}

export function targetNavigated(runId, url) {
  if (!originOf(url)) throw new Error("navigation url is not http(s)")
  return { type: "target.navigated", runId: requireRunId(runId), url }
}

export function toolResult({ runId, callId, url, result }) {
  return {
    type: "tool.result",
    runId: requireRunId(runId),
    callId: String(callId),
    url,
    result: result || {},
  }
}

export function toolFailure({ runId, callId, error }) {
  return {
    type: "tool.result",
    runId: requireRunId(runId),
    callId: String(callId),
    error: normalizeErrorSlug(error),
  }
}

/**
 * Shape-check one inbound host message. The host is a local process we
 * launched, but its payloads carry model- and page-derived text, so the panel
 * only ever sees fields that survived this.
 */
export function parseHostMessage(raw) {
  if (!raw || typeof raw !== "object") return null
  const runId = typeof raw.runId === "string" ? raw.runId : null
  const text = (value, limit) =>
    typeof value === "string" ? value.slice(0, limit) : ""
  switch (raw.type) {
    case "run.started":
      return {
        type: "run.started",
        runId,
        runDir: text(raw.runDir, 1024),
        cli: text(raw.cli, 200),
      }
    case "run.output":
      return {
        type: "run.output",
        runId,
        channel: ["stdout", "stderr", "notice"].includes(raw.channel)
          ? raw.channel
          : "stdout",
        text: text(raw.text, 4000),
      }
    case "run.ended":
      return {
        type: "run.ended",
        runId,
        state: raw.state === "revoked" ? "revoked" : "stopped",
        reason: text(raw.reason, 64),
      }
    case "error":
      return {
        type: "error",
        runId,
        code: text(raw.code, 64),
        message: text(raw.message, 400),
      }
    case "tool.call": {
      if (!ALLOWED_TOOLS.includes(raw.tool)) return null
      if (typeof raw.callId !== "string" || !raw.callId) return null
      return {
        type: "tool.call",
        runId,
        callId: raw.callId.slice(0, 64),
        tool: raw.tool,
        args: raw.args && typeof raw.args === "object" ? raw.args : {},
        tabId: Number.isInteger(raw.tabId) ? raw.tabId : null,
      }
    }
    default:
      return null
  }
}
