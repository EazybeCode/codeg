import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
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

// Disposable fixtures only. The signed-in user is 1, and is an org ADMIN
// (`orgSession.role`), which matters: admin must not unlock anyone else's item.
const NODE = {
  id: 4,
  orgId: 3,
  parentId: null,
  kind: "project",
  name: "Roadmap",
  revision: "1",
}
const base = {
  orgId: 3,
  nodeId: 4,
  kind: "document" as const,
  creatorId: 1,
  visibility: "private" as const,
  originChatId: null as string | null,
  authRevision: "3",
  contentRevision: "1",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}
const MINE_PRIVATE = {
  ...base,
  id: "11111111-1111-4111-8111-111111111111",
  title: "Fixture private doc",
}
const MINE_SHARED = {
  ...base,
  id: "22222222-2222-4222-8222-222222222222",
  title: "Fixture shared doc",
  visibility: "organization" as const,
}
const THEIRS_SHARED = {
  ...base,
  id: "33333333-3333-4333-8333-333333333333",
  title: "Colleague shared doc",
  creatorId: 2,
  visibility: "organization" as const,
}
const CHAT_ID = "44444444-4444-4444-8444-444444444444"
const MINE_CHAT = {
  ...base,
  id: CHAT_ID,
  kind: "chat" as const,
  title: "Fixture team chat",
}
const GENERATED = {
  ...base,
  id: "55555555-5555-4555-8555-555555555555",
  title: "Generated attachment",
  originChatId: CHAT_ID,
}

function workspace(
  patch?: (id: string, body: Record<string, unknown>) => Response
) {
  localStorage.setItem("neev.ws.section", "docs")
  const patches: { id: string; body: Record<string, unknown> }[] = []
  // What the server answers for the chat's generated file: its chat's audience.
  let chatAudience: "private" | "organization" = "private"
  const all = [MINE_PRIVATE, MINE_SHARED, THEIRS_SHARED, MINE_CHAT, GENERATED]
  const routes: Record<string, (init?: RequestInit) => Response> = {
    "GET /api/neev/nodes": () => json({ nodes: [NODE], nextAfter: null }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({
        items: [
          MINE_PRIVATE,
          MINE_SHARED,
          THEIRS_SHARED,
          { ...GENERATED, visibility: chatAudience },
        ],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [MINE_CHAT], nextAfter: null }),
  }
  for (const item of all)
    routes[`PATCH /api/neev/resources/${item.id}`] = (init) => {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>
      patches.push({ id: item.id, body })
      if (item.id === CHAT_ID)
        chatAudience = body.visibility as "private" | "organization"
      if (patch) return patch(item.id, body)
      return json({
        ...item,
        visibility: body.visibility,
        authRevision: String(Number(item.authRevision) + 1),
      })
    }
  const mock = mockNativeSession(
    undefined,
    routes as Parameters<typeof mockNativeSession>[1]
  )
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
  return { patches, mock }
}

/** The folder's own list, not the sidebar tree. */
async function folder(user: ReturnType<typeof userEvent.setup>) {
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  return within(await screen.findByRole("list", { name: "Roadmap" }))
}

it("shares the creator's private document from the folder, after the full-history question", async () => {
  const user = userEvent.setup()
  const { patches } = workspace()
  const list = await folder(user)
  await user.click(
    await list.findByRole("button", { name: "Share Fixture private doc" })
  )
  // Nothing is sent until the reader has read what sharing means.
  expect(patches).toEqual([])
  const dialog = await screen.findByRole("dialog", {
    name: "Share full history?",
  })
  expect(dialog).toHaveTextContent(
    "Everyone in Neev demo will be able to read this resource and its full history."
  )
  await user.click(
    within(dialog).getByRole("button", { name: "Share full history" })
  )
  await waitFor(() => expect(patches).toHaveLength(1))
  // The same PATCH the header sends: only the visibility moves, and the
  // revision read with the row guards against a concurrent change.
  expect(patches[0]).toEqual({
    id: MINE_PRIVATE.id,
    body: {
      nodeId: 4,
      title: "Fixture private doc",
      visibility: "organization",
      archived: false,
      expectedAuthRevision: "3",
    },
  })
  // The row now offers the way back, described by the existing warning.
  const back = await list.findByRole("button", {
    name: "Make Fixture private doc private",
  })
  expect(back).toHaveAccessibleDescription(
    "Making this private blocks future access, but copies that people already downloaded cannot be recalled."
  )
})

it("sends nothing when the question is cancelled", async () => {
  const user = userEvent.setup()
  const { patches } = workspace()
  const list = await folder(user)
  await user.click(
    await list.findByRole("button", { name: "Share Fixture private doc" })
  )
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Cancel",
    })
  )
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(patches).toEqual([])
})

it("makes the creator's shared item private as an explicit action", async () => {
  const user = userEvent.setup()
  const { patches } = workspace()
  const list = await folder(user)
  await user.click(
    await list.findByRole("button", { name: "Make Fixture shared doc private" })
  )
  await waitFor(() => expect(patches).toHaveLength(1))
  expect(patches[0].body.visibility).toBe("private")
  expect(
    await list.findByRole("button", { name: "Share Fixture shared doc" })
  ).toBeInTheDocument()
})

it("shares a team chat from the folder the same way", async () => {
  const user = userEvent.setup()
  const { patches } = workspace()
  const list = await folder(user)
  await user.click(
    await list.findByRole("button", { name: "Share Fixture team chat" })
  )
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Share full history",
    })
  )
  await waitFor(() => expect(patches).toHaveLength(1))
  expect(patches[0]).toMatchObject({
    id: CHAT_ID,
    body: { visibility: "organization" },
  })
  // Its generated file follows: re-read, it now shows as shared, still with
  // no control of its own.
  const generated = (
    await list.findByRole("button", { name: /Generated attachment/ })
  ).closest("li") as HTMLElement
  expect(
    await within(generated).findByRole("img", {
      name: "Shared with organization",
    })
  ).toBeInTheDocument()
})

/**
 * The signed-in user is an organization admin. The server still refuses a
 * visibility change on someone else's item, so the list offers none — only
 * that it is shared.
 */
it("offers an admin no control over a colleague's item", async () => {
  const user = userEvent.setup()
  const { patches } = workspace()
  const list = await folder(user)
  await list.findByRole("button", { name: /Colleague shared doc/ })
  expect(
    list.queryByRole("button", { name: "Make Colleague shared doc private" })
  ).toBeNull()
  expect(
    list.queryByRole("button", { name: "Share Colleague shared doc" })
  ).toBeNull()
  const row = list
    .getByRole("button", { name: /Colleague shared doc/ })
    .closest("li") as HTMLElement
  expect(
    within(row).getByRole("img", { name: "Shared with organization" })
  ).toBeInTheDocument()
  expect(patches).toEqual([])
})

/** A file made in a chat follows the chat; it has no audience of its own. */
it("gives a chat's generated file no sharing control of its own", async () => {
  const user = userEvent.setup()
  workspace()
  const list = await folder(user)
  await list.findByRole("button", { name: /Generated attachment/ })
  expect(
    list.queryByRole("button", { name: "Share Generated attachment" })
  ).toBeNull()
  expect(
    list.queryByRole("button", { name: "Make Generated attachment private" })
  ).toBeNull()
})

it("keeps the question open and says why when the server refuses", async () => {
  const user = userEvent.setup()
  const { patches } = workspace(() => json({ code: "conflict" }, 409))
  const list = await folder(user)
  await user.click(
    await list.findByRole("button", { name: "Share Fixture private doc" })
  )
  const dialog = await screen.findByRole("dialog")
  await user.click(
    within(dialog).getByRole("button", { name: "Share full history" })
  )
  await waitFor(() => expect(patches).toHaveLength(1))
  expect(await within(dialog).findByRole("alert")).toBeInTheDocument()
  // Still private: nothing changed on the row (behind the modal, so hidden
  // from the accessibility tree while the question is open).
  expect(
    list.getByRole("button", {
      name: "Share Fixture private doc",
      hidden: true,
    })
  ).toBeInTheDocument()
})

// ---------------------------------------------------------------------------
// Sharing a folder's existing items. Not a folder permission: the folder is
// already visible to the organization, and items made later stay private.
// ---------------------------------------------------------------------------

const colleague = (n: number) => ({
  ...base,
  id: `66666666-6666-4666-8666-${n.toString(16).padStart(12, "0")}`,
  title: `Colleague item ${n}`,
  creatorId: 2,
  visibility: "organization" as const,
})
const PAGE_TWO_MINE = {
  ...base,
  id: "77777777-7777-4777-8777-777777777777",
  title: "Second page private doc",
}
const CHILD = {
  id: 6,
  orgId: 3,
  parentId: 4,
  kind: "folder",
  name: "Specs",
  revision: "1",
}
const CHILD_MINE = {
  ...base,
  id: "88888888-8888-4888-8888-888888888888",
  nodeId: 6,
  title: "Subfolder private doc",
}

/** A folder's new-items setting, as the server would answer it. */
const policy = (over: Record<string, unknown> = {}) => ({
  nodeId: 4,
  revision: "1",
  defaultVisibility: "inherit",
  effectiveVisibility: "private",
  inheritedFrom: null,
  canManage: false,
  ...over,
})

/**
 * A timeout for the folder flows built on the 100-row fixture.
 *
 * The fixture is deliberately a full first page (so the real client accepts a
 * cursor and pagination is exercised for real), and every role query walks
 * that whole tree. On a quiet host these finish in 1-4 s; under a concurrent
 * cargo build they were seen to exceed vitest's 5 s default without any
 * assertion failing. This is headroom for load, not a wait for anything.
 */
const HEAVY = 20_000

function folderWorkspace(
  patch: (id: string, attempt: number) => Response | null = () => null,
  options: {
    sharing?: Record<number, () => Response>
    saveSharing?: (body: Record<string, unknown>) => Response
    create?: (kind: string, body: Record<string, unknown>) => Response
    routes?: Record<string, (init?: RequestInit) => Response>
  } = {}
) {
  localStorage.setItem("neev.ws.section", "docs")
  // A full first page, so the real client accepts the cursor: 98 colleague
  // rows plus two of mine. The rest is on page two.
  const firstPage = [
    ...Array.from({ length: 98 }, (_, n) => colleague(n + 1)),
    MINE_PRIVATE,
    MINE_SHARED,
  ]
  const attempts = new Map<string, number>()
  const routes: Record<string, (init?: RequestInit) => Response> = {
    "GET /api/neev/nodes": () =>
      json({ nodes: [NODE, CHILD], nextAfter: null }),
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: firstPage, nextAfter: "p2" }),
    "GET /api/neev/nodes/4/documents?after=p2": () =>
      json({
        items: [PAGE_TWO_MINE, GENERATED, colleague(99)],
        nextAfter: null,
      }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [MINE_CHAT], nextAfter: null }),
    "GET /api/neev/nodes/6/documents": () =>
      json({ items: [CHILD_MINE], nextAfter: null }),
    "GET /api/neev/nodes/6/chats": () => json({ items: [], nextAfter: null }),
    // Not a manager by default: these tests are about existing items.
    "GET /api/neev/nodes/4/sharing":
      options.sharing?.[4] ?? (() => json(policy())),
    "GET /api/neev/nodes/6/sharing":
      options.sharing?.[6] ?? (() => json(policy({ nodeId: 6 }))),
  }
  const sharingSaves: Record<string, unknown>[] = []
  routes["PATCH /api/neev/nodes/4/sharing"] = (init) => {
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>
    sharingSaves.push(body)
    return options.saveSharing
      ? options.saveSharing(body)
      : json({ code: "forbidden" }, 403)
  }
  Object.assign(routes, options.routes ?? {})
  const creates: { kind: string; body: Record<string, unknown> }[] = []
  for (const kind of ["documents", "chats"])
    routes[`POST /api/neev/nodes/4/${kind}`] = (init) => {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>
      creates.push({ kind, body })
      return options.create
        ? options.create(kind, body)
        : json(
            {
              ...base,
              id: "99999999-9999-4999-8999-999999999999",
              kind: kind === "documents" ? "document" : "chat",
              title: String(body.title),
              visibility: body.visibility ?? "private",
            },
            201
          )
    }
  for (const row of [MINE_PRIVATE, PAGE_TWO_MINE, MINE_CHAT, CHILD_MINE])
    routes[`PATCH /api/neev/resources/${row.id}`] = (init) => {
      const attempt = (attempts.get(row.id) ?? 0) + 1
      attempts.set(row.id, attempt)
      const override = patch(row.id, attempt)
      if (override) return override
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>
      return json({ ...row, visibility: body.visibility, authRevision: "4" })
    }
  const { fetcher } = mockNativeSession(
    undefined,
    routes as Parameters<typeof mockNativeSession>[1]
  )
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeWorkspace />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const writes = () =>
    fetcher.mock.calls
      .map(
        ([path, init]) => `${(init as RequestInit)?.method ?? "GET"} ${path}`
      )
      .filter((call) => !call.startsWith("GET "))
  return { attempts, writes, sharingSaves, creates }
}

async function openFolderShare(user: ReturnType<typeof userEvent.setup>) {
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  await user.click(
    await screen.findByRole("button", { name: "Share items in Roadmap" })
  )
  return screen.findByRole("dialog", { name: "Share Roadmap" })
}

/** The optional second step: the reader's existing private items. */
async function includeExisting(
  user: ReturnType<typeof userEvent.setup>,
  dialog: ReturnType<typeof within>
) {
  await user.click(
    await dialog.findByRole("checkbox", {
      name: "Also share my existing private items",
    })
  )
}

it(
  "previews every one of the caller's private items, across pages, before sharing any",
  async () => {
    const user = userEvent.setup()
    const { writes } = folderWorkspace()
    const dialog = within(await openFolderShare(user))
    // What the folder is, and what this is not.
    expect(
      dialog.getByText(
        "Roadmap and its subfolders are already visible to everyone in Neev demo. Each item inside is private or shared on its own."
      )
    ).toBeInTheDocument()
    // Not a manager: the new-items setting is shown, not offered.
    expect(
      await dialog.findByText(
        "Only the folder's creator or an admin can change this."
      )
    ).toBeInTheDocument()
    // Existing items are a separate, optional step, off until asked for.
    expect(
      dialog.getByRole("checkbox", {
        name: "Also share my existing private items",
      })
    ).not.toBeChecked()
    expect(dialog.getByRole("button", { name: "Apply" })).toBeDisabled()
    await includeExisting(user, dialog)
    expect(
      dialog.getByText(
        "Everyone in Neev demo will be able to read the shared items and their full history. Documents attached to a shared chat follow it."
      )
    ).toBeInTheDocument()
    expect(
      dialog.getByRole("checkbox", { name: "Include subfolders" })
    ).not.toBeChecked()
    expect(
      await dialog.findByText("3 of your private items will be shared:")
    ).toBeInTheDocument()
    expect(
      dialog.getByText("Only items directly in Roadmap.")
    ).toBeInTheDocument()
    const listed = dialog
      .getByRole("list", { name: "Also share my existing private items" })
      .textContent?.split(/(?=Fixture|Second)/)
      .filter(Boolean)
    expect(listed).toEqual([
      "Fixture private doc",
      "Second page private doc",
      "Fixture team chat",
    ])
    // Nobody else's items — not even their titles — and no generated file.
    expect(dialog.queryByText(/Colleague item/)).toBeNull()
    expect(dialog.queryByText("Generated attachment")).toBeNull()
    expect(writes()).toEqual([])

    await user.click(dialog.getByRole("button", { name: "Share 3 items" }))
    expect(await dialog.findByText("3 items shared.")).toBeInTheDocument()
    // Only per-item PATCHes: no folder write, no default for later items.
    expect(writes().sort()).toEqual(
      [
        `PATCH /api/neev/resources/${MINE_PRIVATE.id}`,
        `PATCH /api/neev/resources/${PAGE_TWO_MINE.id}`,
        `PATCH /api/neev/resources/${CHAT_ID}`,
      ].sort()
    )
  },
  HEAVY
)

it(
  "says exactly what failed and retries only that",
  async () => {
    const user = userEvent.setup()
    const { attempts } = folderWorkspace((id, attempt) =>
      id === PAGE_TWO_MINE.id && attempt === 1
        ? json({ code: "unavailable" }, 503)
        : null
    )
    const dialog = within(await openFolderShare(user))
    await includeExisting(user, dialog)
    await user.click(
      await dialog.findByRole("button", { name: "Share 3 items" })
    )
    expect(await dialog.findByText("2 items shared.")).toBeInTheDocument()
    expect(dialog.getByText("1 could not be shared.")).toBeInTheDocument()
    // Never "all shared" while one is not.
    expect(dialog.queryByText("3 items shared.")).toBeNull()

    await user.click(dialog.getByRole("button", { name: "Retry failed" }))
    expect(await dialog.findByText("3 items shared.")).toBeInTheDocument()
    expect(dialog.queryByText(/could not be shared/)).toBeNull()
    expect(attempts.get(PAGE_TWO_MINE.id)).toBe(2)
    expect(attempts.get(MINE_PRIVATE.id)).toBe(1)
    expect(attempts.get(CHAT_ID)).toBe(1)
  },
  HEAVY
)

it(
  "reaches into subfolders only when asked",
  async () => {
    const user = userEvent.setup()
    const { writes } = folderWorkspace()
    const dialog = within(await openFolderShare(user))
    await includeExisting(user, dialog)
    await dialog.findByText("3 of your private items will be shared:")
    expect(dialog.queryByText("Subfolder private doc")).toBeNull()

    await user.click(
      dialog.getByRole("checkbox", { name: "Include subfolders" })
    )
    expect(
      await dialog.findByText("4 of your private items will be shared:")
    ).toBeInTheDocument()
    expect(dialog.getByText("Roadmap and 1 subfolder.")).toBeInTheDocument()
    expect(dialog.getByText("Subfolder private doc")).toBeInTheDocument()
    expect(writes()).toEqual([])
  },
  HEAVY
)

// ---------------------------------------------------------------------------
// New items: the folder's default (schema 21). Creation-time only.
// ---------------------------------------------------------------------------

const MANAGER = () => json(policy({ canManage: true }))

it("lets a manager share new items here, saving only the folder setting", async () => {
  const user = userEvent.setup()
  const { writes, sharingSaves } = folderWorkspace(undefined, {
    sharing: { 4: MANAGER },
    saveSharing: (body) =>
      json(
        policy({
          canManage: true,
          revision: "2",
          defaultVisibility: body.defaultVisibility,
          effectiveVisibility: body.defaultVisibility,
        })
      ),
  })
  const dialog = within(await openFolderShare(user))
  // Before: the folder page says what new items get, from the server.
  expect(screen.getByText("New items here are private")).toBeInTheDocument()
  expect(
    await dialog.findByText(
      /New items here are currently private\. No folder above sets this, so it is private\./
    )
  ).toBeInTheDocument()
  // Opening "Share" preselects sharing new items — and saves nothing yet.
  expect(
    dialog.getByRole("radio", { name: "Shared with everyone in Neev demo" })
  ).toBeChecked()
  expect(
    dialog.getByText(
      "Existing items keep their current access, and anything marked private stays private."
    )
  ).toBeInTheDocument()
  expect(writes()).toEqual([])

  await user.click(dialog.getByRole("button", { name: "Apply" }))
  expect(
    await dialog.findByText(
      "New items in Roadmap will be shared with Neev demo."
    )
  ).toBeInTheDocument()
  // Exactly one write: the folder's default, against the revision it read.
  // No item was touched; existing items were not asked for.
  expect(sharingSaves).toEqual([
    { expectedRevision: "1", defaultVisibility: "organization" },
  ])
  expect(writes()).toEqual(["PATCH /api/neev/nodes/4/sharing"])
  await user.click(dialog.getByRole("button", { name: "Done" }))
  expect(
    await screen.findByText("New items here are shared with Neev demo")
  ).toBeInTheDocument()
})

it("shows a non-manager the setting without letting them change it", async () => {
  const user = userEvent.setup()
  const { writes } = folderWorkspace(undefined, {
    sharing: {
      4: () =>
        json(
          policy({
            defaultVisibility: "organization",
            effectiveVisibility: "organization",
          })
        ),
    },
  })
  const dialog = within(await openFolderShare(user))
  const shared = await dialog.findByRole("radio", {
    name: "Shared with everyone in Neev demo",
  })
  // What is set, shown as set; none of it changeable.
  expect(shared).toBeChecked()
  for (const radio of dialog.getAllByRole("radio")) expect(radio).toBeDisabled()
  expect(dialog.getByText(/Set on this folder\./)).toBeInTheDocument()
  expect(dialog.getByRole("button", { name: "Apply" })).toBeDisabled()
  expect(writes()).toEqual([])
})

it("says where an inherited setting comes from", async () => {
  const user = userEvent.setup()
  folderWorkspace(undefined, {
    sharing: {
      6: () =>
        json(
          policy({
            nodeId: 6,
            effectiveVisibility: "organization",
            inheritedFrom: 4,
          })
        ),
    },
  })
  // Open the subfolder's own page.
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  await user.click((await screen.findAllByRole("button", { name: "Specs" }))[0])
  await user.click(
    await screen.findByRole("button", { name: "Share items in Specs" })
  )
  const dialog = within(
    await screen.findByRole("dialog", { name: "Share Specs" })
  )
  expect(
    await dialog.findByText(
      /New items here are currently shared with Neev demo\. Set on Roadmap\./
    )
  ).toBeInTheDocument()
})

/**
 * Someone else changed the setting while the dialog was open. Nothing is
 * forced over it, the current value is read again, and the existing-items
 * step that was also asked for is NOT sent: "nothing else was changed" has to
 * be true when it is said.
 */
it("stops at a conflicting folder setting, re-reads it, and shares nothing else", async () => {
  const user = userEvent.setup()
  let reads = 0
  const { writes } = folderWorkspace(undefined, {
    sharing: {
      4: () => {
        reads += 1
        return json(policy({ canManage: true }))
      },
    },
    saveSharing: () => json({ code: "conflict" }, 409),
  })
  const dialog = within(await openFolderShare(user))
  await includeExisting(user, dialog)
  await dialog.findByText("3 of your private items will be shared:")
  const before = reads
  await user.click(dialog.getByRole("button", { name: "Apply" }))
  expect(
    await dialog.findByText(
      "Someone changed this setting meanwhile. Review it and apply again; nothing else was changed."
    )
  ).toBeInTheDocument()
  await waitFor(() => expect(reads).toBe(before + 1))
  expect(writes()).toEqual(["PATCH /api/neev/nodes/4/sharing"])
  expect(dialog.queryByText(/items shared\./)).toBeNull()
})

it("reports a refused folder setting honestly", async () => {
  const user = userEvent.setup()
  folderWorkspace(undefined, {
    sharing: { 4: MANAGER },
    saveSharing: () => json({ code: "forbidden" }, 403),
  })
  const dialog = within(await openFolderShare(user))
  await dialog.findByRole("radio", {
    name: "Shared with everyone in Neev demo",
  })
  await user.click(dialog.getByRole("button", { name: "Apply" }))
  expect(
    await dialog.findByText(
      "The setting for new items was not saved; nothing else was changed."
    )
  ).toBeInTheDocument()
  expect(dialog.queryByText(/will be shared with Neev demo\./)).toBeNull()
})

/**
 * A server without the setting (or a failed read) shows the error and claims
 * nothing about new items; the existing-items step still works.
 */
it("claims nothing about new items when the setting cannot be read", async () => {
  const user = userEvent.setup()
  folderWorkspace(undefined, {
    sharing: { 4: () => json({ code: "unavailable" }, 503) },
  })
  const dialog = within(await openFolderShare(user))
  expect(await dialog.findByRole("alert")).toBeInTheDocument()
  expect(dialog.queryAllByRole("radio")).toEqual([])
  expect(screen.queryByText(/New items here are/)).toBeNull()
  await includeExisting(user, dialog)
  expect(
    await dialog.findByRole("button", { name: "Share 3 items" })
  ).toBeEnabled()
})

it("creates a private exception in a shared folder in one request", async () => {
  const user = userEvent.setup()
  const { creates } = folderWorkspace(undefined, {
    sharing: {
      4: () =>
        json(
          policy({
            defaultVisibility: "organization",
            effectiveVisibility: "organization",
          })
        ),
    },
  })
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  const privately = await screen.findByRole("checkbox", {
    name: /Create privately/,
  })
  expect(privately).toHaveAccessibleDescription(
    "The next document or chat you create here will be private."
  )
  await user.click(privately)
  await user.click(screen.getByRole("button", { name: "New document" }))
  await waitFor(() => expect(creates).toHaveLength(1))
  // The audience travels WITH the creation: private from its first moment.
  expect(creates[0].kind).toBe("documents")
  expect(creates[0].body.visibility).toBe("private")
})

it("leaves the folder to decide when no exception is chosen", async () => {
  const user = userEvent.setup()
  const { creates } = folderWorkspace(undefined, {
    sharing: {
      4: () =>
        json(
          policy({
            defaultVisibility: "organization",
            effectiveVisibility: "organization",
          })
        ),
    },
  })
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  await screen.findByRole("checkbox", { name: /Create privately/ })
  await user.click(screen.getByRole("button", { name: "New agent chat" }))
  await waitFor(() => expect(creates).toHaveLength(1))
  expect(creates[0].kind).toBe("chats")
  expect(Object.keys(creates[0].body)).not.toContain("visibility")
})

it("offers no private exception where new items are already private", async () => {
  const user = userEvent.setup()
  folderWorkspace()
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  await screen.findByText("New items here are private")
  expect(
    screen.queryByRole("checkbox", { name: /Create privately/ })
  ).toBeNull()
})

/** A late answer for the folder the reader has already left is never shown. */
it("never shows a late setting for a folder that is no longer on screen", async () => {
  const user = userEvent.setup()
  let release: (() => void) | undefined
  folderWorkspace(undefined, {
    sharing: {
      4: () =>
        new Promise<Response>((resolve) => {
          release = () =>
            resolve(
              json(
                policy({
                  defaultVisibility: "organization",
                  effectiveVisibility: "organization",
                })
              )
            )
        }) as unknown as Response,
    },
  })
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  await waitFor(() => expect(release).toBeDefined())
  // Leave for the subfolder before Roadmap's answer arrives.
  await user.click((await screen.findAllByRole("button", { name: "Specs" }))[0])
  expect(
    await screen.findByText("New items here are private")
  ).toBeInTheDocument()
  await act(async () => {
    release?.()
  })
  await act(async () => {
    await Promise.resolve()
  })
  expect(screen.queryByText(/New items here are shared/)).toBeNull()
  expect(
    screen.queryByRole("checkbox", { name: /Create privately/ })
  ).toBeNull()
})

/**
 * Saving the default moves the folder's revision (the backend bumps
 * `neev_nodes.revision`). The tree must adopt the returned one, or the next
 * rename or move of that folder is sent with the old revision and refused as
 * a conflict the reader did nothing to cause.
 */
it(
  "renames a folder with the revision its new-items setting moved it to",
  async () => {
    const user = userEvent.setup()
    let renamed: Record<string, unknown> | undefined
    folderWorkspace(undefined, {
      sharing: { 4: MANAGER },
      saveSharing: (body) =>
        json(
          policy({
            canManage: true,
            revision: "2",
            defaultVisibility: body.defaultVisibility,
            effectiveVisibility: body.defaultVisibility,
          })
        ),
      routes: {
        "PATCH /api/neev/nodes/4": (init) => {
          renamed = JSON.parse(String(init?.body)) as Record<string, unknown>
          return json({ ...NODE, name: String(renamed.name), revision: "3" })
        },
      },
    })
    const dialog = within(await openFolderShare(user))
    await dialog.findByRole("radio", {
      name: "Shared with everyone in Neev demo",
    })
    await user.click(dialog.getByRole("button", { name: "Apply" }))
    await dialog.findByText(
      "New items in Roadmap will be shared with Neev demo."
    )
    await user.click(dialog.getByRole("button", { name: "Done" }))
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())

    await user.click(
      screen.getByRole("button", { name: "Actions for Roadmap" })
    )
    await user.click(
      await screen.findByRole("menuitem", { name: "Edit details" })
    )
    const editor = within(await screen.findByRole("dialog"))
    const name = editor.getByLabelText("Name")
    fireEvent.change(name, { target: { value: "Roadmap 2027" } })
    await user.click(editor.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(renamed).toBeDefined())
    // The revision the default save returned — not the "1" the tree loaded with.
    expect(renamed).toEqual({
      kind: "project",
      name: "Roadmap 2027",
      parentId: null,
      expectedRevision: "2",
    })
    // A large fixture (100+ rows) makes every role query slow; the flow is long.
  },
  HEAVY
)

/**
 * And the other way round: a rename moves the folder's revision, so the
 * answer the page read first is out of date. Opening Share reads it again, and
 * the save carries the revision of THAT fresh answer — the server's own, not
 * one borrowed from the tree.
 */
it(
  "saves the new-items setting with the revision a fresh read reports after a rename",
  async () => {
    const user = userEvent.setup()
    const saves: Record<string, unknown>[] = []
    let serverRevision = "1"
    folderWorkspace(undefined, {
      sharing: {
        4: () => json(policy({ canManage: true, revision: serverRevision })),
      },
      saveSharing: (body) => {
        saves.push(body)
        return body.expectedRevision === serverRevision
          ? json(
              policy({
                canManage: true,
                revision: "4",
                defaultVisibility: body.defaultVisibility,
                effectiveVisibility: body.defaultVisibility,
              })
            )
          : json({ code: "conflict" }, 409)
      },
      routes: {
        "PATCH /api/neev/nodes/4": (init) => {
          const body = JSON.parse(String(init?.body)) as Record<string, unknown>
          serverRevision = "3"
          return json({ ...NODE, name: String(body.name), revision: "3" })
        },
      },
    })
    await user.click(
      (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
    )
    await user.click(
      screen.getByRole("button", { name: "Actions for Roadmap" })
    )
    await user.click(
      await screen.findByRole("menuitem", { name: "Edit details" })
    )
    const editor = within(await screen.findByRole("dialog"))
    fireEvent.change(editor.getByLabelText("Name"), {
      target: { value: "Roadmap" },
    })
    await user.click(editor.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())

    const dialog = within(await openFolderShare(user))
    await dialog.findByRole("radio", {
      name: "Shared with everyone in Neev demo",
    })
    await user.click(dialog.getByRole("button", { name: "Apply" }))
    expect(
      await dialog.findByText(
        "New items in Roadmap will be shared with Neev demo."
      )
    ).toBeInTheDocument()
    expect(saves).toEqual([
      { expectedRevision: "3", defaultVisibility: "organization" },
    ])
  },
  HEAVY
)

/**
 * Review F1. Another manager changed this folder's setting (revision 5 -> 6).
 * The tree already knows revision 6; the page's answer is still 5. Opening
 * Share re-reads it — and until that answer is in, nothing may be saved:
 * neither with 5 (a needless conflict) nor with 6 borrowed from the tree,
 * which would overwrite a change the reader was never shown.
 */
it(
  "saves nothing until the setting the reader is shown is the fresh one",
  async () => {
    const user = userEvent.setup()
    let reads = 0
    let release: (() => void) | undefined
    const { sharingSaves } = folderWorkspace(undefined, {
      routes: {
        "GET /api/neev/nodes": () =>
          json({ nodes: [{ ...NODE, revision: "6" }, CHILD], nextAfter: null }),
      },
      sharing: {
        4: () => {
          reads += 1
          if (reads === 1)
            return json(policy({ canManage: true, revision: "5" }))
          // The re-read Share asks for is still out.
          return new Promise<Response>((resolve) => {
            release = () =>
              resolve(
                json(
                  policy({
                    canManage: true,
                    revision: "6",
                    // What the other manager set, explicitly.
                    defaultVisibility: "private",
                    effectiveVisibility: "private",
                  })
                )
              )
          }) as unknown as Response
        },
      },
      saveSharing: (body) =>
        json(
          policy({
            canManage: true,
            revision: "7",
            defaultVisibility: body.defaultVisibility,
            effectiveVisibility: body.defaultVisibility,
          })
        ),
    })
    const dialog = within(await openFolderShare(user))
    await waitFor(() => expect(release).toBeDefined())
    // The stale answer is not offered as something to act on.
    expect(
      dialog.getByText("Loading the setting for new items…")
    ).toBeInTheDocument()
    expect(dialog.queryAllByRole("radio")).toEqual([])
    const apply = dialog.getByRole("button", { name: "Apply" })
    expect(apply).toBeDisabled()
    await user.click(apply)
    expect(sharingSaves).toEqual([])

    await act(async () => {
      release?.()
    })
    // Now the reader sees what the other manager set, and decides on that.
    expect(
      await dialog.findByText(
        /New items here are currently private\. Set on this folder\./
      )
    ).toBeInTheDocument()
    await user.click(dialog.getByRole("button", { name: "Apply" }))
    await dialog.findByText(
      "New items in Roadmap will be shared with Neev demo."
    )
    expect(sharingSaves).toEqual([
      { expectedRevision: "6", defaultVisibility: "organization" },
    ])
  },
  HEAVY
)

/**
 * Review F3. The page said new items are private; meanwhile another manager
 * made them shared. A default create must not quietly make a shared item: it
 * is held, the page says what changed, and the private choice appears. The
 * reader's next click is an informed one.
 */
it(
  "holds a create when new items have become shared since the page said private",
  async () => {
    const user = userEvent.setup()
    let shared = false
    const { creates } = folderWorkspace(undefined, {
      sharing: {
        4: () =>
          json(
            shared
              ? policy({
                  defaultVisibility: "organization",
                  effectiveVisibility: "organization",
                })
              : policy()
          ),
      },
    })
    await user.click(
      (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
    )
    await screen.findByText("New items here are private")
    expect(
      screen.queryByRole("checkbox", { name: /Create privately/ })
    ).toBeNull()

    shared = true
    await user.click(screen.getByRole("button", { name: "New document" }))
    expect(
      await screen.findByText(
        "New items here are now shared with Neev demo. Nothing was created; choose again, or tick Create privately."
      )
    ).toBeInTheDocument()
    expect(creates).toEqual([])
    expect(
      screen.getByText("New items here are shared with Neev demo")
    ).toBeInTheDocument()
    expect(
      screen.getByRole("checkbox", { name: /Create privately/ })
    ).toBeInTheDocument()

    // The next click, made knowing, goes through with the folder's default.
    await user.click(screen.getByRole("button", { name: "New document" }))
    await waitFor(() => expect(creates).toHaveLength(1))
    expect(Object.keys(creates[0].body)).not.toContain("visibility")
  },
  HEAVY
)

it(
  "re-reads what new items get when the window regains focus",
  async () => {
    const user = userEvent.setup()
    let shared = false
    folderWorkspace(undefined, {
      sharing: {
        4: () =>
          json(
            shared
              ? policy({
                  defaultVisibility: "organization",
                  effectiveVisibility: "organization",
                })
              : policy()
          ),
      },
    })
    await user.click(
      (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
    )
    await screen.findByText("New items here are private")
    shared = true
    await act(async () => {
      window.dispatchEvent(new Event("focus"))
    })
    expect(
      await screen.findByText("New items here are shared with Neev demo")
    ).toBeInTheDocument()
  },
  HEAVY
)

/**
 * When the setting cannot be read, nothing is assumed — least of all that new
 * items are private. The private choice is offered, and works atomically.
 */
it(
  "offers the private choice when the setting cannot be read",
  async () => {
    const user = userEvent.setup()
    const { creates } = folderWorkspace(undefined, {
      sharing: { 4: () => json({ code: "unavailable" }, 503) },
    })
    await user.click(
      (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
    )
    await user.click(
      await screen.findByRole("checkbox", { name: /Create privately/ })
    )
    await user.click(screen.getByRole("button", { name: "New document" }))
    await waitFor(() => expect(creates).toHaveLength(1))
    expect(creates[0].body.visibility).toBe("private")
  },
  HEAVY
)

/**
 * Review R1. The page said new items are private, and the check right before
 * creating fails. The promise on screen can no longer be confirmed — another
 * manager may have made new items shared — so nothing is created. The page
 * says so and offers the private choice.
 */
it(
  "holds a create when the check fails after the page said private",
  async () => {
    const user = userEvent.setup()
    let failing = false
    const { creates } = folderWorkspace(undefined, {
      sharing: {
        4: () =>
          failing ? json({ code: "unavailable" }, 503) : json(policy()),
      },
    })
    await user.click(
      (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
    )
    await screen.findByText("New items here are private")

    failing = true
    await user.click(screen.getByRole("button", { name: "New document" }))
    expect(
      await screen.findByText(
        "Couldn't check what new items here get. Nothing was created; try again, or tick Create privately."
      )
    ).toBeInTheDocument()
    expect(creates).toEqual([])

    // The private choice is now offered, and is atomic.
    await user.click(screen.getByRole("checkbox", { name: /Create privately/ }))
    await user.click(screen.getByRole("button", { name: "New document" }))
    await waitFor(() => expect(creates).toHaveLength(1))
    expect(creates[0].body.visibility).toBe("private")
  },
  HEAVY
)

it(
  "creates on retry once the check succeeds again",
  async () => {
    const user = userEvent.setup()
    let failing = false
    const { creates } = folderWorkspace(undefined, {
      sharing: {
        4: () =>
          failing ? json({ code: "unavailable" }, 503) : json(policy()),
      },
    })
    await user.click(
      (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
    )
    await screen.findByText("New items here are private")
    failing = true
    await user.click(screen.getByRole("button", { name: "New document" }))
    await screen.findByText(/Couldn't check what new items here get/)
    expect(creates).toEqual([])

    failing = false
    await user.click(screen.getByRole("button", { name: "New document" }))
    await waitFor(() => expect(creates).toHaveLength(1))
    // Confirmed private by the server, so the folder decides: no override sent.
    expect(Object.keys(creates[0].body)).not.toContain("visibility")
  },
  HEAVY
)
