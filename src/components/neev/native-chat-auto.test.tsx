/*
 * Auto routing in a resource chat's "Ask agent" mode.
 *
 * The same switch and the same note Agent Chats uses: the server decides over
 * the caller's own connected agents and falls back to the composer's
 * selection, and this composer only sends `auto`, follows the run that
 * actually started, and says what it ran on.
 */
import { act, render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import { NativeChat } from "./native-chat"
import { NativeBoundary } from "./native-boundary"
import {
  composerText,
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  typeInComposer,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const resource: NativeResource = {
  id: "11111111-1111-4111-8111-111111111111",
  orgId: 3,
  nodeId: 4,
  kind: "chat",
  creatorId: 1,
  title: "Design chat",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}
const path = `/api/neev/chats/${resource.id}/messages`
const RUN_ID = "0123456789abcdef0123456789abcdef"
const RETRY_RUN_ID = "fedcba9876543210fedcba9876543210"
const SESSION_ID = "2b4cd22b-bb13-2882-c4cb-0773b9534435"
const AGENT_PATH = `POST /api/neev/chats/${resource.id}/agent`
const RUN_EVENTS = `GET /api/neev/agent/runs/${RUN_ID}/events`
const TEAM_SESSION = (provider: string) =>
  `GET /api/neev/chats/${resource.id}/agent?provider=${provider}`
const TURNS = `GET /api/neev/agent/sessions/${SESSION_ID}/turns`
const askMessage = {
  id: "44444444-4444-4444-8444-444444444444",
  sequence: "1",
  authorId: 1,
  authorName: "Sagar",
  body: "help",
  createdAt: 1_700_000_000,
  authorKind: "human",
  agentLabel: null,
}
const runningTurn = {
  id: "66666666-6666-4666-8666-666666666666",
  sessionId: SESSION_ID,
  sequence: "1",
  runId: RUN_ID,
  prompt: "help",
  response: "",
  activity: [],
  status: "running",
  errorCode: null,
  provider: "claude",
  model: "default",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
  completedAt: null,
}
const holder = (provider: string, model: string, effort: string | null) => ({
  id: SESSION_ID,
  orgId: 3,
  ownerId: 1,
  title: "Team invocation",
  provider,
  model,
  effort,
  fast: false,
  boundChatId: resource.id,
  archived: false,
  revision: "2",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
})
const READY = {
  state: "ready",
  router: "jev",
  maxTaskChars: 6000,
  maxPreviousChars: 2000,
  supervision: "off",
  usage: [],
}
/** Codex was selected; Auto ran Claude Opus at high. */
const ROUTED_TO_CLAUDE = {
  provider: "claude",
  model: "opus",
  label: "Opus",
  effort: "high",
  fast: false,
  outcome: "routed",
  reason: null,
  router: "jev",
}
const row = (provider: string, state: string) => ({
  provider,
  state,
  accountLabel: state === "connected" ? "sagar@example.test" : null,
  checkedAt: state === "connected" ? 1_700_000_000 : null,
  errorCode: null,
})
const bothConnected = {
  runtimeEnabled: true,
  connections: [row("claude", "connected"), row("codex", "connected")],
}

const capable = () => json({ ...nativeCapabilities, runtimeEnabled: true })

function sseBody(...frames: string[]) {
  return new Response(frames.map((frame) => `data: ${frame}\n\n`).join(""), {
    status: 200,
    headers: { "Content-Type": "text/event-stream" },
  })
}

function liveStream() {
  const encoder = new TextEncoder()
  let controller!: ReadableStreamDefaultController<Uint8Array>
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c
    },
  })
  return {
    response: new Response(body, {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    }),
    push: (frame: unknown) =>
      controller.enqueue(encoder.encode(`data: ${JSON.stringify(frame)}\n\n`)),
  }
}

beforeEach(() => {
  localStorage.clear()
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

function mount(fresh = true) {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeChat
          resource={resource}
          fresh={fresh}
          onBack={() => {}}
          onChanged={() => {}}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

async function ready() {
  await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() =>
    expect(screen.queryByText("Loading this conversation…")).toBeNull()
  )
}

async function typeAsk(text = "help") {
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    text
  )
}

/** The routes every test here shares: an empty chat with no holder yet. */
function base(invoked: () => boolean) {
  return {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: invoked() ? [askMessage] : [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    "GET /api/neev/agent/connections": () => json(bothConnected),
    [TEAM_SESSION("claude")]: () => json({ code: "not_found" }, 404),
    [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
    [TURNS]: () => json({ items: [], nextAfter: null }),
  }
}

const autoChip = () => screen.queryByRole("button", { name: /^Auto:/ })

it("offers Auto beside the agent controls only when the router is ready", async () => {
  mockNativeSession(undefined, {
    ...base(() => false),
    "GET /api/neev/agent/routing": () => json(READY),
  })
  const user = userEvent.setup()
  mount()
  await ready()
  // On by default once the router is ready — the same default Agent Chats has.
  const chip = await screen.findByRole("button", { name: /^Auto:/ })
  expect(chip).toHaveAttribute("aria-pressed", "true")
  // The device preference Agent Chats reads: switching it here switches it
  // there.
  await user.click(chip)
  expect(chip).toHaveAttribute("aria-pressed", "false")
  expect(localStorage.getItem("neev.agent.auto")).toBe("0")
  // A team message is not an agent turn, so there is nothing to route.
  await user.click(screen.getByRole("button", { name: "Team chat" }))
  expect(autoChip()).toBeNull()
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  expect(await screen.findByRole("button", { name: /^Auto:/ })).toBeVisible()
})

it("offers no Auto control when no router is configured", async () => {
  // The default routing answer from the session mock is "off".
  mockNativeSession(
    undefined,
    base(() => false)
  )
  mount()
  await ready()
  await screen.findByRole("button", { name: "Model" })
  await waitFor(() => expect(autoChip()).toBeNull())
})

it("honours a switch turned off in Agent Chats and sends a manual ask unchanged", async () => {
  localStorage.setItem("neev.agent.auto", "0")
  const bodies: Record<string, unknown>[] = []
  let invoked = false
  mockNativeSession(undefined, {
    ...base(() => invoked),
    "GET /api/neev/agent/routing": () => json(READY),
    [AGENT_PATH]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      invoked = true
      return json(
        {
          message: askMessage,
          turn: { ...runningTurn, status: "failed", errorCode: "x" },
          runId: RUN_ID,
        },
        201
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await ready()
  expect(await screen.findByRole("button", { name: /^Auto:/ })).toHaveAttribute(
    "aria-pressed",
    "false"
  )
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(bodies).toHaveLength(1))
  // Absent, not false: byte-identical to an ask from before Auto existed.
  expect(bodies[0]).not.toHaveProperty("auto")
  expect(bodies[0]).toMatchObject({ provider: "claude", model: "default" })
})

it("with Auto on, lets any connected agent take the ask and follows the one that ran", async () => {
  // Claude was connected when the chat opened and the composer points at it;
  // by the time Send is pressed only Codex is.
  let reads = 0
  const bodies: Record<string, unknown>[] = []
  let invoked = false
  const events: string[] = []
  mockNativeSession(undefined, {
    ...base(() => invoked),
    "GET /api/neev/agent/routing": () => json(READY),
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections:
          reads++ === 0
            ? [row("claude", "connected"), row("codex", "not_signed_in")]
            : [row("claude", "not_signed_in"), row("codex", "connected")],
      }),
    [TEAM_SESSION("codex")]: () =>
      invoked
        ? json(holder("codex", "default", "low"))
        : json({ code: "not_found" }, 404),
    [AGENT_PATH]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      invoked = true
      return json(
        {
          message: askMessage,
          turn: { ...runningTurn, provider: "codex" },
          runId: RUN_ID,
          route: {
            provider: "codex",
            model: "default",
            label: "Default",
            effort: "low",
            fast: false,
            outcome: "routed",
            reason: null,
            router: "jev",
          },
        },
        201
      )
    },
    [RUN_EVENTS]: () => {
      events.push(RUN_ID)
      return sseBody(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${runningTurn.id}"}`,
        '{"type":"chunk","text":"codex says hi"}',
        '{"type":"finished","status":"completed","errorCode":null}'
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await ready()
  await screen.findByRole("button", { name: /^Auto:/ })
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(bodies).toHaveLength(1))
  // The selection travels as the fallback, with Auto asked for.
  expect(bodies[0]).toMatchObject({
    provider: "claude",
    model: "default",
    auto: true,
  })
  // No connect dialog: the gate needed ANY connected agent.
  expect(screen.queryByRole("dialog")).toBeNull()
  await waitFor(() => expect(events).toEqual([RUN_ID]))
  // The controls now say what ran, read back from that provider's holder.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Codex · Default"
    )
  )
  expect(await screen.findByText("Auto: Default · low")).toBeVisible()
})

it("with Auto off, keeps today's gate on the selected provider", async () => {
  localStorage.setItem("neev.agent.auto", "0")
  let reads = 0
  const invoke = vi.fn(() => json({ code: "unavailable" }, 503))
  mockNativeSession(undefined, {
    ...base(() => false),
    "GET /api/neev/agent/routing": () => json(READY),
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections:
          reads++ === 0
            ? [row("claude", "connected"), row("codex", "not_signed_in")]
            : [row("claude", "not_signed_in"), row("codex", "connected")],
      }),
    [AGENT_PATH]: invoke,
  })
  const user = userEvent.setup()
  mount()
  await ready()
  await screen.findByRole("button", { name: /^Auto:/ })
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(await screen.findByRole("dialog")).toBeVisible()
  expect(invoke).not.toHaveBeenCalled()
  expect(
    composerText(screen.getByRole("textbox", { name: "Message", hidden: true }))
  ).toBe("help")
})

it("labels, streams, steers and stops the routed run, and shows what it ran on", async () => {
  const stream = liveStream()
  let invoked = false
  let canceled: unknown = null
  const steers: string[] = []
  const bodies: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    ...base(() => invoked),
    "GET /api/neev/agent/routing": () => json(READY),
    [TEAM_SESSION("claude")]: () =>
      invoked
        ? json(holder("claude", "opus", "high"))
        : json({ code: "not_found" }, 404),
    [AGENT_PATH]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      invoked = true
      return json(
        {
          message: askMessage,
          turn: { ...runningTurn, provider: "claude", model: "opus" },
          runId: RUN_ID,
          route: ROUTED_TO_CLAUDE,
        },
        201
      )
    },
    [RUN_EVENTS]: () => stream.response,
    [`POST /api/neev/agent/runs/${RUN_ID}/steer`]: (init) => {
      const body = JSON.parse(init.body as string)
      steers.push(body.text)
      return json({ accepted: true, runId: RUN_ID, requestId: body.requestId })
    },
    "POST /api/neev/agent/runs/cancel": (init) => {
      canceled = JSON.parse(init.body as string)
      return json({ canceled: true, status: "running" })
    },
  })
  const user = userEvent.setup()
  mount()
  await ready()
  await screen.findByRole("button", { name: /^Auto:/ })
  // The composer points at Codex; Auto will run Claude instead.
  await user.click(screen.getByRole("button", { name: "Model" }))
  await user.click(screen.getByRole("menuitem", { name: "More models" }))
  const all = await screen.findByRole("menu", { name: "More models" })
  await act(async () => {
    within(within(all).getByRole("group", { name: "Codex" }))
      .getByRole("menuitemradio", { name: "Default" })
      .click()
  })
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Codex · Default"
    )
  )
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(bodies).toHaveLength(1))
  expect(bodies[0]).toMatchObject({ provider: "codex", auto: true })
  act(() => {
    stream.push({
      type: "started",
      runId: RUN_ID,
      turnId: runningTurn.id,
      supportsSteering: true,
    })
    stream.push({ type: "chunk", text: "Opus is on it" })
  })
  expect(await screen.findByText("Opus is on it")).toBeVisible()
  // The live reply is labelled by the agent that is actually writing it.
  const preview = screen.getByText("Opus is on it").closest("li")!
  expect(within(preview).getByText("claude")).toBeVisible()
  expect(within(preview).queryByText("codex")).toBeNull()
  // The controls switch to what ran, and the note says so.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Opus"
    )
  )
  expect(screen.getByText("Auto: Opus · high")).toBeVisible()
  // Steering and Stop are keyed on the run, routed or not.
  await typeAsk("Focus on edge cases")
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(steers).toEqual(["Focus on edge cases"]))
  await user.click(
    await screen.findByRole("button", { name: "Stop generation" })
  )
  await waitFor(() => expect(canceled).toEqual({ runId: RUN_ID }))
})

it("says honestly when Auto fell back to the selection", async () => {
  let invoked = false
  mockNativeSession(undefined, {
    ...base(() => invoked),
    "GET /api/neev/agent/routing": () => json(READY),
    [AGENT_PATH]: () => {
      invoked = true
      return json(
        {
          message: askMessage,
          turn: runningTurn,
          runId: RUN_ID,
          route: {
            provider: "claude",
            model: "default",
            label: "Default",
            effort: null,
            fast: false,
            outcome: "fallback",
            reason: "throttled",
            router: "jev",
          },
        },
        201
      )
    },
    [RUN_EVENTS]: () =>
      sseBody(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${runningTurn.id}"}`,
        '{"type":"finished","status":"completed","errorCode":null}'
      ),
  })
  const user = userEvent.setup()
  mount()
  await ready()
  await screen.findByRole("button", { name: /^Auto:/ })
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(
    await screen.findByText(
      "Auto used your selection, Default (too many Auto decisions; try again shortly)"
    )
  ).toBeVisible()
})

it("clears the note on Retry, which re-runs the stored selection", async () => {
  let invoked = false
  let retried = false
  const failed = {
    ...runningTurn,
    model: "opus",
    status: "failed",
    errorCode: "provider",
    completedAt: 1_700_000_003,
    activity: [
      {
        seq: 1,
        offset: 0,
        id: "result",
        kind: "status",
        state: "result",
        detail: "failed",
      },
      {
        seq: 2,
        offset: 0,
        id: "error",
        kind: "status",
        state: "error",
        detail: "stream disconnected",
        category: "network",
      },
    ],
  }
  mockNativeSession(undefined, {
    ...base(() => invoked),
    "GET /api/neev/agent/routing": () => json(READY),
    [TEAM_SESSION("claude")]: () =>
      invoked
        ? json(holder("claude", "opus", "high"))
        : json({ code: "not_found" }, 404),
    [TURNS]: () => json({ items: invoked ? [failed] : [], nextAfter: null }),
    [`${TURNS}?after=1`]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: () => {
      invoked = true
      return json(
        {
          message: askMessage,
          turn: { ...runningTurn, model: "opus" },
          runId: RUN_ID,
          route: ROUTED_TO_CLAUDE,
        },
        201
      )
    },
    [RUN_EVENTS]: () =>
      sseBody(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${runningTurn.id}"}`,
        '{"type":"finished","status":"failed","errorCode":"provider"}'
      ),
    [`POST /api/neev/agent/runs/${RUN_ID}/retry`]: () => {
      retried = true
      return json(
        {
          turn: {
            ...runningTurn,
            model: "opus",
            id: "77777777-7777-4777-8777-777777777777",
            sequence: "2",
            runId: RETRY_RUN_ID,
          },
          runId: RETRY_RUN_ID,
        },
        201
      )
    },
    [`GET /api/neev/agent/runs/${RETRY_RUN_ID}/events`]: () =>
      sseBody(
        `{"type":"started","runId":"${RETRY_RUN_ID}","turnId":"77777777-7777-4777-8777-777777777777"}`,
        '{"type":"finished","status":"completed","errorCode":null}'
      ),
  })
  const user = userEvent.setup()
  mount()
  await ready()
  await screen.findByRole("button", { name: /^Auto:/ })
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(await screen.findByText("Auto: Opus · high")).toBeVisible()
  const card = await screen.findByRole("alert")
  await user.click(within(card).getByRole("button", { name: "Retry" }))
  await waitFor(() => expect(retried).toBe(true))
  // A retry is not a new decision; the note would describe one.
  await waitFor(() =>
    expect(screen.queryByText("Auto: Opus · high")).toBeNull()
  )
})

it("shows the ask once when a lost response is replayed without a route", async () => {
  let invoked = false
  const bodies: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    ...base(() => invoked),
    "GET /api/neev/agent/routing": () => json(READY),
    [AGENT_PATH]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      // The first attempt is committed but its answer never arrives.
      invoked = true
      if (bodies.length === 1) throw new TypeError("lost response")
      // The replay returns the stored ask and turn, and no new decision.
      return json(
        { message: askMessage, turn: runningTurn, runId: RUN_ID },
        201
      )
    },
    [RUN_EVENTS]: () =>
      sseBody(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${runningTurn.id}"}`,
        '{"type":"finished","status":"completed","errorCode":null}'
      ),
  })
  const user = userEvent.setup()
  mount()
  await ready()
  await screen.findByRole("button", { name: /^Auto:/ })
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(bodies).toHaveLength(1))
  await screen.findByRole("alert")
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(bodies).toHaveLength(2))
  // The same ask, so the server replays it rather than asking twice.
  expect(bodies[1].requestId).toBe(bodies[0].requestId)
  expect(bodies[1].auto).toBe(true)
  const thread = within(await screen.findByRole("list", { name: "Messages" }))
  await waitFor(() => expect(thread.getAllByText("help")).toHaveLength(1))
  expect(screen.queryByText(/^Auto: /)).toBeNull()
})
