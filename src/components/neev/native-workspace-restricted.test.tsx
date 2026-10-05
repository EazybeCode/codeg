/*
 * Restricted folders (contract v2): the reader sees exactly the nodes the
 * server returns. A folder whose parent is hidden is a "shared root", shown
 * under its own heading and never under, or named after, the hidden parent.
 * Items shared with the reader in hidden places open by id, with a breadcrumb
 * that says "Shared with you" instead of any folder name.
 */
import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeBoundary } from "./native-boundary"
import { NativeWorkspace } from "./native-workspace"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"
import { resetMenuLevels } from "./native-sharing"
import { readTabs, writeTabs } from "@/lib/neev-workspace-tabs"
import { act, fireEvent } from "@testing-library/react"

vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => false }))
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
  resetMenuLevels()
})

const OPEN = {
  id: 4,
  orgId: 3,
  parentId: null,
  kind: "project",
  name: "Roadmap",
  revision: "1",
}
// Its parent (99, "Secret board") is hidden from this reader: only the id.
const SHARED_ROOT = {
  id: 7,
  orgId: 3,
  parentId: 99,
  kind: "folder",
  name: "Launch assets",
  revision: "1",
}
const SHARED_DOC = {
  id: "55555555-5555-4555-8555-555555555555",
  orgId: 3,
  nodeId: 99,
  kind: "document",
  creatorId: 2,
  title: "Pricing memo",
  visibility: "shared",
  originChatId: null,
  authRevision: "2",
  contentRevision: "1",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}

function workspace(extra: Record<string, () => Response> = {}) {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/nodes": () =>
      json({ nodes: [OPEN, SHARED_ROOT], nextAfter: null }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/7/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/7/chats": () => json({ items: [], nextAfter: null }),
    ...extra,
  })
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
  return mock
}

it("shows a folder whose parent is hidden under Shared with me, never under its parent", async () => {
  workspace()
  const shared = await screen.findByRole("region", { name: "Shared with me" })
  expect(
    within(shared).getAllByRole("button", { name: /Launch assets/ }).length
  ).toBeGreaterThan(0)
  // The open project is in the ordinary tree, not in the shared group…
  expect(within(shared).queryByRole("button", { name: /Roadmap/ })).toBeNull()
  expect(
    (await screen.findAllByRole("button", { name: /Roadmap/ })).length
  ).toBeGreaterThan(0)
  // …and nothing ever names the hidden parent.
  expect(screen.queryByText(/Secret board/)).toBeNull()
})

it("opens an item shared in a hidden place by id, with no folder name in the path", async () => {
  const user = userEvent.setup()
  workspace({
    "GET /api/neev/shared/resources": () => json({ resources: [SHARED_DOC] }),
    [`GET /api/neev/resources/${SHARED_DOC.id}`]: () => json(SHARED_DOC),
    [`GET /api/neev/resources/${SHARED_DOC.id}/access`]: () =>
      json({
        resourceId: SHARED_DOC.id,
        visibility: "shared",
        authRevision: "2",
        level: "view",
        canManage: false,
        boundTo: null,
        direct: [],
        inherited: [],
      }),
    [`GET /api/neev/documents/${SHARED_DOC.id}/content`]: () =>
      json({
        document: null,
        markdown: "Draft",
        revision: {
          format: "markdown",
          formatVersion: 1,
          revision: "1",
          authorId: 2,
          authorName: "Ana",
          byteLength: "5",
          sha256: "a".repeat(64),
          createdAt: 0,
          parentRevision: null,
          restoredFrom: null,
        },
      }),
  })
  await user.click(
    await screen.findByRole("button", { name: /Shared with me/ })
  )
  await user.click(await screen.findByRole("button", { name: /Pricing memo/ }))
  const path = await screen.findByRole("navigation", { name: "Location" })
  await waitFor(() => expect(path).toHaveTextContent("Shared with you"))
  expect(path).toHaveTextContent("Pricing memo")
  expect(screen.queryByText(/Secret board/)).toBeNull()
})

it("does not mistake a folder whose parent is still loading for a shared one", async () => {
  mockNativeSession(undefined, {
    // A second page is still to come: the parent may be on it.
    "GET /api/neev/nodes": () =>
      json({ nodes: [OPEN, SHARED_ROOT], nextAfter: 7 }),
    // The next page has not arrived yet.
    "GET /api/neev/nodes?after=7": () => new Promise<Response>(() => {}),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
  })
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
  // The tree is drawn while more pages remain (page two waits for "Load
  // more"): the folder whose parent may be on it is not filed as shared.
  await screen.findAllByRole("button", { name: /Roadmap/ })
  expect(screen.queryByRole("region", { name: "Shared with me" })).toBeNull()
})

it("offers a View recipient of a folder nothing to create there", async () => {
  const user = userEvent.setup()
  workspace({
    "GET /api/neev/nodes/7/access": () =>
      json({
        nodeId: 7,
        revision: "1",
        canManage: false,
        canRestrict: false,
        restricted: true,
        level: "view",
        direct: [],
        inherited: [],
      }),
  })
  const shared = await screen.findByRole("region", { name: "Shared with me" })
  await user.click(
    within(shared).getByRole("button", { name: "Launch assets" })
  )
  expect(
    await screen.findByText(
      "You can view this folder. Ask its owner for Edit to add or change things here."
    )
  ).toBeVisible()
  expect(screen.queryByRole("button", { name: "New document" })).toBeNull()
})

/** Radix opens a menu on key events, not on a bare click in jsdom. */
async function openMenu(name: string) {
  const trigger = (await screen.findAllByRole("button", { name }))[0]
  await act(async () => {
    fireEvent.keyDown(trigger, { key: "Enter" })
  })
  return screen.findByRole("menu")
}

it("reads a folder's level only when its menu opens, once, and offers a View folder no writes", async () => {
  const { fetcher } = workspace({
    "GET /api/neev/nodes/4/access": () =>
      json({
        nodeId: 4,
        revision: "1",
        canManage: false,
        canRestrict: false,
        restricted: true,
        level: "view",
        direct: [],
        inherited: [],
      }),
  })
  await screen.findAllByRole("button", { name: /Roadmap/ })
  const accessReads = () =>
    fetcher.mock.calls.filter(([path]) =>
      /\/api\/neev\/nodes\/\d+\/access$/.test(String(path))
    ).length
  // Nothing on the initial load: no request per folder.
  expect(accessReads()).toBe(0)
  const menu = await openMenu("Actions for Roadmap")
  await waitFor(() =>
    expect(
      within(menu).getByRole("menuitem", { name: "New document" })
    ).toHaveAttribute("aria-disabled", "true")
  )
  expect(within(menu).getByText("You can view this folder only.")).toBeVisible()
  expect(
    within(menu).getByRole("menuitem", { name: "Edit details" })
  ).toHaveAttribute("aria-disabled", "true")
  expect(accessReads()).toBe(1)
  // Reopening uses what was read for this scope and revision.
  await act(async () => {
    fireEvent.keyDown(menu, { key: "Escape" })
  })
  await openMenu("Actions for Roadmap")
  expect(accessReads()).toBe(1)
})

it("enables a folder menu's writes once it is known to be editable", async () => {
  workspace()
  await screen.findAllByRole("button", { name: /Roadmap/ })
  const menu = await openMenu("Actions for Roadmap")
  await waitFor(() =>
    expect(
      within(menu).getByRole("menuitem", { name: "New document" })
    ).not.toHaveAttribute("aria-disabled")
  )
})

it("clears a restored tab whose item is no longer readable, keeping no title or content", async () => {
  const GONE = "66666666-6666-4666-8666-666666666666"
  writeTabs(1, 3, [{ kind: "resource", resourceId: GONE, nodeId: 4 }], 0)
  workspace({
    // Access was revoked (or its folder restricted) since the tab was saved.
    [`GET /api/neev/resources/${GONE}`]: () => json({ code: "not_found" }, 404),
    [`GET /api/neev/documents/${GONE}/content`]: () =>
      json({ code: "not_found" }, 404),
  })
  await screen.findAllByRole("button", { name: /Roadmap/ })
  await waitFor(() =>
    expect(
      screen
        .queryAllByRole("tab")
        .some((tab) => tab.textContent?.includes("Old secret title"))
    ).toBe(false)
  )
  expect(screen.queryByText("Old secret title")).toBeNull()
  expect(screen.queryByText("Secret body")).toBeNull()
  // The saved tab strip no longer points at the denied item.
  await waitFor(() =>
    expect(
      readTabs(1, 3).tabs.some(
        (tab) => tab.kind === "resource" && tab.resourceId === GONE
      )
    ).toBe(false)
  )
})

it("reads past pages that came back empty because every row on them is hidden", async () => {
  const user = userEvent.setup()
  const later = { ...OPEN, id: 30, name: "Later project" }
  mockNativeSession(undefined, {
    // Page one is all hidden rows: empty, but the cursor moves on.
    "GET /api/neev/nodes": () => json({ nodes: [], nextAfter: 3 }),
    "GET /api/neev/nodes?after=3": () => json({ nodes: [OPEN], nextAfter: 10 }),
    // "Load more" also meets an empty page before the next visible folder.
    "GET /api/neev/nodes?after=10": () => json({ nodes: [], nextAfter: 20 }),
    "GET /api/neev/nodes?after=20": () =>
      json({ nodes: [later], nextAfter: null }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/30/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/30/chats": () => json({ items: [], nextAfter: null }),
  })
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
  // Not "no projects": the empty first page was read past on its own.
  expect(
    (await screen.findAllByRole("button", { name: /Roadmap/ })).length
  ).toBeGreaterThan(0)
  expect(
    screen.queryByText("Create a project to give your team’s work a home.")
  ).toBeNull()
  await user.click(screen.getByRole("button", { name: "Load more" }))
  expect(
    (await screen.findAllByRole("button", { name: /Later project/ })).length
  ).toBeGreaterThan(0)
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Load more" })).toBeNull()
  )
})
