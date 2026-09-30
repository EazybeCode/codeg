// @vitest-environment-options {"url": "https://app.neev.so/"}
//
// `/agents/signin`: the extension's landing page. What matters: the fragment is
// gone from the address bar at once and never kept; nothing is sent until the
// server confirms the attempt is this user's, pending, in time and waiting on a
// browser; and "Connected" is said only after the server says so.
import { render, screen, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import {
  captureSigninFragment,
  NativeSigninReturn,
} from "./native-signin-return"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"
import {
  installBrowserPostMessage,
  installSigninExtension,
} from "./test-signin-extension"
import { armSigninExtension } from "@/lib/neev-signin-extension"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const ATTEMPT_ID = "0123456789abcdef0123456789abcdef"
const FAR_FUTURE = Math.floor(Date.now() / 1000) + 600
const SECRET = "ac_SECRET_code_value"
const HASH = `#attempt=${ATTEMPT_ID}&provider=codex&code=${SECRET}&state=st_1`
const ATTEMPT = `GET /api/neev/agent/connections/codex/attempts/${ATTEMPT_ID}`
const CALLBACK = `POST /api/neev/agent/connections/codex/attempts/${ATTEMPT_ID}/callback`

const CALLBACK_APPROVAL = {
  kind: "browserCallback",
  verificationUrl: "https://auth.openai.com/oauth/authorize?state=x",
  callbackPort: 1455,
  callbackPath: "/auth/callback",
  fallbackUrl: null,
  needsCode: false,
  userCode: null,
}
const waiting = () =>
  json({ state: "pending", expiresAt: FAR_FUTURE, approval: CALLBACK_APPROVAL })
const connections = (state: string) => () =>
  json({
    runtimeEnabled: true,
    connections: [
      {
        provider: "codex",
        state,
        accountLabel: null,
        checkedAt: null,
        errorCode: null,
      },
    ],
  })

function arrive(hash = HASH) {
  window.history.replaceState(null, "", `/agents/signin${hash}`)
}

function mount() {
  return render(
    <NativeTestRoot>
      <NativeSigninReturn pollMs={5} maxPolls={20} />
    </NativeTestRoot>
  )
}

const logged: unknown[] = []
beforeEach(() => {
  logged.length = 0
  for (const level of ["log", "info", "warn", "error", "debug"] as const)
    vi.spyOn(console, level).mockImplementation((...args) => {
      logged.push(args)
    })
})
let harness: ReturnType<typeof installSigninExtension> | null = null
afterEach(() => {
  harness?.reset()
  harness = null
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  window.history.replaceState(null, "", "/")
  localStorage.clear()
  sessionStorage.clear()
})

/** Nothing about the callback may be left behind anywhere a script can read. */
function expectNoTrace() {
  expect(window.location.hash).toBe("")
  expect(window.location.href).not.toContain(SECRET)
  expect(JSON.stringify(window.history.state)).not.toContain(SECRET)
  expect(JSON.stringify({ ...localStorage })).not.toContain(SECRET)
  expect(JSON.stringify({ ...sessionStorage })).not.toContain(SECRET)
  expect(document.body.textContent).not.toContain(SECRET)
  expect(JSON.stringify(logged)).not.toContain(SECRET)
}

it("removes the fragment from the address bar the moment it is read", () => {
  arrive()
  captureSigninFragment()
  expect(window.location.pathname).toBe("/agents/signin")
  expect(window.location.hash).toBe("")
  expectNoTrace()
})

it("checks the attempt, delivers once, and waits for the server before saying Connected", async () => {
  // The real extension, with this attempt armed — as it is when it sent the
  // browser here. The page must drop THIS attempt's rule once delivered.
  installBrowserPostMessage()
  const extension = (harness = installSigninExtension())
  const state = "st_1_the_attempts_own_state"
  expect(
    (
      await armSigninExtension("bridge", {
        attemptId: ATTEMPT_ID,
        provider: "codex",
        verificationUrl:
          "https://auth.openai.com/oauth/authorize?client_id=x&redirect_uri=" +
          encodeURIComponent("http://localhost:1455/auth/callback") +
          `&state=${state}`,
        expiresAtSec: FAR_FUTURE,
        open: false,
      })
    ).armed
  ).toBe(true)
  expect(extension.rules.size).toBe(1)
  const order: string[] = []
  let polls = 0
  let release = () => {}
  const held = new Promise<void>((resolve) => (release = resolve))
  const bodies: unknown[] = []
  const mock = mockNativeSession(undefined, {
    [ATTEMPT]: async () => {
      if (!order.includes("callback")) {
        order.push("check")
        return waiting()
      }
      polls += 1
      if (polls === 1) {
        await held
        return json({ state: "pending", expiresAt: FAR_FUTURE, approval: null })
      }
      return json({ state: "completed", expiresAt: FAR_FUTURE, approval: null })
    },
    [CALLBACK]: (init) => {
      order.push("callback")
      bodies.push(JSON.parse(init.body as string))
      expect((init.headers as Record<string, string>)["x-neev-csrf"]).toBe(
        "synthetic-csrf"
      )
      return json({ state: "pending", expiresAt: FAR_FUTURE, approval: null })
    },
    "GET /api/neev/agent/connections": connections("connected"),
  })
  arrive()
  mount()
  // Delivered, not done: the CLI is still finishing.
  expect(
    await screen.findByText(
      "Almost done — waiting for the provider to confirm…"
    )
  ).toBeVisible()
  expect(screen.queryByText(/Connected/)).toBeNull()
  expect(order).toEqual(["check", "callback"])
  expect(bodies).toEqual([
    { requestKey: expect.any(String), query: `code=${SECRET}&state=st_1` },
  ])
  await waitFor(() => expect(extension.rules.size).toBe(0))
  release()
  expect(
    await screen.findByText("Connected — you can close this tab.")
  ).toBeVisible()
  expect(order.filter((step) => step === "callback")).toHaveLength(1)
  expect(mock.unknown).toEqual([])
  expectNoTrace()
})

it("sends nothing for an attempt that is no longer waiting on a browser", async () => {
  for (const [answer, text] of [
    [
      json({ state: "expired", expiresAt: FAR_FUTURE, approval: null }),
      "This sign-in expired. Start again from Neev.",
    ],
    [
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: {
          kind: "device",
          verificationUrl: "https://auth.openai.com/codex/device",
          userCode: "ABCD-EFGH",
          needsCode: false,
        },
      }),
      "This sign-in is no longer waiting for the browser. Start again from Neev.",
    ],
    [
      json({
        state: "pending",
        expiresAt: Math.floor(Date.now() / 1000) - 5,
        approval: CALLBACK_APPROVAL,
      }),
      "This sign-in expired. Start again from Neev.",
    ],
    // Not this user's, or unknown.
    [json({ code: "not_found" }, 404), "This sign-in isn't available."],
  ] as const) {
    const posted: string[] = []
    mockNativeSession(undefined, {
      [ATTEMPT]: () => answer.clone(),
      [CALLBACK]: () => {
        posted.push("callback")
        return waiting()
      },
    })
    arrive()
    const view = mount()
    expect(await screen.findByText(text)).toBeVisible()
    expect(posted).toEqual([])
    expectNoTrace()
    view.unmount()
    vi.unstubAllGlobals()
  }
})

it("says a refused callback plainly, and never Connected", async () => {
  mockNativeSession(undefined, {
    [ATTEMPT]: waiting,
    [CALLBACK]: () => json({ code: "invalid" }, 400),
  })
  arrive()
  mount()
  expect(
    await screen.findByText(
      "That address didn't match this sign-in. Copy it again, or start again."
    )
  ).toBeVisible()
  expect(screen.queryByText(/Connected/)).toBeNull()
  expectNoTrace()
})

it("does not call it Connected when the attempt finished but the row did not", async () => {
  let delivered = false
  mockNativeSession(undefined, {
    [ATTEMPT]: () =>
      delivered
        ? json({ state: "completed", expiresAt: FAR_FUTURE, approval: null })
        : waiting(),
    [CALLBACK]: () => {
      delivered = true
      return json({ state: "pending", expiresAt: FAR_FUTURE, approval: null })
    },
    "GET /api/neev/agent/connections": connections("reconnect_required"),
  })
  arrive()
  mount()
  expect(
    await screen.findByText(
      "The sign-in finished, but the connection isn't ready yet. Check it in Neev."
    )
  ).toBeVisible()
  expect(screen.queryByText(/you can close this tab/)).toBeNull()
})

it("drops the callback when nobody is signed in here", async () => {
  const mock = mockNativeSession(null)
  arrive()
  mount()
  expect(
    await screen.findByText(
      "Open Neev in this browser and sign in, then press Continue in browser again. Nothing from this page was kept."
    )
  ).toBeVisible()
  // Not even asked about: no attempt read, no callback sent.
  const asked = mock.fetcher.mock.calls.map(([path]) => String(path))
  expect(asked.some((path) => path.includes("/attempts/"))).toBe(false)
  expectNoTrace()
})

it("refuses a fragment that is not one attempt's callback, sending nothing", async () => {
  const mock = mockNativeSession(undefined)
  arrive(`#attempt=${ATTEMPT_ID}&provider=codex&code=${SECRET}`)
  mount()
  expect(
    await screen.findByText(
      "There's nothing to finish here. Start the sign-in from Neev."
    )
  ).toBeVisible()
  await waitFor(() =>
    expect(
      mock.fetcher.mock.calls.some(([path]) =>
        String(path).includes("/agent/connections")
      )
    ).toBe(false)
  )
  expectNoTrace()
})

it("follows the attempt when the callback had already arrived (409)", async () => {
  let delivered = false
  mockNativeSession(undefined, {
    [ATTEMPT]: () =>
      delivered
        ? json({ state: "completed", expiresAt: FAR_FUTURE, approval: null })
        : waiting(),
    // A paste (or a second relay) got there first.
    [CALLBACK]: () => {
      delivered = true
      return json({ code: "conflict" }, 409)
    },
    "GET /api/neev/agent/connections": connections("connected"),
  })
  arrive()
  mount()
  expect(
    await screen.findByText("Connected — you can close this tab.")
  ).toBeVisible()
  expectNoTrace()
})

it("replays an unreachable delivery under the same key, then follows it", async () => {
  const keys: unknown[] = []
  let delivered = false
  mockNativeSession(undefined, {
    [ATTEMPT]: () =>
      delivered
        ? json({ state: "completed", expiresAt: FAR_FUTURE, approval: null })
        : waiting(),
    [CALLBACK]: (init) => {
      keys.push(JSON.parse(init.body as string).requestKey)
      // The supervisor is briefly away twice; the gate is not used up.
      if (keys.length < 3) return json({ code: "unavailable" }, 503)
      delivered = true
      return json({ state: "pending", expiresAt: FAR_FUTURE, approval: null })
    },
    "GET /api/neev/agent/connections": connections("connected"),
  })
  arrive()
  mount()
  expect(
    await screen.findByText("Connected — you can close this tab.")
  ).toBeVisible()
  expect(keys).toHaveLength(3)
  expect(new Set(keys).size).toBe(1)
  expect(keys[0]).toMatch(/^[A-Za-z0-9_-]{8,64}$/)
  expectNoTrace()
})

it("offers Try again, with the same key, when the server stays unreachable", async () => {
  const keys: unknown[] = []
  let up = false
  mockNativeSession(undefined, {
    [ATTEMPT]: () =>
      up
        ? json({ state: "completed", expiresAt: FAR_FUTURE, approval: null })
        : waiting(),
    [CALLBACK]: (init) => {
      keys.push(JSON.parse(init.body as string).requestKey)
      return up
        ? json({ state: "pending", expiresAt: FAR_FUTURE, approval: null })
        : json({ code: "unavailable" }, 503)
    },
    "GET /api/neev/agent/connections": connections("connected"),
  })
  arrive()
  mount()
  const retry = await screen.findByRole("button", { name: "Try again" })
  expect(screen.queryByText(/Connected/)).toBeNull()
  up = true
  retry.click()
  expect(
    await screen.findByText("Connected — you can close this tab.")
  ).toBeVisible()
  expect(new Set(keys).size).toBe(1)
  expectNoTrace()
})

it("says each final refusal of the callback the contract's way", async () => {
  for (const [status, text] of [
    [404, "This sign-in isn't available."],
    [429, "Too many tries — start again."],
  ] as const) {
    mockNativeSession(undefined, {
      [ATTEMPT]: waiting,
      [CALLBACK]: () => json({ code: "x" }, status),
    })
    arrive()
    const view = mount()
    expect(await screen.findByText(text)).toBeVisible()
    expect(screen.queryByText(/Connected/)).toBeNull()
    expectNoTrace()
    view.unmount()
    vi.unstubAllGlobals()
  }
})

it("says a sign-in declined on the provider's page was cancelled", async () => {
  let delivered = false
  arrive(
    `#attempt=${ATTEMPT_ID}&provider=claude&error=access_denied&state=st_1`
  )
  // Claude's attempt lives under its own provider path.
  mockNativeSession(undefined, {
    [ATTEMPT.replace("/codex/", "/claude/")]: () =>
      delivered
        ? json({ state: "failed", expiresAt: FAR_FUTURE, approval: null })
        : json({
            state: "pending",
            expiresAt: FAR_FUTURE,
            approval: {
              ...CALLBACK_APPROVAL,
              verificationUrl: "https://claude.com/cai/oauth/authorize?state=x",
              callbackPort: 53117,
              callbackPath: "/callback",
            },
          }),
    [CALLBACK.replace("/codex/", "/claude/")]: () => {
      delivered = true
      return json({ state: "pending", expiresAt: FAR_FUTURE, approval: null })
    },
  })
  mount()
  expect(await screen.findByText("Sign-in was cancelled.")).toBeVisible()
})
