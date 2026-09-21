import { act, render, screen, within, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeBoundary } from "./native-boundary"
import { NativeWorkspace } from "./native-workspace"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
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
it("shows the person and organization without GitHub or chat controls", async () => {
  const mock = mockNativeSession(undefined, {
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
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
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
  expect(screen.getByRole("button", { name: "New project" })).toBeEnabled()
  expect(
    screen.queryByRole("button", { name: "New chat" })
  ).not.toBeInTheDocument()
  // Agent chats are reached from inside a container. The sessions are still
  // private and unfiled — the project is the way in, not where they live. With
  // no runtime bound it says so rather than offering a connection that cannot
  // be exercised.
  expect(
    screen.queryByRole("navigation", { name: "Sections" })
  ).not.toBeInTheDocument()
  await user.click(await screen.findByRole("button", { name: "Expand Space" }))
  await user.click(await screen.findByRole("button", { name: "Agent chats" }))
  expect(await screen.findByText(/No saved agent chats yet/)).toBeVisible()
  expect(screen.getByRole("button", { name: "New agent chat" })).toBeEnabled()
  expect(screen.getByText("Agents are turned off.")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("clears the organization tree when a foreground content 403 exposes revoked membership", async () => {
  let bootstraps = 0
  const mock = mockNativeSession(undefined, {
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
    await screen.findByRole("button", { name: "Revoked project" })
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
  const trigger = await screen.findByRole("button", { name: "New project" })
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

it.each(["Cancel", "Escape"])(
  "returns to the workspace and restores focus after draft %s",
  async (action) => {
    const mock = mockNativeSession(undefined, {
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
      "GET /api/neev/nodes/4/documents": () =>
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
    // Full-page draft with an inline title (no small modal), focused for naming.
    const draft = await screen.findByRole("region", { name: "New document" })
    expect(draft).toHaveTextContent("Inside Menu project")
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Title" })).toHaveFocus()
    )
    if (action === "Escape") await user.keyboard("{Escape}")
    else await user.click(screen.getByRole("button", { name: "Cancel" }))
    await waitFor(() =>
      expect(screen.queryByRole("region", { name: "New document" })).toBeNull()
    )
    expect(trigger).toHaveFocus()
    expect(
      screen.getByText("Select an item from the sidebar, or create one.")
    ).toBeVisible()
    expect(mock.unknown).toEqual([])
  }
)

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
    [`GET /api/neev/resources/${created.id}`]: () => json(created),
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
  await user.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "wip"
  )
  // Switching to another item guards the unsaved draft.
  await user.click(screen.getByRole("button", { name: "Beta" }))
  expect(screen.getByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("wip")
  // Discarding opens the other conversation.
  await user.click(screen.getByRole("button", { name: "Beta" }))
  await user.click(screen.getByRole("button", { name: "Discard changes" }))
  await waitFor(() =>
    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("")
  )
  // Creating a conversation reveals it in the sidebar and opens it.
  await user.click(screen.getByRole("button", { name: "New chat" }))
  await user.type(screen.getByRole("textbox", { name: "Title" }), "Gamma")
  await user.click(screen.getByRole("button", { name: "Create" }))
  await within(screen.getByRole("navigation", { name: "Projects" })).findByRole(
    "button",
    { name: "Gamma" }
  )
  expect(mock.unknown).toEqual([])
})

it("lists a container's documents and team chats together", async () => {
  const document = { ...resource, kind: "document", title: "Brief" }
  const mock = mockNativeSession(undefined, {
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
  // And both ways to add to it, plus the way in to agent chats.
  expect(screen.getByRole("button", { name: "New document" })).toBeEnabled()
  expect(screen.getByRole("button", { name: "New chat" })).toBeEnabled()
  expect(screen.getByRole("button", { name: "Agent chats" })).toBeEnabled()
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

function restoringRoutes(resource: () => Response) {
  return {
    "GET /api/neev/nodes": () => json({ nodes: [NODE], nextAfter: null }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [DOC], nextAfter: null }),
    // A container lists both kinds now, so both are stubbed.
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
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
  expect(await screen.findByText("Roadmap")).toBeVisible()
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
    expect(await screen.findByText("Roadmap")).toBeVisible()
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
  await screen.findByText("Roadmap")
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
  await screen.findByText("Roadmap")
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
  await screen.findByText("Roadmap")
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
  await user.click(await screen.findByRole("button", { name: "Roadmap" }))
  await user.click(await screen.findByRole("button", { name: "Launch plan" }))
  // A pointer, not content: just which resource, under which container.
  await waitFor(() =>
    expect(
      JSON.parse(localStorage.getItem("neev.ws.open.1:3") as string)
    ).toEqual({ resourceId: DOC_ID, nodeId: 4 })
  )
  // Leaving for the agent chats closes the resource, so there is nothing to
  // restore — the next reload lands where the reader actually left off.
  await user.click(screen.getByRole("button", { name: "Agent chats" }))
  await waitFor(() =>
    expect(localStorage.getItem("neev.ws.open.1:3")).toBeNull()
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
