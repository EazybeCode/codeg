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
})

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
          expiresAt: 1_700_000_600,
          connection: null,
        },
        202
      )
    },
    [ATTEMPT]: () => {
      polls += 1
      // Settles only once the server says so; the client never decides.
      return polls < 3
        ? json({ state: "pending", expiresAt: 1_700_000_600 })
        : (() => {
            state = "connected"
            return json({ state: "completed", expiresAt: 1_700_000_600 })
          })()
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: "Connect" }))
  // A pending attempt is "waiting", never "Connected", and can be cancelled.
  expect(await screen.findByText("Waiting for sign-in…")).toBeVisible()
  expect(screen.getByRole("button", { name: "Cancel sign-in" })).toBeEnabled()
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
          expiresAt: 1_700_000_600,
          connection: null,
        },
        202
      ),
    // With no supervisor bound this is what actually happens today.
    [ATTEMPT]: () => json({ state: "expired", expiresAt: 1_700_000_600 }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: "Connect" }))
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
          expiresAt: 1_700_000_600,
          connection: null,
        },
        202
      )
    },
    [ATTEMPT]: () => json({ state: "pending", expiresAt: 1_700_000_600 }),
    [CANCEL]: (init) => {
      canceled = JSON.parse(init.body as string)
      state = "not_signed_in"
      return json({ operationId: OPERATION_ID, generation: "3" })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: "Connect" }))
  await user.click(
    await screen.findByRole("button", { name: "Cancel sign-in" })
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
          expiresAt: 1_700_000_600,
          connection: null,
        },
        202
      )
    },
    [ATTEMPT]: () => json({ state: "pending", expiresAt: 1_700_000_600 }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: "Connect" }))
  expect(await screen.findByRole("alert")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Connect" }))
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
  expect(screen.getByRole("button", { name: "Reconnect" })).toBeEnabled()
  await user.click(screen.getByRole("button", { name: "Disconnect" }))
  await waitFor(() => expect(body).toEqual({ requestKey: expect.any(String) }))
  expect(await screen.findByText("Disconnecting…")).toBeVisible()
  // Nothing is offered while the server is mid-teardown.
  expect(screen.queryByRole("button", { name: "Disconnect" })).toBeNull()
})

it("offers no action at all when no runtime is bound", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => json(wiring("disabled", false)),
  })
  mount()
  expect(await screen.findByText("Agents are turned off.")).toBeVisible()
  for (const name of ["Connect", "Reconnect", "Disconnect"])
    expect(screen.queryByRole("button", { name })).toBeNull()
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
  await user.click(await screen.findByRole("button", { name: "Connect" }))
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(screen.queryByText("Waiting for sign-in…")).toBeNull()
})
