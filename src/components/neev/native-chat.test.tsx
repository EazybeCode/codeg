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
  // The composer remembers its mode and provider per chat, per reader; start
  // every test from "never chosen" so the defaults under test are the ones
  // being exercised.
  localStorage.clear()
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})
function mount(fresh = false) {
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

const RUN_ID = "0123456789abcdef0123456789abcdef"
// A REAL derived team session id from the database: `team_session_id` hashes
// (org, owner, chat, provider) into the 8-4-4-4-12 shape, so its version and
// variant nibbles are hash output (`2882`, `c4cb`) and not UUID bits. Using it
// here keeps invocation, discovery and replay honest about what the server
// actually sends.
const SESSION_ID = "2b4cd22b-bb13-2882-c4cb-0773b9534435"
const AGENT_PATH = `POST /api/neev/chats/${resource.id}/agent`
const RUN_EVENTS = `GET /api/neev/agent/runs/${RUN_ID}/events`
const TEAM_SESSION = (provider: string) =>
  `GET /api/neev/chats/${resource.id}/agent?provider=${provider}`
const TURNS = `GET /api/neev/agent/sessions/${SESSION_ID}/turns`
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
  sessionId: SESSION_ID,
  sequence: "1",
  runId: RUN_ID,
  prompt: "help",
  response: "",
  // Every turn carries its tool trace; an empty one is still a trace.
  activity: [],
  status: "running",
  errorCode: null,
  provider: "claude",
  model: "default",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
  completedAt: null,
}
const teamSession = {
  id: SESSION_ID,
  orgId: 3,
  ownerId: 1,
  title: "Team invocation",
  provider: "claude",
  model: "default",
  boundChatId: resource.id,
  archived: false,
  revision: "1",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
}
/** No invocation has ever been made here — an empty history, not an error. */
const noSessions = {
  [TEAM_SESSION("claude")]: () => json({ code: "not_found" }, 404),
  [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
}

function sseBody(...frames: string[]) {
  return new Response(frames.map((frame) => `data: ${frame}\n\n`).join(""), {
    status: 200,
    headers: { "Content-Type": "text/event-stream" },
  })
}

async function typeAsk(user: ReturnType<typeof userEvent.setup>) {
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "help"
  )
}

it("carries the audience promise on the control that makes it true", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
  })
  mount(true)
  const agent = await screen.findByRole("button", { name: "Ask agent" })
  const promise =
    "Your question and the agent's reply are both posted to this conversation."
  // It used to be a paragraph pinned under the composer: always there, never
  // changing, true of every send — so it stopped being read. It is now the
  // description of the toggle someone is deciding about, and its tooltip.
  expect(
    document.getElementById(agent.getAttribute("aria-describedby") ?? "")
  ).toHaveTextContent(promise)
  expect(agent).toHaveAttribute("title", promise)
  // Team mode never claims it, because team mode does not do it.
  expect(screen.getByRole("button", { name: "Team chat" })).not.toHaveAttribute(
    "aria-describedby"
  )
  // And nothing standing is printed under the row for it.
  expect(
    screen.queryAllByText(promise).filter((node) => node.tagName === "P")
  ).toHaveLength(1)
})

it("opens a new chat in agent mode and an existing one in team mode", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
  })
  const user = userEvent.setup()
  const view = mount(true)
  const agent = await screen.findByRole("button", { name: "Ask agent" })
  expect(agent).toHaveAttribute("aria-pressed", "true")
  // Both modes are one click away, inside the same conversation: there is no
  // separate agent thread to switch to.
  const team = screen.getByRole("button", { name: "Team chat" })
  expect(team).toHaveAttribute("aria-pressed", "false")
  await user.click(team)
  expect(team).toHaveAttribute("aria-pressed", "true")
  expect(agent).toHaveAttribute("aria-pressed", "false")
  // The run configuration lives inside the agent composer, not in a panel
  // beside it — and team mode carries none of it.
  expect(screen.queryByRole("button", { name: "Model" })).toBeNull()
  await user.click(agent)
  expect(screen.getByRole("button", { name: "Model" })).toBeVisible()
  view.unmount()
  // Nothing chosen for this chat yet, and no agent history in it: team.
  localStorage.clear()
  mount()
  expect(
    await screen.findByRole("button", { name: "Team chat" })
  ).toHaveAttribute("aria-pressed", "true")
})

it("remembers the chosen mode and provider across a reload", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
  })
  const user = userEvent.setup()
  const view = mount()
  // A human-only thread opens in team mode; choosing Agent is a decision.
  await user.click(await screen.findByRole("button", { name: "Ask agent" }))
  // Provider is chosen by picking a model under its heading: one grouped menu
  // rather than a provider select beside a model select.
  await user.click(screen.getByRole("button", { name: "Model" }))
  await user.click(
    within(await screen.findByRole("group", { name: "Codex" })).getByRole(
      "menuitemradio",
      { name: "Default" }
    )
  )
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Codex · Default"
    )
  )
  view.unmount()
  mount()
  // Forgetting this is not a cosmetic slip: Enter would post to the team
  // instead of invoking, and the sender would only find out afterwards.
  expect(
    await screen.findByRole("button", { name: "Ask agent" })
  ).toHaveAttribute("aria-pressed", "true")
  // The closed trigger names the provider, because both providers call their
  // default model "Default": without it nobody could tell which agent their
  // next turn goes to without opening the menu.
  expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
    "Codex · Default"
  )
})

it("opens a thread that already has agent replies in agent mode", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: [askMessage, replyMessage], nextAfter: null }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
  })
  mount()
  expect(
    await screen.findByRole("button", { name: "Ask agent" })
  ).toHaveAttribute("aria-pressed", "true")
})

it("falls back to team mode and says so when no runtime is bound", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount(true)
  expect(
    await screen.findByRole("button", { name: "Team chat" })
  ).toHaveAttribute("aria-pressed", "true")
  // Choosing it anyway is honest about why it cannot be used.
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  expect(screen.getByText("Agents are turned off.")).toBeVisible()
  const box = screen.getByRole("textbox", { name: "Message" })
  await user.type(box, "help")
  expect(screen.getByRole("button", { name: "Send" })).toBeDisabled()
  // Enter does not quietly swallow the prompt: it says why, and keeps it.
  fireEvent.keyDown(box, { key: "Enter" })
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(box).toHaveValue("help")
})

it("stays writable through a long run and keeps what was typed during it", async () => {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(c) {
            controller = c
          },
        }),
        { status: 200, headers: { "Content-Type": "text/event-stream" } }
      ),
  })
  const user = userEvent.setup()
  mount(true)
  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))
  const box = await screen.findByRole("textbox", { name: "Message" })
  // The ask went durable and cleared the box, but the box itself stays open:
  // a run can last minutes and thinking does not pause for it.
  await waitFor(() => expect(box).toHaveValue(""))
  expect(box).toBeEnabled()
  await user.type(box, "and also this")
  act(() =>
    controller.enqueue(
      new TextEncoder().encode(
        'data: {"type":"finished","status":"completed","errorCode":null}\n\n'
      )
    )
  )
  act(() => controller.close())
  // Finishing the run publishes the reply; it does not touch what was typed
  // while it ran.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled()
  )
  expect(box).toHaveValue("and also this")
})

it("posts the ask and the persisted agent reply to the one conversation", async () => {
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
    ...noSessions,
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
  mount(true)
  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))
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
  // Both rows land in the same thread, and the reply is labelled as
  // agent-authored by provider, attributed to the human who invoked it.
  const thread = await screen.findByRole("list", { name: "Messages" })
  expect(await within(thread).findByText("do X")).toBeVisible()
  expect(within(thread).getByText("help")).toBeVisible()
  expect(within(thread).getByText("agent · asked by Sagar")).toBeVisible()
  // The ask was shown immediately from the invocation AND came back in the
  // reread; it is one message either way.
  await waitFor(() =>
    expect(within(thread).getAllByRole("listitem")).toHaveLength(2)
  )
  expect(mock.unknown).toEqual([])
})

it("sends on Enter, keeps Shift+Enter a newline, and never sends mid-composition", async () => {
  const invocations: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: (init) => {
      invocations.push(JSON.parse(init.body as string))
      return json(
        { message: askMessage, turn: runningTurn, runId: RUN_ID },
        201
      )
    },
    [RUN_EVENTS]: () =>
      sseBody('{"type":"finished","status":"completed","errorCode":null}'),
  })
  const user = userEvent.setup()
  mount(true)
  const box = await screen.findByRole("textbox", { name: "Message" })
  await user.type(box, "help")
  fireEvent.keyDown(box, { key: "Enter", shiftKey: true })
  expect(invocations).toHaveLength(0)
  // An input method is mid-conversion: Enter accepts a candidate, not a send.
  fireEvent.keyDown(box, { key: "Enter", isComposing: true })
  fireEvent.keyDown(box, { key: "Enter", keyCode: 229 })
  expect(invocations).toHaveLength(0)
  fireEvent.keyDown(box, { key: "Enter" })
  await waitFor(() => expect(invocations).toHaveLength(1))
  expect(invocations[0].prompt).toBe("help")
})

it("names the tool in flight, counts the seconds, and opens the call", async () => {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  const encoder = new TextEncoder()
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(c) {
            controller = c
          },
        }),
        { status: 200, headers: { "Content-Type": "text/event-stream" } }
      ),
  })
  const user = userEvent.setup()
  mount(true)
  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))
  // Working status is immediate, before any activity has arrived, and it
  // counts from the moment the run started. (The Send button says "Working…"
  // too, so this asks the thread rather than the page.)
  const thread = within(await screen.findByRole("list", { name: "Messages" }))
  expect(await thread.findByRole("button", { name: /Working…/ })).toBeVisible()
  expect(screen.getByText("0s")).toBeVisible()
  await waitFor(() => expect(controller).toBeDefined())
  act(() =>
    controller.enqueue(
      encoder.encode(
        'data: {"type":"activity","kind":"tool_call","seq":1,"offset":0,' +
          '"id":"call-1","name":"Bash","input":"{\\"command\\":\\"ls -a\\"}",' +
          '"inputTruncated":false}\n\n'
      )
    )
  )
  // The CURRENT tool, not a generic spinner.
  expect(await screen.findByText("Running Bash…")).toBeVisible()
  // The same frame twice is one row: `seq` is the identity, not the position.
  act(() =>
    controller.enqueue(
      encoder.encode(
        'data: {"type":"activity","kind":"tool_call","seq":1,"offset":0,' +
          '"id":"call-1","name":"Bash","input":"{\\"command\\":\\"ls -a\\"}",' +
          '"inputTruncated":false}\n\n'
      )
    )
  )
  await user.click(screen.getByRole("button", { name: /Running Bash/ }))
  expect(screen.getAllByText("Bash")).toHaveLength(1)
  // The call opens into its real arguments — a public summary of what ran.
  await user.click(screen.getByRole("button", { name: /^Bash/ }))
  expect(screen.getByText('{"command":"ls -a"}')).toBeVisible()
  act(() => controller.close())
})

it("shows Cancel while the invoked run streams and cancels it by run id", async () => {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  let canceled: unknown = null
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
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
  mount(true)
  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))
  await screen.findByText("partial")
  await user.click(screen.getByRole("button", { name: "Cancel" }))
  await waitFor(() => expect(canceled).toEqual({ runId: RUN_ID }))
  act(() => controller.close())
}, 10000)

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
    ...noSessions,
    [AGENT_PATH]: () => {
      posted = true
      return json(
        { message: askMessage, turn: refusedTurn, runId: RUN_ID },
        201
      )
    },
  })
  const user = userEvent.setup()
  mount(true)
  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))
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
    ...noSessions,
    [AGENT_PATH]: () => json({ code: "unavailable" }, 503),
  })
  const user = userEvent.setup()
  mount(true)
  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(await screen.findByRole("alert")).toBeVisible()
})

it("points an unconnected provider at the one place it can be fixed", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          {
            provider: "claude",
            state: "reconnect_required",
            accountLabel: "sagar@example.test",
            checkedAt: 1_700_000_000,
            errorCode: "unauthorized",
          },
        ],
      }),
  })
  mount(true)
  expect(await screen.findByText("claude is not connected yet.")).toBeVisible()
  expect(
    screen.getByRole("link", { name: "Personal AI connections" })
  ).toHaveAttribute("href", "/settings")
})

const completedTurn = {
  ...runningTurn,
  response: "do X",
  status: "completed",
  completedAt: 1_700_000_050,
  activity: [
    {
      seq: 1,
      offset: 0,
      id: "call-1",
      kind: "tool_call",
      name: "Read",
      input: '{"file_path":"/tmp/notes.md"}',
      inputTruncated: false,
    },
    {
      seq: 2,
      offset: 0,
      id: "result-1",
      kind: "tool_result",
      callId: "call-1",
      output: "two lines",
      outputTruncated: false,
      isError: false,
    },
  ],
}

it("replays this reader's own stored trace when the conversation is reopened", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: [askMessage, replyMessage], nextAfter: null }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    [TEAM_SESSION("claude")]: () => json(teamSession),
    [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
    [TURNS]: () => json({ items: [completedTurn], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  // The reply is there from the message list; the trace comes from the turn.
  expect(await screen.findByText("do X")).toBeVisible()
  const summary = await screen.findByRole("button", {
    name: /1 tool calls, 1 results/,
  })
  await user.click(summary)
  expect(screen.getByText("Read")).toBeVisible()
  expect(screen.getByText("/tmp/notes.md")).toBeVisible()
  expect(screen.getByText("two lines")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("re-reads only what is new after a run completes", async () => {
  const reads: string[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: [askMessage, replyMessage], nextAfter: null }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    [TEAM_SESSION("claude")]: () => json(teamSession),
    [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
    [TURNS]: () => {
      reads.push("all")
      return json({ items: [completedTurn], nextAfter: null })
    },
    [`${TURNS}?after=1`]: () => {
      reads.push("after=1")
      return json({ items: [], nextAfter: null })
    },
    [AGENT_PATH]: () =>
      json(
        {
          message: { ...askMessage, id: replyMessage.id, sequence: "3" },
          turn: { ...runningTurn, sequence: "2" },
          runId: RUN_ID,
        },
        201
      ),
    [`GET ${path}?after=3`]: () => json({ items: [], nextAfter: null }),
    [RUN_EVENTS]: () =>
      sseBody('{"type":"finished","status":"completed","errorCode":null}'),
  })
  const user = userEvent.setup()
  mount()
  await screen.findByRole("button", { name: /1 tool calls/ })
  expect(reads).toEqual(["all"])
  await user.type(screen.getByRole("textbox", { name: "Message" }), "and then?")
  await user.click(screen.getByRole("button", { name: "Send" }))
  // The settled turn is settled forever, so the next read starts after it
  // rather than pulling the whole history down again.
  await waitFor(() => expect(reads).toEqual(["all", "after=1"]))
  expect(mock.unknown).toEqual([])
})

it("never attaches another person's private trace to their reply", async () => {
  // The holder the server handed back is owned by someone else. Even if it
  // arrived, its turns are not this reader's to see.
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({
        items: [
          { ...askMessage, authorId: 2, authorName: "Riya" },
          { ...replyMessage, authorId: 2, authorName: "Riya" },
        ],
        nextAfter: null,
      }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    [TEAM_SESSION("claude")]: () => json({ ...teamSession, ownerId: 2 }),
    [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
    [TURNS]: () => json({ items: [completedTurn], nextAfter: null }),
  })
  mount()
  expect(await screen.findByText("do X")).toBeVisible()
  await waitFor(() => expect(screen.queryByText(/tool calls/)).toBeNull())
  expect(screen.queryByText("Read")).toBeNull()
})

it("rejoins a run that was still going, without re-sending anything", async () => {
  const attachments: string[] = []
  const posts: string[] = []
  let replied = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({
        items: replied ? [askMessage, replyMessage] : [askMessage],
        nextAfter: null,
      }),
    [`GET ${path}?after=1`]: () =>
      json({ items: replied ? [replyMessage] : [], nextAfter: null }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: () => {
      posts.push("invoked")
      return json(
        { message: askMessage, turn: runningTurn, runId: RUN_ID },
        201
      )
    },
    [TEAM_SESSION("claude")]: () => json(teamSession),
    [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
    [TURNS]: () =>
      json({
        items: [replied ? completedTurn : runningTurn],
        nextAfter: null,
      }),
    [RUN_EVENTS]: () => {
      attachments.push(RUN_ID)
      replied = true
      return sseBody(
        '{"type":"chunk","text":"resumed"}',
        '{"type":"finished","status":"completed","errorCode":null}'
      )
    },
  })
  mount()
  // Reopening attaches to the run already in flight and streams it …
  expect(await screen.findByText("do X")).toBeVisible()
  // … exactly once, and it never re-posts the prompt.
  expect(posts).toEqual([])
  expect(attachments).toEqual([RUN_ID])
  expect(
    within(screen.getByRole("list", { name: "Messages" })).getAllByRole(
      "listitem"
    )
  ).toHaveLength(2)
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
it("keeps the chat header on one compact row with a title that can give way", async () => {
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

it("keeps what was typed while a send was in flight", async () => {
  // The composer stays usable during a send, because an agent run shares this
  // box and can take minutes. That makes the clear a race: by the time the
  // server answers, the next message may already be half-written. Clearing the
  // box wholesale would delete it in front of the person typing it.
  let release!: (value: Response) => void
  const user = userEvent.setup()
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: () =>
      new Promise<Response>((resolve) => {
        release = resolve
      }),
  })
  mount()
  const composer = await screen.findByRole("textbox", { name: "Message" })
  await user.type(composer, "first message")
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(release).toBeDefined())

  // Still usable, and used.
  expect(composer).toBeEnabled()
  await user.clear(composer)
  await user.type(composer, "second thought")
  await act(async () => {
    release(json({ ...message, body: "first message" }, 201))
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
  // The send cleared what it sent, and nothing else.
  expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue(
    "second thought"
  )
})

it("lets the FIRST ask be configured, and sends that configuration with it", async () => {
  // The holder for (chat, provider) is created BY this ask. If the controls
  // waited for it, the first and costliest turn would be the one turn nobody
  // could configure — so the choice travels with the invocation instead.
  const invocations: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: (init) => {
      invocations.push(JSON.parse(init.body as string))
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
  mount(true)

  // Nothing is disabled and nothing says "ask first": all three controls are
  // live before a single turn has run.
  const model = await screen.findByRole("button", { name: "Model" })
  expect(model).toBeEnabled()
  expect(screen.getByRole("button", { name: "Reasoning" })).toBeEnabled()

  await user.click(model)
  await user.click(
    within(await screen.findByRole("group", { name: "Claude Code" })).getByRole(
      "menuitemradio",
      { name: "Opus" }
    )
  )
  // Opus is the Claude model that can do fast mode, so the chip opens up —
  // but it is still off until it is pressed.
  const fast = await screen.findByRole("button", { name: "Fast" })
  await waitFor(() => expect(fast).toBeEnabled())
  expect(fast).toHaveAttribute("aria-pressed", "false")
  await user.click(fast)
  await user.click(screen.getByRole("button", { name: "Reasoning" }))
  await user.click(
    await screen.findByRole("menuitemradio", { name: "Very high" })
  )

  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))

  await waitFor(() => expect(invocations).toHaveLength(1))
  expect(invocations[0]).toEqual({
    prompt: "help",
    provider: "claude",
    model: "opus",
    effort: "xhigh",
    fast: true,
    requestId: expect.any(String),
    expectedAuthRevision: "1",
  })
})

it("keeps the draft and the chosen configuration when the first ask fails", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () => json({ code: "unavailable" }, 503),
  })
  const user = userEvent.setup()
  mount(true)
  await user.click(await screen.findByRole("button", { name: "Model" }))
  await user.click(
    within(await screen.findByRole("group", { name: "Claude Code" })).getByRole(
      "menuitemradio",
      { name: "Haiku" }
    )
  )
  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))

  // Nothing was persisted, so nothing typed or chosen is thrown away: the ask
  // can be sent again without re-deciding anything.
  await waitFor(() =>
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("help")
  )
  expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
    "Haiku"
  )
})

it("switches provider through the model menu and keeps each holder's own configuration", async () => {
  // Each provider has its OWN holder for this chat. Switching must show that
  // holder's stored configuration, never the other one's, or the row would
  // describe a configuration the run will not use.
  const CODEX_SESSION_ID = "3c4cd22b-bb13-2882-c4cb-0773b9534435"
  const invocations: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    [TEAM_SESSION("claude")]: () =>
      json({ ...teamSession, model: "opus", effort: "max", fast: true }),
    [TEAM_SESSION("codex")]: () =>
      json({
        ...teamSession,
        id: CODEX_SESSION_ID,
        provider: "codex",
        model: "default",
        effort: "minimal",
        fast: false,
      }),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [`GET /api/neev/agent/sessions/${CODEX_SESSION_ID}/turns`]: () =>
      json({ items: [], nextAfter: null }),
    [AGENT_PATH]: (init) => {
      invocations.push(JSON.parse(init.body as string))
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
  mount(true)

  // Claude's holder: Opus, Max, fast.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Claude Code · Opus"
    )
  )
  expect(screen.getByRole("button", { name: "Reasoning" })).toHaveTextContent(
    "Max"
  )
  expect(screen.getByRole("button", { name: "Fast" })).toHaveAttribute(
    "aria-pressed",
    "true"
  )

  await user.click(screen.getByRole("button", { name: "Model" }))
  await user.click(
    within(await screen.findByRole("group", { name: "Codex" })).getByRole(
      "menuitemradio",
      { name: "Default" }
    )
  )

  // Codex's own holder takes over: its level, and its speed. Claude's `max`
  // could not survive anyway — Codex does not advertise it.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Codex · Default"
    )
  )
  expect(screen.getByRole("button", { name: "Reasoning" })).toHaveTextContent(
    "Minimal"
  )
  expect(screen.getByRole("button", { name: "Fast" })).toHaveAttribute(
    "aria-pressed",
    "false"
  )

  await typeAsk(user)
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(invocations).toHaveLength(1))
  // The ask goes to the provider the menu selected, under that provider's
  // configuration.
  expect(invocations[0]).toMatchObject({
    provider: "codex",
    model: "default",
    effort: "minimal",
  })
  expect(invocations[0]).not.toHaveProperty("fast")
})
