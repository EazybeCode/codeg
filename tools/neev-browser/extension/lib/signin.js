/**
 * Direct sign-in relay (direct-signin-ui-contract-v2.md).
 *
 * The vendor CLI signing the user in runs on the Neev server and listens on ITS
 * loopback (`http://localhost:<port><path>`). After the user signs in on the
 * provider's own page, the provider redirects the browser there. For each
 * attempt this arms ONE declarativeNetRequest session rule that turns exactly
 * THAT attempt's callback — matched by its exact `state` — into the Neev page,
 * with the redirect's query in the URL FRAGMENT (never sent to any server).
 *
 * Why it cannot hijack anything else (review B1/I1):
 *  - the rule matches only a GET main_frame navigation to
 *    http://localhost|127.0.0.1:<port><path>?…state=<this attempt's state>…,
 *    case-sensitively; a local `codex login`, IDE or Conductor sign-in on the
 *    same port has its own random state and is never touched;
 *  - port, path and state are DERIVED from the official verification URL the
 *    page holds (pinned provider prefix, loopback redirect_uri, pinned path) —
 *    nothing the page merely asserts;
 *  - the redirect target is this constant, never the sender's origin;
 *  - one rule per attempt (max 4), removed at the attempt's expiry, on
 *    disarm, and at browser restart (session rules).
 *
 * Holds no Neev credential and never observes the code: Chrome rewrites the
 * navigation; this script only installs and removes rules.
 */

export const VERSION = "0.2.1"
export const SIGNIN_ORIGIN = "https://app.neev.so"
export const RETURN_URL = `${SIGNIN_ORIGIN}/agents/signin`
export const FIRST_RULE_ID = 7100
export const MAX_RULES = 4
export const MAX_ARM_MS = 15 * 60 * 1000

/** Per provider: the pinned authorize URL prefix and the CLI listener path
 * (measured on codex-cli 0.155.1 and claude 2.1.267). Codex's port is fixed. */
export const PROVIDERS = Object.freeze({
  codex: {
    prefix: "https://auth.openai.com/oauth/authorize?",
    path: "/auth/callback",
    port: 1455,
  },
  claude: {
    prefix: "https://claude.com/cai/oauth/authorize?",
    path: "/callback",
    port: null,
  },
})

const ATTEMPT = /^[0-9a-f]{32}$/
const STATE = /^[A-Za-z0-9._~-]{16,256}$/
const ARM_KEYS = new Set([
  "type",
  "attemptId",
  "provider",
  "verificationUrl",
  "expiresAtSec",
  "open",
])

function escapeRegex(text) {
  return text.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")
}

/**
 * Pure: the loopback target the CLI's OWN authorize URL names, or null.
 * The URL must start with the provider's pinned prefix and its redirect_uri
 * must be exactly http://localhost|127.0.0.1:<port><pinned path>.
 */
export function targetFromVerificationUrl(provider, verificationUrl) {
  const spec = PROVIDERS[provider]
  if (!spec || typeof verificationUrl !== "string") return null
  if (verificationUrl.length > 2048 || !verificationUrl.startsWith(spec.prefix)) {
    return null
  }
  let url
  let redirect
  try {
    url = new URL(verificationUrl)
    const redirects = url.searchParams.getAll("redirect_uri")
    if (redirects.length !== 1) return null
    redirect = new URL(redirects[0])
  } catch {
    return null
  }
  const states = url.searchParams.getAll("state")
  if (states.length !== 1 || !STATE.test(states[0])) return null
  if (
    redirect.protocol !== "http:" ||
    !["localhost", "127.0.0.1"].includes(redirect.hostname) ||
    redirect.username ||
    redirect.password ||
    redirect.search ||
    redirect.hash ||
    redirect.pathname !== spec.path
  ) {
    return null
  }
  const port = Number(redirect.port)
  if (!Number.isInteger(port) || port < 1024 || port > 65535) return null
  if (spec.port !== null && port !== spec.port) return null
  return { port, path: spec.path, state: states[0] }
}

/**
 * Pure: validate an arm request. `context` is `{ origin, fromTab }` — both are
 * taken from Chrome's `sender`, never from the message.
 */
export function validateArm(message, context, nowMs) {
  if (!context || context.origin !== SIGNIN_ORIGIN || !context.fromTab) {
    return { ok: false, error: "origin" }
  }
  if (
    !message ||
    typeof message !== "object" ||
    Object.keys(message).some((key) => !ARM_KEYS.has(key))
  ) {
    return { ok: false, error: "shape" }
  }
  const { attemptId, provider, verificationUrl, expiresAtSec } = message
  if (typeof attemptId !== "string" || !ATTEMPT.test(attemptId)) {
    return { ok: false, error: "attempt" }
  }
  if (!Object.hasOwn(PROVIDERS, provider)) {
    return { ok: false, error: "provider" }
  }
  const target = targetFromVerificationUrl(provider, verificationUrl)
  if (!target) return { ok: false, error: "url" }
  // Unix SECONDS (N1). A millisecond value is ~1000x too far in the future
  // and is refused here rather than arming a rule that never expires.
  const expiresAtMs = Number.isInteger(expiresAtSec) ? expiresAtSec * 1000 : NaN
  if (
    !Number.isFinite(expiresAtMs) ||
    expiresAtMs <= nowMs ||
    expiresAtMs - nowMs > MAX_ARM_MS
  ) {
    return { ok: false, error: "expiry" }
  }
  if (message.open !== undefined && typeof message.open !== "boolean") {
    return { ok: false, error: "shape" }
  }
  return {
    ok: true,
    arm: {
      attemptId,
      provider,
      verificationUrl,
      expiresAtMs,
      open: message.open === true,
      ...target,
    },
  }
}

/** Pure: the session rule for a validated arm, under `ruleId`. */
export function ruleFor(arm, ruleId) {
  const query =
    `((?:[^#]*&)?state=${escapeRegex(arm.state)}(?:&[^#]*)?)`
  return {
    id: ruleId,
    priority: 1,
    action: {
      type: "redirect",
      redirect: {
        regexSubstitution:
          `${RETURN_URL}#attempt=${arm.attemptId}&provider=${arm.provider}&\\1`,
      },
    },
    condition: {
      regexFilter:
        `^http://(?:localhost|127\\.0\\.0\\.1):${arm.port}` +
        `${escapeRegex(arm.path)}\\?${query}$`,
      isUrlFilterCaseSensitive: true,
      resourceTypes: ["main_frame"],
      requestMethods: ["get"],
    },
  }
}

/**
 * The relay. `env` supplies:
 *   now() -> ms
 *   loadRegistry() / saveRegistry(entries)   persisted across worker restarts
 *     entries: [{ attemptId, ruleId, expiresAtMs }]
 *   updateRules({ add: [rule], remove: [ruleId] })
 *   schedule(attemptId, whenMs) / unschedule(attemptId)   per-attempt alarms
 *   openTab(url) -> boolean
 */
export class SigninRelay {
  constructor(env) {
    this.env = env
  }

  /** Drop expired entries (and their rules). Run on worker start and arm. */
  async prune() {
    const now = this.env.now()
    const entries = await this.env.loadRegistry()
    const live = entries.filter((entry) => entry.expiresAtMs > now)
    const dead = entries.filter((entry) => entry.expiresAtMs <= now)
    if (dead.length) {
      await this.env.updateRules({ add: [], remove: dead.map((e) => e.ruleId) })
      for (const entry of dead) await this.env.unschedule(entry.attemptId)
      await this.env.saveRegistry(live)
    }
    return live
  }

  async handle(message, context) {
    const type = message && message.type
    if (type === "neev.signin.ping") {
      if (!context || context.origin !== SIGNIN_ORIGIN) {
        return { ok: false, error: "origin" }
      }
      return { ok: true, version: VERSION, bridge: context.bridge === true }
    }
    if (type === "neev.signin.arm") return this.arm(message, context)
    if (type === "neev.signin.disarm") {
      if (!context || context.origin !== SIGNIN_ORIGIN) {
        return { ok: false, error: "origin" }
      }
      if (typeof message.attemptId !== "string" || !ATTEMPT.test(message.attemptId)) {
        return { ok: false, error: "attempt" }
      }
      await this.disarm(message.attemptId)
      return { ok: true }
    }
    return { ok: false, error: "type" }
  }

  async arm(message, context) {
    const checked = validateArm(message, context, this.env.now())
    if (!checked.ok) return checked
    const arm = checked.arm
    const live = await this.prune()
    const existing = live.find((entry) => entry.attemptId === arm.attemptId)
    let ruleId = existing ? existing.ruleId : null
    if (ruleId === null) {
      const used = new Set(live.map((entry) => entry.ruleId))
      for (let slot = 0; slot < MAX_RULES; slot += 1) {
        if (!used.has(FIRST_RULE_ID + slot)) {
          ruleId = FIRST_RULE_ID + slot
          break
        }
      }
    }
    if (ruleId === null) return { ok: false, error: "capacity" }
    await this.env.updateRules({ add: [ruleFor(arm, ruleId)], remove: [ruleId] })
    const entries = live.filter((entry) => entry.attemptId !== arm.attemptId)
    entries.push({ attemptId: arm.attemptId, ruleId, expiresAtMs: arm.expiresAtMs })
    await this.env.saveRegistry(entries)
    await this.env.schedule(arm.attemptId, arm.expiresAtMs)
    let opened = false
    if (arm.open) {
      try {
        opened = (await this.env.openTab(arm.verificationUrl)) === true
      } catch {
        opened = false
      }
    }
    return { ok: true, opened }
  }

  /** Remove ONE attempt's rule; other attempts stay armed. Idempotent. */
  async disarm(attemptId) {
    const entries = await this.env.loadRegistry()
    const mine = entries.filter((entry) => entry.attemptId === attemptId)
    if (mine.length) {
      await this.env.updateRules({ add: [], remove: mine.map((e) => e.ruleId) })
      await this.env.saveRegistry(entries.filter((e) => e.attemptId !== attemptId))
    }
    await this.env.unschedule(attemptId)
  }
}

export const ALARM_PREFIX = "neev-signin:"
