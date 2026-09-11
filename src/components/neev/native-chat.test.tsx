import { act, render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeChat } from "./native-chat"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
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
        json(
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
