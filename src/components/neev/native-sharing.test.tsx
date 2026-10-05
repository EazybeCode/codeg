import {
  act,
  render,
  renderHook,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import type { NativeResource } from "@/lib/neev-content-client"
import type { NeevAgentSession } from "@/lib/neev-agent-sessions-client"
import {
  ShareDialog,
  SharedSessionsList,
  useResourceLevel,
  type ShareTarget,
} from "./native-sharing"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
import { ANA, BEN, DESIGN_TEAM, serveDirectory } from "./sharing-test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

afterEach(() => vi.unstubAllGlobals())

const RID = "11111111-1111-4111-8111-111111111111"
const resource: NativeResource = {
  id: RID,
  orgId: 3,
  nodeId: 4,
  kind: "document",
  creatorId: 1,
  title: "Launch plan",
  visibility: "shared",
  originChatId: null,
  authRevision: "5",
  contentRevision: "1",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
} as NativeResource

const anaPrincipal = {
  kind: "user" as const,
  userId: 2,
  displayName: "Ana Lima",
  avatarUrl: null,
}
const access = (over: Record<string, unknown> = {}) => ({
  resourceId: RID,
  visibility: "shared",
  authRevision: "5",
  level: "owner",
  canManage: true,
  boundTo: null,
  direct: [],
  inherited: [],
  ...over,
})

function open(target: ShareTarget, title = "Launch plan") {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <ShareDialog
          open
          onOpenChange={() => {}}
          title={title}
          target={target}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

const teams = () => json({ teams: [DESIGN_TEAM] })

it("adds a person with View by default, upgrades to Edit, and removes them", async () => {
  const writes: { method: string; body: Record<string, unknown> }[] = []
  let current = access()
  const write = (init: RequestInit) => {
    const body = JSON.parse(init.body as string)
    writes.push({ method: init.method!, body })
    current =
      init.method === "DELETE"
        ? access({ authRevision: String(Number(current.authRevision) + 1) })
        : access({
            authRevision: String(Number(current.authRevision) + 1),
            direct: [{ principal: anaPrincipal, level: body.level }],
          })
    return json(current)
  }
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () => json(current),
    "GET /api/neev/teams": teams,
    [`PUT /api/neev/resources/${RID}/grants`]: write,
    [`DELETE /api/neev/resources/${RID}/grants`]: write,
  })
  serveDirectory([ANA, BEN])
  const user = userEvent.setup()
  open({ kind: "resource", resource, onVisibility: vi.fn() })
  // Shared with nobody named: it is Folder access.
  expect(
    await screen.findByRole("radio", { name: /^Folder access/ })
  ).toBeChecked()
  await user.type(
    screen.getByRole("searchbox", { name: "Add people or teams" }),
    "ana"
  )
  await user.click(await screen.findByRole("button", { name: /Ana Lima/ }))
  await waitFor(() => expect(writes).toHaveLength(1))
  expect(writes[0].body).toEqual({
    expectedAuthRevision: "5",
    principal: { kind: "user", userId: 2 },
    level: "view",
  })
  // Named now: Specific people & teams.
  expect(
    await screen.findByRole("radio", { name: /^Specific people/ })
  ).toBeChecked()
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Access for Ana Lima" }),
    "edit"
  )
  await waitFor(() => expect(writes).toHaveLength(2))
  expect(writes[1].body).toMatchObject({
    expectedAuthRevision: "6",
    level: "edit",
  })
  await user.click(screen.getByRole("button", { name: "Remove Ana Lima" }))
  await waitFor(() => expect(writes).toHaveLength(3))
  expect(writes[2]).toEqual({
    method: "DELETE",
    body: { expectedAuthRevision: "7", principal: { kind: "user", userId: 2 } },
  })
})

it("never implies removing a direct grant removes folder access", async () => {
  const fromFolder = {
    nodeId: 4,
    nodeName: "Marketing",
    principal: anaPrincipal,
    level: "view",
  }
  let current = access({
    direct: [{ principal: anaPrincipal, level: "edit" }],
    inherited: [fromFolder],
  })
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () => json(current),
    "GET /api/neev/teams": teams,
    [`DELETE /api/neev/resources/${RID}/grants`]: () => {
      current = access({ authRevision: "6", inherited: [fromFolder] })
      return json(current)
    },
  })
  serveDirectory([])
  const user = userEvent.setup()
  open({ kind: "resource", resource, onVisibility: vi.fn() })
  const inherited = await screen.findByRole("region", {
    name: "Access from folders",
  })
  expect(within(inherited).getByText("View · from Marketing")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Remove Ana Lima" }))
  expect(
    await screen.findByText(
      "Ana Lima can still open this through Marketing. Change that in the folder's sharing."
    )
  ).toBeVisible()
})

it("says a private item is an exception to folder access, and changes audience via the item's own update", async () => {
  const onVisibility = vi.fn()
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () =>
      json(
        access({
          visibility: "private",
          inherited: [
            {
              nodeId: 4,
              nodeName: "Marketing",
              principal: anaPrincipal,
              level: "edit",
            },
          ],
        })
      ),
    "GET /api/neev/teams": teams,
  })
  const user = userEvent.setup()
  open({
    kind: "resource",
    resource: { ...resource, visibility: "private" },
    onVisibility,
  })
  expect(
    await screen.findByText(
      "This item is private, so access to Marketing does not apply to it."
    )
  ).toBeVisible()
  // Private: nobody is named, and no picker is offered.
  expect(screen.queryByRole("searchbox")).toBeNull()
  expect(
    screen.queryByRole("region", { name: "Access from folders" })
  ).toBeNull()
  await user.click(screen.getByRole("radio", { name: /^Everyone in/ }))
  // Widening asks the full-history question before anything is sent.
  expect(onVisibility).not.toHaveBeenCalled()
  await user.click(screen.getByRole("button", { name: "Share full history" }))
  expect(onVisibility).toHaveBeenCalledWith("organization")
})

it("shows a non-owner who has access without offering any change", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () =>
      json(
        access({
          level: "view",
          canManage: false,
          direct: [{ principal: anaPrincipal, level: "view" }],
        })
      ),
  })
  open({
    kind: "resource",
    resource: { ...resource, creatorId: 9 },
    onVisibility: vi.fn(),
  })
  expect(
    await screen.findByText("Only the owner can change who has access.")
  ).toBeVisible()
  expect(screen.queryByRole("searchbox")).toBeNull()
  expect(screen.queryByRole("button", { name: /^Remove/ })).toBeNull()
  for (const radio of screen.getAllByRole("radio")) expect(radio).toBeDisabled()
})

it("names the contract's refusal when grants need Folder access first", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () => json(access()),
    "GET /api/neev/teams": teams,
    [`PUT /api/neev/resources/${RID}/grants`]: () =>
      json({ code: "resource_not_shared" }, 409),
  })
  serveDirectory([ANA])
  const user = userEvent.setup()
  open({ kind: "resource", resource, onVisibility: vi.fn() })
  await user.click(await screen.findByRole("button", { name: /Design/ }))
  expect(
    await screen.findByText(
      "Switch this item to Folder access or Specific people & teams before adding people."
    )
  ).toBeVisible()
})

it("shares a folder with a team and explains it only reaches Folder-access items", async () => {
  const bodies: Record<string, unknown>[] = []
  const node = {
    id: 4,
    orgId: 3,
    parentId: null,
    name: "Marketing",
    kind: "project",
  }
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/access": () =>
      json({
        nodeId: 4,
        revision: "2",
        canManage: true,
        canRestrict: true,
        restricted: false,
        level: "owner",
        direct: [],
        inherited: [],
      }),
    "GET /api/neev/teams": teams,
    "PUT /api/neev/nodes/4/grants": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json({
        nodeId: 4,
        revision: "3",
        canRestrict: true,
        restricted: false,
        level: "owner",
        canManage: true,
        direct: [
          {
            principal: {
              kind: "team",
              teamId: DESIGN_TEAM.id,
              name: "Design",
              memberCount: 2,
            },
            level: "view",
          },
        ],
        inherited: [],
      })
    },
  })
  serveDirectory([])
  const user = userEvent.setup()
  open({ kind: "node", node: node as never }, "Marketing")
  expect(
    await screen.findByText(
      /only items that use Folder access|items that use Folder access/
    )
  ).toBeVisible()
  await user.click(await screen.findByRole("button", { name: /Design/ }))
  await waitFor(() =>
    expect(bodies).toEqual([
      {
        expectedRevision: "2",
        principal: { kind: "team", teamId: DESIGN_TEAM.id },
        level: "view",
      },
    ])
  )
  // A folder's own audience is who can SEE it (v2), not an item audience.
  expect(screen.queryByRole("radio", { name: /^Folder access/ })).toBeNull()
  expect(screen.getByRole("radio", { name: /^Everyone in/ })).toBeChecked()
})

it("shares an Agent Chat View-only, with no Edit choice", async () => {
  const bodies: Record<string, unknown>[] = []
  const session = {
    id: "22222222-2222-4222-8222-222222222222",
    ownerId: 1,
    title: "Parser",
  }
  mockNativeSession(undefined, {
    [`GET /api/neev/agent/sessions/${session.id}/access`]: () =>
      json({
        sessionId: session.id,
        revision: "1",
        level: "owner",
        canManage: true,
        direct: [],
      }),
    "GET /api/neev/teams": () => json({ teams: [] }),
    [`PUT /api/neev/agent/sessions/${session.id}/grants`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json({
        sessionId: session.id,
        revision: "2",
        level: "owner",
        canManage: true,
        direct: [{ principal: anaPrincipal, level: "view" }],
      })
    },
  })
  serveDirectory([ANA])
  const user = userEvent.setup()
  open(
    { kind: "session", session: session as unknown as NeevAgentSession },
    "Parser"
  )
  expect(await screen.findByText(/never used for them/)).toBeVisible()
  await user.click(await screen.findByRole("button", { name: /Ana Lima/ }))
  await waitFor(() =>
    expect(bodies).toEqual([
      {
        expectedRevision: "1",
        principal: { kind: "user", userId: 2 },
        level: "view",
      },
    ])
  )
  expect(await screen.findByText("Ana Lima")).toBeVisible()
  expect(screen.queryByRole("combobox")).toBeNull()
})

it("reads the caller's level only when it can be below edit", async () => {
  let reads = 0
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () => {
      reads += 1
      return json(access({ level: "view", canManage: false }))
    },
  })
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <NativeTestRoot>{children}</NativeTestRoot>
  )
  // The owner, a private item, and an organization item: no request at all.
  const mine = renderHook(() => useResourceLevel(resource), { wrapper })
  await waitFor(() => expect(mine.result.current).toBe("owner"))
  const org = renderHook(
    () =>
      useResourceLevel({
        ...resource,
        creatorId: 9,
        visibility: "organization",
      }),
    { wrapper }
  )
  await waitFor(() => expect(org.result.current).toBe("edit"))
  expect(reads).toBe(0)
  // Someone else's shared item: one read, and View until it says otherwise.
  const theirs = renderHook(
    () => useResourceLevel({ ...resource, creatorId: 9 }),
    {
      wrapper,
    }
  )
  expect(theirs.result.current).toBe("unknown")
  await waitFor(() => expect(theirs.result.current).toBe("view"))
  expect(reads).toBe(1)
})

it("lists Agent Chats shared with me and opens one", async () => {
  const onOpen = vi.fn()
  mockNativeSession(orgSession, {
    "GET /api/neev/agent/shared-sessions": () =>
      json({
        sessions: [
          {
            session: {
              id: "33333333-3333-4333-8333-333333333333",
              orgId: 3,
              ownerId: 2,
              title: "Pricing research",
              provider: "claude",
              model: "default",
              effort: null,
              fast: false,
              boundChatId: null,
              nodeId: null,
              archived: false,
              revision: "1",
              createdAt: 1,
              updatedAt: 1,
            },
            owner: { userId: 2, displayName: "Ana Lima", avatarUrl: null },
          },
        ],
      }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <SharedSessionsList scopeKey="1:3:0" onOpen={onOpen} />
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("button", { name: /Shared with me/ })
  )
  await user.click(screen.getByRole("button", { name: /Pricing research/ }))
  expect(screen.getByText("Shared by Ana Lima")).toBeVisible()
  expect(onOpen).toHaveBeenCalledWith("33333333-3333-4333-8333-333333333333")
  await act(async () => {})
})

it("drops a late access answer for an item no longer open", async () => {
  const OTHER = "44444444-4444-4444-8444-444444444444"
  let answerFirst!: (response: Response) => void
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () =>
      new Promise<Response>((resolve) => {
        answerFirst = resolve
      }),
    [`GET /api/neev/resources/${OTHER}/access`]: () =>
      new Promise<Response>(() => {}),
  })
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <NativeTestRoot>{children}</NativeTestRoot>
  )
  const hook = renderHook(
    ({ id }) => useResourceLevel({ ...resource, id, creatorId: 9 }),
    { wrapper, initialProps: { id: RID } }
  )
  await waitFor(() => expect(answerFirst).toBeDefined())
  hook.rerender({ id: OTHER })
  // The first item's "edit" arrives after the reader moved on: still unknown,
  // so no write control is offered for the second item on its strength.
  await act(async () => answerFirst(json(access({ level: "edit" }))))
  expect(hook.result.current).toBe("unknown")
})

it("never shows one item's people while another's are loading", async () => {
  const OTHER = "44444444-4444-4444-8444-444444444444"
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${RID}/access`]: () =>
      json(access({ direct: [{ principal: anaPrincipal, level: "edit" }] })),
    [`GET /api/neev/resources/${OTHER}/access`]: () =>
      new Promise<Response>(() => {}),
    "GET /api/neev/teams": teams,
  })
  serveDirectory([])
  const tree = (id: string) => (
    <NativeTestRoot>
      <NativeBoundary>
        <ShareDialog
          open
          onOpenChange={() => {}}
          title="Item"
          target={{
            kind: "resource",
            resource: { ...resource, id },
            onVisibility: vi.fn(),
          }}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const view = render(tree(RID))
  expect(
    await screen.findByText("Ana Lima", undefined, { timeout: 4000 })
  ).toBeVisible()
  view.rerender(tree(OTHER))
  await waitFor(() => expect(screen.queryByText("Ana Lima")).toBeNull(), {
    timeout: 4000,
  })
  expect(screen.getByText("Loading who has access…")).toBeVisible()
})

// ---- v2: restricted folders ----

const folderNode = {
  id: 4,
  orgId: 3,
  parentId: null,
  name: "Marketing",
  kind: "project",
  revision: "2",
}
const nodeAccess = (over: Record<string, unknown> = {}) => ({
  nodeId: 4,
  revision: "2",
  canManage: true,
  canRestrict: true,
  restricted: false,
  level: "owner",
  direct: [],
  inherited: [],
  ...over,
})

it("restricts a folder at once, and asks before showing it to everyone again", async () => {
  const bodies: Record<string, unknown>[] = []
  let current = nodeAccess()
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/access": () => json(current),
    "GET /api/neev/teams": teams,
    "PUT /api/neev/nodes/4/restriction": (init) => {
      const body = JSON.parse(init.body as string)
      bodies.push(body)
      current = nodeAccess({
        restricted: body.restricted,
        revision: String(Number(current.revision) + 1),
      })
      return json(current)
    },
  })
  serveDirectory([])
  const user = userEvent.setup()
  open({ kind: "node", node: folderNode as never }, "Marketing")
  await user.click(
    await screen.findByRole("radio", { name: /^Only you and the people/ })
  )
  await waitFor(() =>
    expect(bodies).toEqual([{ expectedRevision: "2", restricted: true }])
  )
  expect(screen.getByRole("radio", { name: /^Only you/ })).toBeChecked()
  // Re-opening widens: nothing is sent until it is confirmed.
  await user.click(screen.getByRole("radio", { name: /^Everyone in/ }))
  const question = screen.getByRole("group", {
    name: "Show this folder to everyone?",
  })
  expect(bodies).toHaveLength(1)
  await user.click(
    within(question).getByRole("button", { name: "Show to everyone" })
  )
  await waitFor(() =>
    expect(bodies[1]).toEqual({ expectedRevision: "3", restricted: false })
  )
})

it("shows a folder recipient their level, with no restriction or sharing control", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/access": () =>
      json(
        nodeAccess({
          canManage: false,
          canRestrict: false,
          restricted: true,
          level: "view",
        })
      ),
  })
  open({ kind: "node", node: folderNode as never }, "Marketing")
  expect(await screen.findByText("Your access: View")).toBeVisible()
  for (const radio of screen.getAllByRole("radio")) expect(radio).toBeDisabled()
  expect(screen.queryByRole("searchbox")).toBeNull()
})

it("says what organization means for an item whose folder is restricted or hidden", async () => {
  for (const folder of ["restricted", "hidden"] as const) {
    mockNativeSession(undefined, {
      [`GET /api/neev/resources/${RID}/access`]: () =>
        json(access({ visibility: "organization" })),
      "GET /api/neev/nodes/4/access": () =>
        folder === "hidden"
          ? json({ code: "not_found" }, 404)
          : json(nodeAccess({ restricted: true })),
    })
    const view = open({
      kind: "resource",
      resource: { ...resource, visibility: "organization" },
      onVisibility: vi.fn(),
    })
    expect(
      await screen.findByRole("radio", {
        name: /^Everyone who can open its folder/,
      })
    ).toBeChecked()
    expect(screen.queryByRole("radio", { name: /^Everyone in/ })).toBeNull()
    view.unmount()
    vi.unstubAllGlobals()
  }
})

it("lets someone who may share a folder, but not restrict it, change only its people", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/access": () =>
      json(nodeAccess({ canManage: true, canRestrict: false, level: "edit" })),
    "GET /api/neev/teams": teams,
  })
  serveDirectory([])
  open({ kind: "node", node: folderNode as never }, "Marketing")
  // Restriction belongs to the creator (or the restricted root's creator).
  const restrict = await screen.findByRole("radio", { name: /^Only you/ })
  expect(restrict).toBeDisabled()
  expect(screen.getByRole("radio", { name: /^Everyone in/ })).toBeDisabled()
  // Sharing with people is still theirs to do.
  expect(
    screen.getByRole("searchbox", { name: "Add people or teams" })
  ).toBeEnabled()
})
