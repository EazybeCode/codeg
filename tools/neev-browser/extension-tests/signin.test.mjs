/**
 * Direct sign-in relay v2 (direct-signin-ui-contract-v2.md; review B1/I1/N1/N2).
 *
 * Rules are applied here the way Chrome applies them (regexFilter, then
 * regexSubstitution with \1 -> the captured query) to the callback URLs the
 * pinned CLIs really produce (codex 0.155.1 :1455/auth/callback, claude
 * 2.1.267 :<random>/callback). The page bridge runs in a fake window.
 */

import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import test from "node:test"
import { fileURLToPath } from "node:url"
import vm from "node:vm"

import {
  FIRST_RULE_ID,
  MAX_ARM_MS,
  MAX_RULES,
  SigninRelay,
  ruleFor,
  targetFromVerificationUrl,
  validateArm,
} from "../extension/lib/signin.js"

const NOW = 1_800_000_000_000
const NOW_SEC = NOW / 1000
const ORIGIN = "https://app.neev.so"
const CTX = { origin: ORIGIN, fromTab: true, bridge: true }
const STATE = "Qx7bW2mZ0kP9sR4tV1yU8nA3cF6hJ5dL2gE0iO7uK4w"
const OTHER = "Zz9aA1bB2cC3dD4eE5fF6gG7hH8iI9jJ0kK1lL2mM3n"

function codexUrl(state = STATE, redirect = "http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback") {
  return (
    "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_x" +
    `&redirect_uri=${redirect}&code_challenge=abc&code_challenge_method=S256&state=${state}`
  )
}
function claudeUrl(port = 33127, state = STATE) {
  return (
    "https://claude.com/cai/oauth/authorize?code=true&client_id=c&response_type=code" +
    `&redirect_uri=http%3A%2F%2Flocalhost%3A${port}%2Fcallback&state=${state}`
  )
}
function arm(overrides = {}) {
  return {
    type: "neev.signin.arm",
    attemptId: "a".repeat(32),
    provider: "codex",
    verificationUrl: codexUrl(),
    expiresAtSec: NOW_SEC + 600,
    ...overrides,
  }
}
function redirect(rule, url, { method = "get", type = "main_frame" } = {}) {
  if (!rule.condition.requestMethods.includes(method)) return null
  if (!rule.condition.resourceTypes.includes(type)) return null
  const flags = rule.condition.isUrlFilterCaseSensitive ? "" : "i"
  const re = new RegExp(rule.condition.regexFilter, flags)
  if (!re.test(url)) return null
  return url.replace(re, rule.action.redirect.regexSubstitution.replace("\\1", "$1"))
}
function codexRule() {
  const checked = validateArm(arm(), CTX, NOW)
  assert.equal(checked.ok, true, JSON.stringify(checked))
  return ruleFor(checked.arm, FIRST_RULE_ID)
}

test("targets are derived from the official URL, never asserted", () => {
  assert.deepEqual(targetFromVerificationUrl("codex", codexUrl()), {
    port: 1455,
    path: "/auth/callback",
    state: STATE,
  })
  assert.deepEqual(targetFromVerificationUrl("claude", claudeUrl()), {
    port: 33127,
    path: "/callback",
    state: STATE,
  })
  for (const [provider, url] of [
    ["codex", codexUrl().replace("https://auth.openai.com", "https://evil.example")],
    ["codex", "https://auth.openai.com.evil.example/oauth/authorize?" + codexUrl().split("?")[1]],
    ["codex", codexUrl(STATE, "http%3A%2F%2Flocalhost%3A1456%2Fauth%2Fcallback")],
    ["codex", codexUrl(STATE, "http%3A%2F%2Fevil.example%3A1455%2Fauth%2Fcallback")],
    ["codex", codexUrl(STATE, "https%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback")],
    ["codex", codexUrl(STATE, "http%3A%2F%2Flocalhost%3A1455%2Fother")],
    ["codex", codexUrl("short")],
    ["codex", codexUrl(STATE) + "&state=" + OTHER],
    ["claude", claudeUrl(80)],
    ["claude", codexUrl()],
    ["gemini", codexUrl()],
  ]) {
    assert.equal(targetFromVerificationUrl(provider, url), null, url.slice(0, 80))
  }
})

test("the rule matches ONLY this attempt's callback (B1)", () => {
  const rule = codexRule()
  assert.deepEqual(rule.condition.resourceTypes, ["main_frame"])
  assert.deepEqual(rule.condition.requestMethods, ["get"])
  assert.equal(rule.condition.isUrlFilterCaseSensitive, true)
  const query = `code=ac_1.2-3&scope=openid&state=${STATE}`
  for (const host of ["localhost", "127.0.0.1"]) {
    assert.equal(
      redirect(rule, `http://${host}:1455/auth/callback?${query}`),
      `${ORIGIN}/agents/signin#attempt=${"a".repeat(32)}&provider=codex&${query}`
    )
  }
  assert.ok(redirect(rule, `http://localhost:1455/auth/callback?state=${STATE}&code=x`))
  for (const url of [
    // A local `codex login` / IDE / Conductor sign-in: its own state.
    `http://localhost:1455/auth/callback?code=x&state=${OTHER}`,
    `http://localhost:1455/auth/callback?code=x&state=${STATE}X`,
    `http://localhost:1455/auth/callback?code=x&state=${STATE.slice(0, -1)}`,
    `http://localhost:1455/auth/callback?code=x&xstate=${STATE}`,
    `http://localhost:1455/auth/callback?code=x&state=${STATE.toLowerCase()}`,
    `http://localhost:1455/auth/callback?code=x`,
    `http://localhost:1456/auth/callback?code=x&state=${STATE}`,
    `http://localhost:1455/other?code=x&state=${STATE}`,
    `https://localhost:1455/auth/callback?code=x&state=${STATE}`,
    `http://localhost.evil.example:1455/auth/callback?code=x&state=${STATE}`,
    `http://localhost:1455/auth/callback?code=x#state=${STATE}`,
  ]) {
    assert.equal(redirect(rule, url), null, url)
  }
  // Only a top-level GET navigation — never a page's fetch/iframe/POST.
  const hit = `http://localhost:1455/auth/callback?code=x&state=${STATE}`
  assert.equal(redirect(rule, hit, { type: "xmlhttprequest" }), null)
  assert.equal(redirect(rule, hit, { type: "sub_frame" }), null)
  assert.equal(redirect(rule, hit, { method: "post" }), null)
})

test("the code lands in the fragment of the constant Neev page", () => {
  const out = new URL(
    redirect(codexRule(), `http://localhost:1455/auth/callback?code=SECRET&state=${STATE}`)
  )
  assert.equal(out.origin, ORIGIN)
  assert.equal(out.pathname, "/agents/signin")
  assert.equal(out.search, "")
  assert.match(out.hash, /code=SECRET/)
})

test("claude: random port and /callback, bound to its state", () => {
  const checked = validateArm(
    arm({ provider: "claude", verificationUrl: claudeUrl(40111) }),
    CTX,
    NOW
  )
  const rule = ruleFor(checked.arm, FIRST_RULE_ID + 1)
  assert.match(
    redirect(rule, `http://localhost:40111/callback?code=c&state=${STATE}`),
    /provider=claude&code=c&state=/
  )
  assert.equal(redirect(rule, `http://localhost:40111/callback?code=c&state=${OTHER}`), null)
})

test("units are unix SECONDS; old and ambiguous fields are refused (N1)", () => {
  assert.equal(validateArm(arm(), CTX, NOW).ok, true)
  assert.equal(validateArm(arm(), CTX, NOW).arm.expiresAtMs, (NOW_SEC + 600) * 1000)
  const cases = [
    [arm({ expiresAtSec: (NOW_SEC + 600) * 1000 }), "expiry"], // ms sent as seconds
    [arm({ expiresAtSec: NOW_SEC - 1 }), "expiry"],
    [arm({ expiresAtSec: NOW_SEC + MAX_ARM_MS / 1000 + 1 }), "expiry"],
    [arm({ expiresAtSec: String(NOW_SEC + 600) }), "expiry"],
    [{ ...arm(), expiresAt: NOW + 600000 }, "shape"],
    [{ ...arm(), port: 1455 }, "shape"],
    [{ ...arm(), path: "/auth/callback" }, "shape"],
    [arm({ open: "yes" }), "shape"],
    [arm({ attemptId: "../x" }), "attempt"],
    [arm({ provider: "gemini" }), "provider"],
    [arm({ verificationUrl: "https://evil.example/" }), "url"],
  ]
  for (const [message, error] of cases) {
    assert.deepEqual(validateArm(message, CTX, NOW), { ok: false, error })
  }
  for (const context of [
    { origin: "https://evil.example", fromTab: true },
    { origin: "http://app.neev.so", fromTab: true },
    { origin: ORIGIN, fromTab: false },
    null,
  ]) {
    assert.deepEqual(validateArm(arm(), context, NOW), { ok: false, error: "origin" })
  }
})

function fakeEnv() {
  const store = { registry: [], rules: new Map(), alarms: new Map(), opened: [] }
  const env = {
    now: () => store.now ?? NOW,
    loadRegistry: async () => JSON.parse(JSON.stringify(store.registry)),
    saveRegistry: async (entries) => {
      store.registry = JSON.parse(JSON.stringify(entries))
    },
    updateRules: async ({ add, remove }) => {
      for (const id of remove) store.rules.delete(id)
      for (const rule of add) store.rules.set(rule.id, rule)
    },
    schedule: async (attemptId, when) => store.alarms.set(attemptId, when),
    unschedule: async (attemptId) => store.alarms.delete(attemptId),
    openTab: async (url) => {
      store.opened.push(url)
      return true
    },
  }
  return { env, store }
}

test("attempts get their own rules; disarm removes only its own (I1)", async () => {
  const { env, store } = fakeEnv()
  const relay = new SigninRelay(env)
  const codex = "a".repeat(32)
  const claude = "b".repeat(32)
  assert.deepEqual(await relay.handle(arm({ attemptId: codex, open: true }), CTX), {
    ok: true,
    opened: true,
  })
  assert.deepEqual(store.opened, [codexUrl()])
  assert.deepEqual(
    await relay.handle(
      arm({ attemptId: claude, provider: "claude", verificationUrl: claudeUrl() }),
      CTX
    ),
    { ok: true, opened: false }
  )
  assert.deepEqual([...store.rules.keys()].sort(), [FIRST_RULE_ID, FIRST_RULE_ID + 1])
  // Re-arming the same attempt reuses its slot.
  await relay.handle(arm({ attemptId: codex }), CTX)
  assert.equal(store.rules.size, 2)
  // A restarted worker (new relay, same persisted registry) disarms precisely.
  const restarted = new SigninRelay(env)
  assert.deepEqual(
    await restarted.handle({ type: "neev.signin.disarm", attemptId: codex }, CTX),
    { ok: true }
  )
  assert.deepEqual([...store.rules.keys()], [FIRST_RULE_ID + 1])
  assert.ok(store.alarms.has(claude) && !store.alarms.has(codex))
})

test("capacity, expiry prune and origin checks", async () => {
  const { env, store } = fakeEnv()
  const relay = new SigninRelay(env)
  for (let n = 0; n < MAX_RULES; n += 1) {
    const result = await relay.handle(arm({ attemptId: String(n).repeat(32) }), CTX)
    assert.equal(result.ok, true)
  }
  assert.deepEqual(await relay.handle(arm({ attemptId: "f".repeat(32) }), CTX), {
    ok: false,
    error: "capacity",
  })
  // After expiry, prune frees every slot and its rule.
  store.now = (NOW_SEC + 601) * 1000
  assert.deepEqual(await relay.prune(), [])
  assert.equal(store.rules.size, 0)
  store.now = NOW
  assert.deepEqual(
    await relay.handle({ type: "neev.signin.ping" }, { origin: "https://evil.example" }),
    { ok: false, error: "origin" }
  )
  assert.deepEqual(await relay.handle({ type: "neev.signin.ping" }, CTX), {
    ok: true,
    version: "0.2.1",
    bridge: true,
  })
})

function loadBridge({ origin = ORIGIN, respond = (m) => ({ ok: true, echo: m.type }) } = {}) {
  const listeners = []
  const posted = []
  const sent = []
  const win = {
    location: { origin },
    addEventListener: (type, fn) => type === "message" && listeners.push(fn),
    postMessage: (payload, targetOrigin) => posted.push({ payload, targetOrigin }),
  }
  const runtime = {
    lastError: undefined,
    sendMessage(envelope, callback) {
      sent.push(envelope)
      callback(respond(envelope.message))
    },
  }
  const source = readFileSync(
    fileURLToPath(new URL("../extension/content/signin-bridge.js", import.meta.url)),
    "utf8"
  )
  vm.runInNewContext(source, { window: win, chrome: { runtime } })
  const dispatch = (event) => listeners.forEach((fn) => fn(event))
  // Objects built inside the vm context have that realm's prototypes.
  const plain = (list) => JSON.parse(JSON.stringify(list))
  return {
    win,
    get posted() {
      return plain(posted)
    },
    get sent() {
      return plain(sent)
    },
    dispatch,
    listeners,
  }
}

test("page bridge: any extension id, this origin only (N2)", () => {
  const bridge = loadBridge()
  const { win, dispatch } = bridge
  assert.deepEqual(bridge.posted[0], {
    payload: { channel: "neev-signin", direction: "extension", type: "ready", version: "0.2.1" },
    targetOrigin: ORIGIN,
  })
  const good = {
    source: win,
    origin: ORIGIN,
    data: { channel: "neev-signin", direction: "page", id: "req-1", message: { type: "neev.signin.ping" } },
  }
  dispatch(good)
  assert.deepEqual(bridge.sent, [{ neevSigninBridge: true, message: { type: "neev.signin.ping" } }])
  assert.deepEqual(bridge.posted[1], {
    payload: { channel: "neev-signin", direction: "extension", id: "req-1", response: { ok: true, echo: "neev.signin.ping" } },
    targetOrigin: ORIGIN,
  })
  for (const bad of [
    { ...good, source: {} },
    { ...good, origin: "https://evil.example" },
    { ...good, data: { ...good.data, channel: "other" } },
    { ...good, data: { ...good.data, direction: "extension" } },
    { ...good, data: { ...good.data, id: "../x" } },
    { ...good, data: { ...good.data, message: "x".repeat(10) } },
    { ...good, data: { ...good.data, message: { blob: "x".repeat(9000) } } },
  ]) {
    dispatch(bad)
  }
  assert.equal(bridge.sent.length, 1)
})

test("page bridge does nothing on any other origin", () => {
  const { posted, listeners } = loadBridge({ origin: "https://evil.example" })
  assert.equal(listeners.length, 0)
  assert.equal(posted.length, 0)
})
