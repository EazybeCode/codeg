// @vitest-environment-options {"url": "https://app.neev.so/settings"}
//
// Claude and Codex sign in through the CLI's own browser login first
// (`direct-signin-ui-contract-v2.md`). What these pin: one primary action; the
// extension — found through its page bridge, run for real — opens the page
// itself; fallbacks tucked away while the browser can come back by itself and
// up front when it cannot; a pasted address checked before it is sent and
// never kept; every `/callback` status handled as the contract says; and
// "Connected" only from the server.
import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import { NativeAgentConnectionsPanel } from "./native-agent-connections"
import { NativeBoundary } from "./native-boundary"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"
import {
  installBrowserPostMessage,
  installSigninExtension,
} from "./test-signin-extension"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const ATTEMPT_ID = "0123456789abcdef0123456789abcdef"
const FAR_FUTURE = Math.floor(Date.now() / 1000) + 600
const STATE = "Zq8v-3kP_wR2mX9aL0bN7cY4"
/** Shaped like the pinned CLIs' real authorize URLs: loopback redirect, state. */
const CODEX_AUTH =
  "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_x" +
  `&redirect_uri=${encodeURIComponent("http://localhost:1455/auth/callback")}` +
  `&state=${STATE}`
const CLAUDE_AUTH =
  "https://claude.com/cai/oauth/authorize?code=true&client_id=x" +
  `&redirect_uri=${encodeURIComponent("http://localhost:53117/callback")}` +
  `&state=${STATE}`
/** The CLIs' own loopback wait, beyond the extension's 1.5 s ping. */
const DISCOVERY = { timeout: 4_000 }

const base = (provider: string) => `/api/neev/agent/connections/${provider}`

const CODEX_CALLBACK = {
  kind: "browserCallback",
  verificationUrl: CODEX_AUTH,
  callbackPort: 1455,
  callbackPath: "/auth/callback",
  // No fallbackUrl key at all, exactly as `approval_dto` sends it for Codex.
  needsCode: false,
  userCode: null,
}
const CLAUDE_CALLBACK = {
  ...CODEX_CALLBACK,
  verificationUrl: CLAUDE_AUTH,
  callbackPort: 53117,
  callbackPath: "/callback",
  fallbackUrl: CLAUDE_AUTH + "&paste=1",
}

function row(provider: string, state: string) {
  return {
    provider,
    state,
    accountLabel: null,
    checkedAt: null,
    errorCode: null,
  }
}

function pendingConnect() {
  return json(
    {
      outcome: "pending",
      attemptId: ATTEMPT_ID,
      expiresAt: FAR_FUTURE,
      connection: null,
    },
    202
  )
}

function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentConnectionsPanel pollMs={5} showSettingsLink={false} />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

/**
 * One provider's browser-callback sign-in. `callback` answers the `/callback`
 * POSTs in order (default: delivered, still pending); `state` flips the poll.
 */
function signIn(
  provider: "codex" | "claude",
  approval: unknown,
  callback: (() => Response)[] = []
) {
  const state = { connected: false, completed: false, failed: false }
  const posted: { url: string; body: Record<string, unknown> }[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          row(provider, state.connected ? "connected" : "not_signed_in"),
        ],
      }),
    [`POST ${base(provider)}/connect`]: (init) => {
      posted.push({ url: "connect", body: JSON.parse(init.body as string) })
      return pendingConnect()
    },
    [`GET ${base(provider)}/attempts/${ATTEMPT_ID}`]: () =>
      state.completed
        ? json({ state: "completed", expiresAt: FAR_FUTURE, approval: null })
        : state.failed
          ? json({ state: "failed", expiresAt: FAR_FUTURE, approval: null })
          : json({ state: "pending", expiresAt: FAR_FUTURE, approval }),
    [`POST ${base(provider)}/attempts/${ATTEMPT_ID}/callback`]: (init) => {
      posted.push({ url: "callback", body: JSON.parse(init.body as string) })
      const next = callback.shift()
      return next
        ? next()
        : json({ state: "pending", expiresAt: FAR_FUTURE, approval: null }, 202)
    },
  })
  const callbacks = () => posted.filter((call) => call.url === "callback")
  return { state, posted, callbacks, mock }
}

async function startCodex(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Connect codex" }))
}

async function pasteAddress(
  user: ReturnType<typeof userEvent.setup>,
  address: string
) {
  const field = await screen.findByRole(
    "textbox",
    { name: "Paste the address from the tab that says it can't connect" },
    DISCOVERY
  )
  await user.type(field, address)
  await user.click(screen.getByRole("button", { name: "Continue" }))
  return field
}

let extension: ReturnType<typeof installSigninExtension> | null = null
beforeEach(() => installBrowserPostMessage())
afterEach(() => {
  extension?.reset()
  extension = null
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  localStorage.clear()
})

it("finds the extension by its bridge and lets it open the page — fallbacks stay tucked away", async () => {
  extension = installSigninExtension()
  const open = vi.fn()
  vi.stubGlobal("open", open)
  const { state, posted } = signIn("codex", CODEX_CALLBACK)
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  const go = await screen.findByRole("button", { name: "Continue in browser" })
  await waitFor(() => expect(go).toBeEnabled())
  expect(
    screen.getByText("Browser didn't come back?").closest("details")
  ).not.toHaveAttribute("open")
  expect(screen.queryByText(/isn't answering here/)).toBeNull()
  // Asked for by name: the server reads an absent method as the legacy flow.
  expect(posted[0].body).toEqual({
    requestKey: expect.any(String),
    method: "browser",
  })

  await user.click(go)
  // The extension's real relay accepted the arm and opened the tab itself.
  await waitFor(() => expect(extension!.opened).toEqual([CODEX_AUTH]))
  expect(extension.received).toContainEqual({
    type: "neev.signin.arm",
    attemptId: ATTEMPT_ID,
    provider: "codex",
    verificationUrl: CODEX_AUTH,
    expiresAtSec: FAR_FUTURE, // the API's seconds, verbatim
    open: true,
  })
  expect(extension.rules.size).toBe(1)
  // So the page did not open a second one, and needs no backup link.
  expect(open).not.toHaveBeenCalled()
  expect(
    screen.queryByRole("link", { name: "Open the sign-in page" })
  ).toBeNull()
  // The browser has not come back yet: still waiting, never "Connected".
  expect(screen.getByText("Waiting for sign-in…")).toBeVisible()
  expect(screen.queryByText("Connected")).toBeNull()
  // The server settling it is what connects it — and drops only its rule.
  state.connected = true
  state.completed = true
  expect(await screen.findByText("Connected")).toBeVisible()
  await waitFor(() => expect(extension!.rules.size).toBe(0))
})

it("opens the page itself, with a backup link, when the extension armed but did not open it", async () => {
  extension = installSigninExtension({ openTab: false })
  const open = vi.fn()
  vi.stubGlobal("open", open)
  signIn("codex", CODEX_CALLBACK)
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  const go = await screen.findByRole("button", { name: "Continue in browser" })
  await waitFor(() => expect(go).toBeEnabled())
  await user.click(go)
  await waitFor(() =>
    expect(open).toHaveBeenCalledWith(CODEX_AUTH, "_blank", "noreferrer")
  )
  // Our own open ran after an await, where a popup blocker may have eaten it.
  expect(
    await screen.findByRole("link", { name: "Open the sign-in page" })
  ).toHaveAttribute("href", CODEX_AUTH)
  // Still armed: the browser can come back by itself, so no paste box up front.
  expect(extension.rules.size).toBe(1)
  expect(
    screen.getByText("Browser didn't come back?").closest("details")
  ).not.toHaveAttribute("open")
})

it("treats a refused arm as no relay: opens the page and puts the paste box in front", async () => {
  // The relay's clock is past this attempt's expiry, so its real validation
  // answers `{ok:false, error:"expiry"}` — as any refusal would.
  extension = installSigninExtension({ now: () => (FAR_FUTURE + 60) * 1000 })
  const open = vi.fn()
  vi.stubGlobal("open", open)
  signIn("codex", CODEX_CALLBACK)
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  const go = await screen.findByRole("button", { name: "Continue in browser" })
  await waitFor(() => expect(go).toBeEnabled())
  await user.click(go)
  await waitFor(() =>
    expect(open).toHaveBeenCalledWith(CODEX_AUTH, "_blank", "noreferrer")
  )
  expect(extension.rules.size).toBe(0)
  expect(extension.opened).toEqual([])
  expect(await screen.findByText(/isn't answering here/)).toBeVisible()
  expect(
    screen.getByText("Browser didn't come back?").closest("details")
  ).toHaveAttribute("open")
})

it("with no extension, opens the page in the click and puts the paste box up front", async () => {
  const open = vi.fn()
  vi.stubGlobal("open", open)
  const { state, callbacks } = signIn("codex", CODEX_CALLBACK)
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  expect(
    await screen.findByText(/isn't answering here/, {}, DISCOVERY)
  ).toBeVisible()
  expect(screen.getByText(/State mismatch/)).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Continue in browser" }))
  expect(open).toHaveBeenCalledWith(CODEX_AUTH, "_blank", "noreferrer")

  // Another port is another sign-in: refused here, never sent.
  const field = await pasteAddress(
    user,
    `http://localhost:9999/auth/callback?code=ac_1&state=${STATE}`
  )
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "That isn't the address this sign-in is waiting for"
  )
  expect(field).toHaveValue("")
  expect(callbacks()).toEqual([])

  await pasteAddress(
    user,
    `http://localhost:1455/auth/callback?code=ac_1&state=${STATE}`
  )
  await waitFor(() =>
    expect(callbacks()).toEqual([
      {
        url: "callback",
        body: {
          requestKey: expect.stringMatching(/^[A-Za-z0-9_-]{8,64}$/),
          query: `code=ac_1&state=${STATE}`,
        },
      },
    ])
  )
  expect(field).toHaveValue("")
  expect(screen.queryByText(/ac_1/)).toBeNull()
  expect(JSON.stringify({ ...localStorage })).not.toContain("ac_1")
  expect(screen.getByText("Waiting for sign-in…")).toBeVisible()
  expect(screen.queryByText("Connected")).toBeNull()
  state.connected = true
  state.completed = true
  expect(await screen.findByText("Connected")).toBeVisible()
})

it("says each refusal the way the contract does, and keeps the sign-in alive", async () => {
  const { callbacks } = signIn("codex", CODEX_CALLBACK, [
    () => json({ code: "invalid" }, 400),
    () => json({ code: "not_found" }, 404),
    () => json({ code: "limited" }, 429),
  ])
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  const address = `http://127.0.0.1:1455/auth/callback?code=ac_1&state=${STATE}`
  for (const text of [
    "That address didn't match this sign-in. Copy it again, or start again.",
    "This sign-in isn't available.",
    "Too many tries — start again.",
  ]) {
    await pasteAddress(user, address)
    expect(await screen.findByText(text, {}, DISCOVERY)).toBeVisible()
  }
  expect(callbacks()).toHaveLength(3)
  // Each paste is its own delivery.
  expect(new Set(callbacks().map((call) => call.body.requestKey)).size).toBe(3)
  expect(
    screen.getByRole("button", { name: "Cancel codex sign-in" })
  ).toBeEnabled()
})

it("retries an unreachable delivery under the SAME request key", async () => {
  const { state, callbacks } = signIn("codex", CODEX_CALLBACK, [
    () => json({ code: "unavailable" }, 503),
  ])
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  await pasteAddress(
    user,
    `http://localhost:1455/auth/callback?code=ac_1&state=${STATE}`
  )
  expect(
    await screen.findByText("Couldn't reach the sign-in; try again.")
  ).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Try again" }))
  await waitFor(() => expect(callbacks()).toHaveLength(2))
  expect(callbacks()[1].body).toEqual(callbacks()[0].body)
  state.connected = true
  state.completed = true
  expect(await screen.findByText("Connected")).toBeVisible()
})

it("reads a 409 as 'already delivered': no error, the poll settles it", async () => {
  const { state } = signIn("codex", CODEX_CALLBACK, [
    () => {
      // The extension's relay got there first.
      state.connected = true
      state.completed = true
      return json({ code: "conflict" }, 409)
    },
  ])
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  await pasteAddress(
    user,
    `http://localhost:1455/auth/callback?code=ac_1&state=${STATE}`
  )
  expect(await screen.findByText("Connected")).toBeVisible()
  expect(screen.queryByRole("alert")).toBeNull()
})

it("says a declined sign-in was cancelled", async () => {
  const { state } = signIn("codex", CODEX_CALLBACK, [
    () => {
      state.failed = true
      return json(
        { state: "pending", expiresAt: FAR_FUTURE, approval: null },
        202
      )
    },
  ])
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  await pasteAddress(
    user,
    `http://localhost:1455/auth/callback?error=access_denied&state=${STATE}`
  )
  expect(await screen.findByText("Sign-in was cancelled.")).toBeVisible()
})

it("lets Codex fall back to a device code by cancelling and starting again", async () => {
  vi.stubGlobal("open", vi.fn())
  let rowState = "not_signed_in"
  let method: string | undefined
  const order: string[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [row("codex", rowState)] }),
    [`POST ${base("codex")}/connect`]: (init) => {
      method = JSON.parse(init.body as string).method
      order.push(`connect:${method ?? "default"}`)
      rowState = "connecting"
      return pendingConnect()
    },
    [`POST ${base("codex")}/cancel`]: () => {
      order.push("cancel")
      rowState = "not_signed_in"
      return json({
        operationId: "fedcba9876543210fedcba9876543210",
        generation: "2",
        connection: row("codex", "not_signed_in"),
      })
    },
    [`GET ${base("codex")}/attempts/${ATTEMPT_ID}`]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval:
          method === "device"
            ? {
                kind: "device",
                verificationUrl: "https://auth.openai.com/codex/device",
                userCode: "WXYZ-12345",
                needsCode: false,
              }
            : CODEX_CALLBACK,
      }),
  })
  const user = userEvent.setup()
  mount()
  await startCodex(user)
  await user.click(await screen.findByText("Browser didn't come back?"))
  await user.click(screen.getByRole("button", { name: "Use a code instead" }))
  expect(await screen.findByText("WXYZ-12345")).toBeVisible()
  expect(order).toEqual(["connect:browser", "cancel", "connect:device"])
})

it("offers Claude today's paste-a-code page as the secondary path", async () => {
  extension = installSigninExtension()
  const submitted: unknown[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [row("claude", "not_signed_in")],
      }),
    [`POST ${base("claude")}/connect`]: () => pendingConnect(),
    [`GET ${base("claude")}/attempts/${ATTEMPT_ID}`]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: CLAUDE_CALLBACK,
      }),
    [`POST ${base("claude")}/attempts/${ATTEMPT_ID}/code`]: (init) => {
      submitted.push(JSON.parse(init.body as string))
      return json(
        { state: "pending", expiresAt: FAR_FUTURE, approval: null },
        202
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Connect claude" })
  )
  const details = (
    await screen.findByText("Browser didn't come back?")
  ).closest("details")!
  expect(
    within(details).queryByRole("button", { name: "Use a code instead" })
  ).toBeNull()
  await user.click(screen.getByText("Browser didn't come back?"))
  await user.click(
    within(details).getByRole("button", { name: "Paste a code instead" })
  )
  expect(
    within(details).getByRole("link", {
      name: "Open the page that shows a code",
    })
  ).toHaveAttribute("href", CLAUDE_CALLBACK.fallbackUrl)
  await user.type(
    within(details).getByRole("textbox", {
      name: "Paste the authorization code",
    }),
    "ac_paste"
  )
  await user.click(within(details).getByRole("button", { name: "Submit code" }))
  await waitFor(() => expect(submitted).toEqual([{ code: "ac_paste" }]))
})

it("does not ask for device-code setup over a browser sign-in", async () => {
  vi.stubGlobal("open", vi.fn())
  signIn("codex", CODEX_CALLBACK)
  const user = userEvent.setup()
  mount()
  expect(
    await screen.findByText(/needs device code authorization/)
  ).toBeVisible()
  await startCodex(user)
  await screen.findByRole("button", { name: "Continue in browser" })
  expect(screen.queryByText(/needs device code authorization/)).toBeNull()
})
