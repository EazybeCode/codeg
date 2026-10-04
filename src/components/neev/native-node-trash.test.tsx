import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { afterEach, expect, it, vi } from "vitest"
import type { NativeNode } from "@/lib/neev-client"
import {
  MoveNodeToTrashButton,
  NodeTrashHost,
  NodeTrashSection,
} from "./native-node-trash"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
afterEach(() => vi.unstubAllGlobals())

const folder: NativeNode = {
  id: 7,
  orgId: 1,
  parentId: 3,
  kind: "folder",
  name: "Launch",
  revision: "4",
}

/** The dialog's confirm button: the last "Move to Trash" on screen. */
function last<T>(items: T[]): T {
  return items[items.length - 1]
}

const clear = { agentActive: false, inUse: false, privateOthers: false }

function Workspace({ onTrashed = vi.fn() }: { onTrashed?: () => void }) {
  return (
    <NativeTestRoot>
      <MoveNodeToTrashButton node={folder} />
      <NodeTrashHost onTrashed={onTrashed} />
    </NativeTestRoot>
  )
}

it("is only offered while a host can confirm it", async () => {
  mockNativeSession()
  render(
    <NativeTestRoot>
      <MoveNodeToTrashButton node={folder} />
    </NativeTestRoot>
  )
  await waitFor(() => expect(document.body).toBeTruthy())
  expect(screen.queryByRole("button", { name: /Move to Trash/ })).toBeNull()
})

it("confirms with the contents, then moves the folder with its revision", async () => {
  const onTrashed = vi.fn()
  const { fetcher } = mockNativeSession(undefined, {
    "GET /api/neev/nodes/7/trash-preview": () =>
      json({
        folders: 2,
        documents: 3,
        chats: 1,
        agentChats: 2,
        blocked: clear,
      }),
    "POST /api/neev/nodes/7/trash": () =>
      json({ batchId: "b", nodes: 3, resources: 4 }),
  })
  render(<Workspace onTrashed={onTrashed} />)
  fireEvent.click(await screen.findByRole("button", { name: /Move to Trash/ }))
  expect(await screen.findByText("Move “Launch” to Trash?")).toBeVisible()
  const contents = await screen.findByTestId("trash-contents")
  expect(contents).toHaveTextContent("2 folders")
  expect(contents).toHaveTextContent("3 documents")
  expect(contents).toHaveTextContent("1 chat")
  expect(contents).toHaveTextContent("2 agent chats of yours")
  const confirm = last(
    screen.getAllByRole("button", { name: /Move to Trash/ })
  )!
  await waitFor(() => expect(confirm).toBeEnabled())
  fireEvent.click(confirm)
  await waitFor(() => expect(onTrashed).toHaveBeenCalledWith(folder))
  const call = fetcher.mock.calls.find(
    ([path, init]) =>
      path === "/api/neev/nodes/7/trash" && init?.method === "POST"
  )
  expect(JSON.parse(String(call?.[1]?.body))).toEqual({ expectedRevision: "4" })
  await waitFor(() =>
    expect(screen.queryByText("Move “Launch” to Trash?")).toBeNull()
  )
})

it("explains a running agent and does not let the trash proceed", async () => {
  const { fetcher } = mockNativeSession(undefined, {
    "GET /api/neev/nodes/7/trash-preview": () =>
      json({
        folders: 0,
        documents: 1,
        chats: 0,
        agentChats: 0,
        blocked: { ...clear, agentActive: true },
      }),
  })
  render(<Workspace />)
  fireEvent.click(await screen.findByRole("button", { name: /Move to Trash/ }))
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "An agent is still running in here"
  )
  expect(
    last(screen.getAllByRole("button", { name: /Move to Trash/ }))
  ).toBeDisabled()
  expect(
    fetcher.mock.calls.some(([path]) => path === "/api/neev/nodes/7/trash")
  ).toBe(false)
})

it("explains other members' private items without naming or counting them", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/7/trash-preview": () =>
      json({
        folders: 0,
        documents: 1,
        chats: 0,
        agentChats: 0,
        blocked: { ...clear, privateOthers: true },
      }),
  })
  render(<Workspace />)
  fireEvent.click(await screen.findByRole("button", { name: /Move to Trash/ }))
  const alert = await screen.findByRole("alert")
  expect(alert).toHaveTextContent(
    "Contains private content belonging to another member"
  )
  expect(alert.textContent).not.toMatch(/\d/)
})

it("re-checks when the server refuses after the preview", async () => {
  let previews = 0
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/7/trash-preview": () => {
      previews += 1
      return json({
        folders: 0,
        documents: 0,
        chats: 0,
        agentChats: 0,
        blocked: previews > 1 ? { ...clear, agentActive: true } : clear,
      })
    },
    "POST /api/neev/nodes/7/trash": () =>
      json({ code: "trash_blocked", reason: "agent_active" }, 409),
  })
  render(<Workspace />)
  fireEvent.click(await screen.findByRole("button", { name: /Move to Trash/ }))
  expect(await screen.findByText("It has nothing in it.")).toBeVisible()
  fireEvent.click(
    last(screen.getAllByRole("button", { name: /Move to Trash/ }))!
  )
  await waitFor(() => expect(previews).toBe(2))
  expect(
    await screen.findByText(/An agent is still running in here/)
  ).toBeVisible()
})

it("lists trashed folders and restores one", async () => {
  const onRestored = vi.fn()
  let restored = false
  const item = {
    nodeId: 7,
    kind: "folder",
    name: "Launch",
    parentId: 3,
    parentName: "Marketing",
    parentTrashed: false,
    trashedBy: 2,
    trashedByName: "Asha",
    trashedAt: 1,
    folders: 0,
    documents: 1,
    chats: 0,
    agentChats: 0,
  }
  const blocked = {
    ...item,
    nodeId: 9,
    name: "Child",
    parentId: 8,
    parentName: "Gone",
    parentTrashed: true,
  }
  const { fetcher } = mockNativeSession(undefined, {
    "GET /api/neev/trash/nodes": () =>
      json({ items: restored ? [blocked] : [item, blocked] }),
    "POST /api/neev/nodes/7/restore": () => {
      restored = true
      return json({ nodes: 1, resources: 1 })
    },
  })
  render(
    <NativeTestRoot>
      <NodeTrashSection onRestored={onRestored} />
    </NativeTestRoot>
  )
  expect(await screen.findByText("Launch")).toBeVisible()
  expect(screen.getByText("Restores to Marketing")).toBeVisible()
  expect(screen.getByText("Restore Gone first")).toBeVisible()
  expect(screen.getByRole("button", { name: "Restore Child" })).toBeDisabled()
  fireEvent.click(screen.getByRole("button", { name: "Restore Launch" }))
  await waitFor(() => expect(onRestored).toHaveBeenCalled())
  await waitFor(() => expect(screen.queryByText("Launch")).toBeNull())
  const call = fetcher.mock.calls.find(
    ([path]) => path === "/api/neev/nodes/7/restore"
  )
  expect(call?.[1]?.method).toBe("POST")
})
