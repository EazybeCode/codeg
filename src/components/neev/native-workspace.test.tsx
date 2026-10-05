import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { writeTabs } from "@/lib/neev-workspace-tabs"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeBoundary } from "./native-boundary"
import { NativeWorkspace } from "./native-workspace"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
  composerText,
  typeInComposer,
} from "./test-helpers"
const viewport = vi.hoisted(() => ({ mobile: false }))
const router = vi.hoisted(() => ({ replace: vi.fn(), push: vi.fn() }))
vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => viewport.mobile }))
vi.mock("next/navigation", () => ({
  useRouter: () => router,
}))
afterEach(() => {
  vi.unstubAllGlobals()
  vi.clearAllMocks()
  localStorage.clear()
  viewport.mobile = false
})
it("collapses desktop navigation without leaving the workspace", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/nodes": () => json({ nodes: [], nextAfter: null }),
  })
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  const user = userEvent.setup()
  const toggle = await screen.findByRole("button", { name: "Open navigation" })
  expect(toggle).toHaveAttribute("aria-expanded", "true")
  await user.click(toggle)
  expect(screen.queryByRole("complementary")).toBeNull()
  await user.click(toggle)
  expect(screen.getByRole("complementary")).toBeVisible()
})
it("hides and disables an open workspace menu until visibility auth is revalidated", async () => {
  let revalidating = false
  let release!: (response: Response) => void
  const pending = new Promise<Response>((resolve) => {
    release = resolve
  })
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/session": () => (revalidating ? pending : json(orgSession)),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 4,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Private planning",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
  })
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  const user = userEvent.setup()
  await user.click(
    await screen.findByRole("button", { name: "Actions for Private planning" })
  )
  const menu = screen.getByRole("menu")
  expect(menu).toBeVisible()
  revalidating = true
  await act(async () => document.dispatchEvent(new Event("visibilitychange")))
  expect(menu).not.toBeVisible()
  expect(menu.closest("[inert]")).not.toBeNull()
  expect(screen.queryByRole("menuitem")).toBeNull()
  expect(menu).not.toContainElement(document.activeElement as HTMLElement)
  revalidating = false
  await act(async () => release(json(orgSession)))
  expect(await screen.findByRole("menu")).toBeVisible()
  expect(mock.unknown).toEqual([])
})
const CHAT_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
const chatResource = {
  id: CHAT_ID,
  orgId: 3,
  nodeId: 7,
  kind: "chat",
  creatorId: 1,
  title: "New chat",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}

it("shows the person and organization without GitHub or chat controls", async () => {
  const created: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [], nextAfter: null }),
    // A real server lists the row it just made; an empty list here would
    // evict the created chat from the tab's own cache.
    "GET /api/neev/nodes/7/chats": () =>
      json({
        items: created.length ? [{ ...chatResource, nodeId: 7 }] : [],
        nextAfter: null,
      }),
    // One entry, and what it makes is an ordinary chat resource — not a
    // standalone private session that exists only in the client.
    "POST /api/neev/nodes/7/chats": (init) => {
      created.push(JSON.parse(init.body as string))
      return json({ ...chatResource, nodeId: 7, title: "New chat" }, 201)
    },
    [`GET /api/neev/resources/${CHAT_ID}`]: () =>
      json({ ...chatResource, nodeId: 7, title: "New chat" }),
    [`GET /api/neev/chats/${CHAT_ID}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: false,
        connections: [
          {
            provider: "claude",
            state: "disabled",
            accountLabel: null,
            checkedAt: null,
            errorCode: null,
          },
        ],
      }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
  expect(await screen.findByText("Neev demo")).toBeVisible()
  expect(screen.getByText("Sagar")).toBeVisible()
  // Two: the sidebar's and the landing's, which are the same action.
  expect(screen.getAllByRole("button", { name: "New project" })).toHaveLength(2)
  expect(
    screen.queryByRole("button", { name: "New chat" })
  ).not.toBeInTheDocument()
  // Agent chats are reached from inside a container, and only from there. The
  // sessions stay private to their owner; the server holds which project each
  // belongs to. With no runtime bound it says so rather than offering a
  // connection that cannot be exercised.
  expect(
    screen.queryByRole("navigation", { name: "Sections" })
  ).not.toBeInTheDocument()
  await user.click(
    await screen.findByRole("button", { name: "Actions for Space" })
  )
  // One agent entry, and no provider asked for up front — that is chosen
  // inside the conversation.
  expect(screen.queryByRole("menuitem", { name: /Claude|Codex/ })).toBeNull()
  await user.click(
    await screen.findByRole("menuitem", { name: "New agent chat" })
  )
  // It created an actual chat the server owns, rather than a composer for a
  // private session that exists nowhere until the first message.
  await waitFor(() => expect(created).toHaveLength(1))
  expect(screen.getByRole("tab", { name: /New chat/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(await screen.findByRole("textbox", { name: "Message" })).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("clears the organization tree when a foreground content 403 exposes revoked membership", async () => {
  let bootstraps = 0
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/session": () =>
      ++bootstraps === 1
        ? json(orgSession)
        : json({ code: "unauthorized" }, 401),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 4,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Revoked project",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/4/documents": () => json({ code: "forbidden" }, 403),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  // Expanding the container lists its items; a 403 there reveals revoked access.
  await user.click(
    (await screen.findAllByRole("button", { name: "Revoked project" }))[0]
  )
  await waitFor(() => expect(bootstraps).toBe(2))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(screen.queryByText("Neev demo")).toBeNull()
  expect(screen.queryByText("Revoked project")).toBeNull()
  expect(screen.queryByRole("button", { name: "New project" })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("supports keyboard project creation, nested placement and dialog focus restoration", async () => {
  const bodies: unknown[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/3/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes": () => json({ nodes: [], nextAfter: null }),
    "GET /api/neev/nodes/1/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/2/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/3/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/1/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/2/documents": () =>
      json({ items: [], nextAfter: null }),
    "POST /api/neev/nodes": (init) => {
      const body = JSON.parse(init.body as string)
      bodies.push(body)
      return json({ id: bodies.length, orgId: 3, ...body, revision: "1" }, 201)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  const [trigger] = await screen.findAllByRole("button", {
    name: "New project",
  })
  trigger.focus()
  await user.keyboard("{Enter}")
  expect(await screen.findByRole("dialog")).toBeVisible()
  expect(screen.getByLabelText("Name")).toHaveFocus()
  await user.keyboard("{Escape}")
  expect(trigger).toHaveFocus()
  await user.keyboard("{Enter}")
  await user.type(screen.getByLabelText("Name"), "Launch")
  await user.click(screen.getByRole("button", { name: "Save" }))
  await within(screen.getByRole("navigation", { name: "Projects" })).findByRole(
    "button",
    { name: "Launch" }
  )
  const menu = screen.getByRole("button", { name: "Actions for Launch" })
  menu.focus()
  await user.keyboard("{Enter}")
  await user.click(
    await screen.findByRole("menuitem", { name: "New subfolder" })
  )
  expect(await screen.findByRole("dialog")).toHaveTextContent("Inside Launch")
  await user.type(screen.getByLabelText("Name"), "Research")
  await user.click(screen.getByRole("button", { name: "Save" }))
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(menu).toHaveFocus()
  expect(bodies).toEqual([
    { kind: "project", name: "Launch", parentId: null },
    { kind: "folder", name: "Research", parentId: 1 },
  ])
  await user.click(screen.getByRole("button", { name: "Actions for Research" }))
  await user.click(
    await screen.findByRole("menuitem", { name: "New subfolder" })
  )
  await user.type(screen.getByLabelText("Name"), "Interviews")
  await user.click(screen.getByRole("button", { name: "Save" }))
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(bodies[2]).toEqual({ kind: "folder", name: "Interviews", parentId: 2 })
  expect(mock.unknown).toEqual([])
})

it("uses an accessible drawer at the mobile breakpoint with Escape focus return", async () => {
  viewport.mobile = true
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 1,
            orgId: 3,
            parentId: null,
            kind: "project",
            name: "Mobile project",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  const trigger = await screen.findByRole("button", { name: "Open navigation" })
  expect(screen.queryByRole("navigation", { name: "Projects" })).toBeNull()
  trigger.focus()
  await user.keyboard("{Enter}")
  const drawer = await screen.findByRole("dialog")
  expect(
    within(drawer).getByRole("button", { name: "New project" })
  ).toBeEnabled()
  expect(
    within(drawer).getByRole("button", { name: "Mobile project" })
  ).toBeVisible()
  await user.keyboard("{Escape}")
  expect(trigger).toHaveFocus()
  expect(screen.queryByRole("dialog")).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("makes a document the moment it is asked for, with no form to fill in", async () => {
  // No title to think of first: the thing exists, opens in its own tab, and
  // takes its name from what gets written in it.
  const created: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 4,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Menu project",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    // A real server lists the row it just made.
    "GET /api/neev/nodes/4/documents": () =>
      json({
        items: created.map((body) => ({
          ...resource,
          kind: "document",
          nodeId: 4,
          title: body.title,
          contentRevision: "0",
        })),
        nextAfter: null,
      }),
    "POST /api/neev/nodes/4/documents": (init) => {
      const body = JSON.parse(init.body as string)
      created.push(body)
      return json(
        {
          ...resource,
          kind: "document",
          nodeId: 4,
          title: body.title,
          contentRevision: "0",
        },
        201
      )
    },
    // The panel re-reads the resource it was handed, by its own authority.
    [`GET /api/neev/resources/${resource.id}`]: () =>
      json({
        ...resource,
        kind: "document",
        nodeId: 4,
        title: "Untitled",
        contentRevision: "0",
      }),
    [`GET /api/neev/documents/${resource.id}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  const trigger = await screen.findByRole("button", {
    name: "Actions for Menu project",
  })
  trigger.focus()
  await user.keyboard("{Enter}")
  await user.click(
    await screen.findByRole("menuitem", { name: "New document" })
  )

  // Created straight away, with a placeholder name rather than a prompt.
  await waitFor(() => expect(created).toHaveLength(1))
  expect(created[0]).toMatchObject({ title: "Untitled" })
  // Nothing was asked for first: no dialog stood between the click and the
  // document. The title field that exists is the document's own, already
  // carrying the placeholder — a thing to edit, not a form to fill in.
  expect(screen.queryByRole("dialog")).toBeNull()
  expect(await screen.findByRole("textbox", { name: "Title" })).toHaveValue(
    "Untitled"
  )
  // And it is open, in its own tab.
  expect(await screen.findByRole("tab", { name: /Untitled/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(mock.unknown).toEqual([])
})

it("moves a nested folder with its wire revision and retains the draft on conflict", async () => {
  const nodes = [
    {
      id: 1,
      orgId: 3,
      parentId: null,
      kind: "project",
      name: "Launch",
      revision: "1",
    },
    {
      id: 2,
      orgId: 3,
      parentId: 1,
      kind: "folder",
      name: "Research",
      revision: "7",
    },
    {
      id: 3,
      orgId: 3,
      parentId: 2,
      kind: "folder",
      name: "Interviews",
      revision: "1",
    },
    {
      id: 4,
      orgId: 3,
      parentId: null,
      kind: "project",
      name: "Operations",
      revision: "1",
    },
  ]
  let body: unknown
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/1/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes": () => json({ nodes, nextAfter: null }),
    "GET /api/neev/nodes/1/documents": () =>
      json({ items: [], nextAfter: null }),
    "PATCH /api/neev/nodes/2": (init) => {
      body = JSON.parse(init.body as string)
      return json({ code: "conflict" }, 409)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Launch" }))
  await user.click(screen.getByRole("button", { name: "Actions for Research" }))
  await user.click(
    await screen.findByRole("menuitem", { name: "Edit details" })
  )
  const dialog = await screen.findByRole("dialog")
  expect(
    within(dialog).queryByRole("option", { name: "Interviews · #3" })
  ).toBeNull()
  expect(
    within(dialog).queryByRole("option", { name: "Research · #2" })
  ).toBeNull()
  await user.selectOptions(within(dialog).getByLabelText("Parent folder"), "4")
  await user.click(within(dialog).getByRole("button", { name: "Save" }))
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(/changed/)
  expect(within(dialog).getByLabelText("Parent folder")).toHaveValue("4")
  expect(body).toEqual({
    kind: "folder",
    name: "Research",
    parentId: 4,
    expectedRevision: "7",
  })
  expect(mock.unknown).toEqual([])
})

const resource = {
  id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  orgId: 3,
  nodeId: 7,
  kind: "chat" as const,
  creatorId: 1,
  title: "Alpha",
  visibility: "organization" as const,
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}
/** A one-project tree holding a single document, for the sidebar tests. */
function treeRoutes(doc: Omit<typeof resource, "kind"> & { kind: "document" }) {
  return {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [doc], nextAfter: null }),
    "GET /api/neev/nodes/7/chats": () => json({ items: [], nextAfter: null }),
    [`GET /api/neev/resources/${doc.id}`]: () => json(doc),
    [`GET /api/neev/documents/${doc.id}/content`]: () =>
      json({ revision: null, markdown: "", document: null }),
    [`GET /api/neev/documents/${doc.id}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
  }
}
it("opens conversations from the sidebar, guards switching items, and reveals a created one", async () => {
  const chatA = resource
  const chatB = {
    ...resource,
    id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    title: "Beta",
  }
  const created = {
    ...resource,
    id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
    title: "Gamma",
  }
  let items = [chatA, chatB]
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/chats": () => json({ items, nextAfter: null }),
    [`GET /api/neev/resources/${chatA.id}`]: () => json(chatA),
    [`GET /api/neev/resources/${chatB.id}`]: () => json(chatB),
    [`GET /api/neev/resources/${created.id}`]: () =>
      json(items.find((item) => item.id === created.id) ?? created),
    [`GET /api/neev/chats/${chatA.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/chats/${chatB.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/chats/${created.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    "POST /api/neev/nodes/7/chats": (init) => {
      const body = JSON.parse(init.body as string)
      const row = { ...created, title: body.title }
      items = [...items, row]
      return json(row, 201)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  // Direct selection of a real conversation from the sidebar.
  await user.click(await screen.findByRole("button", { name: "Alpha" }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "wip"
  )
  // A second tab is asked for, so the first conversation stays mounted behind
  // its own tab and nothing is discarded — hence no question. Browsing without
  // asking would REPLACE this panel, and that path is guarded instead.
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await user.click(
    within(screen.getByRole("navigation", { name: "Projects" })).getByRole(
      "button",
      { name: "Beta" }
    )
  )
  expect(screen.queryByRole("dialog")).toBeNull()
  // Two tabs, and the one in front is the conversation just opened.
  expect(screen.getByRole("tab", { name: /Beta/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  await waitFor(() =>
    expect(screen.getByRole("tab", { name: /Alpha/ })).toHaveAttribute(
      "aria-selected",
      "false"
    )
  )
  // Back to the first: the half-written message is still there, untouched and
  // never once in danger.
  await user.click(screen.getByRole("tab", { name: /Alpha/ }))
  await waitFor(() =>
    expect(
      composerText(screen.getAllByRole("textbox", { name: "Message" })[0])
    ).toBe("wip")
  )
  // Creating fills the tab in front, so step off the half-written one first —
  // the guarded path that would ask about it has a test of its own.
  await user.click(screen.getByRole("tab", { name: /Beta/ }))
  // Creating a conversation makes it at once — no name asked for — and
  // reveals it in the sidebar under the placeholder it was given.
  await user.click(screen.getByRole("button", { name: "Actions for Space" }))
  await user.click(screen.getByRole("menuitem", { name: "New agent chat" }))
  expect(screen.queryByRole("textbox", { name: "Title" })).toBeNull()
  await within(screen.getByRole("navigation", { name: "Projects" })).findByRole(
    "button",
    { name: "New chat" }
  )
  expect(mock.unknown).toEqual([])
})

/** Two conversations in one project, which is all the navigation tests need. */
function twoChats() {
  const chatA = resource
  const chatB = {
    ...resource,
    id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    title: "Beta",
  }
  return {
    chatA,
    chatB,
    routes: {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/7/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes": () =>
        json({
          nodes: [
            {
              id: 7,
              orgId: 3,
              kind: "project",
              parentId: null,
              name: "Space",
              revision: "1",
            },
          ],
          nextAfter: null,
        }),
      "GET /api/neev/nodes/7/chats": () =>
        json({ items: [chatA, chatB], nextAfter: null }),
      [`GET /api/neev/resources/${chatA.id}`]: () => json(chatA),
      [`GET /api/neev/resources/${chatB.id}`]: () => json(chatB),
      [`GET /api/neev/chats/${chatA.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
      [`GET /api/neev/chats/${chatB.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
    },
  }
}

it("asks before browsing away from an unsent message, and means both answers", async () => {
  // Browsing replaces the panel in front, which unmounts it — exactly as
  // destructive as closing the tab, so it asks the same question.
  const mock = mockNativeSession(undefined, twoChats().routes)
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Alpha" }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "half a thought"
  )

  // Going to what is ALREADY in front destroys nothing, so it asks nothing.
  await user.click(within(tree).getByRole("button", { name: "Alpha" }))
  expect(screen.queryByRole("dialog")).toBeNull()

  // Going somewhere else does ask — and Keep editing means keep: the message,
  // the tab, and the place in the strip are all exactly as they were.
  await user.click(within(tree).getByRole("button", { name: "Beta" }))
  expect(await screen.findByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  expect(screen.getAllByRole("tab")).toHaveLength(1)
  expect(screen.getByRole("tab", { name: /Alpha/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(composerText(screen.getByRole("textbox", { name: "Message" }))).toBe(
    "half a thought"
  )

  // Asked again, and answered the other way: the conversation is replaced in
  // place rather than opened beside what was there.
  await user.click(within(tree).getByRole("button", { name: "Beta" }))
  expect(await screen.findByRole("dialog")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Discard changes" }))
  await waitFor(() =>
    expect(screen.getByRole("tab", { name: /Beta/ })).toHaveAttribute(
      "aria-selected",
      "true"
    )
  )
  expect(screen.getAllByRole("tab")).toHaveLength(1)
  expect(screen.queryByRole("tab", { name: /Alpha/ })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("abandons a navigation whose strip moved while the question was up", async () => {
  /*
   * The question blocks the READER, not the panels.
   *
   * A panel closing its own tab (`dropTab`, when its subject turns out to be
   * gone) or swapping one for the chat it became (`replaceTab`) needs no
   * input, so neither is held back by the prompt. The answer that arrives
   * afterwards was decided against a strip that no longer exists, and
   * committing the list captured before it would undo what happened in the
   * meantime — reopening a closed tab, or reverting a chat to its draft.
   *
   * The close below stands in for exactly that: it is dispatched directly,
   * past the modal, because what it simulates is a panel acting on its own
   * rather than a reader clicking.
   */
  const { chatB, routes } = twoChats()
  const mock = mockNativeSession(undefined, routes)
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Alpha" }))
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await user.click(within(tree).getByRole("button", { name: "Beta" }))
  await user.click(screen.getByRole("tab", { name: /Alpha/ }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "half a thought"
  )

  // Ask to go elsewhere; the question comes up about the half-written message.
  await user.click(within(tree).getByRole("button", { name: "Space" }))
  expect(await screen.findByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )

  // The strip moves underneath it, as a panel would move it.
  fireEvent.click(
    screen.getByRole("button", { name: `Close ${chatB.title}`, hidden: true })
  )
  await waitFor(() =>
    // Read including hidden nodes: the strip is behind a modal.
    expect(screen.getAllByRole("tab", { hidden: true })).toHaveLength(1)
  )

  // Answered now — and abandoned rather than applied to the strip it was
  // asked about: the closed tab stays closed, and nothing opened over it.
  await user.click(screen.getByRole("button", { name: "Discard changes" }))
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(screen.getAllByRole("tab")).toHaveLength(1)
  expect(screen.getByRole("tab", { name: /Alpha/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(screen.queryByRole("tab", { name: /Beta/ })).toBeNull()
  expect(screen.queryByRole("tab", { name: "Space" })).toBeNull()

  // The guard went nowhere either: asked again, against what is actually
  // open, it still asks.
  await typeInComposer(
    screen.getByRole("textbox", { name: "Message" }),
    "and another"
  )
  await user.click(within(tree).getByRole("button", { name: "Space" }))
  expect(await screen.findByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  expect(mock.unknown).toEqual([])
})

it("asks before browsing away from an unsent agent prompt", async () => {
  // An unsent prompt is work in progress too: the tab it is written in is
  // replaced by browsing, so the same question is put about it.
  const SESSION = "44444444-4444-4444-8444-444444444444"
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () =>
      json({
        sessions: [
          {
            id: SESSION,
            orgId: 3,
            ownerId: 1,
            title: "Parser chat",
            provider: "claude",
            model: "default",
            boundChatId: null,
            nodeId: 7,
            archived: false,
            revision: "1",
            createdAt: 1,
            updatedAt: 1,
          },
        ],
      }),
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          {
            provider: "claude",
            state: "connected",
            accountLabel: "sagar@example.test",
            checkedAt: 1,
            errorCode: null,
          },
        ],
      }),
    [`GET /api/neev/agent/sessions/${SESSION}/turns`]: () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/7/chats": () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  await user.click(
    await within(tree).findByRole("button", { name: /^Parser chat/ })
  )
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Ask the agent" }),
    "half a prompt"
  )

  await user.click(within(tree).getByRole("button", { name: "Space" }))
  expect(await screen.findByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  expect(screen.getByRole("tab", { name: /^Parser chat/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(
    composerText(screen.getByRole("textbox", { name: "Ask the agent" }))
  ).toBe("half a prompt")

  await user.click(within(tree).getByRole("button", { name: "Space" }))
  await user.click(
    await screen.findByRole("button", { name: "Discard changes" })
  )
  await waitFor(() =>
    expect(screen.getByRole("tab", { name: /Space/ })).toHaveAttribute(
      "aria-selected",
      "true"
    )
  )
  expect(screen.queryByRole("tab", { name: /^Parser chat/ })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("consumes the blank tab when the thing chosen is already open", async () => {
  // Ask for somewhere to put something, choose something already open, and
  // the blank goes with it: an orphan nobody asked to keep is worse than the
  // second tab they did.
  const mock = mockNativeSession(undefined, twoChats().routes)
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Alpha" }))
  await user.click(screen.getByRole("button", { name: "New tab" }))
  // The blank says what it is rather than looking like a panel that failed.
  expect(screen.getByRole("tab", { name: "New tab" })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(
    screen.getByText("Choose a document or a chat to open here.")
  ).toBeVisible()

  await user.click(within(tree).getByRole("button", { name: "Alpha" }))
  await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(1))
  expect(screen.queryByRole("tab", { name: "New tab" })).toBeNull()
  expect(screen.getByRole("tab", { name: /Alpha/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )

  // Filled rather than added to: a blank asked for and then used up leaves the
  // strip one tab longer, not two.
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await user.click(within(tree).getByRole("button", { name: "Beta" }))
  await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(2))
  expect(screen.getByRole("tab", { name: /Beta/ })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(mock.unknown).toEqual([])
})

/**
 * The open tabs are one stop in the Tab order. Arrows, Home and End move
 * between them and show the one they land on; Delete closes the focused one
 * and keeps focus in the strip.
 */
it("moves between open tabs with the keyboard and closes one with Delete", async () => {
  mockNativeSession(undefined, twoChats().routes)
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Alpha" }))
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await user.click(within(tree).getByRole("button", { name: "Beta" }))
  await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(2))
  const alpha = () => screen.getByRole("tab", { name: /Alpha/ })
  const beta = () => screen.getByRole("tab", { name: /Beta/ })

  // Only the shown tab is in the Tab order, and so is only its close.
  expect(beta()).toHaveAttribute("tabindex", "0")
  expect(alpha()).toHaveAttribute("tabindex", "-1")
  expect(screen.getByRole("button", { name: /Close Alpha/ })).toHaveAttribute(
    "tabindex",
    "-1"
  )

  beta().focus()
  await user.keyboard("{ArrowLeft}")
  expect(alpha()).toHaveFocus()
  expect(alpha()).toHaveAttribute("aria-selected", "true")
  await user.keyboard("{ArrowLeft}") // wraps
  expect(beta()).toHaveFocus()
  await user.keyboard("{Home}")
  expect(alpha()).toHaveFocus()
  await user.keyboard("{End}")
  expect(beta()).toHaveFocus()
  expect(beta()).toHaveAttribute("aria-selected", "true")

  await user.keyboard("{Delete}")
  await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(1))
  expect(alpha()).toHaveFocus()
  expect(alpha()).toHaveAttribute("aria-selected", "true")
})

it("mirrors the tab arrows in a right-to-left layout", async () => {
  mockNativeSession(undefined, twoChats().routes)
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <div dir="rtl">
        <NativeWorkspace />
      </div>
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Alpha" }))
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await user.click(within(tree).getByRole("button", { name: "Beta" }))
  // A third tab, so "back" and "forward" land on different tabs.
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(3))
  const beta = screen.getByRole("tab", { name: /Beta/ })
  const blank = screen.getByRole("tab", { name: "New tab" })

  // Drawn right to left: Right goes back to Beta, Left goes forward again.
  blank.focus()
  await user.keyboard("{ArrowRight}")
  expect(beta).toHaveFocus()
  expect(beta).toHaveAttribute("aria-selected", "true")
  await user.keyboard("{ArrowLeft}")
  expect(blank).toHaveFocus()
  expect(blank).toHaveAttribute("aria-selected", "true")
})

/**
 * Closing the last tab from the keyboard leaves focus somewhere useful — the
 * start page that replaces the strip — not on the page body. An unsaved
 * change still asks first, and keeping it keeps the tab.
 */
it("keeps focus on the page when Delete closes the last tab, and still asks about unsaved work", async () => {
  mockNativeSession(undefined, twoChats().routes)
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Alpha" }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "half a thought"
  )

  screen.getByRole("tab", { name: /Alpha/ }).focus()
  await user.keyboard("{Delete}")
  expect(await screen.findByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(screen.getAllByRole("tab")).toHaveLength(1)
  expect(screen.getByRole("textbox", { name: "Message" })).toHaveTextContent(
    "half a thought"
  )

  screen.getByRole("tab", { name: /Alpha/ }).focus()
  await user.keyboard("{Delete}")
  await user.click(
    await screen.findByRole("button", { name: "Discard changes" })
  )
  await waitFor(() => expect(screen.queryAllByRole("tab")).toHaveLength(0))
  await waitFor(() =>
    expect(
      screen.getByRole("heading", { level: 1, name: "Projects" })
    ).toHaveFocus()
  )
})

it("lists a container's documents and team chats together", async () => {
  const document = { ...resource, kind: "document", title: "Brief" }
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [document], nextAfter: null }),
    "GET /api/neev/nodes/7/chats": () =>
      json({ items: [resource], nextAfter: null }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  // One container, both kinds, at the same time: there is no mode that hides
  // the documents to show the chats.
  expect(await screen.findByRole("button", { name: "Brief" })).toBeVisible()
  expect(await screen.findByRole("button", { name: "Alpha" })).toBeVisible()
  // Creation is one menu on the row, not a permanent row per kind repeated
  // under every container.
  expect(screen.queryByRole("button", { name: "New document" })).toBeNull()
  expect(screen.queryByRole("button", { name: "New chat" })).toBeNull()
  await user.click(screen.getByRole("button", { name: "Actions for Space" }))
  // One conversation entry, not one per provider and not one per mode: agent
  // or team, and which provider answers, are chosen inside the conversation.
  for (const label of ["New document", "New agent chat", "New subfolder"])
    expect(screen.getByRole("menuitem", { name: label })).toBeVisible()
  expect(screen.queryByRole("menuitem", { name: /Claude|Codex/ })).toBeNull()
  expect(screen.queryByRole("menuitem", { name: "New chat" })).toBeNull()
  expect(mock.unknown).toEqual([])
})

const DOC_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
const NODE = {
  id: 4,
  orgId: 3,
  parentId: null,
  kind: "project",
  name: "Roadmap",
  revision: "1",
}
const DOC = {
  id: DOC_ID,
  orgId: 3,
  nodeId: 4,
  kind: "document",
  creatorId: 1,
  title: "Launch plan",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "1",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}

/** The pointer a previous visit would have left behind, for this user + org. */
function rememberOpen(ref: unknown = { resourceId: DOC_ID, nodeId: 4 }) {
  localStorage.setItem("neev.ws.section", "docs")
  localStorage.setItem("neev.ws.open.1:3", JSON.stringify(ref))
}

const NEW_CHAT_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
const NEW_CHAT = {
  ...DOC,
  id: NEW_CHAT_ID,
  kind: "chat",
  title: "New chat",
  contentRevision: "0",
}

function restoringRoutes(resource: () => Response) {
  const made: unknown[] = []
  return {
    "GET /api/neev/nodes": () => json({ nodes: [NODE], nextAfter: null }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [DOC], nextAfter: null }),
    // A container lists both kinds now, so both are stubbed.
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: made.length ? [NEW_CHAT] : [], nextAfter: null }),
    // Starting an agent chat creates an ordinary chat resource.
    "POST /api/neev/nodes/4/chats": () => {
      made.push(1)
      return json(NEW_CHAT, 201)
    },
    [`GET /api/neev/resources/${NEW_CHAT_ID}`]: () => json(NEW_CHAT),
    [`GET /api/neev/chats/${NEW_CHAT_ID}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/resources/${DOC_ID}`]: resource,
    [`GET /api/neev/documents/${DOC_ID}/content`]: () =>
      json({
        revision: {
          format: "markdown",
          formatVersion: 1,
          revision: "1",
          authorId: 1,
          authorName: "Sagar",
          byteLength: "5",
          sha256: "a".repeat(64),
          createdAt: 0,
          parentRevision: null,
          restoredFrom: null,
        },
        markdown: "Hello",
        document: null,
      }),
    [`GET /api/neev/documents/${DOC_ID}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
  }
}

/**
 * A rename reaching the tab strip when no list holds the document.
 *
 * This is the restore path: the tab is reopened from a previous session, the
 * panel fetches the resource by id, and the node's list may hold nothing for
 * it. `onChanged` updates that list and nothing else, so on its own it is a
 * no-op here — the strip kept the name captured when the panel first resolved,
 * and a rename made inside the document changed the document and nothing else.
 */
it("carries a rename to the tab strip on a restored tab", async () => {
  rememberOpen()
  const engineDoc = { ...DOC, engine: "lasuite" as const }
  const renamed = { ...engineDoc, title: "Renamed while restored" }
  let current: typeof engineDoc = engineDoc
  mockNativeSession(undefined, {
    ...restoringRoutes(() => json(current)),
    // The node lists nothing, which is what makes the tab depend on the
    // resolved-title fallback rather than on a list row — the restore path.
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
  })
  mountWorkspace()

  // It starts with the name the panel resolved, not "Loading your workspace…".
  await screen.findByRole("tab", { name: /Launch plan/ })
  const frame = await waitFor(() => {
    // The tab button carries the same title text, so take the frame itself.
    const found = document.querySelector("iframe")
    expect(found).not.toBeNull()
    return found as HTMLIFrameElement
  })
  await waitFor(() => expect(frame.contentWindow).toBeTruthy())

  // The editor confirms a rename the way it does in a browser: an id, no title.
  current = renamed
  await act(async () => {
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "neev-doc-metadata-updated", resourceId: DOC_ID },
        source: frame.contentWindow,
        origin: window.location.origin,
      })
    )
  })

  // The document was renamed; the strip has to say so, with no list to help it.
  await waitFor(() => {
    const names = screen.getAllByRole("tab").map((tab) => tab.textContent ?? "")
    expect(names.join(" | ")).toContain("Renamed while restored")
  })
})

/**
 * The engine document's controls live in the row the breadcrumb already uses.
 *
 * They used to be a band of their own between the tab strip and the frame —
 * the blank strip above every title. Asserted by where they are in the DOM,
 * not by a class: the row holding the breadcrumb is the one that must hold
 * them, and the tab panel must hold none of them.
 */
it("puts an engine document's controls in the breadcrumb row, not a band above it", async () => {
  rememberOpen()
  const engineDoc = { ...DOC, engine: "lasuite" as const }
  mockNativeSession(undefined, {
    ...restoringRoutes(() => json(engineDoc)),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [engineDoc], nextAfter: null }),
  })
  mountWorkspace()
  await screen.findByRole("tab", { name: /Launch plan/ })
  const visibility = await screen.findByRole("img", { name: "Private" })
  const row = screen.getByRole("navigation", { name: "Location" }).parentElement
  expect(row).toContainElement(visibility)
  expect(row).toContainElement(
    screen.getByRole("button", { name: "Content actions" })
  )
  // The document itself: the section the frame sits in.
  const panel = document.querySelector("iframe")?.closest("section")
  expect(panel).toBeTruthy()
  if (!panel) return
  expect(panel).not.toContainElement(visibility)
  expect(panel.querySelector("header")).toBeNull()
  // Exactly one set: the panel drew none of its own as well.
  expect(screen.getAllByRole("img", { name: "Private" })).toHaveLength(1)
})

it("gives a restored tab the icon of what it actually is", async () => {
  // Every resource tab used to draw the document icon, so a team chat in the
  // strip wore a page — while the SAME chat in the sidebar one panel away
  // correctly wore a speech bubble. The strip is scanned to tell a conversation
  // from a document, so the icon has to follow the resource's real kind.
  const user = userEvent.setup()
  rememberOpen()
  mockNativeSession(undefined, {
    ...restoringRoutes(() => json(DOC)),
    // A real chat resource listed beside the document, so both kinds are open.
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [NEW_CHAT], nextAfter: null }),
  })
  mountWorkspace()
  // The document tab is the restored one; the chat is opened from the tree.
  await screen.findByRole("tab", { name: /Launch plan/ })
  // A second tab is asked for; browsing alone replaces what is in front.
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await user.click(await screen.findByRole("button", { name: "New chat" }))

  const iconOf = (name: RegExp) => {
    const icon = screen.getByRole("tab", { name }).querySelector("svg")
    // A tab with no icon at all is the same bug wearing a different hat.
    expect(icon).not.toBeNull()
    return icon!.getAttribute("class") ?? ""
  }
  await waitFor(() => expect(iconOf(/New chat/)).toMatch(/messages-square/))
  expect(iconOf(/Launch plan/)).toMatch(/file-text/)
  // And the two really differ, which is the whole point of the icon.
  expect(iconOf(/New chat/)).not.toBe(iconOf(/Launch plan/))
})

function mountWorkspace() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

it("returns the reader to the resource they had open before a reload", async () => {
  rememberOpen()
  mockNativeSession(
    undefined,
    restoringRoutes(() => json(DOC))
  )
  mountWorkspace()
  // The container is revealed and its items listed …
  expect((await screen.findAllByText("Roadmap")).length).toBeGreaterThan(0)
  // … and the document is actually OPEN: its title shows both as the rail entry
  // and in the open document's own header. The empty-section placeholder would
  // give neither.
  await waitFor(() =>
    expect(screen.getAllByTitle("Launch plan")).toHaveLength(2)
  )
  expect(screen.queryByText("Pick a document to open")).toBeNull()
})

it("restores nothing when the resource is gone or no longer permitted", async () => {
  for (const status of [404, 403]) {
    localStorage.clear()
    rememberOpen()
    mockNativeSession(
      undefined,
      restoringRoutes(() => json({ code: "not_found" }, status))
    )
    const view = mountWorkspace()
    // A deleted or newly-forbidden id lands the reader in their section, not on
    // an error — they never asked for this fetch.
    expect((await screen.findAllByText("Roadmap")).length).toBeGreaterThan(0)
    expect(screen.queryByTitle("Launch plan")).toBeNull()
    expect(screen.queryByRole("alert")).toBeNull()
    view.unmount()
    vi.unstubAllGlobals()
  }
})

it("ignores a pointer left by a different organization", async () => {
  // The signed-in org is 3; this pointer belongs to another tenant's key.
  localStorage.setItem("neev.ws.section", "docs")
  localStorage.setItem(
    "neev.ws.open.1:9",
    JSON.stringify({ resourceId: DOC_ID, nodeId: 4 })
  )
  let fetched = 0
  mockNativeSession(
    undefined,
    restoringRoutes(() => {
      fetched += 1
      return json(DOC)
    })
  )
  mountWorkspace()
  await screen.findAllByText("Roadmap")
  expect(screen.queryByTitle("Launch plan")).toBeNull()
  // Not even a lookup: a pointer from another tenant is never dereferenced.
  expect(fetched).toBe(0)
})

it("ignores a pointer whose resource belongs to the other section", async () => {
  rememberOpen()
  // Stored while in Documents, but the server says it is a chat now.
  mockNativeSession(
    undefined,
    restoringRoutes(() => json({ ...DOC, kind: "chat", originChatId: null }))
  )
  mountWorkspace()
  await screen.findAllByText("Roadmap")
  expect(screen.queryByTitle("Launch plan")).toBeNull()
})

it("discards a malformed pointer without a lookup", async () => {
  rememberOpen({ resourceId: "not-a-uuid", nodeId: 4 })
  let fetched = 0
  mockNativeSession(
    undefined,
    restoringRoutes(() => {
      fetched += 1
      return json(DOC)
    })
  )
  mountWorkspace()
  await screen.findAllByText("Roadmap")
  expect(fetched).toBe(0)
})

it("remembers the opened resource, and forgets it on the way back", async () => {
  localStorage.setItem("neev.ws.section", "docs")
  mockNativeSession(
    undefined,
    restoringRoutes(() => json(DOC))
  )
  const user = userEvent.setup()
  mountWorkspace()
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  await user.click(
    within(screen.getByRole("navigation", { name: "Projects" })).getByRole(
      "button",
      { name: "Launch plan" }
    )
  )
  // A pointer, not content: just which resource, under which container.
  await waitFor(() =>
    expect(
      JSON.parse(localStorage.getItem("neev.ws.open.1:3") as string)
    ).toEqual({ resourceId: DOC_ID, nodeId: 4 })
  )
  // An agent chat IS a resource now, so starting one moves the pointer to it
  // rather than clearing it: the next reload lands on the conversation the
  // reader actually left open.
  await user.click(screen.getByRole("button", { name: "Actions for Roadmap" }))
  await user.click(screen.getByRole("menuitem", { name: "New agent chat" }))
  await waitFor(() =>
    expect(
      JSON.parse(localStorage.getItem("neev.ws.open.1:3") as string)
    ).toEqual({ resourceId: NEW_CHAT_ID, nodeId: 4 })
  )
})

it("reveals a restored resource nested under collapsed ancestors", async () => {
  // Project → folder → subfolder; the chat lives at the bottom.
  const tree = [
    NODE,
    { ...NODE, id: 5, parentId: 4, kind: "folder", name: "Reviews" },
    { ...NODE, id: 6, parentId: 5, kind: "folder", name: "Q3" },
  ]
  const CHAT_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
  const chat = {
    ...DOC,
    id: CHAT_ID,
    nodeId: 6,
    kind: "chat",
    title: "Nested team chat",
  }
  localStorage.setItem("neev.ws.section", "team")
  localStorage.setItem(
    "neev.ws.open.1:3",
    JSON.stringify({ resourceId: CHAT_ID, nodeId: 6 })
  )
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/6/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes": () => json({ nodes: tree, nextAfter: null }),
    "GET /api/neev/nodes/6/chats": () =>
      json({ items: [chat], nextAfter: null }),
    [`GET /api/neev/resources/${CHAT_ID}`]: () => json(chat),
    [`GET /api/neev/chats/${CHAT_ID}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  mountWorkspace()
  // Every ancestor is expanded, not just the chat's own folder — otherwise the
  // restored item sits inside a collapsed project and cannot be seen.
  const rail = await screen.findByRole("navigation", { name: "Projects" })
  for (const name of ["Roadmap", "Reviews", "Q3"])
    await waitFor(() =>
      expect(
        within(rail).getByRole("button", { name: `Collapse ${name}` })
      ).toBeVisible()
    )
  // And the chat itself is both listed and open.
  await waitFor(() =>
    expect(screen.getAllByTitle("Nested team chat").length).toBeGreaterThan(1)
  )
})

it("lands on Projects, never on a global agent list", async () => {
  // The rail used to persist a section, so a reload could land straight on a
  // global Agent Chats surface with its own New agent chat — a destination
  // outside Projects, which is the one thing the workspace should not have.
  localStorage.setItem("neev.ws.section", "agents")
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/7/chats": () => json({ items: [], nextAfter: null }),
  })
  mountWorkspace()

  // Level 1: the landing's own heading, not the sidebar's section label.
  expect(
    await screen.findByRole("heading", { level: 1, name: "Projects" })
  ).toBeVisible()
  // Once the tree has loaded, the copy points at the projects that exist.
  expect(
    await screen.findByText("Open a project to work in it, or create one.")
  ).toBeVisible()
  // No agent surface, and nothing offering to start one from nowhere.
  expect(screen.queryByRole("button", { name: "New agent chat" })).toBeNull()
  expect(screen.queryByText(/No saved agent chats yet/)).toBeNull()
  // The landing's own way to start.
  expect(
    screen.getAllByRole("button", { name: "New project" }).length
  ).toBeGreaterThan(0)
})

it("opens a project's contents by its name, and folds it by its chevron", async () => {
  // Clicking the name used to only fold the row, leaving the reader looking at
  // whatever was open before — the one thing the screen did not show was the
  // project they had just clicked.
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/7/chats": () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  // The centre is now that project, and opening it expanded it.
  expect(
    await screen.findByRole("heading", { level: 1, name: "Space" })
  ).toBeVisible()
  expect(
    within(tree).getByRole("button", { name: "Collapse Space" })
  ).toBeVisible()

  // The chevron still folds, and folding does not throw the reader out.
  await user.click(within(tree).getByRole("button", { name: "Collapse Space" }))
  expect(
    within(tree).getByRole("button", { name: "Expand Space" })
  ).toBeVisible()
  expect(screen.getByRole("heading", { level: 1, name: "Space" })).toBeVisible()
})

it("keeps documents and team chats in the tree while an agent chat is open", async () => {
  // The tree lists every kind, so opening an agent chat is not a change of
  // content. Invalidating the cache here emptied a tree that was still right.
  const opened: unknown[] = []
  // This project is node 7, so the chat it makes hangs under node 7.
  const made = { ...NEW_CHAT, nodeId: 7 }
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({
        items: [{ ...resource, kind: "document" as const, title: "Brief" }],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/chats": () =>
      json({ items: opened.length ? [made] : [], nextAfter: null }),
    // The entry makes a chat resource; the tree must survive that.
    "POST /api/neev/nodes/7/chats": () => {
      opened.push(1)
      return json(made, 201)
    },
    [`GET /api/neev/resources/${NEW_CHAT_ID}`]: () => json(made),
    [`GET /api/neev/chats/${NEW_CHAT_ID}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
  })
  const user = userEvent.setup()
  mountWorkspace()

  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  expect(
    await within(tree).findByRole("button", { name: "Brief" })
  ).toBeVisible()

  await user.click(screen.getByRole("button", { name: "Actions for Space" }))
  await user.click(screen.getByRole("menuitem", { name: "New agent chat" }))

  // The conversation is open AND the project still shows what it holds.
  expect(await screen.findByRole("textbox", { name: "Message" })).toBeVisible()
  expect(within(tree).getByRole("button", { name: "Brief" })).toBeVisible()
})

it("marks only the active tab's item as current in the tree", async () => {
  // Panels stay mounted, so the pointers a panel set outlive it. Reading them
  // for the highlight lit up the last document AND the last agent chat at
  // once, whichever tab was actually in front.
  const SESSION = "44444444-4444-4444-8444-444444444444"
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () =>
      json({
        sessions: [
          {
            id: SESSION,
            orgId: 3,
            ownerId: 1,
            title: "Parser chat",
            provider: "claude",
            model: "default",
            boundChatId: null,
            nodeId: 7,
            archived: false,
            revision: "1",
            createdAt: 1,
            updatedAt: 1,
          },
        ],
      }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/documents": () =>
      json({
        items: [{ ...resource, kind: "document" as const, title: "Brief" }],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/chats": () => json({ items: [], nextAfter: null }),
    [`GET /api/neev/documents/${resource.id}/content`]: () =>
      json({ revision: null, markdown: "", document: null }),
    [`GET /api/neev/documents/${resource.id}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  await user.click(await within(tree).findByRole("button", { name: "Brief" }))
  // Browsing replaces what is in front, so a SECOND tab is asked for.
  await user.click(screen.getByRole("button", { name: "New tab" }))
  await user.click(
    await within(tree).findByRole("button", { name: /^Parser chat/ })
  )

  // The chat is in front, so the chat is the only thing marked current.
  expect(
    within(tree).getByRole("button", { name: /^Parser chat/ })
  ).toHaveAttribute("aria-current", "page")
  expect(
    within(tree).getByRole("button", { name: "Brief" })
  ).not.toHaveAttribute("aria-current")

  // Back to the document, and the marking follows — one at a time.
  await user.click(screen.getByRole("tab", { name: /Brief/ }))
  await waitFor(() =>
    expect(within(tree).getByRole("button", { name: "Brief" })).toHaveAttribute(
      "aria-current",
      "page"
    )
  )
  expect(
    within(tree).getByRole("button", { name: /^Parser chat/ })
  ).not.toHaveAttribute("aria-current")
})

/**
 * Renaming from the sidebar, which is where renaming was missing.
 *
 * It was reachable only by opening a thing first and finding the menu in its
 * header, which is why readers reported that Neev had no rename at all.
 *
 * The assertion that matters is the LAST one. A tab that is already open holds
 * its own copy of the resource, and the panel used to prefer whichever copy it
 * happened to have fetched first — so a rename made afterwards, in the sidebar,
 * left the tab showing the old name. `authRevision` decides now, so the newer
 * write wins wherever it came from.
 */
it("renames from the sidebar, and the open tab takes the newer name", async () => {
  const doc = { ...resource, kind: "document" as const, title: "Brief" }
  const renamed = { ...doc, title: "Brief, renamed", authRevision: "2" }
  let patched: Record<string, unknown> | undefined
  mockNativeSession(undefined, {
    ...treeRoutes(doc),
    [`PATCH /api/neev/resources/${doc.id}`]: (init: RequestInit) => {
      patched = JSON.parse(init.body as string)
      return json(renamed)
    },
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  await user.click(await within(tree).findByRole("button", { name: "Brief" }))
  await screen.findByRole("tab", { name: /Brief/ })

  // The menu is on the row itself, not behind opening the document.
  await act(async () => {
    fireEvent.keyDown(
      within(tree).getByRole("button", { name: "Actions for Brief" }),
      { key: "Enter" }
    )
  })
  await user.click(await screen.findByRole("menuitem", { name: "Rename" }))
  // Scoped to the dialog: the open document has its own title field.
  const dialog = await screen.findByRole("dialog")
  const field = within(dialog).getByLabelText("Title")
  await user.clear(field)
  await user.type(field, "Brief, renamed")
  await user.click(within(dialog).getByRole("button", { name: "Save" }))

  // The rest of the row is carried back unchanged; only the name moves.
  await waitFor(() => expect(patched).toBeDefined())
  expect(patched).toMatchObject({
    title: "Brief, renamed",
    nodeId: doc.nodeId,
    visibility: doc.visibility,
    archived: false,
    expectedAuthRevision: "1",
  })

  await waitFor(() =>
    expect(
      within(tree).getByRole("button", { name: "Brief, renamed" })
    ).toBeInTheDocument()
  )
  // The tab is the point: it holds its own copy and must take the newer one.
  expect(
    screen.getByRole("tab", { name: /Brief, renamed/ })
  ).toBeInTheDocument()
})

/**
 * "Move to trash" is only an honest name if what it moves can be found again.
 *
 * There is no hard delete in the API — this archives — so the row must be
 * reachable and restorable rather than simply gone, which is what archiving
 * looked like when nothing listed it.
 */
it("moves a document to the trash and offers it back from the trash view", async () => {
  const doc = { ...resource, kind: "document" as const, title: "Brief" }
  const trashed = { ...doc, archived: true, authRevision: "2" }
  const bodies: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    ...treeRoutes(doc),
    // The server stops listing it live once it is trashed, and starts listing
    // it archived — one row, one place, exactly as the backend behaves.
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: bodies.length ? [] : [doc], nextAfter: null }),
    "GET /api/neev/nodes/7/documents?archived=true": () =>
      json({ items: bodies.length ? [trashed] : [], nextAfter: null }),
    "GET /api/neev/nodes/7/chats?archived=true": () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${doc.id}`]: (init: RequestInit) => {
      bodies.push(JSON.parse(init.body as string))
      return json(bodies.length === 1 ? trashed : doc)
    },
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  await within(tree).findByRole("button", { name: "Brief" })

  await act(async () => {
    fireEvent.keyDown(
      within(tree).getByRole("button", { name: "Actions for Brief" }),
      { key: "Enter" }
    )
  })
  await user.click(
    await screen.findByRole("menuitem", { name: "Move to trash" })
  )
  await waitFor(() => expect(bodies[0]).toMatchObject({ archived: true }))

  // Gone from the tree, as a deletion should look.
  await waitFor(() =>
    expect(within(tree).queryByRole("button", { name: "Brief" })).toBeNull()
  )

  // And findable again, which is what makes the name truthful.
  await user.click(screen.getByRole("button", { name: "Show trash" }))
  const row = await within(tree).findByRole("button", { name: /^Brief/ })
  expect(row).toHaveTextContent("In trash")
  await act(async () => {
    fireEvent.keyDown(
      within(tree).getByRole("button", { name: "Actions for Brief" }),
      { key: "Enter" }
    )
  })
  await user.click(await screen.findByRole("menuitem", { name: "Restore" }))
  await waitFor(() => expect(bodies[1]).toMatchObject({ archived: false }))
})

/**
 * Renaming from the open document's own header menu, which is the path the
 * reader reaches first — and the one that left the rest of the screen stale.
 *
 * `NativeContent.update` published its result with `setSelected`, so the
 * document renamed itself and nothing above it heard: the tab strip and the
 * breadcrumb kept the old name indefinitely. Everything the chrome reads from
 * lives above that component, so the result has to go out through
 * `onResourceChanged`, exactly as an in-frame rename already did.
 */
it("renames from the document's own menu, and the tab and breadcrumb follow", async () => {
  const doc = { ...resource, kind: "document" as const, title: "Brief" }
  const renamed = { ...doc, title: "Brief, renamed", authRevision: "2" }
  mockNativeSession(undefined, {
    ...treeRoutes(doc),
    [`PATCH /api/neev/resources/${doc.id}`]: () => json(renamed),
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  await user.click(await within(tree).findByRole("button", { name: "Brief" }))
  await screen.findByRole("tab", { name: /Brief/ })

  await act(async () => {
    fireEvent.keyDown(screen.getByRole("button", { name: "Content actions" }), {
      key: "Enter",
    })
  })
  await user.click(await screen.findByRole("menuitem", { name: "Rename" }))
  const dialog = await screen.findByRole("dialog")
  const field = within(dialog).getByLabelText("Title")
  await user.clear(field)
  await user.type(field, "Brief, renamed")
  await user.click(within(dialog).getByRole("button", { name: "Save" }))

  // The chrome, not just the page that was already open.
  await waitFor(() =>
    expect(
      screen.getByRole("tab", { name: /Brief, renamed/ })
    ).toBeInTheDocument()
  )
  expect(
    within(tree).getByRole("button", { name: "Brief, renamed" })
  ).toBeInTheDocument()
})

/**
 * The other direction: a change made in the sidebar must reach the document
 * that is already open.
 *
 * `NativeContent` keeps its own copy of the open resource and only replaced it
 * when asked to OPEN something — which a rename or a trashing from the sidebar
 * is not. So trashing the document you were reading left its header saying
 * "Private" with no sign it had been trashed, and its editor still writable,
 * while the sidebar row beside it said "In trash".
 */
it("carries a sidebar trashing into the document that is already open", async () => {
  const doc = { ...resource, kind: "document" as const, title: "Brief" }
  const trashed = { ...doc, archived: true, authRevision: "2" }
  const bodies: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    ...treeRoutes(doc),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: bodies.length ? [] : [doc], nextAfter: null }),
    [`PATCH /api/neev/resources/${doc.id}`]: (init: RequestInit) => {
      bodies.push(JSON.parse(init.body as string))
      return json(trashed)
    },
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  await user.click(await within(tree).findByRole("button", { name: "Brief" }))
  await screen.findByRole("tab", { name: /Brief/ })
  // Not trashed yet, so the open document says nothing about a trash.
  expect(screen.queryByText(/Archived/)).toBeNull()

  await act(async () => {
    fireEvent.keyDown(
      within(tree).getByRole("button", { name: "Actions for Brief" }),
      { key: "Enter" }
    )
  })
  await user.click(
    await screen.findByRole("menuitem", { name: "Move to trash" })
  )
  await waitFor(() => expect(bodies[0]).toMatchObject({ archived: true }))

  // The open document agrees with the sidebar, without being reopened.
  await waitFor(() => expect(screen.getByText(/Archived/)).toBeInTheDocument())
})

/**
 * The same sync, for a chat. `NativeChat` keeps its own `latest` copy too, so
 * a sidebar rename updated the row, the tab and the breadcrumb while the
 * conversation's own heading went on calling itself by its old name.
 */
it("carries a sidebar rename into the chat that is already open", async () => {
  const chat = { ...resource, title: "New chat" }
  const renamed = { ...chat, title: "Parser notes", authRevision: "2" }
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Space",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/7/chats": () =>
      json({ items: [chat], nextAfter: null }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${chat.id}`]: () => json(renamed),
  })
  const user = userEvent.setup()
  mountWorkspace()

  const tree = await screen.findByRole("navigation", { name: "Projects" })
  await user.click(await within(tree).findByRole("button", { name: "Space" }))
  await user.click(
    await within(tree).findByRole("button", { name: "New chat" })
  )
  // The conversation's own heading, before anything is renamed.
  await screen.findByRole("heading", { name: "New chat" })

  await act(async () => {
    fireEvent.keyDown(
      within(tree).getByRole("button", { name: "Actions for New chat" }),
      { key: "Enter" }
    )
  })
  await user.click(await screen.findByRole("menuitem", { name: "Rename" }))
  const dialog = await screen.findByRole("dialog")
  const field = within(dialog).getByLabelText("Title")
  await user.clear(field)
  await user.type(field, "Parser notes")
  await user.click(within(dialog).getByRole("button", { name: "Save" }))

  await waitFor(() =>
    expect(
      screen.getByRole("heading", { name: "Parser notes" })
    ).toBeInTheDocument()
  )
  expect(
    within(tree).getByRole("button", { name: "Parser notes" })
  ).toBeInTheDocument()
})

/*
 * A team chat and an agent chat are the same kind of row, so their files
 * toggle is the same control: named, with its state announced, and folded
 * away from a quiet sidebar until hover or focus — the agent row's used to
 * stand visible on every row, one size smaller, while the team chat's had no
 * `aria-expanded` at all.
 */
it("gives team and agent chats the same files toggle", async () => {
  const agent = {
    id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    orgId: 3,
    ownerId: 1,
    nodeId: 7,
    title: "Refactor the parser",
    provider: "claude",
    model: "default",
    boundChatId: null,
    archived: false,
    revision: "1",
    createdAt: 1_700_000_000,
    updatedAt: 1_700_000_000,
  }
  mockNativeSession(undefined, {
    ...treeRoutes({
      ...resource,
      id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
      kind: "document" as const,
      title: "Brief",
    }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [agent] }),
    "GET /api/neev/nodes/7/chats": () =>
      json({ items: [resource], nextAfter: null }),
    [`GET /api/neev/chats/${resource.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/agent/sessions/${agent.id}/turns`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mountWorkspace()
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await within(tree).findByRole("button", { name: "Alpha" })
  await within(tree).findByText("Refactor the parser")
  const toggles = within(tree).getAllByRole("button", {
    name: "Show generated files",
  })
  expect(toggles).toHaveLength(2)
  for (const toggle of toggles) {
    expect(toggle).toHaveAttribute("aria-expanded", "false")
    expect(toggle).toHaveAttribute("title", "Show generated files")
  }
  // One treatment, not two: same size, same hover reveal.
  expect(toggles[0].className).toBe(toggles[1].className)
  expect(toggles[0]).toHaveClass("size-7", "[@media(hover:hover)]:opacity-0")

  await user.click(toggles[0])
  const open = within(tree).getByRole("button", {
    name: "Hide generated files",
  })
  expect(open).toHaveAttribute("aria-expanded", "true")
  // Kept on screen while its list is showing, hover or not.
  expect(open).toHaveClass("[@media(hover:hover)]:opacity-100")
  expect(await within(tree).findByText("No generated files")).toBeVisible()
})

// ---------------------------------------------------------------------------
// Engine documents browsed away from are kept, hidden, for a quick way back.
// ---------------------------------------------------------------------------

const A_ID = "a1a1a1a1-a1a1-4a1a-8a1a-a1a1a1a1a1a1"
const B_ID = "b2b2b2b2-b2b2-4b2b-8b2b-b2b2b2b2b2b2"
const C_ID = "c3c3c3c3-c3c3-4c3c-8c3c-c3c3c3c3c3c3"
const engineDoc = (id: string, title: string) => ({
  ...DOC,
  id,
  title,
  engine: "lasuite" as const,
})
const DOC_A = engineDoc(A_ID, "Alpha doc")
const DOC_B = engineDoc(B_ID, "Beta doc")
const DOC_C = engineDoc(C_ID, "Gamma doc")

/**
 * A project holding engine documents. Each resource read goes through
 * `read`, so a test can count it, hold it or refuse it.
 */
function keptWorkspace(
  read: (id: string) => Response | Promise<Response> = (id) =>
    json([DOC_A, DOC_B, DOC_C].find((doc) => doc.id === id))
) {
  localStorage.setItem("neev.ws.section", "docs")
  const reads: string[] = []
  const routes: Record<string, () => Response | Promise<Response>> = {
    "GET /api/neev/nodes": () => json({ nodes: [NODE], nextAfter: null }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [DOC_A, DOC_B, DOC_C], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
  }
  for (const id of [A_ID, B_ID, C_ID])
    routes[`GET /api/neev/resources/${id}`] = () => {
      reads.push(id)
      return read(id)
    }
  const mock = mockNativeSession(
    undefined,
    routes as Parameters<typeof mockNativeSession>[1]
  )
  mountWorkspace()
  return { reads, mock }
}

const frameOf = (id: string) =>
  document.querySelector<HTMLIFrameElement>(
    `iframe[src="/document-engine/docs/${id}/"]`
  )
const shown = (element: Element | null) =>
  !!element && element.isConnected && !element.closest("[hidden]")

async function toFolder(user: ReturnType<typeof userEvent.setup>) {
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
}
async function toDoc(user: ReturnType<typeof userEvent.setup>, title: string) {
  await user.click(
    await within(
      screen.getByRole("navigation", { name: "Projects" })
    ).findByRole("button", { name: title })
  )
}
async function frameShown(id: string) {
  return waitFor(() => {
    const frame = frameOf(id)
    expect(shown(frame)).toBe(true)
    return frame as HTMLIFrameElement
  })
}

/**
 * Watches one element for ever being shown, and for ever being taken out of
 * the DOM. Attribute history rather than a final state, so a panel that is
 * shown and hidden again between two assertions is still caught.
 */
function watch(element: Element) {
  const seen = { shown: false, removed: false }
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      if (
        record.type === "attributes" &&
        record.oldValue !== null &&
        record.target instanceof Element &&
        record.target.contains(element) &&
        !record.target.hasAttribute("hidden")
      )
        seen.shown = true
      if (record.type === "childList")
        for (const node of record.removedNodes)
          if (node === element || node.contains(element)) seen.removed = true
    }
  })
  observer.observe(document.body, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ["hidden"],
    attributeOldValue: true,
  })
  return { seen, stop: () => observer.disconnect() }
}

it("brings a kept document back with the very same frame, after checking access first", async () => {
  let release: (() => void) | undefined
  let hold = false
  const { reads } = keptWorkspace((id) => {
    const answer = json([DOC_A, DOC_B, DOC_C].find((doc) => doc.id === id))
    if (!hold) return answer
    return new Promise<Response>((resolve) => {
      release = () => resolve(answer)
    })
  })
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const frame = await frameShown(A_ID)
  const watched = watch(frame)

  // Back to the folder: the document is kept, out of sight and out of the strip.
  await toFolder(user)
  await waitFor(() => expect(shown(frameOf(A_ID))).toBe(false))
  expect(frameOf(A_ID)).toBe(frame)
  expect(
    screen.getAllByRole("tab").map((tab) => tab.textContent ?? "")
  ).not.toContain("Alpha doc")
  watched.seen.shown = false

  // Coming back reads the resource BEFORE anything is revealed.
  hold = true
  const before = reads.length
  await toDoc(user, "Alpha doc")
  await waitFor(() => expect(reads.length).toBe(before + 1))
  await act(async () => {
    await Promise.resolve()
  })
  expect(shown(frame)).toBe(false)
  expect(watched.seen.shown).toBe(false)

  await act(async () => {
    release?.()
  })
  await waitFor(() => expect(shown(frame)).toBe(true))
  // Not a lookalike: the same element, never taken out of the document.
  expect(frameOf(A_ID)).toBe(frame)
  expect(watched.seen.removed).toBe(false)
  watched.stop()
})

it("never shows a kept document again once access is gone", async () => {
  let refuse = false
  keptWorkspace((id) =>
    refuse
      ? json({ code: "not_found" }, 404)
      : json([DOC_A, DOC_B, DOC_C].find((doc) => doc.id === id))
  )
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const frame = await frameShown(A_ID)
  await toFolder(user)
  await waitFor(() => expect(shown(frame)).toBe(false))

  const watched = watch(frame)
  refuse = true
  await toDoc(user, "Alpha doc")
  // The kept frame is let go without ever being visible again. Whatever opens
  // instead is a fresh mount that authorizes for itself.
  await waitFor(() => expect(frame.isConnected).toBe(false))
  expect(watched.seen.shown).toBe(false)
  watched.stop()
})

it("abandons a reveal whose strip moved while the check was out", async () => {
  let release: (() => void) | undefined
  let hold = false
  keptWorkspace((id) => {
    const answer = json([DOC_A, DOC_B, DOC_C].find((doc) => doc.id === id))
    if (!(hold && id === A_ID)) return answer
    return new Promise<Response>((resolve) => {
      release = () => resolve(answer)
    })
  })
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const frame = await frameShown(A_ID)
  await toFolder(user)
  await waitFor(() => expect(shown(frame)).toBe(false))

  hold = true
  const watched = watch(frame)
  await toDoc(user, "Alpha doc")
  // The reader moves on before the answer is in.
  await toDoc(user, "Beta doc")
  await frameShown(B_ID)
  await act(async () => {
    release?.()
  })
  await act(async () => {
    await Promise.resolve()
  })
  expect(shown(frame)).toBe(false)
  expect(watched.seen.shown).toBe(false)
  expect(shown(frameOf(B_ID))).toBe(true)
  watched.stop()
})

it("keeps only the last two documents", async () => {
  keptWorkspace()
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const alpha = await frameShown(A_ID)
  await toDoc(user, "Beta doc")
  const beta = await frameShown(B_ID)
  await toDoc(user, "Gamma doc")
  const gamma = await frameShown(C_ID)
  await toFolder(user)
  await waitFor(() => expect(alpha.isConnected).toBe(false))
  expect(beta.isConnected).toBe(true)
  expect(gamma.isConnected).toBe(true)
  expect(shown(beta) || shown(gamma)).toBe(false)
})

it("lets a document go after ten minutes unseen, and opens it afresh", async () => {
  const { reads } = keptWorkspace()
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const frame = await frameShown(A_ID)
  await toFolder(user)
  await waitFor(() => expect(shown(frame)).toBe(false))

  const now = Date.now()
  const clock = vi.spyOn(Date, "now").mockReturnValue(now + 11 * 60 * 1000)
  const before = reads.length
  await toDoc(user, "Alpha doc")
  const fresh = await frameShown(A_ID)
  expect(fresh).not.toBe(frame)
  expect(frame.isConnected).toBe(false)
  // Expired means the ordinary open, not a reveal's check.
  expect(reads.length).toBe(before)
  clock.mockRestore()
})

it("does not keep a document that was closed on purpose", async () => {
  keptWorkspace()
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const frame = await frameShown(A_ID)
  await user.click(screen.getByRole("button", { name: "Close Alpha doc" }))
  await waitFor(() => expect(frame.isConnected).toBe(false))
})

it("never writes a kept document into the saved tabs", async () => {
  keptWorkspace()
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  await frameShown(A_ID)
  await toFolder(user)
  await waitFor(() => expect(shown(frameOf(A_ID))).toBe(false))
  expect(frameOf(A_ID)).not.toBeNull()
  const saved = localStorage.getItem("neev.ws.tabs.1:3") ?? ""
  expect(saved).toContain("project")
  expect(saved).not.toContain(A_ID)
})

it("lets a kept document go when the session is lost on the check", async () => {
  let refuse = false
  keptWorkspace((id) =>
    refuse
      ? json({ code: "unauthorized" }, 401)
      : json([DOC_A, DOC_B, DOC_C].find((doc) => doc.id === id))
  )
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const frame = await frameShown(A_ID)
  await toFolder(user)
  await waitFor(() => expect(shown(frame)).toBe(false))
  const watched = watch(frame)
  refuse = true
  await toDoc(user, "Alpha doc")
  await waitFor(() => expect(frame.isConnected).toBe(false))
  expect(watched.seen.shown).toBe(false)
  watched.stop()
})

/**
 * Bringing one kept document back must not move the OTHER one.
 *
 * Kept panels are not in the strip, so in strip order they would be reshuffled
 * by every reveal — and a browser reloads an iframe that is moved in the DOM.
 * jsdom does not reload, so this watches for the move itself.
 */
it("never moves another kept frame when one is brought back", async () => {
  keptWorkspace()
  const user = userEvent.setup()
  await toFolder(user)
  await toDoc(user, "Alpha doc")
  const alpha = await frameShown(A_ID)
  await toDoc(user, "Beta doc")
  const beta = await frameShown(B_ID)
  await toFolder(user)
  await waitFor(() => expect(shown(beta)).toBe(false))

  const watchedBeta = watch(beta)
  const watchedAlpha = watch(alpha)
  await toDoc(user, "Alpha doc")
  await waitFor(() => expect(shown(alpha)).toBe(true))
  expect(frameOf(A_ID)).toBe(alpha)
  expect(frameOf(B_ID)).toBe(beta)
  expect(watchedAlpha.seen.removed).toBe(false)
  expect(watchedBeta.seen.removed).toBe(false)
  watchedAlpha.stop()
  watchedBeta.stop()
})

it("opens a fork in a NEW tab beside the chat it came from", async () => {
  const FORK_ID = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
  const REPLY_ID = "55555555-5555-4555-8555-555555555555"
  const forkChat = { ...NEW_CHAT, id: FORK_ID, title: "New chat (fork)" }
  const reply = {
    id: REPLY_ID,
    sequence: "1",
    authorId: 1,
    authorName: "Sagar",
    authorKind: "agent",
    agentLabel: "claude",
    body: "Here is the plan.",
    createdAt: 1_700_000_000,
    attachments: [],
  }
  // The chat is the one tab open, as a reload restores it.
  writeTabs(1, 3, [{ kind: "resource", resourceId: NEW_CHAT_ID, nodeId: 4 }], 0)
  const routes = restoringRoutes(() => json(DOC))
  mockNativeSession(undefined, {
    ...routes,
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [NEW_CHAT], nextAfter: null }),
    [`GET /api/neev/chats/${NEW_CHAT_ID}/messages`]: () =>
      json({ items: [reply], nextAfter: null }),
    [`POST /api/neev/chats/${NEW_CHAT_ID}/fork`]: () =>
      json(
        {
          chatId: FORK_ID,
          sourceChatId: NEW_CHAT_ID,
          throughMessageId: REPLY_ID,
          nodeId: 4,
          copiedMessages: 1,
          copiedAttachments: 0,
          archivedAttachmentsNotCopied: 0,
          replayed: false,
        },
        201
      ),
    [`GET /api/neev/resources/${FORK_ID}`]: () => json(forkChat),
    [`GET /api/neev/chats/${FORK_ID}/messages`]: () =>
      json({
        items: [{ ...reply, id: "66666666-6666-4666-8666-666666666666" }],
        nextAfter: null,
      }),
  })
  const user = userEvent.setup()
  mountWorkspace()
  await screen.findByRole("tab", { name: /New chat/ })
  await screen.findByText("Here is the plan.")
  await user.click(screen.getByRole("button", { name: "More actions" }))
  await user.click(
    await screen.findByRole("menuitem", { name: "Fork to new tab" })
  )
  // Two tabs now: the original is still open, and the fork is in front.
  const fork = await screen.findByRole("tab", { name: /New chat \(fork\)/ })
  await waitFor(() => expect(fork).toHaveAttribute("aria-selected", "true"))
  const names = screen.getAllByRole("tab").map((tab) => tab.textContent ?? "")
  expect(names).toHaveLength(2)
  expect(names.some((name) => /New chat$/.test(name.trim()))).toBe(true)
})
