import { afterEach, expect, it, vi } from "vitest"
import {
  descendantIds,
  listTrashedNodes,
  moveNodeToTrash,
  parseNodeTrashPreview,
  parseTrashedNode,
  previewNodeTrash,
  restoreNode,
  trashBlocker,
} from "./neev-node-trash"

afterEach(() => vi.unstubAllGlobals())

function respond(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  })
}

const item = {
  nodeId: 7,
  kind: "folder",
  name: "Launch",
  parentId: 3,
  parentName: "Marketing",
  parentTrashed: false,
  trashedBy: 2,
  trashedByName: "Asha",
  trashedAt: 1700000000,
  folders: 1,
  documents: 2,
  chats: 0,
  agentChats: 0,
}

it("parses previews and names the first blocker", () => {
  const preview = parseNodeTrashPreview({
    folders: 2,
    documents: 3,
    chats: 1,
    agentChats: 0,
    blocked: { agentActive: false, inUse: false, privateOthers: true },
  })
  expect(preview.documents).toBe(3)
  expect(trashBlocker(preview)).toBe("privateOthers")
  expect(
    trashBlocker({
      ...preview,
      blocked: { agentActive: true, inUse: true, privateOthers: true },
    })
  ).toBe("agentActive")
  expect(
    trashBlocker({
      ...preview,
      blocked: { agentActive: false, inUse: false, privateOthers: false },
    })
  ).toBeNull()
  expect(() => parseNodeTrashPreview({ folders: -1 })).toThrow()
  // A count where a yes/no belongs is refused: blockers never carry a number.
  expect(() =>
    parseNodeTrashPreview({
      folders: 0,
      documents: 0,
      chats: 0,
      agentChats: 0,
      blocked: { agentActive: false, inUse: false, privateOthers: 2 },
    })
  ).toThrow()
  // An older server without agent chat counts is refused, not silently zero.
  expect(() =>
    parseNodeTrashPreview({
      folders: 0,
      documents: 0,
      chats: 0,
      blocked: { agentActive: false, inUse: false, privateOthers: false },
    })
  ).toThrow()
})

it("parses trash items and refuses malformed ones", () => {
  expect(parseTrashedNode(item)).toEqual(item)
  expect(
    parseTrashedNode({ ...item, parentId: null, parentName: null })
  ).toMatchObject({ parentId: null, parentName: null })
  expect(() =>
    parseTrashedNode({ ...item, kind: "working_directory" })
  ).toThrow()
  expect(() => parseTrashedNode({ ...item, nodeId: 0 })).toThrow()
  expect(() => parseTrashedNode({ ...item, parentTrashed: "no" })).toThrow()
})

it("calls the trash endpoints with revision and CSRF", async () => {
  const fetcher = vi.fn(async (path: string, init: RequestInit = {}) => {
    const key = `${init.method ?? "GET"} ${path}`
    if (key === "GET /api/neev/nodes/7/trash-preview")
      return respond({
        folders: 0,
        documents: 1,
        chats: 0,
        agentChats: 0,
        blocked: { agentActive: false, inUse: false, privateOthers: false },
      })
    if (key === "POST /api/neev/nodes/7/trash")
      return respond({ batchId: "b", nodes: 1, resources: 1 })
    if (key === "POST /api/neev/nodes/7/restore")
      return respond({ nodes: 1, resources: 1 })
    if (key === "GET /api/neev/trash/nodes") return respond({ items: [item] })
    throw new Error(key)
  })
  vi.stubGlobal("fetch", fetcher)
  expect((await previewNodeTrash(7)).documents).toBe(1)
  expect(await moveNodeToTrash(7, "4", "csrf")).toEqual({
    nodes: 1,
    resources: 1,
  })
  const trashCall = fetcher.mock.calls.find(([path]) => path.endsWith("/trash"))
  expect(JSON.parse(String(trashCall?.[1]?.body))).toEqual({
    expectedRevision: "4",
  })
  expect(
    (trashCall?.[1]?.headers as Record<string, string>)["x-neev-csrf"]
  ).toBe("csrf")
  expect(await restoreNode(7, "csrf")).toEqual({ nodes: 1, resources: 1 })
  expect(await listTrashedNodes()).toEqual([item])
})

it("surfaces a blocked trash as a conflict", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      respond({ code: "trash_blocked", reason: "agent_active" }, 409)
    )
  )
  await expect(moveNodeToTrash(7, "1", "csrf")).rejects.toMatchObject({
    kind: "conflict",
    status: 409,
  })
})

it("collects descendants from a loaded tree", () => {
  const nodes = [
    { id: 1, parentId: null },
    { id: 2, parentId: 1 },
    { id: 3, parentId: 2 },
    { id: 4, parentId: null },
  ]
  expect(descendantIds(nodes, 1).sort()).toEqual([2, 3])
  expect(descendantIds(nodes, 4)).toEqual([])
})
