import { render, screen, waitFor } from "@testing-library/react"
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
    await screen.findByRole("button", { name: "Connect claude" })
  )
  // A pending attempt is "waiting", never "Connected", and can be cancelled.
  expect(await screen.findByText("Waiting for sign-in…")).toBeVisible()
  expect(
    screen.getByRole("button", { name: "Cancel claude sign-in" })
  ).toBeEnabled()
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
    await screen.findByRole("button", { name: "Connect claude" })
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
      return json({ operationId: OPERATION_ID, generation: "3" })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Connect claude" })
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
    await screen.findByRole("button", { name: "Connect claude" })
  )
  expect(await screen.findByRole("alert")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Connect claude" }))
  await waitFor(() => expect(keys).toHaveLength(2))
  // The same key means the server returns the SAME attempt.
  expect(keys[0]).toBe(keys[1])
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
      return json({ operationId: OPERATION_ID, generation: "4" })
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

it("does not offer a connection test the server could never settle", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("connected")),
  })
  mount()
  expect(await screen.findByText("Connected")).toBeVisible()
  expect(
    screen.getByText("Testing a connection is not available yet.")
  ).toBeVisible()
  expect(screen.queryByRole("button", { name: /test/i })).toBeNull()
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
    await screen.findByRole("button", { name: "Connect claude" })
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
      return json({ operationId: OPERATION_ID, generation: "3" })
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
    await screen.findByRole("button", { name: "Connect claude" })
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
    await screen.findByRole("button", { name: "Connect claude" })
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
    await screen.findByRole("button", { name: "Connect claude" })
  )
  await screen.findByText("Waiting for sign-in…")
  await new Promise((resolve) => setTimeout(resolve, 40))
  // Nobody is watching a hidden tab; the loop stays alive but does no work.
  expect(polls).toBe(0)
  hidden.mockReturnValue("visible")
  await waitFor(() => expect(polls).toBeGreaterThan(0))
})

it("rides out a transient poll failure and then reports the wait as failed", async () => {
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
    await screen.findByRole("button", { name: "Connect claude" })
  )
  // One blip must not abandon a live sign-in, but a dead endpoint must not be
  // hammered forever either.
  expect(await screen.findByText("Sign-in failed.")).toBeVisible()
  const settled = polls
  expect(settled).toBeGreaterThan(1)
  await new Promise((resolve) => setTimeout(resolve, 60))
  expect(polls).toBe(settled)
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
    await screen.findByRole("button", { name: "Connect claude" })
  ).toBeEnabled()
  expect(screen.getByRole("button", { name: "Disconnect codex" })).toBeEnabled()
  expect(screen.queryByRole("button", { name: "Connect codex" })).toBeNull()
})

const DEVICE_APPROVAL = {
  kind: "device",
  verificationUrl: "https://auth.openai.com/codex/device",
  userCode: "ABCD-EFGH",
  needsCode: false,
}
const CODE_APPROVAL = {
  kind: "authorization_code",
  verificationUrl: "https://claude.ai/oauth/authorize?x=1",
  userCode: null,
  needsCode: true,
}
const SUBMIT = `POST /api/neev/agent/connections/claude/attempts/${ATTEMPT_ID}/code`

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
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () => pendingConnect(),
    [ATTEMPT]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: DEVICE_APPROVAL,
      }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Connect claude" })
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
      return json({ state: "completed" }, 202)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Connect claude" })
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
    await screen.findByRole("button", { name: "Connect claude" })
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
    "GET /api/neev/agent/connections": () => json(wiring("not_signed_in")),
    [CONNECT]: () => pendingConnect(),
    [ATTEMPT]: () =>
      json({ state: "pending", expiresAt: FAR_FUTURE, approval }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Connect claude" })
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
