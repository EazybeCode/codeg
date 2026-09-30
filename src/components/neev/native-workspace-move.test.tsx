/**
 * TDD tests for drag-and-drop moving of resources and agent sessions between
 * projects/folders in the Neev sidebar.
 *
 * These tests validate:
 * - Dragging chat/document/session rows
 * - Projects/folders as valid drop targets with hover indication
 * - Rejecting external file drops and cross-org payloads
 * - Not interfering with attachment drops
 * - Owner-only draggability
 * - Ignoring same-parent and archived rows
 * - Proper cache updates after move
 * - Mutation blocking duplicates
 * - Success/error display
 */
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"
import { NativeWorkspace } from "./native-workspace"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
  typeInComposer,
  composerText,
} from "./test-helpers"

import { usePublishRunningChat } from "@/lib/neev-running-chats"

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

const RESOURCE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
const RESOURCE_ID_2 = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
const SESSION_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"

const chatResource = {
  id: RESOURCE_ID,
  orgId: 3,
  nodeId: 7,
  kind: "chat" as const,
  creatorId: 1,
  title: "Team planning",
  visibility: "private" as const,
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}

const documentResource = {
  ...chatResource,
  id: RESOURCE_ID_2,
  kind: "document" as const,
  title: "Design spec",
}

const agentSession = {
  id: SESSION_ID,
  orgId: 3,
  ownerId: 1,
  title: "Parser session",
  provider: "claude",
  model: "default",
  boundChatId: null,
  nodeId: 7,
  archived: false,
  revision: "1",
  createdAt: 1,
  updatedAt: 1,
}

/** Two projects where items can be moved between. */
function twoProjectsRoutes(
  options: {
    chats?: (Omit<typeof chatResource, "visibility"> & { visibility: "private" | "organization" })[]
    documents?: (typeof documentResource)[]
    sessions?: (typeof agentSession)[]
    patchResource?: (init: RequestInit) => Response | Promise<Response>
    patchSessionNode?: (init: RequestInit) => Response
  } = {}
) {
  const { chats = [], documents = [], sessions = [] } = options
  // Helper to make route handlers that filter by archived state
  const nodeChats = (nodeId: number, archived: boolean) => () =>
    json({
      items: chats.filter(
        (c) => c.nodeId === nodeId && c.archived === archived
      ),
      nextAfter: null,
    })
  const nodeDocs = (nodeId: number, archived: boolean) => () =>
    json({
      items: documents.filter(
        (d) => d.nodeId === nodeId && d.archived === archived
      ),
      nextAfter: null,
    })
  const nodeSessions = (archived: boolean) => () =>
    json({ sessions: sessions.filter((s) => s.archived === archived) })
  return {
    "GET /api/neev/agent/sessions": nodeSessions(false),
    "GET /api/neev/agent/sessions?archived=true": nodeSessions(true),
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
            name: "Project A",
            revision: "1",
          },
          {
            id: 8,
            orgId: 3,
            kind: "project",
            parentId: null,
            name: "Project B",
            revision: "1",
          },
          {
            id: 9,
            orgId: 3,
            kind: "folder",
            parentId: 7,
            name: "Subfolder",
            revision: "1",
          },
        ],
        nextAfter: null,
      }),
    // Routes for node resources - correctly filter by archived state
    "GET /api/neev/nodes/7/chats": nodeChats(7, false),
    "GET /api/neev/nodes/7/chats?archived=true": nodeChats(7, true),
    "GET /api/neev/nodes/7/documents": nodeDocs(7, false),
    "GET /api/neev/nodes/7/documents?archived=true": nodeDocs(7, true),
    "GET /api/neev/nodes/8/chats": nodeChats(8, false),
    "GET /api/neev/nodes/8/chats?archived=true": nodeChats(8, true),
    "GET /api/neev/nodes/8/documents": nodeDocs(8, false),
    "GET /api/neev/nodes/8/documents?archived=true": nodeDocs(8, true),
    "GET /api/neev/nodes/9/chats": nodeChats(9, false),
    "GET /api/neev/nodes/9/chats?archived=true": nodeChats(9, true),
    "GET /api/neev/nodes/9/documents": nodeDocs(9, false),
    "GET /api/neev/nodes/9/documents?archived=true": nodeDocs(9, true),
    [`GET /api/neev/resources/${RESOURCE_ID}`]: () => json(chatResource),
    [`GET /api/neev/resources/${RESOURCE_ID_2}`]: () => json(documentResource),
    [`GET /api/neev/chats/${RESOURCE_ID}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/documents/${RESOURCE_ID_2}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/agent/sessions/${SESSION_ID}/turns`]: () =>
      json({ items: [], nextAfter: null }),
    ...(options.patchResource
      ? { [`PATCH /api/neev/resources/${RESOURCE_ID}`]: options.patchResource }
      : {}),
    ...(options.patchSessionNode
      ? {
          [`PATCH /api/neev/agent/sessions/${SESSION_ID}/node`]:
            options.patchSessionNode,
        }
      : {}),
  }
}

/** Helper to create a drag event with a proper dataTransfer */
function createDragEvent(
  type: string,
  data?: { type: string; data: string }
): DragEvent {
  const event = new Event(type, {
    bubbles: true,
    cancelable: true,
  }) as DragEvent
  const dataTransfer = {
    data: new Map<string, string>(),
    setData(type: string, val: string) {
      this.data.set(type, val)
    },
    getData(type: string) {
      return this.data.get(type) ?? ""
    },
    clearData() {
      this.data.clear()
    },
    get types() {
      return [...this.data.keys()]
    },
    effectAllowed: "none" as DataTransfer["effectAllowed"],
    dropEffect: "none" as DataTransfer["dropEffect"],
    files: [] as unknown as FileList,
    items: [] as unknown as DataTransferItemList,
    setDragImage: vi.fn(),
  }
  if (data) {
    dataTransfer.setData(data.type, data.data)
  }
  Object.defineProperty(event, "dataTransfer", { value: dataTransfer })
  return event
}

/** Helper to get the sidebar navigation element scoped for queries */
async function getSidebar() {
  await screen.findByRole("button", { name: "Expand Project A" })
  return screen.getByRole("navigation", { name: "Projects" })
}

describe("drag-and-drop moving items between projects", () => {
  it("makes chat resources draggable when owned by the current user", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    await user.click(
      await screen.findByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await screen.findByRole("button", { name: "Team planning" })
    const container = chatRow.closest("[draggable]")

    expect(container).not.toBeNull()
    expect(container).toHaveAttribute("draggable", "true")
    expect(mock.unknown).toEqual([])
  })

  it("makes document resources draggable when owned by the current user", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        documents: [documentResource],
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    await user.click(
      await screen.findByRole("button", { name: "Expand Project A" })
    )

    const docRow = await screen.findByRole("button", { name: "Design spec" })
    const container = docRow.closest("[draggable]")

    expect(container).not.toBeNull()
    expect(container).toHaveAttribute("draggable", "true")
    expect(mock.unknown).toEqual([])
  })

  it("makes agent sessions draggable when owned by the current user", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        sessions: [agentSession],
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    await user.click(
      await screen.findByRole("button", { name: "Expand Project A" })
    )

    // Use exact match to avoid matching the actions button
    const sessionRow = await screen.findByRole("button", {
      name: "Parser session claude",
    })
    const container = sessionRow.closest("[draggable]")

    expect(container).not.toBeNull()
    expect(container).toHaveAttribute("draggable", "true")
    expect(mock.unknown).toEqual([])
  })

  it("does not make shared items draggable if not owner", async () => {
    const sharedChat = {
      ...chatResource,
      creatorId: 999, // Different owner
      visibility: "organization" as const,
    }
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [sharedChat],
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    await user.click(
      await screen.findByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await screen.findByRole("button", { name: "Team planning" })
    const container = chatRow.closest("[draggable]")

    // Should not be draggable since not owned by current user
    expect(container?.getAttribute("draggable")).not.toBe("true")
    expect(mock.unknown).toEqual([])
  })

  it("does not make archived items draggable", async () => {
    const archivedChat = { ...chatResource, archived: true }
    const mock = mockNativeSession(orgSession, {
      ...twoProjectsRoutes({ chats: [archivedChat] }),
    })
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    await user.click(
      await screen.findByRole("button", { name: "Expand Project A" })
    )
    await waitFor(() => expect(mock.unknown).toEqual([]))
    expect(
      screen.queryByRole("button", { name: "Team planning" })
    ).not.toBeInTheDocument()
    expect(mock.unknown).toEqual([])
  })

  it("shows visual drop indicator when dragging over a valid project target", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    // Scope queries to the sidebar nav
    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await within(sidebar).findByRole("button", {
      name: "Team planning",
    })
    const draggableRow = chatRow.closest("[draggable]")!
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    // Start drag
    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    // Drag over project B
    fireEvent(
      projectB,
      createDragEvent("dragenter", {
        type: "application/x-neev-move",
        data: "",
      })
    )
    fireEvent(
      projectB,
      createDragEvent("dragover", { type: "application/x-neev-move", data: "" })
    )

    // Should show drop indicator class
    const projectRow = projectB.closest("[data-drop-target]")
    expect(projectRow).toHaveAttribute("data-drop-active", "true")

    expect(mock.unknown).toEqual([])
  })

  it("moves a resource to another project via drag and drop", async () => {
    let patchBody: unknown
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
        patchResource: (init) => {
          patchBody = JSON.parse(init.body as string)
          return json({
            ...chatResource,
            nodeId: 8, // Moved to Project B
            authRevision: "2",
          })
        },
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await within(sidebar).findByRole("button", {
      name: "Team planning",
    })
    const draggableRow = chatRow.closest("[draggable]")!
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    // Perform drag and drop
    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )
    fireEvent(
      projectB,
      createDragEvent("dragenter", {
        type: "application/x-neev-move",
        data: "",
      })
    )
    fireEvent(
      projectB,
      createDragEvent("dragover", { type: "application/x-neev-move", data: "" })
    )
    fireEvent(
      projectB,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    await waitFor(() => expect(patchBody).toBeDefined())
    expect(patchBody).toMatchObject({
      nodeId: 8, // Target project
      expectedAuthRevision: "1",
    })
    expect(mock.unknown).toEqual([])
  })

  it("moves an agent session to another project via drag and drop", async () => {
    let patchBody: unknown
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        sessions: [agentSession],
        patchSessionNode: (init) => {
          patchBody = JSON.parse(init.body as string)
          return json({
            ...agentSession,
            nodeId: 8, // Moved to Project B
            revision: "2",
          })
        },
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )

    const sessionRow = await within(sidebar).findByRole("button", {
      name: "Parser session claude",
    })
    const draggableRow = sessionRow.closest("[draggable]")!
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    // Perform drag and drop
    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "session",
          id: SESSION_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )
    fireEvent(
      projectB,
      createDragEvent("dragenter", {
        type: "application/x-neev-move",
        data: "",
      })
    )
    fireEvent(
      projectB,
      createDragEvent("dragover", { type: "application/x-neev-move", data: "" })
    )
    fireEvent(
      projectB,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "session",
          id: SESSION_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    await waitFor(() => expect(patchBody).toBeDefined())
    expect(patchBody).toMatchObject({
      nodeId: 8, // Target project
      expectedRevision: "1",
    })
    expect(mock.unknown).toEqual([])
  })

  it("rejects drops from external file sources", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
      })
    )
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    // Try dropping an external file
    const dropEvent = createDragEvent("drop")
    Object.defineProperty(dropEvent.dataTransfer, "files", {
      value: [new File(["content"], "test.txt", { type: "text/plain" })],
    })
    Object.defineProperty(dropEvent.dataTransfer, "types", {
      value: ["Files"],
    })

    fireEvent(
      projectB,
      createDragEvent("dragenter", { type: "Files", data: "" })
    )
    fireEvent(
      projectB,
      createDragEvent("dragover", { type: "Files", data: "" })
    )
    fireEvent(projectB, dropEvent)

    // Should not show drop active state for external files
    const projectRow = projectB.closest("[data-drop-target]")
    expect(projectRow?.getAttribute("data-drop-active")).not.toBe("true")
    expect(mock.unknown).toEqual([])
  })

  it("rejects drops with cross-org payloads", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
      })
    )
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    // Try dropping from a different org
    fireEvent(
      projectB,
      createDragEvent("dragenter", {
        type: "application/x-neev-move",
        data: "",
      })
    )
    fireEvent(
      projectB,
      createDragEvent("dragover", { type: "application/x-neev-move", data: "" })
    )
    fireEvent(
      projectB,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 999, // Different org!
        }),
      })
    )

    // Should not perform move
    // The drop should be ignored - verify no PATCH request was made
    await new Promise((resolve) => setTimeout(resolve, 100))
    expect(mock.unknown).toEqual([])
  })

  it("ignores drag to same parent (no-op move)", async () => {
    const patchCalled = vi.fn()
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
        patchResource: () => {
          patchCalled()
          return json(chatResource)
        },
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await within(sidebar).findByRole("button", {
      name: "Team planning",
    })
    const draggableRow = chatRow.closest("[draggable]")!
    const projectA = within(sidebar).getByRole("button", { name: "Project A" })

    // Try to drop on same parent
    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7, // Same as current
          orgId: 3,
        }),
      })
    )
    fireEvent(
      projectA,
      createDragEvent("dragenter", {
        type: "application/x-neev-move",
        data: "",
      })
    )
    fireEvent(
      projectA,
      createDragEvent("dragover", { type: "application/x-neev-move", data: "" })
    )
    fireEvent(
      projectA,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    // Should not call PATCH since it's a no-op
    await new Promise((resolve) => setTimeout(resolve, 100))
    expect(patchCalled).not.toHaveBeenCalled()
    expect(mock.unknown).toEqual([])
  })

  it("blocks duplicate moves while a mutation is pending", async () => {
    let patchCount = 0
    let resolvePatch: () => void
    const patchPromise = new Promise<Response>((resolve) => {
      resolvePatch = () =>
        resolve(
          json({
            ...chatResource,
            nodeId: 8,
            authRevision: "2",
          })
        )
    })

    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
        patchResource: () => {
          patchCount++
          return patchPromise
        },
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await within(sidebar).findByRole("button", {
      name: "Team planning",
    })
    const draggableRow = chatRow.closest("[draggable]")!
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    const dragPayload = {
      type: "application/x-neev-move",
      data: JSON.stringify({
        kind: "resource",
        id: RESOURCE_ID,
        expectedRevision: "1",
        nodeId: 7,
        orgId: 3,
      }),
    }

    // First drop
    fireEvent(draggableRow, createDragEvent("dragstart", dragPayload))
    fireEvent(projectB, createDragEvent("drop", dragPayload))

    // Try another drop while first is pending
    fireEvent(draggableRow, createDragEvent("dragstart", dragPayload))
    fireEvent(projectB, createDragEvent("drop", dragPayload))

    // Should only have called PATCH once
    await waitFor(() => expect(patchCount).toBe(1))

    // Complete the first move
    resolvePatch!()
    await waitFor(() => expect(patchCount).toBe(1))
    expect(mock.unknown).toEqual([])
  })

  it("updates sidebar caches after a successful move", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
        patchResource: () =>
          json({
            ...chatResource,
            nodeId: 8, // Moved to Project B
            authRevision: "2",
          }),
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project B" })
    )

    const chatRow = await within(sidebar).findByRole("button", {
      name: "Team planning",
    })
    const draggableRow = chatRow.closest("[draggable]")!
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    // Verify chat is in Project A initially
    const projectATree = within(sidebar)
      .getByRole("button", { name: "Project A" })
      .closest("li")!
    expect(
      within(projectATree).getByRole("button", { name: "Team planning" })
    ).toBeInTheDocument()

    // Perform the move
    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )
    fireEvent(
      projectB,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    // After move, chat should appear under Project B
    await waitFor(() => {
      const projectBTree = within(sidebar)
        .getByRole("button", { name: "Project B" })
        .closest("li")!
      expect(
        within(projectBTree).queryByRole("button", { name: "Team planning" })
      ).toBeInTheDocument()
    })

    // And should no longer be in Project A
    await waitFor(() => {
      const projectATree2 = within(sidebar)
        .getByRole("button", { name: "Project A" })
        .closest("li")!
      expect(
        within(projectATree2).queryByRole("button", { name: "Team planning" })
      ).not.toBeInTheDocument()
    })

    expect(mock.unknown).toEqual([])
  })

  it("displays error when move fails", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
        patchResource: () => json({ code: "conflict" }, 409),
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await within(sidebar).findByRole("button", {
      name: "Team planning",
    })
    const draggableRow = chatRow.closest("[draggable]")!
    const projectB = within(sidebar).getByRole("button", { name: "Project B" })

    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )
    fireEvent(
      projectB,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    // Should display error
    await waitFor(() => {
      expect(screen.getByRole("alert")).toBeInTheDocument()
    })
    expect(mock.unknown).toEqual([])
  })

  it("supports moving to a subfolder within the same project", async () => {
    let patchBody: unknown
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
        patchResource: (init) => {
          patchBody = JSON.parse(init.body as string)
          return json({
            ...chatResource,
            nodeId: 9, // Moved to Subfolder
            authRevision: "2",
          })
        },
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    const sidebar = await getSidebar()
    await user.click(
      within(sidebar).getByRole("button", { name: "Expand Project A" })
    )

    const chatRow = await within(sidebar).findByRole("button", {
      name: "Team planning",
    })
    const draggableRow = chatRow.closest("[draggable]")!
    const subfolder = within(sidebar).getByRole("button", { name: "Subfolder" })

    // Move to subfolder
    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )
    fireEvent(
      subfolder,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    await waitFor(() => expect(patchBody).toBeDefined())
    expect(patchBody).toMatchObject({
      nodeId: 9, // Subfolder ID
      expectedAuthRevision: "1",
    })
    expect(mock.unknown).toEqual([])
  })

  it("updates active tab metadata when moving the open item", async () => {
    const mock = mockNativeSession(orgSession, {
      ...twoProjectsRoutes({
        chats: [chatResource],
        patchResource: () =>
          json({
            ...chatResource,
            nodeId: 8, // Moved to Project B
            authRevision: "2",
          }),
      }),
    })
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    await user.click(
      await screen.findByRole("button", { name: "Expand Project A" })
    )

    // Open the chat in a tab
    const chatRow = await screen.findByRole("button", { name: "Team planning" })
    await user.click(chatRow)

    // Verify tab is open
    expect(
      screen.getByRole("tab", { name: /Team planning/ })
    ).toBeInTheDocument()

    // Now move it via drag and drop
    const draggableRow = chatRow.closest("[draggable]")!
    const projectB = screen.getByRole("button", { name: "Project B" })

    fireEvent(
      draggableRow,
      createDragEvent("dragstart", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )
    fireEvent(
      projectB,
      createDragEvent("drop", {
        type: "application/x-neev-move",
        data: JSON.stringify({
          kind: "resource",
          id: RESOURCE_ID,
          expectedRevision: "1",
          nodeId: 7,
          orgId: 3,
        }),
      })
    )

    // Tab should still be there, content should not remount
    await waitFor(() => {
      expect(
        screen.getByRole("tab", { name: /Team planning/ })
      ).toBeInTheDocument()
    })
    expect(mock.unknown).toEqual([])
  })
})

describe("Move to menu fallback", () => {
  it("provides a Move to menu option as keyboard/mobile fallback", async () => {
    const mock = mockNativeSession(
      orgSession,
      twoProjectsRoutes({
        chats: [chatResource],
      })
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    await user.click(
      await screen.findByRole("button", { name: "Expand Project A" })
    )

    // Open context menu on the chat
    const actionsButton = await screen.findByRole("button", {
      name: /Actions for Team planning/i,
    })
    await user.click(actionsButton)

    // Should have a Move to option
    const moveOption = await screen.findByRole("menuitem", {
      name: "Move to…",
    })
    expect(moveOption).toBeInTheDocument()

    expect(mock.unknown).toEqual([])
  })
})

function dropWithActualPayload(row: Element, target: Element) {
  const start = createDragEvent("dragstart")
  fireEvent(row, start)
  const drop = new Event("drop", { bubbles: true, cancelable: true })
  Object.defineProperty(drop, "dataTransfer", { value: start.dataTransfer })
  fireEvent(target, drop)
}

it("protects an unsent draft before relocating its open tab", async () => {
  const patch = vi.fn(() =>
    json({ ...chatResource, nodeId: 8, authRevision: "2" })
  )
  mockNativeSession(
    orgSession,
    twoProjectsRoutes({ chats: [chatResource], patchResource: patch })
  )
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("button", { name: "Expand Project A" })
  )
  const row = await screen.findByRole("button", { name: "Team planning" })
  await user.click(row)
  const composer = await screen.findByRole("textbox", { name: "Message" })
  await typeInComposer(composer, "Keep this unsent instruction")
  const target = within(
    screen.getByRole("navigation", { name: "Projects" })
  ).getByRole("button", { name: "Project B" })
  dropWithActualPayload(row.closest("[draggable]")!, target)
  await screen.findByRole("dialog", { name: "Discard unsaved changes?" })
  expect(patch).not.toHaveBeenCalled()
  await user.click(screen.getByRole("button", { name: "Keep editing" }))
  expect(composerText(composer)).toBe("Keep this unsent instruction")
  dropWithActualPayload(row.closest("[draggable]")!, target)
  await user.click(
    await screen.findByRole("button", { name: "Discard changes" })
  )
  await waitFor(() => expect(patch).toHaveBeenCalledTimes(1))
  await waitFor(() =>
    expect(screen.getByLabelText("Location")).toHaveTextContent("Project B")
  )
})

it.each([
  { authRevision: "2" },
  { nodeId: 9 },
  { archived: true },
  { creatorId: 2 },
])("refuses a stale or no-longer-movable resource %j", async (changed) => {
  const patch = vi.fn(() => json(chatResource))
  mockNativeSession(orgSession, {
    ...twoProjectsRoutes({ chats: [chatResource], patchResource: patch }),
    [`GET /api/neev/resources/${RESOURCE_ID}`]: () =>
      json({ ...chatResource, ...changed }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("button", { name: "Expand Project A" })
  )
  const row = await screen.findByRole("button", { name: "Team planning" })
  const target = within(
    screen.getByRole("navigation", { name: "Projects" })
  ).getByRole("button", { name: "Project B" })
  dropWithActualPayload(row.closest("[draggable]")!, target)
  await screen.findByRole("alert")
  expect(patch).not.toHaveBeenCalled()
})

it("does not move running chats or sessions, even with a forged drop", async () => {
  function Running() {
    usePublishRunningChat(orgSession, "chat", RESOURCE_ID, true)
    usePublishRunningChat(orgSession, "agent", SESSION_ID, true)
    return null
  }
  const patch = vi.fn(() => json(chatResource))
  const patchSession = vi.fn(() => json(agentSession))
  mockNativeSession(
    orgSession,
    twoProjectsRoutes({
      chats: [chatResource],
      sessions: [agentSession],
      patchResource: patch,
      patchSessionNode: patchSession,
    })
  )
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Running />
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("button", { name: "Expand Project A" })
  )
  for (const name of ["Team planning", "Parser session claude"]) {
    const row = await screen.findByRole("button", {
      name: new RegExp("^(?!Actions for).*" + name),
    })
    expect(row.closest("[draggable]")).toHaveAttribute("draggable", "false")
  }
  const target = within(
    screen.getByRole("navigation", { name: "Projects" })
  ).getByRole("button", { name: "Project B" })
  fireEvent(
    target,
    createDragEvent("drop", {
      type: "application/x-neev-move",
      data: JSON.stringify({
        kind: "resource",
        id: RESOURCE_ID,
        expectedRevision: "1",
        nodeId: 7,
        orgId: 3,
      }),
    })
  )
  await screen.findByRole("alert")
  expect(patch).not.toHaveBeenCalled()
  expect(patchSession).not.toHaveBeenCalled()
})
