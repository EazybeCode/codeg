import { render, screen, waitFor, act } from "@testing-library/react"
import { StrictMode } from "react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeContent } from "./native-content"
import { NativeBoundary } from "./native-boundary"
import { NativeWorkspace } from "./native-workspace"
import { useNativeAuth } from "./native-auth-provider"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"
vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => false }))
const push = vi.hoisted(() => vi.fn())
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push }),
}))
const node = {
  id: 4,
  orgId: 3,
  kind: "folder" as const,
  parentId: 2,
  name: "Research",
  revision: "1",
}
const chat: NativeResource = {
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
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  push.mockClear()
})
function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeContent node={node} onRequestNavigationGuard={() => {}} />
      </NativeBoundary>
    </NativeTestRoot>
  )
}
it("loads native content after React Strict Mode replays its mount effects", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
  })
  render(
    <StrictMode>
      <NativeTestRoot>
        <NativeBoundary>
          <NativeContent node={node} onRequestNavigationGuard={() => {}} />
        </NativeBoundary>
      </NativeTestRoot>
    </StrictMode>
  )
  expect(
    await screen.findByRole("button", { name: /Design chat/ })
  ).toBeVisible()
  expect(mock.unknown).toEqual([])
})
it("creates private content only inside the selected node and confirms sharing its entire history", async () => {
  let latest = chat
  const bodies: unknown[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "POST /api/neev/nodes/4/chats": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(chat, 201)
    },
    [`GET /api/neev/resources/${chat.id}`]: () => json(latest),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${chat.id}`]: (init) => {
      const body = JSON.parse(init.body as string)
      bodies.push(body)
      latest = { ...latest, ...body, authRevision: "2" }
      delete (latest as unknown as Record<string, unknown>).expectedAuthRevision
      return json(latest)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: "New chat" }))
  expect(screen.getByRole("dialog")).toHaveTextContent("Research")
  await user.type(screen.getByRole("textbox", { name: "Title" }), chat.title)
  await user.click(screen.getByRole("button", { name: "Create" }))
  await screen.findByRole("textbox", { name: "Message" })
  expect(bodies[0]).toEqual({
    title: "Design chat",
    requestId: expect.any(String),
  })
  await user.click(
    screen.getByRole("button", { name: "Share with organization" })
  )
  expect(screen.getByRole("dialog")).toHaveTextContent("Everyone in Neev demo")
  expect(screen.getByRole("dialog")).toHaveTextContent("full history")
  expect(bodies).toHaveLength(1)
  await user.click(screen.getByRole("button", { name: "Share full history" }))
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(bodies[1]).toEqual({
    expectedAuthRevision: "1",
    title: "Design chat",
    nodeId: 4,
    visibility: "organization",
    archived: false,
  })
  expect(mock.unknown).toEqual([])
})

it("guards folder, context-menu, Settings and reload navigation but lets logout erase a dirty draft", async () => {
  const nodes = [
    { ...node, id: 2, kind: "project", parentId: null, name: "Launch" },
    node,
    { ...node, id: 5, kind: "project", parentId: null, name: "Other" },
  ]
  const doc = {
    ...chat,
    kind: "document",
    title: "Draft document",
    contentRevision: "0",
  }
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes": () => json({ nodes, nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [doc], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(doc),
    [`GET /api/neev/documents/${chat.id}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
    "POST /api/neev/logout": () => json({ ok: true }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Launch" }))
  await user.click(screen.getByRole("button", { name: "Research" }))
  await user.click(await screen.findByRole("tab", { name: "Documents" }))
  await user.click(
    await screen.findByRole("button", { name: /Draft document/ })
  )
  await user.type(
    await screen.findByRole("textbox", { name: "Document content" }),
    "secret draft"
  )
  const unload = new Event("beforeunload", { cancelable: true })
  window.dispatchEvent(unload)
  expect(unload.defaultPrevented).toBe(true)
  await user.click(screen.getByRole("button", { name: "Other" }))
  expect(screen.getByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  expect(screen.getByRole("textbox", { name: "Document content" })).toHaveValue(
    "secret draft"
  )
  await user.click(screen.getByRole("link", { name: "Settings" }))
  expect(screen.getByRole("dialog")).toBeVisible()
  expect(push).not.toHaveBeenCalled()
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  await user.click(screen.getByRole("button", { name: "Actions for Other" }))
  await user.click(await screen.findByRole("menuitem", { name: "New chat" }))
  expect(screen.getByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  await user.click(screen.getByRole("button", { name: "Sign out" }))
  await waitFor(() =>
    expect(screen.queryByDisplayValue("secret draft")).toBeNull()
  )
  expect(mock.unknown).toEqual([])
})

it("immediately applies creator archive/reopen actions and moves with the current authority revision", async () => {
  let latest = chat
  const bodies: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: latest.nodeId === 4 ? [latest] : [], nextAfter: null }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [node, { ...node, id: 5, name: "Destination" }],
        nextAfter: null,
      }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(latest),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${chat.id}`]: (init) => {
      const { expectedAuthRevision, ...patch } = JSON.parse(init.body as string)
      bodies.push({ expectedAuthRevision, ...patch })
      latest = {
        ...latest,
        ...patch,
        authRevision: String(Number(expectedAuthRevision) + 1),
      }
      return json(latest)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await screen.findByRole("textbox", { name: "Message" })
  await user.click(screen.getByRole("button", { name: "Archive" }))
  expect(await screen.findByRole("button", { name: "Reopen" })).toBeEnabled()
  expect(screen.getByRole("textbox", { name: "Message" })).toBeDisabled()
  await user.click(screen.getByRole("button", { name: "Reopen" }))
  await waitFor(() =>
    expect(screen.getByRole("textbox", { name: "Message" })).toBeEnabled()
  )
  await user.click(screen.getByRole("button", { name: "Move" }))
  await screen.findByRole("option", { name: "Destination · #5" })
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Destination folder" }),
    "5"
  )
  await user.click(screen.getByRole("button", { name: "Save" }))
  await waitFor(() =>
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
  )
  expect(
    bodies.map((body) => [
      body.expectedAuthRevision,
      body.archived,
      body.nodeId,
    ])
  ).toEqual([
    ["1", true, 4],
    ["2", false, 4],
    ["3", false, 5],
  ])
  expect(mock.unknown).toEqual([])
})

it("never offers independent sharing for a source-bound document", async () => {
  const doc = {
    ...chat,
    kind: "document",
    originChatId: "33333333-3333-4333-8333-333333333333",
  }
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [doc], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(doc),
    [`GET /api/neev/documents/${chat.id}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("tab", { name: "Documents" }))
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await screen.findByRole("textbox", { name: "Document content" })
  expect(screen.getByText(/Audience follows source chat/)).toBeVisible()
  expect(
    screen.queryByRole("button", { name: "Share with organization" })
  ).toBeNull()
  expect(screen.queryByRole("button", { name: "Make private" })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("discards the actual draft before opening a creator action on the same resource", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "discard me"
  )
  await user.click(
    screen.getByRole("button", { name: "Share with organization" })
  )
  await user.click(screen.getByRole("button", { name: "Discard changes" }))
  expect(screen.queryByDisplayValue("discard me")).toBeNull()
  expect(screen.getByRole("dialog")).toHaveTextContent("Share full history?")
  await user.click(screen.getByRole("button", { name: "Cancel" }))
  expect(await screen.findByRole("textbox", { name: "Message" })).toHaveValue(
    ""
  )
  expect(mock.unknown).toEqual([])
})

it("restores keyboard focus to the destination tab after confirming a dirty tab switch", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "dirty"
  )
  screen.getByRole("tab", { name: "Chats" }).focus()
  await user.keyboard("{ArrowRight}")
  await user.click(screen.getByRole("button", { name: "Discard changes" }))
  await waitFor(() =>
    expect(screen.getByRole("tab", { name: "Documents" })).toHaveAttribute(
      "aria-selected",
      "true"
    )
  )
  expect(screen.getByRole("tab", { name: "Documents" })).toHaveFocus()
  expect(mock.unknown).toEqual([])
})

it("auth invalidation clears a private draft even while its Settings guard is open", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let expired = false
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/session": () =>
      json(
        expired
          ? {
              ...orgSession,
              organization: {
                id: 9,
                name: "New organization",
                avatarUrl: null,
              },
            }
          : orgSession
      ),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () =>
      expired ? json({ code: "unauthorized" }, 401) : json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "secret pending draft"
  )
  await user.click(
    screen.getByRole("link", { name: "Personal AI connections" })
  )
  expect(screen.getByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  expired = true
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  expect(screen.queryByRole("dialog")).toBeNull()
  expect(screen.queryByDisplayValue("secret pending draft")).toBeNull()
  expect(push).not.toHaveBeenCalled()
  expect(mock.unknown).toEqual([])
})

function SwitchScope() {
  const auth = useNativeAuth()
  return <button onClick={() => void auth.refresh()}>Switch scope</button>
}
it("rejects a late private list response after auth generation changes even when transport ignores abort", async () => {
  let release!: (response: Response) => void
  let switched = false
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/session": () =>
      json(
        switched
          ? {
              ...orgSession,
              organization: {
                id: 9,
                name: "New organization",
                avatarUrl: null,
              },
            }
          : orgSession
      ),
    "GET /api/neev/nodes/4/chats": () =>
      new Promise((resolve) => {
        release = resolve
      }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <SwitchScope />
      <NativeBoundary>
        <NativeContent node={node} onRequestNavigationGuard={() => {}} />
      </NativeBoundary>
    </NativeTestRoot>
  )
  await screen.findByRole("tab", { name: "Chats" })
  await waitFor(() => expect(release).toBeDefined())
  switched = true
  await user.click(screen.getByRole("button", { name: "Switch scope" }))
  await act(async () => {
    release(
      json({
        items: [{ ...chat, title: "other organization's secret" }],
        nextAfter: null,
      })
    )
  })
  expect(screen.queryByText("other organization's secret")).toBeNull()
  expect(mock.unknown).toEqual([])
})
