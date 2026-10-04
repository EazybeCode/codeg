import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeBoundary } from "./native-boundary"
import { NativeWorkspace } from "./native-workspace"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"

vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => false }))
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

const PROJECT = {
  id: 4,
  orgId: 3,
  parentId: null,
  kind: "project",
  name: "Roadmap",
  revision: "1",
}
const FOLDER = {
  id: 5,
  orgId: 3,
  parentId: 4,
  kind: "folder",
  name: "Launch",
  revision: "2",
}
const NESTED = {
  id: 6,
  orgId: 3,
  parentId: 5,
  kind: "folder",
  name: "Assets",
  revision: "1",
}

it("moves a folder to the trash from the sidebar and restores it in place", async () => {
  const user = userEvent.setup()
  localStorage.setItem("neev.ws.section", "docs")
  let trashed = false
  const empty = () => json({ items: [], nextAfter: null })
  const { fetcher } = mockNativeSession(undefined, {
    "GET /api/neev/nodes": () =>
      json({
        nodes: trashed ? [PROJECT] : [PROJECT, FOLDER, NESTED],
        nextAfter: null,
      }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": empty,
    "GET /api/neev/nodes/4/chats": empty,
    "GET /api/neev/nodes/5/documents": empty,
    "GET /api/neev/nodes/5/chats": empty,
    "GET /api/neev/nodes/4/documents?archived=true": empty,
    "GET /api/neev/nodes/4/chats?archived=true": empty,
    "GET /api/neev/attachments?archived=true": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/5/trash-preview": () =>
      json({
        folders: 1,
        documents: 0,
        chats: 0,
        agentChats: 0,
        blocked: { agentActive: false, inUse: false, privateOthers: false },
      }),
    "POST /api/neev/nodes/5/trash": () => {
      trashed = true
      return json({ batchId: "b", nodes: 2, resources: 0 })
    },
    "GET /api/neev/trash/nodes": () =>
      json({
        items: trashed
          ? [
              {
                nodeId: 5,
                kind: "folder",
                name: "Launch",
                parentId: 4,
                parentName: "Roadmap",
                parentTrashed: false,
                trashedBy: 1,
                trashedByName: "Owner",
                trashedAt: 1,
                folders: 1,
                documents: 0,
                chats: 0,
                agentChats: 0,
              },
            ]
          : [],
      }),
    "POST /api/neev/nodes/5/restore": () => {
      trashed = false
      return json({ nodes: 2, resources: 0 })
    },
  })
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
  // Open the project so its folder row is in the sidebar.
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  await user.click(
    await screen.findByRole("button", { name: "Actions for Launch" })
  )
  await user.click(
    await screen.findByRole("menuitem", { name: /Move to Trash/ })
  )
  const dialog = await screen.findByRole("dialog")
  expect(within(dialog).getByText("Move “Launch” to Trash?")).toBeVisible()
  expect(await within(dialog).findByText("1 folder")).toBeVisible()
  await user.click(
    within(dialog).getByRole("button", { name: /Move to Trash/ })
  )
  await waitFor(() =>
    expect(
      screen.queryByRole("button", { name: "Actions for Launch" })
    ).toBeNull()
  )
  expect(
    fetcher.mock.calls.some(
      ([path, init]) =>
        path === "/api/neev/nodes/5/trash" && init?.method === "POST"
    )
  ).toBe(true)

  await user.click(screen.getByRole("button", { name: "Show trash" }))
  const section = await screen.findByRole("region", {
    name: "Projects and folders",
  })
  expect(await within(section).findByText("Restores to Roadmap")).toBeVisible()
  await user.click(
    within(section).getByRole("button", { name: "Restore Launch" })
  )
  expect(
    await screen.findByRole("button", { name: "Actions for Launch" })
  ).toBeVisible()
})
