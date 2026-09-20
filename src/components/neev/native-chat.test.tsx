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
  expect(screen.getByText("Organization")).toBeVisible()
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
  // This panel's reply is one-shot and never written to the team thread, so it
  // must not imply a shared transcript the server does not store.
  expect(
    screen.getByText(
      "Only you see this reply. It is not posted to the conversation."
    )
  ).toBeVisible()
})

it("enables Ask agent and renders a streamed agent reply when the runtime is available", async () => {
  const sse =
    'data: {"type":"started","run_id":"r1"}\n\n' +
    'data: {"type":"chunk","text":"do "}\n\n' +
    'data: {"type":"chunk","text":"X"}\n\n' +
    'data: {"type":"completed"}\n\n'
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    "POST /api/neev/agent/run": () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(controller) {
            controller.enqueue(new TextEncoder().encode(sse))
            controller.close()
          },
        }),
        { status: 200 }
      ),
  })
  const user = userEvent.setup()
  mount()
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "help"
  )
  // The agent lives in the collapsible right context panel; open it first.
  await user.click(screen.getByRole("button", { name: "Agent panel" }))
  const ask = screen.getByRole("button", { name: "Ask agent" })
  expect(ask).toBeEnabled()
  await user.click(ask)
  expect(await screen.findByText("do X")).toBeVisible()
})

it("surfaces an error when the runtime reports unavailable (503)", async () => {
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    "POST /api/neev/agent/run": () =>
      json({ error: "agent_runtime_disabled" }, 503),
  })
  const user = userEvent.setup()
  mount()
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "help"
  )
  // The agent lives in the collapsible right context panel; open it first.
  await user.click(screen.getByRole("button", { name: "Agent panel" }))
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  expect(await screen.findByRole("alert")).toBeVisible()
})

it("shows Cancel while streaming and cancels the run", async () => {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  let canceled = false
  mockNativeSession(undefined, {
    "GET /api/workos/capabilities": capable,
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}`]: () => json({ items: [], nextAfter: null }),
    "POST /api/neev/agent/run": () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(c) {
            controller = c
            c.enqueue(
              new TextEncoder().encode(
                'data: {"type":"started","run_id":"r1"}\n\ndata: {"type":"chunk","text":"partial"}\n\n'
              )
            )
          },
        }),
        { status: 200 }
      ),
    "POST /api/neev/agent/cancel": (init) => {
      canceled = JSON.parse(init.body as string).run_id === "r1"
      return json({ canceled: true })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "help"
  )
  // The agent lives in the collapsible right context panel; open it first.
  await user.click(screen.getByRole("button", { name: "Agent panel" }))
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  // Wait for streamed output so the run id from the started event is registered.
  await screen.findByText("partial")
  await user.click(screen.getByRole("button", { name: "Cancel" }))
  await waitFor(() => expect(canceled).toBe(true))
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
