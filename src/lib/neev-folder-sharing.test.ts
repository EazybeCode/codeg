import { afterEach, expect, it, vi } from "vitest"
import { createNativeResource } from "./neev-content-client"
import {
  loadFolderSharing,
  newerRevision,
  parseFolderSharing,
  updateFolderSharing,
} from "./neev-folder-sharing"

afterEach(() => vi.unstubAllGlobals())

const json = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  })

const dto = (over: Record<string, unknown> = {}) => ({
  nodeId: 4,
  revision: "7",
  defaultVisibility: "inherit",
  effectiveVisibility: "private",
  inheritedFrom: null,
  canManage: true,
  ...over,
})

function stub(answer: () => Response) {
  const fetcher = vi.fn<
    (path: string, init?: RequestInit) => Promise<Response>
  >(async () => answer())
  vi.stubGlobal("fetch", fetcher)
  return fetcher
}

it("reads the contract's exact shape", async () => {
  const fetcher = stub(() =>
    json(dto({ effectiveVisibility: "organization", inheritedFrom: 2 }))
  )
  expect(await loadFolderSharing(4)).toEqual(
    dto({ effectiveVisibility: "organization", inheritedFrom: 2 })
  )
  expect(fetcher.mock.calls[0][0]).toBe("/api/neev/nodes/4/sharing")
})

/**
 * Anything inconsistent is refused, because the one wrong answer that matters
 * is showing "private" for something the server will share.
 */
it.each([
  ["an unknown field", dto({ extra: true })],
  [
    "a missing field",
    Object.fromEntries(
      Object.entries(dto()).filter(([key]) => key !== "canManage")
    ),
  ],
  ["another folder's answer", dto({ nodeId: 5 })],
  [
    "an explicit setting that is not what applies",
    dto({ defaultVisibility: "private", effectiveVisibility: "organization" }),
  ],
  [
    "an explicit setting that claims to be inherited",
    dto({
      defaultVisibility: "organization",
      effectiveVisibility: "organization",
      inheritedFrom: 2,
    }),
  ],
  [
    "a folder inheriting from itself",
    dto({ effectiveVisibility: "organization", inheritedFrom: 4 }),
  ],
  [
    "nothing set anywhere yet shared",
    dto({ effectiveVisibility: "organization" }),
  ],
  ["an unknown audience", dto({ effectiveVisibility: "public" })],
  ["a non-decimal revision", dto({ revision: "07" })],
  ["a numeric revision", dto({ revision: 7 })],
])("refuses %s", (_label, value) => {
  expect(() => parseFolderSharing(value, 4)).toThrow()
})

it("saves only the expected revision and the chosen default", async () => {
  const fetcher = stub(() =>
    json(
      dto({
        revision: "8",
        defaultVisibility: "organization",
        effectiveVisibility: "organization",
      })
    )
  )
  const saved = await updateFolderSharing(
    4,
    { expectedRevision: "7", defaultVisibility: "organization" },
    "csrf-token"
  )
  expect(saved.revision).toBe("8")
  const [path, init] = fetcher.mock.calls[0]
  expect(path).toBe("/api/neev/nodes/4/sharing")
  expect(init?.method).toBe("PATCH")
  expect(JSON.parse(String(init?.body))).toEqual({
    expectedRevision: "7",
    defaultVisibility: "organization",
  })
})

it("refuses to send a malformed setting", async () => {
  const fetcher = stub(() => json(dto()))
  await expect(
    updateFolderSharing(
      4,
      { expectedRevision: "0", defaultVisibility: "organization" },
      "c"
    )
  ).rejects.toThrow()
  await expect(
    updateFolderSharing(
      4,
      {
        expectedRevision: "7",
        defaultVisibility: "public" as unknown as "private",
      },
      "c"
    )
  ).rejects.toThrow()
  expect(fetcher).not.toHaveBeenCalled()
})

const created = (visibility: string) =>
  json(
    {
      id: "11111111-1111-4111-8111-111111111111",
      orgId: 3,
      nodeId: 4,
      kind: "document",
      creatorId: 1,
      title: "Untitled document",
      visibility,
      originChatId: null,
      authRevision: "1",
      contentRevision: "0",
      archived: false,
      createdAt: 0,
      updatedAt: 0,
    },
    201
  )
const REQUEST = "22222222-2222-4222-8222-222222222222"

it("omits visibility unless an exception is chosen, so the folder decides", async () => {
  const fetcher = stub(() => created("organization"))
  const resource = await createNativeResource(
    3,
    4,
    "document",
    { title: "Untitled document", requestId: REQUEST },
    "c"
  )
  // What the server materialized is what the client reports.
  expect(resource.visibility).toBe("organization")
  expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual({
    title: "Untitled document",
    requestId: REQUEST,
  })
})

it("sends an explicit private exception with the creation itself", async () => {
  const fetcher = stub(() => created("private"))
  await createNativeResource(
    3,
    4,
    "document",
    { title: "Untitled document", requestId: REQUEST, visibility: "private" },
    "c"
  )
  expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual({
    title: "Untitled document",
    requestId: REQUEST,
    visibility: "private",
  })
})

it("never sends an audience for a chat's generated file", async () => {
  const fetcher = stub(() => created("private"))
  await expect(
    createNativeResource(
      3,
      4,
      "document",
      {
        title: "Attached",
        requestId: REQUEST,
        originChatId: "33333333-3333-4333-8333-333333333333",
        visibility: "private",
      },
      "c"
    )
  ).rejects.toThrow()
  expect(fetcher).not.toHaveBeenCalled()
})

/**
 * Review F2. The tree only moves forward: a default save answering with
 * revision 2 after a rename already took the tree to 3 must not pull it back,
 * or the next rename would be refused for a change nobody made.
 */
it("keeps a node revision from going backwards", () => {
  expect(newerRevision("3", "2")).toBe("3")
  expect(newerRevision("2", "3")).toBe("3")
  expect(newerRevision("9", "10")).toBe("10") // numeric, not lexicographic
  expect(newerRevision("18446744073709551616", "18446744073709551615")).toBe(
    "18446744073709551616"
  )
  expect(newerRevision("4", "not-a-revision")).toBe("4")
})
