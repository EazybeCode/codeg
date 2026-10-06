import { cleanup, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeAgentConnectionsPanel } from "./native-agent-connections"
import { NativeBoundary } from "./native-boundary"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const ATTEMPT_ID = "0123456789abcdef0123456789abcdef"
const OPERATION_ID = "fedcba9876543210fedcba9876543210"
const CONNECT = "POST /api/neev/agent/connections/claude/connect"
const CANCEL = "POST /api/neev/agent/connections/claude/cancel"
const DISCONNECT = "POST /api/neev/agent/connections/claude/disconnect"
const ATTEMPT = `GET /api/neev/agent/connections/claude/attempts/${ATTEMPT_ID}`

function connection(state: string, extra: Record<string, unknown> = {}) {
  return {
    provider: "claude",
    state,
    accountLabel: null,
    checkedAt: null,
    errorCode: null,
    ...extra,
  }
}

/**
 * `ReceiptDto` as both `cancel_connect` and `disconnect_provider` actually
 * return it: the two ids AND the connection as it stands afterwards. The old
 * fixtures omitted the connection, which is why a disconnect the server had
 * performed was reported to the user as an unexpected response.
 */
function receipt(state: string, generation = "3") {
  return json({
    operationId: OPERATION_ID,
    generation,
    connection: connection(state),
  })
}

function wiring(state: string, runtimeEnabled = true) {
  return { runtimeEnabled, connections: [connection(state)] }
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

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  localStorage.clear()
})

const STORAGE_KEY = "neev.agent.attempt.1:3"
const FAR_FUTURE = Math.floor(Date.now() / 1000) + 600

it("waits for the sign-in it started instead of claiming it connected", async () => {
  const bodies: Record<string, unknown>[] = []
  let polls = 0
  let state = "not_signed_in"
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring(state)),
    [CONNECT]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      state = "connecting"
      return json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      )
    },
    [ATTEMPT]: () => {
      polls += 1
      // Settles only once the server says so; the client never decides.
      return polls < 3
        ? json({ state: "pending", expiresAt: FAR_FUTURE })
        : (() => {
            state = "connected"
            return json({ state: "completed", expiresAt: FAR_FUTURE })
          })()
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  // A pending attempt is "waiting", never "Connected", and can be cancelled.
  expect(await screen.findByText("Waiting for sign-in…")).toBeVisible()
  expect(
    screen.getByRole("button", { name: "Cancel claude sign-in" })
  ).toBeEnabled()
  // No sign-in extension here: Claude starts its paste-a-code flow (the
  // absent method), never the loopback browser login that would send this
  // browser to its own localhost.
  expect(bodies[0]).toEqual({ requestKey: expect.any(String) })
  // Only the server flipping the connection makes it read Connected.
  expect(await screen.findByText("Connected")).toBeVisible()
  expect(polls).toBeGreaterThanOrEqual(3)
  expect(mock.unknown).toEqual([])
})

it("reports an attempt that lapses rather than leaving it spinning", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () =>
      json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      ),
    // With no supervisor bound this is what actually happens today.
    [ATTEMPT]: () => json({ state: "expired", expiresAt: FAR_FUTURE }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  expect(
    await screen.findByText("Sign-in expired before it completed.")
  ).toBeVisible()
  expect(screen.queryByText("Waiting for sign-in…")).toBeNull()
})

it("cancels an in-flight sign-in with the attempt it is actually waiting on", async () => {
  let canceled: Record<string, unknown> | null = null
  let state = "not_signed_in"
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring(state)),
    [CONNECT]: () => {
      state = "connecting"
      return json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      )
    },
    [ATTEMPT]: () => json({ state: "pending", expiresAt: FAR_FUTURE }),
    [CANCEL]: (init) => {
      canceled = JSON.parse(init.body as string)
      state = "not_signed_in"
      return receipt("not_signed_in")
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  await user.click(
    await screen.findByRole("button", { name: "Cancel claude sign-in" })
  )
  await waitFor(() =>
    expect(canceled).toEqual({
      attemptId: ATTEMPT_ID,
      requestKey: expect.any(String),
    })
  )
  expect(await screen.findByText("Sign-in canceled.")).toBeVisible()
})

it("replays one request key so a retried connect cannot start two sign-ins", async () => {
  const keys: string[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: (init) => {
      keys.push(
        (JSON.parse(init.body as string) as { requestKey: string }).requestKey
      )
      if (keys.length === 1) throw new TypeError("lost response")
      return json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      )
    },
    [ATTEMPT]: () => json({ state: "pending", expiresAt: FAR_FUTURE }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  expect(await screen.findByRole("alert")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Sign in with Claude" }))
  await waitFor(() => expect(keys).toHaveLength(2))
  // The same key means the server returns the SAME attempt.
  expect(keys[0]).toBe(keys[1])
})

/**
 * Live 2026-09-24: the supervisor could not provision a credential volume, so the
 * server cancelled the attempt it had just recorded and answered 503. The key was
 * kept, every later click replayed it, and the server — matching the key to that
 * cancelled attempt — answered 409 forever. The panel reported "This information
 * changed, or the sign-in method is linked to another account" for what was not an
 * account problem, and Connect never worked again without clearing site storage.
 */
it("retires a connect key the server has told us is spent", async () => {
  const keys: string[] = []
  let answer = 503
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: (init) => {
      keys.push(
        (JSON.parse(init.body as string) as { requestKey: string }).requestKey
      )
      if (answer !== 200) {
        const status = answer
        // The 503 leaves the key spent but does not PROVE it; the 409 does.
        answer = answer === 503 ? 409 : 200
        return json({ error: "refused" }, status)
      }
      return json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      )
    },
    [ATTEMPT]: () => json({ state: "pending", expiresAt: FAR_FUTURE }),
  })
  const user = userEvent.setup()
  mount()
  const connect = async () =>
    user.click(
      await screen.findByRole("button", { name: "Sign in with Claude" })
    )
  await connect()
  expect(await screen.findByRole("alert")).toBeVisible()
  // The 503 may have come from a gateway AFTER the backend admitted the request, so
  // the attempt may be live: the key is kept and this click replays it.
  await connect()
  await waitFor(() => expect(keys).toHaveLength(2))
  expect(keys[1]).toBe(keys[0])
  // The 409 is the application's own answer and proves the key is spent, so the next
  // click starts a fresh sign-in instead of replaying a dead one forever.
  await connect()
  await waitFor(() => expect(keys).toHaveLength(3))
  expect(keys[2]).not.toBe(keys[0])
  expect(await screen.findByText("Waiting for sign-in…")).toBeVisible()
})

it("keeps the connect key when a gateway answers for the backend", async () => {
  // 502/504 are the reverse proxy's, not the application's: they are no evidence the
  // attempt died, so replaying the same key resumes it rather than stranding it.
  for (const status of [502, 504]) {
    const keys: string[] = []
    let first = true
    mockNativeSession(undefined, {
      "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
      [CONNECT]: (init) => {
        keys.push(
          (JSON.parse(init.body as string) as { requestKey: string }).requestKey
        )
        if (first) {
          first = false
          return json({ error: "gateway" }, status)
        }
        return json(
          {
            outcome: "pending",
            attemptId: ATTEMPT_ID,
            expiresAt: FAR_FUTURE,
            connection: null,
          },
          202
        )
      },
      [ATTEMPT]: () => json({ state: "pending", expiresAt: FAR_FUTURE }),
    })
    const user = userEvent.setup()
    mount()
    await user.click(
      await screen.findByRole("button", { name: "Sign in with Claude" })
    )
    expect(await screen.findByRole("alert")).toBeVisible()
    await user.click(
      screen.getByRole("button", { name: "Sign in with Claude" })
    )
    await waitFor(() => expect(keys).toHaveLength(2))
    expect(keys[1]).toBe(keys[0])
    cleanup()
    localStorage.clear()
  }
})

it("offers reconnect and disconnect for a connection that needs attention", async () => {
  let body: Record<string, unknown> | null = null
  let state = "reconnect_required"
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          connection(state, {
            accountLabel: "sagar@example.test",
            checkedAt: 1_700_000_000,
            errorCode: "unauthorized",
          }),
        ],
      }),
    [DISCONNECT]: (init) => {
      body = JSON.parse(init.body as string)
      // Teardown is queued; the server moves the row on from `disconnecting`.
      state = "disconnecting"
      return receipt("disconnecting", "4")
    },
  })
  const user = userEvent.setup()
  mount()
  expect(await screen.findByText("Reconnect needed")).toBeVisible()
  expect(screen.getByRole("button", { name: "Reconnect claude" })).toBeEnabled()
  await user.click(screen.getByRole("button", { name: "Disconnect claude" }))
  await waitFor(() => expect(body).toEqual({ requestKey: expect.any(String) }))
  expect(await screen.findByText("Disconnecting…")).toBeVisible()
  // Nothing is offered while the server is mid-teardown.
  expect(screen.queryByRole("button", { name: "Disconnect claude" })).toBeNull()
})

it("offers no action at all when no runtime is bound", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("disabled", false)),
  })
  mount()
  expect(await screen.findByText("Agents are turned off.")).toBeVisible()
  for (const name of ["Connect", "Reconnect", "Disconnect"])
    expect(screen.queryByRole("button", { name: `${name} claude` })).toBeNull()
})

it("checks a connection and shows the answer the server gave", async () => {
  const TEST = "POST /api/neev/agent/connections/claude/test"
  const bodies: Record<string, unknown>[] = []
  let state = "connected"
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring(state)),
    [TEST]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      // The check found the credential no longer usable.
      state = "reconnect_required"
      return json({
        connection: {
          provider: "claude",
          state: "reconnect_required",
          accountLabel: "sagar@example.test",
          checkedAt: 1_700_000_000,
          errorCode: "reauth_required",
        },
      })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Test the claude connection" })
  )
  expect(bodies[0]).toEqual({ requestKey: expect.any(String) })
  // The server's own row replaces the previous state — nothing is inferred.
  expect(await screen.findByText("Reconnect needed")).toBeVisible()
  expect(screen.queryByText("Connected")).toBeNull()
})

it("never turns a failed check into a connected-looking row", async () => {
  const TEST = "POST /api/neev/agent/connections/claude/test"
  for (const status of [409, 429, 503]) {
    localStorage.clear()
    mockNativeSession(undefined, {
      "GET /api/neev/agent/connections": () =>
        json(wiring("reconnect_required")),
      [TEST]: () => json({ code: "unavailable" }, status),
    })
    const user = userEvent.setup()
    const view = mount()
    await user.click(
      await screen.findByRole("button", { name: "Test the claude connection" })
    )
    // Busy, rate-limited and unavailable all surface as themselves; the row
    // keeps the state the server last published.
    expect(await screen.findByRole("alert")).toBeVisible()
    expect(screen.getByText("Reconnect needed")).toBeVisible()
    expect(screen.queryByText("Connected")).toBeNull()
    view.unmount()
    vi.unstubAllGlobals()
  }
})

it("offers no check for a connection that does not exist yet", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
  })
  mount()
  await screen.findByRole("button", { name: "Sign in with Claude" })
  // There is nothing to check before a sign-in.
  expect(
    screen.queryByRole("button", { name: "Test the claude connection" })
  ).toBeNull()
})

it("surfaces a sign-in already in flight rather than queueing another", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    // 409: the server already has one in flight for this user.
    [CONNECT]: () => json({ code: "conflict" }, 409),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(screen.queryByText("Waiting for sign-in…")).toBeNull()
})

it("picks a sign-in back up after a reload so it stays cancellable", async () => {
  // A sign-in outlives the page that started it. Without this the user is
  // stranded on a "connecting" row with no way to abandon it.
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      provider: "claude",
      attemptId: ATTEMPT_ID,
      expiresAt: FAR_FUTURE,
      cancelKey: "cancel-key-1",
      connectKey: "connect-key-1",
    })
  )
  let canceled: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("connecting")),
    [ATTEMPT]: () => json({ state: "pending", expiresAt: FAR_FUTURE }),
    [CANCEL]: (init) => {
      canceled = JSON.parse(init.body as string)
      return receipt("not_signed_in")
    },
  })
  const user = userEvent.setup()
  mount()
  expect(await screen.findByText("Waiting for sign-in…")).toBeVisible()
  await user.click(
    await screen.findByRole("button", { name: "Cancel claude sign-in" })
  )
  // The resumed attempt cancels with the key it was started under.
  await waitFor(() =>
    expect(canceled).toEqual({
      attemptId: ATTEMPT_ID,
      requestKey: "cancel-key-1",
    })
  )
  expect(localStorage.getItem(STORAGE_KEY)).toBeNull()
})

it("discards a stored attempt that has already lapsed", async () => {
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      provider: "claude",
      attemptId: ATTEMPT_ID,
      expiresAt: Math.floor(Date.now() / 1000) - 1,
      cancelKey: "cancel-key-1",
      connectKey: "connect-key-1",
    })
  )
  let polls = 0
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [ATTEMPT]: () => {
      polls += 1
      return json({ state: "pending", expiresAt: FAR_FUTURE })
    },
  })
  mount()
  // Nothing to watch: it offers a fresh Connect and never polls the dead id.
  expect(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  ).toBeEnabled()
  expect(polls).toBe(0)
  expect(localStorage.getItem(STORAGE_KEY)).toBeNull()
})

it("stops polling once the attempt's own deadline passes", async () => {
  let polls = 0
  const soon = Math.floor(Date.now() / 1000) + 1
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () =>
      json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: soon,
          connection: null,
        },
        202
      ),
    // A server that keeps saying "pending" past the deadline must not be
    // polled forever.
    [ATTEMPT]: () => {
      polls += 1
      return json({ state: "pending", expiresAt: soon })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  expect(
    await screen.findByText("Sign-in expired before it completed.")
  ).toBeVisible()
  const settled = polls
  await new Promise((resolve) => setTimeout(resolve, 40))
  expect(polls).toBe(settled)
})

it("skips polling while the tab is hidden and resumes when it returns", async () => {
  let polls = 0
  const hidden = vi
    .spyOn(document, "visibilityState", "get")
    .mockReturnValue("hidden")
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () =>
      json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      ),
    [ATTEMPT]: () => {
      polls += 1
      return json({ state: "completed", expiresAt: FAR_FUTURE })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  await screen.findByText("Waiting for sign-in…")
  await new Promise((resolve) => setTimeout(resolve, 40))
  // Nobody is watching a hidden tab; the loop stays alive but does no work.
  expect(polls).toBe(0)
  hidden.mockReturnValue("visible")
  await waitFor(() => expect(polls).toBeGreaterThan(0))
})

it("rides out a transient poll failure and then stops without a verdict", async () => {
  let polls = 0
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () =>
      json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      ),
    [ATTEMPT]: () => {
      polls += 1
      throw new TypeError("network down")
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  // One blip must not abandon a live sign-in, but a dead endpoint must not be
  // hammered forever either.
  expect(await screen.findByRole("alert")).toBeVisible()
  const settled = polls
  expect(settled).toBeGreaterThan(1)
  await new Promise((resolve) => setTimeout(resolve, 60))
  expect(polls).toBe(settled)
  // Giving up on the poll is not a verdict on the sign-in. The server never
  // answered, so the wait stands and the user keeps the way out.
  expect(screen.queryByText("Sign-in failed.")).toBeNull()
  expect(
    screen.getByRole("button", { name: "Cancel claude sign-in" })
  ).toBeEnabled()
})

it("names the provider on every action so two rows are distinguishable", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          connection("not_signed_in"),
          { ...connection("connected"), provider: "codex" },
        ],
      }),
  })
  mount()
  // Identical verbs on two rows would otherwise be ambiguous to a screen
  // reader and to keyboard navigation.
  expect(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  ).toBeEnabled()
  expect(screen.getByRole("button", { name: "Disconnect codex" })).toBeEnabled()
  expect(
    screen.queryByRole("button", { name: "Sign in with ChatGPT" })
  ).toBeNull()
})

const DEVICE_APPROVAL = {
  kind: "device",
  verificationUrl: "https://auth.openai.com/codex/device",
  userCode: "ABCD-EFGH",
  needsCode: false,
}
// The literal the server sends (`approval_dto`, agent_sessions.rs). This
// fixture used to carry the snake_case spelling the client expected, which is
// why the suite stayed green while live sign-in showed no link to open.
const CODE_APPROVAL = {
  kind: "authorizationCode",
  verificationUrl: "https://claude.com/cai/oauth/authorize?x=1",
  userCode: null,
  needsCode: true,
}
const SUBMIT = `POST /api/neev/agent/connections/claude/attempts/${ATTEMPT_ID}/code`
// A device flow is Codex's; Claude's is the pasted authorization code. The
// verification host is allowlisted per provider, so the two cannot be mixed.
const CODEX_CONNECT = "POST /api/neev/agent/connections/codex/connect"
const CODEX_ATTEMPT = `GET /api/neev/agent/connections/codex/attempts/${ATTEMPT_ID}`

function codexWiring(state: string) {
  return {
    runtimeEnabled: true,
    connections: [{ ...connection(state), provider: "codex" }],
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

it("shows a device code and the page to approve it on", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(codexWiring("not_signed_in")),
    [CODEX_CONNECT]: () => pendingConnect(),
    [CODEX_ATTEMPT]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: DEVICE_APPROVAL,
      }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with ChatGPT" })
  )
  expect(await screen.findByText("ABCD-EFGH")).toBeVisible()
  const link = screen.getByRole("link", { name: "Open the sign-in page" })
  expect(link).toHaveAttribute("href", "https://auth.openai.com/codex/device")
  // A device flow needs nothing back from the user.
  expect(screen.queryByRole("textbox")).toBeNull()
})

it("takes a pasted authorization code and forwards it without keeping it", async () => {
  const submitted: Record<string, unknown>[] = []
  let approved = false
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json(wiring(approved ? "connected" : "not_signed_in")),
    [CONNECT]: () => pendingConnect(),
    [ATTEMPT]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: CODE_APPROVAL,
      }),
    [SUBMIT]: (init) => {
      submitted.push(JSON.parse(init.body as string))
      approved = true
      return json(
        { state: "completed", expiresAt: FAR_FUTURE, approval: null },
        202
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  const field = await screen.findByRole("textbox", {
    name: "Paste the authorization code",
  })
  await user.type(field, "  ac_abc123  ")
  await user.click(screen.getByRole("button", { name: "Submit code" }))
  await waitFor(() => expect(submitted).toEqual([{ code: "ac_abc123" }]))
  // The code is forwarded, never kept — and the settled attempt stops the wait.
  expect(await screen.findByText("Connected")).toBeVisible()
  expect(localStorage.getItem(STORAGE_KEY)).toBeNull()
})

it("surfaces a rejected code submission and keeps the prompt usable", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () => pendingConnect(),
    [ATTEMPT]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: CODE_APPROVAL,
      }),
    // 429: the per-attempt submit cap.
    [SUBMIT]: () => json({ code: "limited" }, 429),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  await user.type(
    await screen.findByRole("textbox", {
      name: "Paste the authorization code",
    }),
    "ac_bad"
  )
  await user.click(screen.getByRole("button", { name: "Submit code" }))
  expect(await screen.findByRole("alert")).toBeVisible()
  // Still waiting, still cancellable — a rejected paste is not a dead end.
  expect(screen.getByText("Waiting for sign-in…")).toBeVisible()
  expect(
    screen.getByRole("button", { name: "Cancel claude sign-in" })
  ).toBeEnabled()
})

it("shows no approval prompt until the server has produced one", async () => {
  let approval: unknown = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(codexWiring("not_signed_in")),
    [CODEX_CONNECT]: () => pendingConnect(),
    [CODEX_ATTEMPT]: () =>
      json({ state: "pending", expiresAt: FAR_FUTURE, approval }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with ChatGPT" })
  )
  await screen.findByText("Waiting for sign-in…")
  // Nothing invented while the supervisor has not spoken.
  expect(
    screen.queryByRole("link", { name: "Open the sign-in page" })
  ).toBeNull()
  approval = DEVICE_APPROVAL
  expect(
    await screen.findByRole("link", { name: "Open the sign-in page" })
  ).toBeVisible()
})

it("keeps a sign-in cancellable when polling gives up", async () => {
  // Observed live: the poll failed repeatedly, the client called the sign-in
  // "failed", cleared the attempt AND its stored copy — leaving a "connecting"
  // row with no Cancel and nothing for a reload to recover, while the server
  // still held a live attempt. Only the server may declare a sign-in over.
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      provider: "claude",
      attemptId: ATTEMPT_ID,
      expiresAt: FAR_FUTURE,
      cancelKey: "cancel-key-1",
      connectKey: "connect-key-1",
    })
  )
  let polls = 0
  let canceled: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("connecting")),
    [ATTEMPT]: () => {
      polls += 1
      return json({ error: "unavailable" }, 503)
    },
    [CANCEL]: (init) => {
      canceled = JSON.parse(init.body as string)
      return receipt("not_signed_in")
    },
  })
  const user = userEvent.setup()
  mount()

  // It stops hammering the endpoint...
  await waitFor(() => expect(polls).toBeGreaterThan(3))
  const settled = polls
  await new Promise((resolve) => setTimeout(resolve, 80))
  expect(polls).toBe(settled)

  // ...without inventing an outcome the server never gave.
  expect(screen.queryByText("Sign-in failed.")).toBeNull()
  expect(await screen.findByRole("alert")).toBeVisible()

  // The wait, its stored copy and the way out all survive.
  expect(screen.getByText("Waiting for sign-in\u2026")).toBeVisible()
  expect(localStorage.getItem(STORAGE_KEY)).not.toBeNull()
  await user.click(
    screen.getByRole("button", { name: "Cancel claude sign-in" })
  )
  await waitFor(() =>
    expect(canceled).toEqual({
      attemptId: ATTEMPT_ID,
      requestKey: "cancel-key-1",
    })
  )
})

it("resumes a stalled sign-in when the user asks for status", async () => {
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      provider: "claude",
      attemptId: ATTEMPT_ID,
      expiresAt: FAR_FUTURE,
      cancelKey: "cancel-key-1",
      connectKey: "connect-key-1",
    })
  )
  let failing = true
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("connecting")),
    [ATTEMPT]: () =>
      failing
        ? json({ error: "unavailable" }, 503)
        : json({
            state: "pending",
            expiresAt: FAR_FUTURE,
            approval: CODE_APPROVAL,
          }),
  })
  const user = userEvent.setup()
  mount()
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(
    screen.queryByRole("link", { name: "Open the sign-in page" })
  ).toBeNull()

  failing = false
  await user.click(screen.getByRole("button", { name: "Refresh status" }))

  // Polling picks the same attempt back up and the approval appears.
  expect(
    await screen.findByRole("link", { name: "Open the sign-in page" })
  ).toBeVisible()
})

it("offers a way out of a sign-in this browser never started", async () => {
  // Started in another browser, or stranded by a page that lost its attempt:
  // with no attempt id there is nothing to cancel, so the row used to have no
  // actions at all. `intents::disconnect` accepts `connecting` and cancels the
  // pending attempt, so that is the recovery.
  let disconnected: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("connecting")),
    [DISCONNECT]: (init) => {
      disconnected = JSON.parse(init.body as string)
      return receipt("not_signed_in")
    },
  })
  const user = userEvent.setup()
  mount()

  const disconnect = await screen.findByRole("button", {
    name: "Disconnect claude",
  })
  // Nothing else: there is no connection yet to test, and a second sign-in
  // over a live one is not a recovery.
  expect(
    screen.queryByRole("button", { name: "Sign in with Claude" })
  ).toBeNull()
  expect(screen.queryByRole("button", { name: "Reconnect claude" })).toBeNull()
  expect(
    screen.queryByRole("button", { name: "Test claude connection" })
  ).toBeNull()

  await user.click(disconnect)
  await waitFor(() => expect(disconnected).not.toBeNull())
})

it("will not start a second provider's sign-in over a live one", async () => {
  // The panel holds one attempt and the supervisor's login slot is shared, so
  // connecting Codex here would overwrite the Claude attempt — and its stored
  // copy — while the user is still authorizing it, leaving nothing to cancel
  // it by.
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      provider: "claude",
      attemptId: ATTEMPT_ID,
      expiresAt: FAR_FUTURE,
      cancelKey: "cancel-key-1",
      connectKey: "connect-key-1",
    })
  )
  let codexConnects = 0
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          connection("connecting"),
          { ...connection("not_signed_in"), provider: "codex" },
        ],
      }),
    [ATTEMPT]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: CODE_APPROVAL,
      }),
    [CODEX_CONNECT]: () => {
      codexConnects += 1
      return pendingConnect()
    },
  })
  const user = userEvent.setup()
  mount()

  expect(await screen.findByText("Waiting for sign-in\u2026")).toBeVisible()
  const codex = screen.getByRole("button", { name: "Sign in with ChatGPT" })
  expect(codex).toBeDisabled()
  // Even if the click lands anyway, nothing is sent and the attempt stands.
  await user.click(codex)
  expect(codexConnects).toBe(0)
  expect(JSON.parse(localStorage.getItem(STORAGE_KEY) as string)).toMatchObject(
    { provider: "claude", attemptId: ATTEMPT_ID }
  )

  // The live sign-in stays fully usable.
  expect(
    screen.getByRole("button", { name: "Cancel claude sign-in" })
  ).toBeEnabled()
  expect(screen.getByRole("button", { name: "Refresh status" })).toBeEnabled()
  expect(
    screen.getByRole("link", { name: "Open the sign-in page" })
  ).toBeVisible()
})

it("stops asking for a code once the server has taken one", async () => {
  // The submit answers with the whole attempt. If it comes back still pending
  // but with no challenge, the server has the code and is working on it —
  // leaving the paste prompt up would invite a second code nothing awaits.
  let approval: unknown = CODE_APPROVAL
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () => pendingConnect(),
    [ATTEMPT]: () =>
      json({ state: "pending", expiresAt: FAR_FUTURE, approval }),
    [SUBMIT]: () => {
      approval = null
      return json(
        { state: "pending", expiresAt: FAR_FUTURE, approval: null },
        202
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with Claude" })
  )
  await user.type(
    await screen.findByRole("textbox", {
      name: "Paste the authorization code",
    }),
    "ac_abc123"
  )
  await user.click(screen.getByRole("button", { name: "Submit code" }))

  // No error: a 202 carrying the attempt is exactly what success looks like.
  expect(screen.queryByRole("alert")).toBeNull()
  // The prompt goes, the wait is reported as still running, and it stays
  // cancellable.
  await waitFor(() =>
    expect(
      screen.queryByRole("textbox", { name: "Paste the authorization code" })
    ).toBeNull()
  )
  expect(await screen.findByText("Sign-in in progress.")).toBeVisible()
  expect(
    screen.getByRole("button", { name: "Cancel claude sign-in" })
  ).toBeEnabled()
})

it("tears down before signing in again so reconnect cannot 409", async () => {
  // A `reconnect_required` row still holds the old credential and the server
  // refuses a second sign-in over it. Connecting straight away returned 409 and
  // left the user to press Disconnect and Connect by hand.
  const order: string[] = []
  let state = "reconnect_required"
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => {
      order.push(`list:${state}`)
      const shown = state
      // The sweep finishes between polls, as it does on the server.
      if (state === "disconnecting") state = "not_signed_in"
      return json(wiring(shown))
    },
    [DISCONNECT]: () => {
      order.push("disconnect")
      // Queued, not done: the server answers `disconnecting` and sweeps later.
      state = "disconnecting"
      return receipt("disconnecting", "4")
    },
    [CONNECT]: () => {
      order.push(`connect:${state}`)
      state = "connecting"
      return pendingConnect()
    },
    [ATTEMPT]: () => json({ state: "pending", expiresAt: FAR_FUTURE }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Reconnect claude" })
  )

  // The sign-in only starts once the server says the credential is gone.
  await waitFor(() => expect(order).toContain("connect:not_signed_in"))
  expect(order.indexOf("disconnect")).toBeLessThan(
    order.indexOf("connect:not_signed_in")
  )
  expect(order.filter((step) => step.startsWith("connect:"))).toEqual([
    "connect:not_signed_in",
  ])
  expect(await screen.findByText("Waiting for sign-in…")).toBeVisible()
})

it("does not sign in again while its own teardown is unfinished", async () => {
  // Still `disconnecting` when the wait runs out: starting anyway would earn
  // the 409 this sequence exists to avoid. The row keeps the honest state.
  let connects = 0
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("disconnecting")),
    [DISCONNECT]: () => receipt("disconnecting", "4"),
    [CONNECT]: () => {
      connects += 1
      return pendingConnect()
    },
  })
  const user = userEvent.setup()
  mount()
  // The row is `disconnecting`, so the only action is the orphan cleanup.
  expect(screen.queryByRole("button", { name: "Reconnect claude" })).toBeNull()
  await waitFor(() => expect(connects).toBe(0))
  expect(screen.queryByText("Waiting for sign-in…")).toBeNull()
  void user
})

/**
 * The prerequisite Codex does not mention until after a sign-in has already
 * failed: device code authorization has to be on for the ChatGPT account.
 *
 * Said before the button that starts it, and said without claiming to know
 * whether it is on — Neev can neither read nor change that setting.
 */
it("says what Codex needs before connecting, and says it only for Codex", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(codexWiring("not_signed_in")),
  })
  mount()
  expect(
    await screen.findByText(
      "Codex needs device code authorization turned on in ChatGPT, under Settings → Security."
    )
  ).toBeInTheDocument()
  // The switch itself, and the provider's own instructions behind it.
  expect(
    screen.getByRole("link", { name: "Open ChatGPT settings" })
  ).toHaveAttribute("href", "https://chatgpt.com/#settings/Security")
  expect(
    screen.getByRole("link", { name: "How signing in on another device works" })
  ).toHaveAttribute(
    "href",
    "https://learn.chatgpt.com/docs/auth#login-on-headless-devices"
  )
  // It never asserts the setting is on or off.
  expect(screen.queryByText(/already enabled|not enabled/i)).toBeNull()
})

it("does not show the Codex prerequisite on another provider, or once connected", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [connection("not_signed_in")],
      }),
  })
  mount()
  await screen.findByRole("button", { name: "Sign in with Claude" })
  expect(
    screen.queryByText(
      "Codex needs device code authorization turned on in ChatGPT, under Settings → Security."
    )
  ).toBeNull()

  cleanup()
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(codexWiring("connected")),
  })
  mount()
  await screen.findByRole("button", { name: "Disconnect codex" })
  expect(
    screen.queryByText(
      "Codex needs device code authorization turned on in ChatGPT, under Settings → Security."
    )
  ).toBeNull()
})

/** Drives the device flow to the point where a code is on screen. */
async function showDeviceCode(user: ReturnType<typeof userEvent.setup>) {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(codexWiring("not_signed_in")),
    [CODEX_CONNECT]: () => pendingConnect(),
    [CODEX_ATTEMPT]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: DEVICE_APPROVAL,
      }),
  })
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Sign in with ChatGPT" })
  )
  await screen.findByText("ABCD-EFGH")
}

it("copies the device code on an explicit click, and asks the server for nothing to do it", async () => {
  const user = userEvent.setup()
  const writeText = vi.fn().mockResolvedValue(undefined)
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } })
  await showDeviceCode(user)

  await user.click(screen.getByRole("button", { name: "Copy code" }))
  expect(writeText).toHaveBeenCalledWith("ABCD-EFGH")
  expect(await screen.findByRole("button", { name: "Copied" })).toBeVisible()
  // Copying is a browser act. Nothing about the attempt or the connection moved.
  expect(screen.queryByRole("alert")).toBeNull()
})

/**
 * `navigator.clipboard` is undefined in a non-secure context, so reading
 * `.writeText` off it throws before any promise exists to reject. A button
 * that silently looked like it had worked left the reader pasting nothing.
 */
it("says so visibly when the clipboard is unavailable", async () => {
  const user = userEvent.setup()
  vi.stubGlobal("navigator", { ...navigator, clipboard: undefined })
  await showDeviceCode(user)

  await user.click(screen.getByRole("button", { name: "Copy code" }))
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Could not copy it. Select the code and copy it by hand."
  )
  // Still offering to try, and never claiming it copied.
  expect(screen.getByRole("button", { name: "Copy code" })).toBeVisible()
  expect(screen.queryByRole("button", { name: "Copied" })).toBeNull()
})
