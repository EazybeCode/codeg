// @vitest-environment-options {"url": "https://app.neev.so/workspace"}
//
// The page's sign-in client against the extension's OWN bridge script and
// relay (see `test-signin-extension.ts`): discovery, the arm schema and its
// units, the rule it installs, the redirect that rule produces, and the page
// parsing that redirect back — one loop, no stand-in schemas. Plus the bridge
// boundaries: timeouts, foreign origins and sources, other ids, and hostile
// answers.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  armSigninExtension,
  disarmSigninExtension,
  discoverSigninExtension,
  isPingAnswer,
  NEEV_SIGNIN_ARM_MS,
  NEEV_SIGNIN_CHANNEL,
  NEEV_SIGNIN_PING_MS,
  readArmAnswer,
} from "@/lib/neev-signin-extension"
import {
  parseApproval,
  parseSigninFragment,
} from "@/lib/neev-agent-sessions-client"
import {
  installBrowserPostMessage,
  installSigninExtension,
  postForeign,
  redirectFor,
  signin,
} from "./test-signin-extension"

const ATTEMPT = "0123456789abcdef0123456789abcdef"
const OTHER = "fedcba9876543210fedcba9876543210"
const STATE = "Zq8v-3kP_wR2mX9aL0bN7cY4"
const codexUrl = (state = STATE) =>
  "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_x" +
  `&redirect_uri=${encodeURIComponent("http://localhost:1455/auth/callback")}` +
  `&scope=openid&code_challenge=c&code_challenge_method=S256&state=${state}`
const claudeUrl = (port: number) =>
  "https://claude.com/cai/oauth/authorize?code=true&client_id=x" +
  `&redirect_uri=${encodeURIComponent(`http://localhost:${port}/callback`)}` +
  `&state=${STATE}AAAA`
/** What the API hands the page: expiry in unix SECONDS. */
const expiresAtSec = () => Math.floor(Date.now() / 1000) + 600

let installed: ReturnType<typeof installSigninExtension> | null = null
beforeEach(() => installBrowserPostMessage())
afterEach(() => {
  installed?.reset()
  installed = null
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

describe("page ⇄ bridge ⇄ relay, for real", () => {
  it("finds the extension through the bridge, whatever its id", async () => {
    installed = installSigninExtension()
    expect(await discoverSigninExtension()).toBe("bridge")
    expect(installed.received).toEqual([{ type: "neev.signin.ping" }])
  })

  it("arms with the relay's own schema and seconds, and the extension opens the page", async () => {
    installed = installSigninExtension()
    const expires = expiresAtSec()
    // The page takes these straight from the API's approval and attempt.
    const approval = parseApproval(
      {
        kind: "browserCallback",
        verificationUrl: codexUrl(),
        callbackPort: 1455,
        callbackPath: "/auth/callback",
        needsCode: false,
        userCode: null,
      },
      "codex"
    )
    expect(
      await armSigninExtension("bridge", {
        attemptId: ATTEMPT,
        provider: "codex",
        verificationUrl: approval.verificationUrl,
        expiresAtSec: expires,
        open: true,
      })
    ).toEqual({ armed: true, opened: true })
    expect(installed.opened).toEqual([codexUrl()])
    expect(installed.registry()).toEqual([
      expect.objectContaining({
        attemptId: ATTEMPT,
        expiresAtMs: expires * 1000,
      }),
    ])
    const [rule] = [...installed.rules.values()]
    expect(rule.condition).toMatchObject({
      resourceTypes: ["main_frame"],
      requestMethods: ["get"],
      isUrlFilterCaseSensitive: true,
    })

    // The provider sends the browser back to the CLI's loopback: the rule turns
    // THIS attempt's callback into the Neev page, and the page reads it back.
    const back = redirectFor(
      installed.rules.values(),
      `http://localhost:1455/auth/callback?code=ac_1&scope=openid&state=${STATE}`
    )
    expect(back).toBe(
      `https://app.neev.so/agents/signin#attempt=${ATTEMPT}&provider=codex` +
        `&code=ac_1&scope=openid&state=${STATE}`
    )
    expect(parseSigninFragment(new URL(back!).hash)).toEqual({
      attemptId: ATTEMPT,
      provider: "codex",
      query: `code=ac_1&scope=openid&state=${STATE}`,
    })
    // A local `codex login` on the same port has its own state: untouched.
    expect(
      redirectFor(
        installed.rules.values(),
        "http://127.0.0.1:1455/auth/callback?code=local&state=someoneElsesState01"
      )
    ).toBeNull()
  })

  it("is what the relay refuses when the units or the old fields come back", () => {
    const context = { origin: signin.SIGNIN_ORIGIN, fromTab: true }
    const base = {
      type: "neev.signin.arm",
      attemptId: ATTEMPT,
      provider: "codex",
      verificationUrl: codexUrl(),
    }
    const now = Date.now()
    expect(
      signin.validateArm(
        { ...base, expiresAtSec: expiresAtSec() },
        context,
        now
      ).ok
    ).toBe(true)
    // Milliseconds are ~1000x too far out: refused, not armed forever.
    expect(
      signin.validateArm(
        { ...base, expiresAtSec: expiresAtSec() * 1000 },
        context,
        now
      ).error
    ).toBe("expiry")
    // v1 fields fail loudly.
    expect(
      signin.validateArm(
        {
          ...base,
          expiresAt: expiresAtSec(),
          port: 1455,
          path: "/auth/callback",
        },
        context,
        now
      ).error
    ).toBe("shape")
  })

  it("keeps two attempts armed at once and disarms only its own", async () => {
    installed = installSigninExtension({ openTab: false })
    expect(
      await armSigninExtension("bridge", {
        attemptId: ATTEMPT,
        provider: "codex",
        verificationUrl: codexUrl(),
        expiresAtSec: expiresAtSec(),
        open: false,
      })
    ).toEqual({ armed: true, opened: false })
    expect(
      (
        await armSigninExtension("bridge", {
          attemptId: OTHER,
          provider: "claude",
          verificationUrl: claudeUrl(53117),
          expiresAtSec: expiresAtSec(),
          open: false,
        })
      ).armed
    ).toBe(true)
    expect(installed.rules.size).toBe(2)
    disarmSigninExtension(ATTEMPT, "bridge")
    await vi.waitFor(() => expect(installed!.rules.size).toBe(1))
    expect(installed.registry().map((entry) => entry.attemptId)).toEqual([
      OTHER,
    ])
  })

  it("reports a refused arm (capacity, a bad URL) as not armed", async () => {
    installed = installSigninExtension()
    // Distinct attempts fill every slot; the next one has nowhere to go.
    const filler = ["1", "2", "3", "4"].map((digit) => digit.repeat(32))
    expect(filler).toHaveLength(signin.MAX_RULES)
    for (const [slot, attemptId] of filler.entries())
      expect(
        (
          await armSigninExtension("bridge", {
            attemptId,
            provider: "codex",
            verificationUrl: codexUrl(`state-number-${slot}-xxxxxxxx`),
            expiresAtSec: expiresAtSec(),
            open: false,
          })
        ).armed
      ).toBe(true)
    expect(
      await armSigninExtension("bridge", {
        attemptId: ATTEMPT,
        provider: "codex",
        verificationUrl: codexUrl(),
        expiresAtSec: expiresAtSec(),
        open: true,
      })
    ).toEqual({ armed: false, opened: false })
    // A URL off the pinned prefix never arms, whatever the page says.
    expect(
      await armSigninExtension("bridge", {
        attemptId: OTHER,
        provider: "codex",
        verificationUrl: "https://evil.test/oauth/authorize?state=" + STATE,
        expiresAtSec: expiresAtSec(),
        open: true,
      })
    ).toEqual({ armed: false, opened: false })
  })

  it("falls back to the legacy id only when the bridge is silent", async () => {
    vi.useFakeTimers()
    installed = installSigninExtension({ bridge: false, legacy: true })
    const found = discoverSigninExtension()
    await vi.advanceTimersByTimeAsync(NEEV_SIGNIN_PING_MS)
    expect(await found).toBe("legacy")
    vi.useRealTimers()
    expect(
      await armSigninExtension("legacy", {
        attemptId: ATTEMPT,
        provider: "codex",
        verificationUrl: codexUrl(),
        expiresAtSec: expiresAtSec(),
        open: true,
      })
    ).toEqual({ armed: true, opened: true })
  })
})

describe("bridge boundaries", () => {
  it("treats silence as no extension, within the contract's timeouts", async () => {
    vi.useFakeTimers()
    // No content script and no legacy extension.
    installed = installSigninExtension({ bridge: false, legacy: false })
    const found = discoverSigninExtension()
    await vi.advanceTimersByTimeAsync(NEEV_SIGNIN_PING_MS - 1)
    let settled = false
    void found.then(() => (settled = true))
    await vi.advanceTimersByTimeAsync(0)
    expect(settled).toBe(false)
    await vi.advanceTimersByTimeAsync(1)
    expect(await found).toBeNull()

    const arming = armSigninExtension("bridge", {
      attemptId: ATTEMPT,
      provider: "codex",
      verificationUrl: codexUrl(),
      expiresAtSec: expiresAtSec(),
      open: true,
    })
    await vi.advanceTimersByTimeAsync(NEEV_SIGNIN_ARM_MS)
    expect(await arming).toEqual({ armed: false, opened: false })
  })

  it("posts only to its own origin, never '*'", async () => {
    installed = installSigninExtension()
    await discoverSigninExtension()
    const targets = vi
      .mocked(window.postMessage)
      .mock.calls.map((call) => call[1])
    expect(new Set(targets)).toEqual(new Set(["https://app.neev.so"]))
  })

  it("ignores answers from another origin, another source, or for another id", async () => {
    vi.useFakeTimers()
    installed = installSigninExtension({ bridge: false })
    // Capture the page's request id, then answer it from every wrong place.
    let id = ""
    window.addEventListener("message", (event) => {
      const data = event.data as { direction?: string; id?: string }
      if (data?.direction === "page" && data.id) id = data.id
    })
    const found = discoverSigninExtension()
    await vi.advanceTimersByTimeAsync(1)
    expect(id).toMatch(/^[A-Za-z0-9_-]{1,64}$/)
    const answer = (overrides: Record<string, unknown>) => ({
      channel: NEEV_SIGNIN_CHANNEL,
      direction: "extension",
      id,
      response: { ok: true, version: "0.2.1", bridge: true },
      ...overrides,
    })
    postForeign(answer({}), { origin: "https://evil.test", source: window })
    postForeign(answer({}), { source: null }) // another frame/worker
    postForeign(answer({ id: "someone-else" }), { source: window })
    postForeign(answer({ channel: "other" }), { source: window })
    postForeign(answer({ direction: "page" }), { source: window })
    postForeign({ ...answer({}), response: undefined }, { source: window })
    await vi.advanceTimersByTimeAsync(2 * NEEV_SIGNIN_PING_MS)
    expect(await found).toBeNull()
  })

  it("reads answers strictly, so a hostile or broken one is 'absent'", () => {
    for (const hostile of [
      null,
      undefined,
      "ok",
      [],
      [{ ok: true, version: "1" }],
      { ok: "true", version: "1" },
      { ok: true },
      { ok: true, version: 2 },
      { ok: true, version: "" },
      { ok: true, version: "x".repeat(33) },
    ])
      expect(isPingAnswer(hostile)).toBe(false)
    expect(isPingAnswer({ ok: true, version: "0.2.1", bridge: true })).toBe(
      true
    )

    for (const hostile of [
      null,
      "ok",
      [],
      { ok: "true", opened: true },
      { ok: 1 },
    ])
      expect(readArmAnswer(hostile)).toEqual({ armed: false, opened: false })
    expect(readArmAnswer({ ok: false, error: "capacity" })).toEqual({
      armed: false,
      opened: false,
    })
    // Only a literal true counts as "the extension opened it".
    expect(readArmAnswer({ ok: true, opened: "yes" })).toEqual({
      armed: true,
      opened: false,
    })
  })
})
