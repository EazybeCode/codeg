import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import { NativeChat } from "./native-chat"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  orgSession,
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
const message = {
  id: "22222222-2222-4222-8222-222222222222",
  sequence: "1",
  authorId: 1,
  authorName: "Sagar",
  body: "My private thought",
  createdAt: 1_700_000_000,
}
const path = `/api/neev/chats/${resource.id}/messages`
beforeEach(() => {
  // The agent context panel remembers open/closed per user in localStorage;
  // start every test from the default (closed) so panel interactions are stable.
  localStorage.clear()
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})
function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeChat
          resource={resource}
          onBack={() => {}}
          onChanged={() => {}}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}
it("renders message Markdown without activating unsafe links", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({
        items: [{ ...message, body: "**Decision** [bad](javascript:alert)" }],
        nextAfter: null,
      }),
  })
  mount()
  expect((await screen.findByText("Decision")).tagName).toBe("STRONG")
  expect(screen.queryByRole("link", { name: "bad" })).toBeNull()
})
it("brings a successfully sent message into view from older history", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: () => json(message, 201),
  })
  mount()
  const history = await screen.findByRole("list", { name: "Messages" })
  Object.defineProperties(history, {
    scrollHeight: { value: 900 },
    clientHeight: { value: 200 },
  })
  history.scrollTop = 0
  fireEvent.scroll(history)
  const user = userEvent.setup()
  await user.type(
    screen.getByRole("textbox", { name: "Message" }),
    message.body
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  await screen.findByText(message.body)
  expect(history.scrollTop).toBe(900)
})

it("retries a committed message with the same request ID and displays its human author", async () => {
  const bodies: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      if (bodies.length === 1) throw new TypeError("lost response")
      return json(message, 201)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    message.body
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(
    message.body
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(await screen.findByText(message.body)).toBeVisible()
  expect(screen.getByText("Sagar")).toBeVisible()
  expect(document.querySelector("time")).toHaveAttribute(
    "datetime",
    "2023-11-14T22:13:20.000Z"
  )
  expect(bodies[0]).toEqual({
    requestId: expect.any(String),
    body: message.body,
    expectedAuthRevision: "1",
  })
  expect(bodies[1]).toEqual(bodies[0])
  // Agent controls now live in the collapsible right context panel.
  await user.click(screen.getByRole("button", { name: "Agent panel" }))
  expect(screen.getByRole("button", { name: "Ask agent" })).toBeDisabled()
  expect(
    screen.getByRole("link", { name: "Personal AI connections" })
  ).toHaveAttribute("href", "/settings")
  expect(mock.unknown).toEqual([])
})

it("pins a dirty message audience across polling and explicitly accepts the latest audience after conflict", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let latest = resource
  const bodies: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(latest),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return bodies.length === 1
        ? json({ code: "conflict" }, 409)
        : json(message, 201)
    },
  })
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
  mount()
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    message.body
  )
  latest = { ...resource, authRevision: "2", visibility: "organization" }
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  // Visibility and delivery status share one meta line so the composer stays
  // above the fold on a phone.
  expect(screen.getByText(/^Organization ·/)).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(bodies[0].expectedAuthRevision).toBe("1")
  await user.click(
    await screen.findByRole("button", { name: "Review latest audience" })
  )
  expect(screen.getByRole("dialog")).toHaveTextContent("Neev demo")
  await user.click(screen.getByRole("button", { name: "Use latest audience" }))
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(bodies[1].expectedAuthRevision).toBe("2")
  expect(bodies[1].requestId).not.toBe(bodies[0].requestId)
  expect(mock.unknown).toEqual([])
})

it("polls metadata before incremental messages only while visible and stops on unmount", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const order: string[] = []
  const second = {
    ...message,
    id: "33333333-3333-4333-8333-333333333333",
    sequence: "2",
    body: "Another thought",
  }
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => {
      order.push("metadata")
      return json(resource)
    },
    [`GET ${path}`]: () => {
      order.push("first")
      return json({ items: [message], nextAfter: null })
    },
    [`GET ${path}?after=1`]: () => {
      order.push("incremental")
      return json({ items: [second], nextAfter: null })
    },
  })
  const view = mount()
  await screen.findByText(message.body)
  expect(order).toEqual(["metadata", "first"])
  const visibility = vi
    .spyOn(document, "visibilityState", "get")
    .mockReturnValue("hidden")
  await act(async () => {
    await vi.advanceTimersByTimeAsync(15000)
  })
  expect(order).toHaveLength(2)
  visibility.mockReturnValue("visible")
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  expect(screen.getByText(second.body)).toBeVisible()
  expect(order).toEqual(["metadata", "first", "metadata", "incremental"])
  view.unmount()
  await act(async () => {
    await vi.advanceTimersByTimeAsync(15000)
  })
  expect(order).toHaveLength(4)
  visibility.mockRestore()
  expect(mock.unknown).toEqual([])
})

it.each([403, 404, 401])(
  "erases private messages and dirty drafts on %s and stops polling",
  async (status) => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
    let revoked = false,
      calls = 0
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/session": () =>
        revoked && status === 403
          ? json({ code: "unauthorized" }, 401)
          : json(
              revoked && status === 401
                ? {
                    ...orgSession,
                    organization: {
                      id: 9,
                      name: "Other organization",
                      avatarUrl: null,
                    },
                  }
                : orgSession
            ),
      [`GET /api/neev/resources/${resource.id}`]: () => {
        calls++
        return revoked
          ? json(
              {
                code:
                  status === 401
                    ? "unauthorized"
                    : status === 403
                      ? "forbidden"
                      : "not_found",
              },
              status
            )
          : json(resource)
      },
      [`GET ${path}`]: () => json({ items: [message], nextAfter: null }),
    })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    mount()
    await screen.findByText(message.body)
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "secret unsent"
    )
    revoked = true
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000)
    })
    expect(screen.queryByText(message.body)).toBeNull()
    expect(screen.queryByDisplayValue("secret unsent")).toBeNull()
    const count = calls
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000)
    })
    expect(calls).toBe(count)
    expect(mock.unknown).toEqual([])
  }
)

it("cannot publish a late message page into a newly selected resource", async () => {
  const other = {
    ...resource,
    id: "44444444-4444-4444-8444-444444444444",
    title: "Other chat",
  }
  let release!: (value: Response) => void
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET /api/neev/resources/${other.id}`]: () => json(other),
    [`GET ${path}`]: () =>
      new Promise((resolve) => {
        release = resolve
      }),
    [`GET /api/neev/chats/${other.id}/messages`]: () =>
      json({
        items: [{ ...message, body: "New scope message" }],
        nextAfter: null,
      }),
  })
  const renderChat = (value: NativeResource) => (
    <NativeTestRoot>
      <NativeBoundary>
        <NativeChat resource={value} onBack={() => {}} onChanged={() => {}} />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const view = render(renderChat(resource))
  await waitFor(() => expect(release).toBeDefined())
  view.rerender(renderChat(other))
  expect(await screen.findByText("New scope message")).toBeVisible()
  await act(async () => {
    release(json({ items: [message], nextAfter: null }))
  })
  expect(screen.queryByText(message.body)).toBeNull()
  expect(screen.getByText("New scope message")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it.each(["1", "9007199254740992"])(
  "orders interleaved messages losslessly from sequence %s",
  async (first) => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
    const second = {
      ...message,
      id: "33333333-3333-4333-8333-333333333333",
      sequence: String(BigInt(first) + 1n),
      body: "Second",
    }
    const third = {
      ...message,
      id: "44444444-4444-4444-8444-444444444444",
      sequence: String(BigInt(first) + 2n),
      body: "Third",
    }
    const mock = mockNativeSession(undefined, {
      [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
      [`GET ${path}`]: () =>
        json({
          items: [{ ...message, sequence: first, body: "First" }],
          nextAfter: null,
        }),
      [`GET ${path}?after=${first}`]: () =>
        json({ items: [second, third], nextAfter: null }),
      [`POST ${path}`]: () => json(third, 201),
    })
    const user = userEvent.setup()
    mount()
    await user.type(
      await screen.findByRole("textbox", { name: "Message" }),
      "Third"
    )
    await user.click(screen.getByRole("button", { name: "Send" }))
    await screen.findByText("Third")
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000)
    })
    expect(
      within(screen.getByRole("list", { name: "Messages" }))
        .getAllByRole("listitem")
        .map((item) => item.querySelector("p")?.textContent)
    ).toEqual(["First", "Second", "Third"])
    expect(mock.unknown).toEqual([])
  }
)

const capable = () => json({ ...nativeCapabilities, runtimeEnabled: true })

it("keeps the human thread primary with the agent in a collapsible panel", async () => {
  localStorage.clear()
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  // Human composer is present; the agent controls are NOT in the main column.
  await screen.findByRole("textbox", { name: "Message" })
  expect(screen.getByRole("button", { name: "Send" })).toBeVisible()
  expect(screen.queryByRole("button", { name: "Ask agent" })).toBeNull()
  // Opening the context panel reveals the agent controls.
  await user.click(screen.getByRole("button", { name: "Agent panel" }))
  expect(screen.getByRole("button", { name: "Ask agent" })).toBeVisible()
})

it("keeps Ask agent disabled while the runtime is unavailable", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "help"
  )
  // The agent lives in the collapsible right context panel; open it first.
  await user.click(screen.getByRole("button", { name: "Agent panel" }))
  expect(screen.getByRole("button", { name: "Ask agent" })).toBeDisabled()
  // Honest disabled state — no faked "Connected".
  expect(screen.getByText("Agents are turned off.")).toBeVisible()
  // Both the ask and the reply become team-visible, so the panel must say so
  // unmistakably before anyone can send (spec §5.3).
  expect(
    screen.getByText(
      "Your question and the agent's reply are both posted to this conversation."
    )
  ).toBeVisible()
})

const RUN_ID = "0123456789abcdef0123456789abcdef"
const AGENT_PATH = `POST /api/neev/chats/${resource.id}/agent`
const RUN_EVENTS = `GET /api/neev/agent/runs/${RUN_ID}/events`
const askMessage = {
  ...message,
  id: "44444444-4444-4444-8444-444444444444",
  sequence: "1",
  body: "help",
  authorKind: "human",
  agentLabel: null,
}
const replyMessage = {
  ...message,
  id: "55555555-5555-4555-8555-555555555555",
  sequence: "2",
  body: "do X",
  // The invoker stays the author of record; the label names the provider.
  authorKind: "agent",
  agentLabel: "claude",
}
const runningTurn = {
  id: "66666666-6666-4666-8666-666666666666",
  sessionId: "77777777-7777-4777-8777-777777777777",
  sequence: "1",
  runId: RUN_ID,
  prompt: "help",
  response: "",
  status: "running",
  errorCode: null,
  provider: "claude",
  model: "default",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
  completedAt: null,
}

function sseBody(...frames: string[]) {
  return new Response(frames.map((frame) => `data: ${frame}\n\n`).join(""), {
    status: 200,
    headers: { "Content-Type": "text/event-stream" },
  })
}

async function openAgentPanel(user: ReturnType<typeof userEvent.setup>) {
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "help"
  )
  // The agent lives in the collapsible right context panel; open it first.
  await user.click(screen.getByRole("button", { name: "Agent panel" }))
}

it("posts the ask and the persisted agent reply to the conversation", async () => {
  const invocations: Record<string, unknown>[] = []
  let replied = false
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({
        items: replied ? [askMessage, replyMessage] : [],
        nextAfter: null,
      }),
    [`GET ${path}?after=1`]: () =>
      json({ items: replied ? [replyMessage] : [], nextAfter: null }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: (init) => {
      invocations.push(JSON.parse(init.body as string))
      return json(
        { message: askMessage, turn: runningTurn, runId: RUN_ID },
        201
      )
    },
    [RUN_EVENTS]: () => {
      // The driver persists the reply as the run completes.
      replied = true
      return sseBody(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${runningTurn.id}"}`,
        '{"type":"chunk","text":"do "}',
        '{"type":"chunk","text":"X"}',
        '{"type":"finished","status":"completed","errorCode":null}'
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await openAgentPanel(user)
  const ask = screen.getByRole("button", { name: "Ask agent" })
  expect(ask).toBeEnabled()
  await user.click(ask)
  // The ask carries the audience the composer was pinned to and an idempotent
  // request id, exactly like posting a message.
  await waitFor(() => expect(invocations).toHaveLength(1))
  expect(invocations[0]).toEqual({
    prompt: "help",
    provider: "claude",
    model: "default",
    requestId: expect.any(String),
    expectedAuthRevision: "1",
  })
  // Both rows land in the thread, and the reply is labelled as agent-authored
  // by provider, attributed to the human who invoked it.
  const thread = await screen.findByRole("list", { name: "Messages" })
  expect(await within(thread).findByText("do X")).toBeVisible()
  expect(within(thread).getByText("help")).toBeVisible()
  expect(within(thread).getByText("claude")).toBeVisible()
  expect(within(thread).getByText("agent · asked by Sagar")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("keeps a refused invocation's ask visible and names the reason", async () => {
  const refusedTurn = {
    ...runningTurn,
    status: "failed",
    errorCode: "disabled",
    completedAt: 1_700_000_001,
  }
  let posted = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: posted ? [askMessage] : [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: () => {
      posted = true
      return json(
        { message: askMessage, turn: refusedTurn, runId: RUN_ID },
        201
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await openAgentPanel(user)
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  // The question stays team-visible even though no reply came …
  const thread = await screen.findByRole("list", { name: "Messages" })
  expect(await within(thread).findByText("help")).toBeVisible()
  // … and the refusal is surfaced rather than left as a pending state.
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(within(thread).queryByText(/asked by/)).toBeNull()
})

it("surfaces an error when the invocation is refused outright (503)", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: () => json({ code: "unavailable" }, 503),
  })
  const user = userEvent.setup()
  mount()
  await openAgentPanel(user)
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  expect(await screen.findByRole("alert")).toBeVisible()
})

it("shows Cancel while the invoked run streams and cancels it by run id", async () => {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  let canceled: unknown = null
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(c) {
            controller = c
            c.enqueue(
              new TextEncoder().encode(
                'data: {"type":"chunk","text":"partial"}\n\n'
              )
            )
          },
        }),
        { status: 200, headers: { "Content-Type": "text/event-stream" } }
      ),
    "POST /api/neev/agent/runs/cancel": (init) => {
      canceled = JSON.parse(init.body as string)
      return json({ canceled: true, status: "running" })
    },
  })
  const user = userEvent.setup()
  mount()
  await openAgentPanel(user)
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  await screen.findByText("partial")
  await user.click(screen.getByRole("button", { name: "Cancel" }))
  await waitFor(() => expect(canceled).toEqual({ runId: RUN_ID }))
  act(() => controller.close())
})

const pushing = () => json({ ...nativeCapabilities, realtime: true })
const streamPath = `GET /api/neev/chats/${resource.id}/stream`

it("renders a teammate's message from a push notice and says updates are live", async () => {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  const encoder = new TextEncoder()
  const teammate = {
    ...message,
    id: "33333333-3333-4333-8333-333333333333",
    sequence: "1",
    authorId: 2,
    authorName: "Riya",
    body: "Deploying now",
  }
  let published = false
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": pushing,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    // The push notice carries no content, so the thread only fills in once the
    // authorized list endpoint hands the message over.
    [`GET ${path}`]: () =>
      json({ items: published ? [teammate] : [], nextAfter: null }),
    [streamPath]: () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(c) {
            controller = c
          },
        }),
        { status: 200, headers: { "Content-Type": "text/event-stream" } }
      ),
  })
  mount()
  await screen.findByRole("textbox", { name: "Message" })
  act(() =>
    controller.enqueue(
      encoder.encode('data: {"type":"sync",' + '"sequence":"0"}\n\n')
    )
  )
  expect(await screen.findByText("Live updates")).toBeVisible()
  expect(screen.queryByText("Deploying now")).toBeNull()
  published = true
  act(() =>
    controller.enqueue(
      encoder.encode('data: {"type":"message","sequence":"1"}\n\n')
    )
  )
  expect(await screen.findByText("Deploying now")).toBeVisible()
  act(() => controller.close())
  expect(mock.unknown).toEqual([])
})

it("keeps polling and says so when the server does not push", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true })
  let published = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": pushing,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({
        items: published
          ? [{ ...message, authorName: "Riya", body: "Posted later" }]
          : [],
        nextAfter: null,
      }),
    // Push advertised but not actually served here (rolled back deployment).
    [streamPath]: () => json({ code: "not_found" }, 404),
  })
  mount()
  await screen.findByRole("textbox", { name: "Message" })
  // No "Live updates" claim when nothing is actually streaming.
  expect(await screen.findByText("Updates every few seconds")).toBeVisible()
  published = true
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5200)
  })
  expect(await screen.findByText("Posted later")).toBeVisible()
  expect(screen.queryByText("Live updates")).toBeNull()
})

/**
 * A layout contract, not a pixel test: jsdom has no layout, so these assert the
 * exact mechanisms whose absence produced the 390x844 bug — a title that wrapped
 * one character per line while a long action label held the row open, pushing
 * the composer off-screen.
 */
it("keeps the chat header on one row with a title that can give way", async () => {
  // The view adopts the server's copy after its first load, so the long title
  // has to come back from the resource route too.
  const longTitled = { ...resource, title: "Nested team review [test]" }
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(longTitled),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
  })
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeChat
          resource={longTitled}
          onBack={() => {}}
          onChanged={() => {}}
          actions={{
            isCreator: true,
            archived: false,
            visibility: "private",
            shareable: true,
            disabled: false,
            onRename: () => {},
            onShare: () => {},
            onMakePrivate: () => {},
            onArchiveToggle: () => {},
            onMove: () => {},
          }}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const title = await screen.findByRole("heading", {
    name: "Nested team review [test]",
  })
  // `break-words` on a squeezed box is what produced one character per line.
  expect(title).toHaveClass("truncate")
  expect(title).not.toHaveClass("break-words")
  // The title's box must be allowed to shrink below its content width.
  expect(title.parentElement).toHaveClass("min-w-0", "flex-1")
  // The actions never compress it, and never wrap into a second row.
  const share = screen.getByRole("button", { name: "Share with organization" })
  expect(share.parentElement).toHaveClass("shrink-0")
  expect(share.parentElement).not.toHaveClass("flex-wrap")
  // The visible label collapses on a phone; the accessible name never does —
  // `getByRole` above already proves that.
  expect(share.querySelector("span")).toHaveClass("hidden", "sm:inline")
})
