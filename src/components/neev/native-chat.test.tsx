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
import { ForkTabsContext, type OpenFork } from "./native-reply-actions"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  orgSession,
  composerText,
  composerEditable,
  typeInComposer,
  type Handler,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"
import messages from "@/i18n/messages/en.json"

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
/**
 * Wait for a mounted chat to finish its first read.
 *
 * The shell is now drawn before the metadata and history come back, so the
 * composer appearing no longer means the chat is loaded — which is what most
 * of these tests were using it for. This is the real signal.
 */
async function ready() {
  await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() =>
    expect(screen.queryByText("Loading this conversation…")).toBeNull()
  )
}
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
  await ready()
  expect((await screen.findByText("Decision")).tagName).toBe("STRONG")
  expect(screen.queryByRole("link", { name: "bad" })).toBeNull()
})
it("asks for history without waiting for the metadata to come back", async () => {
  // The round trip this removes: history was awaited AFTER the metadata, though
  // it reads nothing off it, so every chat open paid two serial round trips
  // where one would do.
  let release: () => void = () => {}
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  let asked = false
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: async () => {
      await held
      return json(resource)
    },
    [`GET ${path}`]: () => {
      asked = true
      return json({ items: [message], nextAfter: null })
    },
  })
  mount()
  // Still holding the metadata response, so this can only be true if the two
  // reads were started together.
  await waitFor(() => expect(asked).toBe(true))
  release()
  await ready()
  await screen.findByText(message.body)
})
it("shows no history when the metadata refuses, though it was already asked for", async () => {
  // The authority check stays FIRST. The history read is in flight by the time
  // the refusal lands, and its answer must never reach the transcript.
  const unhandled = vi.fn()
  window.addEventListener("unhandledrejection", unhandled)
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () =>
      json({ code: "forbidden" }, 403),
    [`GET ${path}`]: () => json({ items: [message], nextAfter: null }),
  })
  mount()
  await waitFor(() =>
    expect(screen.queryByText("Loading this conversation…")).toBeNull()
  )
  expect(screen.queryByText(message.body)).toBeNull()
  window.removeEventListener("unhandledrejection", unhandled)
  expect(unhandled).not.toHaveBeenCalled()
})
it("reports the history failure when the page rejects and the metadata does not", async () => {
  // The sink that keeps the early read from becoming an unhandled rejection must
  // not also swallow it: a history read that fails is still a failed open.
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ code: "unavailable" }, 503),
  })
  mount()
  await waitFor(() =>
    expect(screen.queryByText("Loading this conversation…")).toBeNull()
  )
  expect(screen.queryByText(message.body)).toBeNull()
})
it("brings a successfully sent message into view from older history", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: () => json(message, 201),
  })
  mount()
  await ready()
  const history = await screen.findByRole("list", { name: "Messages" })
  Object.defineProperties(history, {
    scrollHeight: { value: 900 },
    clientHeight: { value: 200 },
  })
  history.scrollTop = 0
  fireEvent.scroll(history)
  const user = userEvent.setup()
  await typeInComposer(
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
  await ready()
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    message.body
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(composerText(screen.getByRole("textbox", { name: "Message" }))).toBe(
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
  await ready()
  await typeInComposer(
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
  await ready()
  await screen.findByText(message.body)
  // Opening starts both at once, so which of the two the mock sees first is not
  // a property worth pinning — only that both were asked for and neither waited.
  expect([...order].sort()).toEqual(["first", "metadata"])
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
  // A POLL keeps the strict order: its metadata read is the authority check, and
  // it is how a revocation is noticed, so the history behind it is never asked
  // for concurrently. This is the half of the ordering that still matters.
  expect(order.slice(2)).toEqual(["metadata", "incremental"])
  expect(order).toHaveLength(4)
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
    mount()
    await ready()
    await screen.findByText(message.body)
    await typeInComposer(
      screen.getByRole("textbox", { name: "Message" }),
      "secret unsent"
    )
    revoked = true
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000)
    })
    expect(screen.queryByText(message.body)).toBeNull()
    expect(screen.queryByText("secret unsent")).toBeNull()
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
    await ready()
    await typeInComposer(
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
  "GET /api/neev/agent/connections": () =>
    json({
      runtimeEnabled: true,
      connections: [
        {
          provider: "claude",
          state: "connected",
          accountLabel: "test",
          checkedAt: 1700000000,
          errorCode: null,
        },
        {
          provider: "codex",
          state: "connected",
          accountLabel: "test",
          checkedAt: 1700000000,
          errorCode: null,
        },
      ],
    }),
  [TEAM_SESSION("claude")]: () => json({ code: "not_found" }, 404),
  [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
}

function sseBody(...frames: string[]) {
  return new Response(frames.map((frame) => `data: ${frame}\n\n`).join(""), {
    status: 200,
    headers: { "Content-Type": "text/event-stream" },
  })
}

/** A stream the test feeds frame by frame. */
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
    close: () => controller.close(),
  }
}

async function typeAsk() {
  await typeInComposer(
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
  await ready()
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
  await ready()
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
  await ready()
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
  await ready()
  // A human-only thread opens in team mode; choosing Agent is a decision.
  await user.click(await screen.findByRole("button", { name: "Ask agent" }))
  // Provider is chosen by picking a model under its heading: one grouped menu
  // rather than a provider select beside a model select.
  await pickFromMoreModels(user, "Codex", "Default")
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Codex · Default"
    )
  )
  view.unmount()
  mount()
  await ready()
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
  await ready()
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
  await ready()
  expect(
    await screen.findByRole("button", { name: "Team chat" })
  ).toHaveAttribute("aria-pressed", "true")
  // Choosing it anyway is honest about why it cannot be used.
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  expect(screen.getByText("Agents are turned off.")).toBeVisible()
  const box = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(box, "help")
  expect(screen.getByRole("button", { name: "Send" })).toBeDisabled()
  // Enter does not quietly swallow the prompt: it says why, and keeps it.
  fireEvent.keyDown(box, { key: "Enter" })
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(composerText(box)).toBe("help")
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
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  const box = await screen.findByRole("textbox", { name: "Message" })
  // The ask went durable and cleared the box, but the box itself stays open:
  // a run can last minutes and thinking does not pause for it.
  await waitFor(() => expect(composerText(box)).toBe(""))
  expect(composerEditable(box)).toBe(true)
  await typeInComposer(box, "and also this")
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
  expect(composerText(box)).toBe("and also this")
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
  await ready()
  await typeAsk()
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

it("hands the streamed reply to the stored one with no gap and no double", async () => {
  // The handover used to clear the preview and THEN go to the network for the
  // stored reply. The answer someone was reading disappeared for two round
  // trips and came back in a row of a different height — which is what "the
  // streaming view isn't good" was describing.
  let releaseHistory: () => void = () => {},
    releaseTraces: () => void = () => {}
  const history = new Promise<void>((resolve) => {
      releaseHistory = resolve
    }),
    traces = new Promise<void>((resolve) => {
      releaseTraces = resolve
    })
  let replied = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    ...noSessions,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: async () => {
      if (replied) await history
      return json({
        items: replied ? [askMessage, replyMessage] : [],
        nextAfter: null,
      })
    },
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    [TEAM_SESSION("claude")]: async () => {
      if (!replied) return json({ code: "not_found" }, 404)
      await traces
      return json(teamSession)
    },
    [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () => {
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
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  // The run is over — the trace has stopped saying it is working — and the
  // reply it streamed is still on screen, with the stored row held back at the
  // network.
  const list = await screen.findByRole("list", { name: "Messages" })
  const thread = within(list)
  // The live row is the one inside the live region; the stored row is not.
  // (A settled reply's Copy announces through its own sr-only region.)
  const preview = () => list.querySelector('[aria-live="polite"]:not(.sr-only)')
  await waitFor(() =>
    expect(thread.queryByRole("button", { name: /Working…/ })).toBeNull()
  )
  expect(preview()).toHaveTextContent("do X")
  expect(thread.getAllByText("do X")).toHaveLength(1)
  // The stored row lands while the preview is still mounted: one answer, not
  // the same answer printed twice, and never none.
  releaseHistory()
  await waitFor(() => expect(preview()).toBeNull())
  expect(thread.getAllByText("do X")).toHaveLength(1)
  releaseTraces()
  await waitFor(() => expect(thread.getAllByRole("listitem")).toHaveLength(2))
  expect(thread.getAllByText("do X")).toHaveLength(1)
})

it("keeps the preview up against an identical answer already in the thread", async () => {
  // Two asks, one answer. Retiring the preview on the body alone would take
  // the older row as this run's reply and blank the answer being read.
  const older = {
    ...replyMessage,
    id: "77777777-7777-4777-8777-777777777777",
    sequence: "2",
  }
  let release: () => void = () => {}
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  let replied = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: [askMessage, older], nextAfter: null }),
    [`GET ${path}?after=2`]: async () => {
      if (replied) await held
      return json({
        items: replied
          ? [
              {
                ...askMessage,
                id: "88888888-8888-4888-8888-888888888888",
                sequence: "3",
              },
              { ...replyMessage, sequence: "4" },
            ]
          : [],
        nextAfter: null,
      })
    },
    ...noSessions,
    [AGENT_PATH]: () =>
      json(
        {
          message: {
            ...askMessage,
            id: "88888888-8888-4888-8888-888888888888",
            sequence: "3",
          },
          turn: runningTurn,
          runId: RUN_ID,
        },
        201
      ),
    [RUN_EVENTS]: () => {
      replied = true
      return sseBody(
        '{"type":"chunk","text":"do X"}',
        '{"type":"finished","status":"completed","errorCode":null}'
      )
    },
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  const thread = within(await screen.findByRole("list", { name: "Messages" }))
  await waitFor(() =>
    expect(thread.queryByRole("button", { name: /Working…/ })).toBeNull()
  )
  // The old reply and the live one: two, because there genuinely are two.
  expect(thread.getAllByText("do X")).toHaveLength(2)
  release()
  // Still two once the new row lands — the preview it replaced is gone.
  await waitFor(() => expect(thread.getAllByRole("listitem")).toHaveLength(4))
  expect(thread.getAllByText("do X")).toHaveLength(2)
})

it("keeps the answer on screen when the re-read that follows the run fails", async () => {
  let broken = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () =>
      broken ? json({ code: "server_error" }, 500) : json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () => {
      broken = true
      return sseBody(
        '{"type":"chunk","text":"do X"}',
        '{"type":"finished","status":"completed","errorCode":null}'
      )
    },
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  const thread = within(await screen.findByRole("list", { name: "Messages" }))
  // The re-read failed and said so. The answer it failed to fetch a copy of is
  // still the one on screen: clearing it here would lose it outright.
  expect(await screen.findByRole("alert")).toBeVisible()
  await waitFor(() =>
    expect(thread.queryByRole("button", { name: /Working…/ })).toBeNull()
  )
  expect(thread.getByText("do X")).toBeVisible()
})

it("does not let a finished run's handover clear the next run's reply", async () => {
  let release: () => void = () => {}
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  let run = 0
  const second = "99999999-9999-4999-8999-999999999999"
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: async () => {
      if (run === 1) await held
      return json({ items: [], nextAfter: null })
    },
    ...noSessions,
    [AGENT_PATH]: () => {
      run += 1
      return json(
        {
          message: { ...askMessage, id: run === 1 ? askMessage.id : second },
          turn: runningTurn,
          runId: RUN_ID,
        },
        201
      )
    },
    [RUN_EVENTS]: () =>
      sseBody(
        `{"type":"chunk","text":"${run === 1 ? "first" : "second"}"}`,
        '{"type":"finished","status":"completed","errorCode":null}'
      ),
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  const thread = within(await screen.findByRole("list", { name: "Messages" }))
  expect(await thread.findByText("first")).toBeVisible()
  // The first run's re-read is still out at the network. Ask again anyway.
  await waitFor(() =>
    expect(thread.queryByRole("button", { name: /Working…/ })).toBeNull()
  )
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  expect(await thread.findByText("second")).toBeVisible()
  // The first run's handover lands last, and belongs to nothing on screen.
  release()
  await waitFor(() =>
    expect(thread.queryByRole("button", { name: /Working…/ })).toBeNull()
  )
  expect(thread.getByText("second")).toBeVisible()
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
  mount(true)
  await ready()
  const box = await screen.findByRole("textbox", { name: "Message" })
  await typeInComposer(box, "help")
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
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  // Working status is immediate, before any activity has arrived, and it
  // counts from the moment the run started. It is announced from the progress
  // footer rather than printed — the tool in flight gets a row of its own in
  // the transcript, so painting its name twice would say it twice. The Send
  // button stays "Send" and only goes disabled.
  const thread = within(await screen.findByRole("list", { name: "Messages" }))
  expect(await thread.findByText("Working…")).toBeVisible()
  // Tenths, from the first frame: the digit that moves is the whole point.
  expect(screen.getByText("0.0s")).toBeVisible()
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
  // One row, not two: `seq` is the identity, not the position.
  expect(screen.getAllByText("Bash")).toHaveLength(1)
  // The command is on the row itself, clipped, without opening anything.
  expect(screen.getByText("ls -a")).toBeVisible()
  // The call opens into its real arguments — a public summary of what ran.
  await user.click(screen.getByRole("button", { name: /^Bash/ }))
  expect(screen.getByText('{"command":"ls -a"}')).toBeVisible()
  act(() => controller.close())
})

it("shows Stop generation while the invoked run streams and cancels it by run id", async () => {
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
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await screen.findByText("partial")
  await user.click(screen.getByRole("button", { name: "Stop generation" }))
  await waitFor(() => expect(canceled).toEqual({ runId: RUN_ID }))
  act(() => {
    controller.enqueue(
      new TextEncoder().encode(
        'data: {"type":"finished","status":"canceled","errorCode":null}\n\n'
      )
    )
    controller.close()
  })
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Stop generation" })).toBeNull()
  )
  expect(
    screen.queryByText(
      "This service is temporarily unavailable. Please try again."
    )
  ).toBeNull()
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
  await ready()
  await typeAsk()
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
  await ready()
  await typeAsk()
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
  await ready()
  // A compact control that GOES where connecting happens, not a sentence
  // describing the state. The sentence is still there as the control's
  // accessible description — honest, without a standing line under every
  // composer — and a provider that needs re-authorising says Reconnect.
  const connect = await screen.findByRole("button", { name: "Reconnect" })
  fireEvent.click(connect)
  expect(await screen.findByRole("dialog")).toBeVisible()
  expect(
    document.getElementById(connect.getAttribute("aria-describedby") ?? "")
  ).toHaveTextContent("claude is not connected yet.")
})

/**
 * Pick a model that is not on the shortlist, through "More models".
 *
 * `fireEvent` for the submenu row: Radix keeps a submenu open using the
 * pointer's position against the trigger's geometry, and every rect is zero in
 * jsdom, so userEvent's move-then-click closes it before the click lands.
 */
async function pickFromMoreModels(
  user: ReturnType<typeof userEvent.setup>,
  group: string,
  label: string
) {
  await user.click(screen.getByRole("button", { name: "Model" }))
  await user.click(screen.getByRole("menuitem", { name: "More models" }))
  const all = await screen.findByRole("menu", { name: "More models" })
  fireEvent.click(
    within(within(all).getByRole("group", { name: group })).getByRole(
      "menuitemradio",
      { name: label }
    )
  )
}

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
  await ready()
  // The reply is there from the message list; the trace comes from the turn,
  // and is drawn in place rather than folded into a drawer.
  await waitFor(() => expect(screen.getByText("do X")).toBeVisible())
  const row = await screen.findByRole("button", { name: /^Read/ })
  // Name and path are on the row already. The output is what a click is for.
  expect(within(row).getByText("Read")).toBeVisible()
  expect(within(row).getByText("/tmp/notes.md")).toBeVisible()
  expect(screen.queryByText("two lines")).toBeNull()
  await user.click(row)
  expect(screen.getByText("two lines")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("says a turn completed even when tools failed on the way, and still counts them", async () => {
  // The real shape of the complaint: the agent reached for `python`, did not
  // find it, tried `node`, did not find that either, finished the job with `rg`
  // and wrote a useful review — and the row said "2 failed" in red with no
  // outcome beside it, so a completed review read as a broken one.
  const recovered = {
    ...completedTurn,
    activity: [
      ...completedTurn.activity,
      {
        seq: 3,
        offset: 0,
        id: "call-2",
        kind: "tool_call",
        name: "Bash",
        input: '{"command":"python parse.py"}',
        inputTruncated: false,
      },
      {
        seq: 4,
        offset: 0,
        id: "result-2",
        kind: "tool_result",
        callId: "call-2",
        output: "python: command not found",
        outputTruncated: false,
        isError: true,
      },
    ],
  }
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: [askMessage, replyMessage], nextAfter: null }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    [TEAM_SESSION("claude")]: () => json(teamSession),
    [TEAM_SESSION("codex")]: () => json({ code: "not_found" }, 404),
    [TURNS]: () => json({ items: [recovered], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await ready()
  // The outcome is stated on the footer. This is the whole point: the turn
  // answered.
  expect(await screen.findByText("Completed")).toBeVisible()
  // And the failure is NOT softened away — it really did fail, it is still
  // counted, and it still says so. One failure reads as one, not "1 steps".
  expect(screen.getByText("1 step failed")).toBeVisible()
  // The failing step is marked failed on its own row, beside the command that
  // failed, and opening it shows verbatim what the tool said.
  const failed = screen.getByRole("button", { name: /^Bash/ })
  expect(within(failed).getByText("python parse.py")).toBeVisible()
  expect(within(failed).getByText("Failed")).toBeVisible()
  await user.click(failed)
  expect(screen.getByText("python: command not found")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("re-reads only what is new after a run completes", async () => {
  const reads: string[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    ...noSessions,
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
  await ready()
  await screen.findByRole("button", { name: /^Read/ })
  expect(reads).toEqual(["all"])
  await typeInComposer(
    screen.getByRole("textbox", { name: "Message" }),
    "and then?"
  )
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
  await ready()
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
  await ready()
  // Reopening attaches to the run already in flight and streams it …
  expect(await screen.findByText("do X")).toBeVisible()
  // … exactly once, and it never re-posts the prompt.
  expect(posts).toEqual([])
  expect(attachments).toEqual([RUN_ID])
  // Two MESSAGES. Counted as direct children rather than by role: a reply now
  // draws its tool rows as a nested list inside its own item, so every
  // `listitem` under the thread is no longer one message.
  expect(screen.getByRole("list", { name: "Messages" }).children).toHaveLength(
    2
  )
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
  await ready()
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
  await ready()
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
  // The label reads just "Share". Who it shares with is the whole consequence
  // of pressing it, so that half moves into the accessible name and the
  // tooltip rather than disappearing: `getByRole` above found this button by
  // the full sentence, and the visible word is contained in it, which is what
  // keeps voice control able to say what it sees (WCAG 2.5.3).
  expect(share.querySelector("span")).toHaveTextContent(/^Share$/)
  expect(share).toHaveAttribute("title", "Share with organization")
  expect(share.getAttribute("aria-label")).toContain(
    share.querySelector("span")?.textContent ?? ""
  )
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
  await ready()
  const composer = await screen.findByRole("textbox", { name: "Message" })
  await typeInComposer(composer, "first message")
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(release).toBeDefined())

  // Still usable, and used.
  expect(composerEditable(composer)).toBe(true)
  await user.clear(composer)
  await typeInComposer(composer, "second thought")
  await act(async () => {
    release(json({ ...message, body: "first message" }, 201))
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
  // The send cleared what it sent, and nothing else.
  expect(composerText(screen.getByRole("textbox", { name: "Message" }))).toBe(
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
  await ready()

  // Nothing is disabled and nothing says "ask first": all three controls are
  // live before a single turn has run.
  const model = await screen.findByRole("button", { name: "Model" })
  expect(model).toBeEnabled()

  await pickFromMoreModels(user, "Claude Code", "Opus")
  await waitFor(() => expect(model).toHaveTextContent("Opus"))

  // Opus is the Claude model that can do fast mode, so the row opens up —
  // but it is still off until it is pressed. Model, reasoning and speed are
  // all in the one menu now, so this is a single open.
  await user.click(model)
  const fast = screen.getByRole("menuitemcheckbox", { name: "Fast" })
  expect(fast).not.toHaveAttribute("aria-disabled", "true")
  expect(fast).toHaveAttribute("aria-checked", "false")
  await user.click(fast)
  // The menu deliberately stays open across a speed toggle, so reasoning is
  // the next click rather than the next reopen.
  await user.click(screen.getByRole("menuitem", { name: /Reasoning/ }))
  const levels = await screen.findByRole("menu", { name: "Reasoning" })
  fireEvent.click(
    within(levels).getByRole("menuitemradio", { name: "Very high" })
  )
  await user.keyboard("{Escape}")

  await typeAsk()
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
  await ready()
  await user.click(await screen.findByRole("button", { name: "Model" }))
  await user.click(
    within(await screen.findByRole("group", { name: "Claude Code" })).getByRole(
      "menuitemradio",
      { name: "Haiku" }
    )
  )
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))

  // Nothing was persisted, so nothing typed or chosen is thrown away: the ask
  // can be sent again without re-deciding anything.
  await waitFor(() =>
    expect(composerText(screen.getByRole("textbox", { name: "Message" }))).toBe(
      "help"
    )
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
    ...noSessions,
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
  await ready()

  // Claude's holder: Opus, Max, fast. "Opus" names its provider by naming
  // itself, so the trigger does not spend width on "Claude Code ·" as well —
  // it only does that for a model called "Default", which both providers have.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Opus"
    )
  )
  // Level and speed ride on the same trigger now.
  expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent("Max")
  await user.click(screen.getByRole("button", { name: "Model" }))
  expect(
    screen.getByRole("menuitemcheckbox", { name: "Fast" })
  ).toHaveAttribute("aria-checked", "true")
  await user.keyboard("{Escape}")

  await pickFromMoreModels(user, "Codex", "Default")

  // Codex's own holder takes over: its level, and its speed. Claude's `max`
  // could not survive anyway — Codex does not advertise it.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
      "Codex · Default"
    )
  )
  expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
    "Minimal"
  )
  await user.click(screen.getByRole("button", { name: "Model" }))
  expect(
    screen.getByRole("menuitemcheckbox", { name: "Fast" })
  ).toHaveAttribute("aria-checked", "false")
  await user.keyboard("{Escape}")

  await typeAsk()
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

/**
 * The panel used to be one line of text — "Loading your workspace…" — until the
 * metadata read AND the whole paged history had come back. Opening a brand new
 * chat therefore spent two round trips showing nothing, and saying the wrong
 * thing about what was missing.
 *
 * The shell is drawable from the resource the caller already fetched, so it is
 * drawn. What must NOT happen is a message leaving before the read that proves
 * this chat is still readable, and the wait for it is shown rather than hidden
 * behind a button that looks ready.
 */
it("draws the conversation before its history arrives, and posts nothing until the metadata read lands", async () => {
  let release: (() => void) | undefined
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () =>
      new Promise((resolve) => {
        release = () => resolve(json(resource))
      }),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: () => json(message, 201),
  })
  mount()

  // The shell is here: its name, and somewhere to type.
  const composer = await screen.findByRole("textbox", { name: "Message" })
  expect(screen.getByTitle("Design chat")).toBeInTheDocument()
  expect(screen.getByText("Loading this conversation…")).toBeInTheDocument()
  // The old behaviour, which must not come back.
  expect(screen.queryByText("Loading your workspace…")).toBeNull()

  const posts = () =>
    mock.fetcher.mock.calls.filter(
      ([target, init]) =>
        target === path && (init as RequestInit)?.method === "POST"
    )

  // Writable immediately, and visibly not sendable yet.
  const user = userEvent.setup()
  await typeInComposer(composer, message.body)
  expect(composerText(composer)).toBe(message.body)
  expect(screen.getByRole("button", { name: "Send" })).toBeDisabled()
  expect(posts()).toEqual([])

  await act(async () => {
    release?.()
    await Promise.resolve()
  })
  await ready()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(posts()).toHaveLength(1))
})

/*
 * ONE control in the composer's corner, whichever state the run is in.
 *
 * Send was already held for the whole run (`sendable` is false while the
 * agent is busy), so the old pair put a live Stop beside a dead Send and
 * shifted the row under the pointer at the moment someone reached for it.
 */
it("shows one composer control that turns into Stop while the invoked run streams", async () => {
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
  await ready()
  await typeAsk()
  // Idle: Send, and nothing beside it.
  expect(screen.queryByRole("button", { name: "Stop generation" })).toBeNull()
  const send = screen.getByRole("button", { name: "Send" })
  const corner = send.parentElement!
  await user.click(send)
  await screen.findByText("partial")
  // Running: Stop, in the same corner, and Send is GONE rather than disabled.
  const stop = await screen.findByRole("button", { name: "Stop generation" })
  expect(screen.queryByRole("button", { name: "Send" })).toBeNull()
  expect(stop.parentElement).toBe(corner)
  expect(stop).toBeEnabled()
  // Inside the form, but it must never submit it.
  expect(stop).toHaveAttribute("type", "button")
  await user.click(stop)
  await waitFor(() => expect(canceled).toEqual({ runId: RUN_ID }))
  act(() => {
    controller.enqueue(
      new TextEncoder().encode(
        'data: {"type":"finished","status":"canceled","errorCode":null}\n\n'
      )
    )
    controller.close()
  })
  // And back to one Send once the run is over.
  expect(await screen.findByRole("button", { name: "Send" })).toHaveAttribute(
    "type",
    "submit"
  )
  expect(screen.queryByRole("button", { name: "Stop generation" })).toBeNull()
}, 10000)

/*
 * Naming a chat from what was said first.
 *
 * The placeholder is the whole guard: a chat someone named is never renamed,
 * and a chat with a transcript is never renamed either. What changed is that
 * being unnamed and EMPTY is enough — a "New chat" made last week and opened
 * from the sidebar today used to stay "New chat" forever, because only the
 * tab that had just been created was armed.
 */
const unnamed: NativeResource = { ...resource, title: "New chat" }
function nameMock(
  start: NativeResource,
  history: (typeof message)[],
  record: { patched: Record<string, unknown>[]; posted: number }
) {
  let latest = start
  return mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(latest),
    [`GET ${path}`]: () => json({ items: history, nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: (init) => {
      record.posted += 1
      const sent = JSON.parse(init.body as string)
      return json({ ...message, sequence: "9", body: sent.body as string }, 201)
    },
    [`PATCH /api/neev/resources/${resource.id}`]: (init) => {
      const body = JSON.parse(init.body as string)
      record.patched.push(body)
      latest = { ...latest, title: body.title as string, authRevision: "2" }
      return json(latest)
    },
  })
}
async function sendWords(words: string) {
  const user = userEvent.setup()
  await typeInComposer(screen.getByRole("textbox", { name: "Message" }), words)
  await user.click(screen.getByRole("button", { name: "Send" }))
}

it("names an unnamed, never-used chat from its first message though the tab is not fresh", async () => {
  const record = { patched: [] as Record<string, unknown>[], posted: 0 }
  nameMock(unnamed, [], record)
  mount(false)
  await ready()
  await sendWords("# Ship the parser rewrite")
  await waitFor(() => expect(record.patched).toHaveLength(1))
  // The heading marker is not part of the name, and the audience and the
  // revision travel unchanged: naming must never widen who can read this.
  expect(record.patched[0]).toEqual({
    nodeId: resource.nodeId,
    title: "Ship the parser rewrite",
    visibility: "private",
    archived: false,
    expectedAuthRevision: "1",
  })
})

it("never renames a chat that already holds messages", async () => {
  const record = { patched: [] as Record<string, unknown>[], posted: 0 }
  nameMock(unnamed, [message], record)
  mount(false)
  await ready()
  await screen.findByText(message.body)
  await sendWords("Second thought")
  // The message landed; the name did not move with it.
  await waitFor(() => expect(record.posted).toBe(1))
  expect(record.patched).toEqual([])
  expect(screen.getByRole("region", { name: "New chat" })).toBeVisible()
})

it("never renames a chat somebody named by hand", async () => {
  const record = { patched: [] as Record<string, unknown>[], posted: 0 }
  nameMock(resource, [], record)
  mount(true)
  await ready()
  await sendWords("Ship the parser rewrite")
  await waitFor(() => expect(record.posted).toBe(1))
  expect(record.patched).toEqual([])
  expect(screen.getByRole("region", { name: "Design chat" })).toBeVisible()
})

it("keeps an agent reply formatted when it carries a generated download", async () => {
  const key = "99999999-9999-4999-8999-999999999999"
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({
        items: [
          {
            ...replyMessage,
            body: "**Saved report**",
            attachments: [
              {
                key,
                fileName: "report.html",
                contentType: "text/plain",
                byteLength: 123,
              },
            ],
          },
        ],
        nextAfter: null,
      }),
  })
  mount()
  expect((await screen.findByText("Saved report")).tagName).toBe("STRONG")
  expect(screen.getByRole("link", { name: /report.html/ })).toHaveAttribute(
    "href",
    `/api/neev/attachments/${key}`
  )
})

const connectionRow = (provider: string, state: string) => ({
  provider,
  state,
  accountLabel: state === "connected" ? "sagar@example.test" : null,
  checkedAt: state === "connected" ? 1700000000 : null,
  errorCode: null,
})
it("defaults a new chat to the connected provider", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          connectionRow("claude", "not_signed_in"),
          connectionRow("codex", "connected"),
        ],
      }),
  })
  mount(true)
  await ready()
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Model" })).toHaveAttribute(
      "title",
      expect.stringContaining("Codex")
    )
  )
  const user = userEvent.setup()
  await pickFromMoreModels(user, "Claude Code", "Haiku")
  expect(await screen.findByRole("dialog")).toBeVisible()
  expect(
    within(screen.getByRole("dialog")).getByRole("heading", {
      name: "Connect · Claude",
    })
  ).toBeVisible()
})
it("opens connection dialog without invoking or losing the draft when disconnected", async () => {
  const invoke = vi.fn(() => json({ code: "unavailable" }, 503))
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [connectionRow("claude", "not_signed_in")],
      }),
    [AGENT_PATH]: invoke,
  })
  mount(true)
  await ready()
  await typeAsk()
  fireEvent.click(screen.getByRole("button", { name: "Send" }))
  expect(await screen.findByRole("dialog")).toBeVisible()
  expect(invoke).not.toHaveBeenCalled()
  expect(
    composerText(screen.getByRole("textbox", { name: "Message", hidden: true }))
  ).toBe("help")
})

/*
 * Shared composer follow-up steering tests.
 *
 * In team chat agent mode, the shared composer supports mid-run follow-ups
 * (steering) when the run reports supportsSteering: true. These tests verify
 * the follow-up send and retry behavior.
 */
it("sends a follow-up to the running invocation and clears composer on acceptance", async () => {
  const stream = liveStream()
  const steers: { text: string; requestId: string }[] = []
  let sseConnected = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () =>
      json({
        ...resource,
        contentRevision: steers.length ? "2" : resource.contentRevision,
      }),
    [`GET ${path}`]: () =>
      json({
        items: steers.length
          ? [
              askMessage,
              {
                ...askMessage,
                id: "88888888-8888-4888-8888-888888888888",
                sequence: "2",
                body: "Focus on edge cases",
              },
            ]
          : [],
        nextAfter: null,
      }),
    [`GET ${path}?after=1`]: () =>
      json({
        items: steers.length
          ? [
              {
                ...askMessage,
                id: "88888888-8888-4888-8888-888888888888",
                sequence: "2",
                body: "Focus on edge cases",
              },
            ]
          : [],
        nextAfter: null,
      }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () => {
      sseConnected = true
      return stream.response
    },
    [`POST /api/neev/agent/runs/${RUN_ID}/steer`]: (init) => {
      const body = JSON.parse(init.body as string)
      steers.push({ text: body.text, requestId: body.requestId })
      return json({ accepted: true, runId: RUN_ID, requestId: body.requestId })
    },
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  // Wait for the SSE connection to be established
  await waitFor(() => expect(sseConnected).toBe(true))
  // Started with steering support
  act(() => {
    stream.push({
      type: "started",
      runId: RUN_ID,
      turnId: runningTurn.id,
      supportsSteering: true,
    })
    stream.push({ type: "chunk", text: "Working on it..." })
  })
  expect(await screen.findByText("Working on it...")).toBeVisible()
  // The follow-up placeholder should appear (visually rendered as text, not an attribute)
  expect(
    await screen.findByText(messages.NativeContent.agentFollowUp)
  ).toBeInTheDocument()
  const followUpComposer = screen.getByRole("textbox", { name: "Message" })
  // Type and send a follow-up
  await typeInComposer(followUpComposer, "Focus on edge cases")
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(steers).toHaveLength(1))
  expect(steers[0].text).toBe("Focus on edge cases")
  // Simulate the SSE input event acknowledging the follow-up
  act(() => {
    stream.push({
      type: "input",
      runId: RUN_ID,
      requestId: steers[0].requestId,
      text: "Focus on edge cases",
      createdAt: Date.now(),
      responseOffset: 17,
    })
  })
  // Composer should be cleared after acceptance
  await waitFor(() => expect(composerText(followUpComposer)).toBe(""))
  await waitFor(() =>
    expect(screen.getAllByText("Focus on edge cases")).toHaveLength(1)
  )
  act(() => stream.close())
})

/*
 * The run is what made the composer busy, and a run that takes follow-ups is
 * the one busy state Enter must still submit in. Held busy, Enter fell through
 * to the editor and the follow-up became a second line of the draft. The
 * corner keeps ONE control: Stop until there is something to send, then Send
 * in the same place, then Stop again once it is taken.
 */
it("steers the run on Enter and keeps one corner control through a follow-up", async () => {
  const stream = liveStream()
  const steers: { text: string; requestId: string }[] = []
  let canceled = false
  let sseConnected = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () => {
      sseConnected = true
      return stream.response
    },
    [`POST /api/neev/agent/runs/${RUN_ID}/steer`]: (init) => {
      const body = JSON.parse(init.body as string)
      steers.push({ text: body.text, requestId: body.requestId })
      return json({ accepted: true, runId: RUN_ID, requestId: body.requestId })
    },
    "POST /api/neev/agent/runs/cancel": () => {
      canceled = true
      return json({ canceled: true, status: "running" })
    },
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await typeAsk()
  const corner = screen.getByRole("button", { name: "Send" }).parentElement!
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(sseConnected).toBe(true))
  act(() => {
    stream.push({
      type: "started",
      runId: RUN_ID,
      turnId: runningTurn.id,
      supportsSteering: true,
    })
    stream.push({ type: "chunk", text: "Working on it..." })
  })
  expect(await screen.findByText("Working on it...")).toBeVisible()
  // Nothing typed: Stop alone, where Send was.
  expect(
    screen.getByRole("button", { name: "Stop generation" })
  ).toHaveProperty("parentElement", corner)
  expect(screen.queryByRole("button", { name: "Send" })).toBeNull()

  const box = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(box, "Focus on edge cases")
  // Something to send: the same corner is Send now, and there is no Stop.
  expect(screen.getByRole("button", { name: "Send" })).toHaveProperty(
    "parentElement",
    corner
  )
  expect(screen.queryByRole("button", { name: "Stop generation" })).toBeNull()

  fireEvent.keyDown(box, { key: "Enter" })
  await waitFor(() => expect(steers).toHaveLength(1))
  expect(steers[0].text).toBe("Focus on edge cases")
  act(() => {
    stream.push({
      type: "input",
      runId: RUN_ID,
      requestId: steers[0].requestId,
      text: "Focus on edge cases",
      createdAt: Date.now(),
      responseOffset: 17,
    })
  })
  await waitFor(() => expect(composerText(box)).toBe(""))
  // Taken: back to Stop, and it still cancels.
  await user.click(
    await screen.findByRole("button", { name: "Stop generation" })
  )
  await waitFor(() => expect(canceled).toBe(true))
  act(() => stream.close())
})

/*
 * Stop never goes out of reach in team chat either: Escape in the composer
 * stops the run while the corner shows Send, and a follow-up whose request
 * never answers puts an enabled Stop back in the corner.
 */
it("keeps Stop reachable in team chat while a follow-up is typed or stuck", async () => {
  const stream = liveStream()
  let steered = 0
  let canceled = 0
  let sseConnected = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () => {
      sseConnected = true
      return stream.response
    },
    [`POST /api/neev/agent/runs/${RUN_ID}/steer`]: () => {
      steered += 1
      return new Promise<Response>(() => {})
    },
    "POST /api/neev/agent/runs/cancel": () => {
      canceled += 1
      return json({ canceled: true, status: "running" })
    },
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(sseConnected).toBe(true))
  act(() => {
    stream.push({
      type: "started",
      runId: RUN_ID,
      turnId: runningTurn.id,
      supportsSteering: true,
    })
    stream.push({ type: "chunk", text: "Working on it..." })
  })
  expect(await screen.findByText("Working on it...")).toBeVisible()
  const box = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(box, "Focus on edge cases")
  const send = screen.getByRole("button", { name: "Send" })
  expect(send).toHaveAttribute("aria-keyshortcuts", "Escape")
  expect(send).toHaveAttribute("title", "Send · Esc stops generation")
  fireEvent.keyDown(box, { key: "Escape" })
  await waitFor(() => expect(canceled).toBe(1))

  fireEvent.keyDown(box, { key: "Enter" })
  await waitFor(() => expect(steered).toBe(1))
  const stop = await screen.findByRole("button", { name: "Stop generation" })
  expect(stop).toBeEnabled()
  expect(composerText(box)).toBe("Focus on edge cases")
  await user.click(stop)
  await waitFor(() => expect(canceled).toBe(2))
  act(() => stream.close())
})

it("retries a follow-up with the same requestId after network failure", async () => {
  const stream = liveStream()
  let steerCalls = 0
  const receivedIds: string[] = []
  let sseConnected = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      json({ message: askMessage, turn: runningTurn, runId: RUN_ID }, 201),
    [RUN_EVENTS]: () => {
      sseConnected = true
      return stream.response
    },
    [`POST /api/neev/agent/runs/${RUN_ID}/steer`]: (init) => {
      steerCalls++
      const body = JSON.parse(init.body as string)
      receivedIds.push(body.requestId)
      // First call fails with network error
      if (steerCalls === 1) {
        return json({ code: "unavailable" }, 503)
      }
      // Second call succeeds
      return json({ accepted: true, runId: RUN_ID, requestId: body.requestId })
    },
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await typeAsk()
  await user.click(screen.getByRole("button", { name: "Send" }))
  // Wait for the SSE connection to be established
  await waitFor(() => expect(sseConnected).toBe(true))
  act(() => {
    stream.push({
      type: "started",
      runId: RUN_ID,
      turnId: runningTurn.id,
      supportsSteering: true,
    })
    stream.push({ type: "chunk", text: "Working..." })
  })
  expect(await screen.findByText("Working...")).toBeVisible()
  // The follow-up placeholder should appear (visually rendered as text, not an attribute)
  expect(
    await screen.findByText(messages.NativeContent.agentFollowUp)
  ).toBeInTheDocument()
  const followUpComposer = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(followUpComposer, "Check imports")
  // First attempt fails
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(steerCalls).toBe(1))
  // The rejection must be handled — error shown, steeringInFlight cleared —
  // before retrying; a press any earlier is swallowed by the in-flight guard.
  expect(
    await screen.findByText(
      "This service is temporarily unavailable. Please try again."
    )
  ).toBeVisible()
  // Text should be preserved after failure
  expect(composerText(followUpComposer)).toBe("Check imports")
  const sendButton = screen.getByRole("button", { name: "Send" })
  await waitFor(() => expect(sendButton).not.toBeDisabled())
  // Retry with Send button click
  await user.click(sendButton)
  await waitFor(() => expect(steerCalls).toBe(2))
  // The same requestId should be used for the retry
  expect(receivedIds[0]).toBe(receivedIds[1])
  act(() => stream.close())
})

// ── Provider failures: the provider's own words, and the action that helps ──
//
// The live incident: a Codex team invocation failed inside the provider, the
// chat said "This service is temporarily unavailable", and after a reload the
// failure vanished — only the ask remained, with nothing saying it had failed
// or offering to run it again.

const RETRY_RUN_ID = "fedcba9876543210fedcba9876543210"
const failedTurn = (
  category: string,
  detail: string,
  errorCode = "provider"
) => ({
  ...runningTurn,
  status: "failed",
  errorCode,
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
      detail,
      category,
    },
  ],
})

it("keeps a failed invocation's real reason and Retry visible after a reload", async () => {
  let retried: unknown = null
  let invoked = 0
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [askMessage], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [TEAM_SESSION("claude")]: () => json(teamSession),
    [TURNS]: () =>
      json({
        items: [
          failedTurn(
            "network",
            "stream disconnected before completion: error sending request"
          ),
        ],
        nextAfter: null,
      }),
    [`${TURNS}?after=1`]: () => json({ items: [], nextAfter: null }),
    [AGENT_PATH]: () => {
      invoked++
      return json({ code: "unexpected" }, 500)
    },
    [`POST /api/neev/agent/runs/${RUN_ID}/retry`]: (init) => {
      retried = JSON.parse(init.body as string)
      return json(
        {
          turn: {
            ...runningTurn,
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
        '{"type":"chunk","text":"done now"}',
        '{"type":"finished","status":"completed","errorCode":null}'
      ),
  })
  const user = userEvent.setup()
  // A REOPEN, not a fresh send: nothing in memory knows this run failed.
  mount()
  await ready()
  const card = await screen.findByRole("alert")
  expect(card).toHaveTextContent(messages.NativeContent.runError_network)
  // The provider's own words, not a generic sentence.
  expect(card).toHaveTextContent(
    "stream disconnected before completion: error sending request"
  )
  expect(
    screen.queryByText(
      "This service is temporarily unavailable. Please try again."
    )
  ).toBeNull()
  await user.click(within(card).getByRole("button", { name: "Retry" }))
  await waitFor(() =>
    expect(retried).toEqual({
      requestId: expect.stringMatching(/^[0-9a-f-]{36}$/),
    })
  )
  // The retry re-ran the stored ask; it did not post the ask again.
  expect(invoked).toBe(0)
  const thread = within(await screen.findByRole("list", { name: "Messages" }))
  expect(thread.getAllByText("help")).toHaveLength(1)
  expect(mock.unknown).toEqual([])
})

it("offers Reconnect, not Retry, when the provider sign-in expired", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [askMessage], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    // Connected BEFORE the failure: the credential that failed is this one.
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          connectionRow("claude", "connected"),
          connectionRow("codex", "connected"),
        ],
      }),
    [TEAM_SESSION("claude")]: () => json(teamSession),
    [TURNS]: () =>
      json({
        items: [
          failedTurn(
            "auth_expired",
            "API Error: 401 OAuth token has expired.",
            "unauthorized"
          ),
        ],
        nextAfter: null,
      }),
  })
  mount()
  await ready()
  const card = await screen.findByRole("alert")
  expect(card).toHaveTextContent(messages.NativeContent.runError_auth_expired)
  expect(card).toHaveTextContent("API Error: 401 OAuth token has expired.")
  expect(within(card).getByRole("button", { name: "Reconnect" })).toBeVisible()
  expect(within(card).queryByRole("button", { name: "Retry" })).toBeNull()
})

it("copies a settled agent reply as its source, and not a person's message", async () => {
  const body = "Try this:\n\n```sh\nmake test\n```"
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({
        items: [askMessage, { ...replyMessage, body }],
        nextAfter: null,
      }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
  })
  const user = userEvent.setup()
  // After setup: user-event installs its own clipboard stub.
  const writeText = vi.fn().mockResolvedValue(undefined)
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  })
  mount()
  await ready()
  await screen.findByText("make test")
  // One for the reply, one for its code; none for the human ask.
  expect(screen.getAllByRole("button", { name: "Copy" })).toHaveLength(1)
  expect(screen.getAllByRole("button", { name: "Copy code" })).toHaveLength(1)
  await user.click(screen.getByRole("button", { name: "Copy" }))
  await waitFor(() => expect(writeText).toHaveBeenCalledWith(body))
})

it("shows a sent message at once, clears the box, and settles it exactly once", async () => {
  let answer!: (response: Response) => void
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: () =>
      new Promise<Response>((resolve) => {
        answer = resolve
      }),
  })
  const user = userEvent.setup()
  mount()
  await ready()
  const box = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(box, message.body)
  await user.click(screen.getByRole("button", { name: "Send" }))
  // Before the server answers: the box is empty and the words are in the
  // thread, marked as still going.
  await waitFor(() => expect(composerText(box)).toBe(""))
  const thread = screen.getByRole("list", { name: "Messages" })
  const pending = within(thread).getByText(message.body).closest("li")!
  expect(pending).toHaveAttribute("aria-busy", "true")
  await waitFor(() => expect(answer).toBeDefined())
  await act(async () => answer(json(message, 201)))
  await waitFor(() =>
    expect(
      within(thread).getByText(message.body).closest("li")
    ).not.toHaveAttribute("aria-busy")
  )
  expect(within(thread).getAllByText(message.body)).toHaveLength(1)
})

it("puts a failed message back without losing what was typed after it", async () => {
  let answer!: (response: Response) => void
  const bodies: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return bodies.length === 1
        ? new Promise<Response>((resolve) => {
            answer = resolve
          })
        : json(message, 201)
    },
  })
  const user = userEvent.setup()
  mount()
  await ready()
  const box = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(box, message.body)
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(composerText(box)).toBe(""))
  await typeInComposer(box, "next thing")
  await waitFor(() => expect(answer).toBeDefined())
  await act(async () => answer(json({ code: "unavailable" }, 503)))
  expect(await screen.findByRole("alert")).toBeVisible()
  // Both survive: the unsent message first, then what came after it.
  // (The box draws the blank line between them as breaks, not text.)
  await waitFor(() =>
    expect(composerText(box)).toBe(`${message.body}next thing`)
  )
  expect(box.querySelectorAll("br").length).toBeGreaterThan(0)
  const thread = screen.getByRole("list", { name: "Messages" })
  expect(within(thread).queryByText(message.body)).toBeNull()
})

it("shows an agent ask at once, before the run is accepted", async () => {
  let answer!: (response: Response) => void
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${path}?after=1`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [AGENT_PATH]: () =>
      new Promise<Response>((resolve) => {
        answer = resolve
      }),
  })
  const user = userEvent.setup()
  mount(true)
  await ready()
  await user.click(await screen.findByRole("button", { name: "Ask agent" }))
  const box = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(box, "help")
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(composerText(box)).toBe(""))
  const thread = screen.getByRole("list", { name: "Messages" })
  expect(within(thread).getByText("help").closest("li")).toHaveAttribute(
    "aria-busy",
    "true"
  )
  await waitFor(() => expect(answer).toBeDefined())
  await act(async () => answer(json({ code: "unavailable" }, 503)))
  await waitFor(() => expect(composerText(box)).toBe("help"))
  expect(within(thread).queryByText("help")).toBeNull()
})

/** History that grows while a send is in flight, read by the 5 s poll. */
function growingHistory() {
  const items: Record<string, unknown>[] = []
  const seen = { reads: 0, withItems: 0 }
  const read = () => {
    seen.reads += 1
    if (items.length) seen.withItems += 1
    return json({ items: [...items], nextAfter: null })
  }
  return {
    items,
    seen,
    routes: {
      [`GET ${path}`]: read,
      [`GET ${path}?after=1`]: read,
      [`GET ${path}?after=2`]: read,
    },
  }
}

it("is not settled by a same-text message from another tab, and shows both", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const history = growingHistory()
  let answer!: (response: Response) => void
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    ...history.routes,
    [`POST ${path}`]: () =>
      new Promise<Response>((resolve) => {
        answer = resolve
      }),
  })
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
  mount()
  await ready()
  await typeInComposer(
    screen.getByRole("textbox", { name: "Message" }),
    message.body
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  const thread = within(screen.getByRole("list", { name: "Messages" }))
  await waitFor(() => expect(thread.getByText(message.body)).toBeVisible())
  // Another tab of the same person sends the same words, and the poll sees it.
  const other = { ...message, id: "99999999-9999-4999-8999-999999999999" }
  history.items.push(other)
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  // The poll really did read the new message back.
  expect(history.seen.withItems).toBeGreaterThan(0)
  // Still pending: a look-alike is not this request's answer.
  expect(thread.getByText(message.body).closest("li")).toHaveAttribute(
    "aria-busy",
    "true"
  )
  await waitFor(() => expect(answer).toBeDefined())
  await act(async () =>
    answer(
      json(
        {
          ...message,
          id: "88888888-8888-4888-8888-888888888888",
          sequence: "2",
        },
        201
      )
    )
  )
  // Both are real messages, so both show; nothing is pending any more.
  await waitFor(() => expect(thread.getAllByText(message.body)).toHaveLength(2))
  for (const row of thread.getAllByText(message.body))
    expect(row.closest("li")).not.toHaveAttribute("aria-busy")
})

it("does not show a message twice when the stream brings it before the response", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const history = growingHistory()
  let answer!: (response: Response) => void
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    ...history.routes,
    [`POST ${path}`]: () =>
      new Promise<Response>((resolve) => {
        answer = resolve
      }),
  })
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
  mount()
  await ready()
  await typeInComposer(
    screen.getByRole("textbox", { name: "Message" }),
    message.body
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  const thread = within(screen.getByRole("list", { name: "Messages" }))
  // The server already stored it and the poll reads it back first.
  history.items.push(message)
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  // The poll really did read the new message back.
  expect(history.seen.withItems).toBeGreaterThan(0)
  expect(thread.getAllByText(message.body)).toHaveLength(1)
  await waitFor(() => expect(answer).toBeDefined())
  await act(async () => answer(json(message, 201)))
  await waitFor(() =>
    expect(thread.getByText(message.body).closest("li")).not.toHaveAttribute(
      "aria-busy"
    )
  )
  expect(thread.getAllByText(message.body)).toHaveLength(1)
})

// ---- Fork to new tab ----

const FORK_ID = "77777777-7777-4777-8777-777777777777"
const FORK_PATH = `POST /api/neev/chats/${resource.id}/fork`
const forked = (over: Record<string, unknown> = {}) => ({
  chatId: FORK_ID,
  sourceChatId: resource.id,
  throughMessageId: replyMessage.id,
  nodeId: resource.nodeId,
  copiedMessages: 2,
  copiedAttachments: 0,
  archivedAttachmentsNotCopied: 0,
  replayed: false,
  ...over,
})

function mountWithTabs(openFork: OpenFork) {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <ForkTabsContext.Provider value={openFork}>
          <NativeChat
            resource={resource}
            onBack={() => {}}
            onChanged={() => {}}
          />
        </ForkTabsContext.Provider>
      </NativeBoundary>
    </NativeTestRoot>
  )
}

function settledThread(fork: Handler) {
  return {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () =>
      json({ items: [askMessage, replyMessage], nextAfter: null }),
    [`GET ${path}?after=2`]: () => json({ items: [], nextAfter: null }),
    ...noSessions,
    [FORK_PATH]: fork,
  }
}

async function chooseFork(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "More actions" }))
  await user.click(
    await screen.findByRole("menuitem", { name: "Fork to new tab" })
  )
}

it("forks a settled agent reply into a NEW tab and keeps this chat open", async () => {
  const bodies: Record<string, unknown>[] = []
  const openFork = vi.fn<OpenFork>(async () => "opened")
  mockNativeSession(
    undefined,
    settledThread((init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(forked(), 201)
    })
  )
  const user = userEvent.setup()
  mountWithTabs(openFork)
  await ready()
  await screen.findByText(replyMessage.body)
  // Only the agent's reply has actions; the person's ask does not.
  expect(screen.getAllByRole("button", { name: "More actions" })).toHaveLength(
    1
  )
  await chooseFork(user)
  await waitFor(() => expect(openFork).toHaveBeenCalledTimes(1))
  expect(bodies).toEqual([
    { throughMessageId: replyMessage.id, requestId: expect.any(String) },
  ])
  // A new tab for the fork — never this one replaced.
  expect(openFork).toHaveBeenCalledWith({
    kind: "resource",
    resourceId: FORK_ID,
  })
  expect(await screen.findByText("Forked to a new tab.")).toBeVisible()
  expect(screen.getByText(replyMessage.body)).toBeVisible()
  expect(screen.getByText(askMessage.body)).toBeVisible()
})

it("keeps one request id across a refused, a failed and a successful fork", async () => {
  const bodies: Record<string, unknown>[] = []
  const openFork = vi.fn<OpenFork>(async () => "opened")
  mockNativeSession(
    undefined,
    settledThread((init) => {
      bodies.push(JSON.parse(init.body as string))
      if (bodies.length === 1) return json({ code: "fork_too_large" }, 409)
      if (bodies.length === 2) return json({ code: "unavailable" }, 503)
      return json(forked({ archivedAttachmentsNotCopied: 2 }), 201)
    })
  )
  const user = userEvent.setup()
  mountWithTabs(openFork)
  await ready()
  await chooseFork(user)
  expect(
    await screen.findByText("This conversation is too long to fork.")
  ).toBeVisible()
  expect(openFork).not.toHaveBeenCalled()
  await chooseFork(user)
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(await screen.findByRole("alert")).toBeVisible()
  await chooseFork(user)
  expect(
    await screen.findByText(
      "Forked to a new tab. 2 deleted files were not copied."
    )
  ).toBeVisible()
  // The same id every time, so a lost response resolves to one fork.
  expect(new Set(bodies.map((body) => body.requestId)).size).toBe(1)
})

it("replays the same fork when the tab strip was full, and starts fresh after", async () => {
  const bodies: Record<string, unknown>[] = []
  const openFork = vi
    .fn<OpenFork>()
    .mockResolvedValueOnce("tab_limit")
    .mockResolvedValue("opened")
  mockNativeSession(
    undefined,
    settledThread((init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(forked({ replayed: bodies.length === 2 }), 201)
    })
  )
  const user = userEvent.setup()
  mountWithTabs(openFork)
  await ready()
  await chooseFork(user)
  expect(
    await screen.findByText(/there is no room for another tab/)
  ).toBeVisible()
  await chooseFork(user)
  expect(await screen.findByText("Forked to a new tab.")).toBeVisible()
  expect(bodies[1].requestId).toBe(bodies[0].requestId)
  // Opened: a later fork of the same reply is a new fork.
  await chooseFork(user)
  await waitFor(() => expect(bodies).toHaveLength(3))
  expect(bodies[2].requestId).not.toBe(bodies[0].requestId)
})

it("sends one fork however often it is chosen while one is on its way", async () => {
  let answer!: (response: Response) => void
  let posts = 0
  mockNativeSession(
    undefined,
    settledThread(() => {
      posts += 1
      return new Promise<Response>((resolve) => {
        answer = resolve
      })
    })
  )
  const user = userEvent.setup()
  mountWithTabs(vi.fn<OpenFork>(async () => "opened"))
  await ready()
  await chooseFork(user)
  expect(await screen.findByText("Forking…")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "More actions" }))
  expect(
    await screen.findByRole("menuitem", { name: "Fork to new tab" })
  ).toHaveAttribute("aria-disabled", "true")
  await user.keyboard("{Escape}")
  expect(posts).toBe(1)
  await act(async () => answer(json(forked(), 201)))
  expect(await screen.findByText("Forked to a new tab.")).toBeVisible()
})

it("counts a text-only message on its way as unsent work until it lands", async () => {
  let answer!: (response: Response) => void
  const dirty = vi.fn()
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}`]: () =>
      new Promise<Response>((resolve) => {
        answer = resolve
      }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeChat
          resource={resource}
          onBack={() => {}}
          onChanged={() => {}}
          onDirtyChange={dirty}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  await ready()
  const box = screen.getByRole("textbox", { name: "Message" })
  await typeInComposer(box, message.body)
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(composerText(box)).toBe(""))
  // The box is empty, but leaving now would abort a send that has not landed.
  await waitFor(() => expect(dirty).toHaveBeenLastCalledWith(true))
  await waitFor(() => expect(answer).toBeDefined())
  await act(async () => answer(json(message, 201)))
  await waitFor(() => expect(dirty).toHaveBeenLastCalledWith(false))
})
