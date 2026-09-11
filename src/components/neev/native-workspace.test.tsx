import { render, screen, within, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeBoundary } from "./native-boundary"
import { NativeWorkspace } from "./native-workspace"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"
const viewport = vi.hoisted(() => ({ mobile: false }))
vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => viewport.mobile }))
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
afterEach(() => {
  vi.unstubAllGlobals()
  viewport.mobile = false
})
it("shows the person and organization without GitHub or chat controls", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes": () => json({ nodes: [], nextAfter: null }),
  })
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
  expect(screen.getByText("Agent chats are being enabled")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("supports keyboard project creation, nested placement and dialog focus restoration", async () => {
  const bodies: unknown[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes": () => json({ nodes: [], nextAfter: null }),
    "GET /api/neev/nodes/1/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/2/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/3/chats": () => json({ items: [], nextAfter: null }),
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
  expect(
    screen.getByRole("navigation", { name: "Folder path" })
  ).toHaveTextContent("LaunchResearch")
  await user.click(screen.getByRole("button", { name: "Actions for Research" }))
  await user.click(
    await screen.findByRole("menuitem", { name: "New subfolder" })
  )
  await user.type(screen.getByLabelText("Name"), "Interviews")
  await user.click(screen.getByRole("button", { name: "Save" }))
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(bodies[2]).toEqual({ kind: "folder", name: "Interviews", parentId: 2 })
  expect(
    screen.getByRole("navigation", { name: "Folder path" })
  ).toHaveTextContent("LaunchResearchInterviews")
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

it("opens content creation for the context-menu node and returns focus after cancellation", async () => {
  const mock = mockNativeSession(undefined, {
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
  expect(screen.getByRole("dialog")).toHaveTextContent("Inside Menu project")
  expect(screen.getByRole("textbox", { name: "Title" })).toHaveFocus()
  await user.keyboard("{Escape}")
  expect(trigger).toHaveFocus()
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
    "GET /api/neev/nodes": () => json({ nodes, nextAfter: null }),
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
