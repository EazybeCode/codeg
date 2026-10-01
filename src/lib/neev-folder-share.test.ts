import { expect, it, vi } from "vitest"
import { NativeClientError } from "./neev-client"
import type { NativeResource } from "./neev-content-client"
import {
  applyFolderShare,
  eligible,
  previewFolderShare,
  type FolderShareDeps,
} from "./neev-folder-share"

const ME = 1
let serial = 0
function item(over: Partial<NativeResource> = {}): NativeResource {
  serial += 1
  const hex = serial.toString(16).padStart(12, "0")
  return {
    id: `00000000-0000-4000-8000-${hex}`,
    orgId: 3,
    nodeId: 4,
    kind: "document",
    creatorId: ME,
    title: `Fixture ${serial}`,
    visibility: "private",
    originChatId: null,
    authRevision: "2",
    contentRevision: "1",
    archived: false,
    createdAt: 0,
    updatedAt: 0,
    ...over,
  }
}

function deps(over: Partial<FolderShareDeps> = {}): FolderShareDeps {
  return {
    listResources: vi.fn(async () => ({ items: [], nextAfter: null })),
    loadNodes: vi.fn(async () => ({ nodes: [], nextAfter: null })),
    loadResource: vi.fn(),
    updateResource: vi.fn(async (_org, _id, input) => ({
      ...item(),
      visibility: input.visibility,
    })),
    ...over,
  } as FolderShareDeps
}

const signal = () => new AbortController().signal

it("counts only the caller's own private, live, independent items", () => {
  expect(eligible(item(), ME)).toBe(true)
  expect(eligible(item({ creatorId: 2 }), ME)).toBe(false)
  expect(eligible(item({ visibility: "organization" }), ME)).toBe(false)
  expect(eligible(item({ archived: true }), ME)).toBe(false)
  expect(
    eligible(item({ originChatId: "11111111-1111-4111-8111-111111111111" }), ME)
    // A chat's generated file follows the chat; it is never shared on its own.
  ).toBe(false)
})

it("reads every page of both kinds before counting anything", async () => {
  const first = Array.from({ length: 100 }, () => item())
  const second = [item(), item({ creatorId: 2, visibility: "organization" })]
  const chats = [item({ kind: "chat" })]
  const listResources = vi.fn(
    async (
      _org: number,
      _node: number,
      kind: string,
      _archived?: boolean,
      after?: string | null
    ) => {
      if (kind === "chat") return { items: chats, nextAfter: null }
      return after === null
        ? { items: first, nextAfter: "page2" }
        : { items: second, nextAfter: null }
    }
  )
  const d = deps({ listResources: listResources as never })
  const preview = await previewFolderShare(
    {
      orgId: 3,
      nodeId: 4,
      userId: ME,
      includeSubfolders: false,
      signal: signal(),
    },
    d
  )
  // 100 + 1 of mine from documents, 1 chat — the colleague's row is not there.
  expect(preview.items).toHaveLength(102)
  expect(preview.items.every((row) => row.creatorId === ME)).toBe(true)
  expect(listResources.mock.calls.map((call) => [call[2], call[4]])).toEqual([
    ["document", null],
    ["document", "page2"],
    ["chat", null],
  ])
  // Live items only: archived ones are never even asked for.
  expect(listResources.mock.calls.every((call) => call[3] === false)).toBe(true)
  // Direct folder only: the tree is not read at all.
  expect(d.loadNodes).not.toHaveBeenCalled()
  expect(preview.nodeIds).toEqual([4])
})

it("includes subfolders only when asked, from a complete read of the tree", async () => {
  const listResources = vi.fn(
    async (_org: number, node: number, kind: string) =>
      kind === "document"
        ? { items: [item({ nodeId: node })], nextAfter: null }
        : { items: [], nextAfter: null }
  )
  const loadNodes = vi.fn(async (_org: number, after: number | null) =>
    after === null
      ? {
          nodes: [
            { id: 4, parentId: null },
            { id: 5, parentId: 4 },
          ],
          nextAfter: 5,
        }
      : {
          nodes: [
            { id: 6, parentId: 5 }, // grandchild, on the second page
            { id: 7, parentId: null }, // unrelated project
          ],
          nextAfter: null,
        }
  )
  const preview = await previewFolderShare(
    {
      orgId: 3,
      nodeId: 4,
      userId: ME,
      includeSubfolders: true,
      signal: signal(),
    },
    deps({
      listResources: listResources as never,
      loadNodes: loadNodes as never,
    })
  )
  expect(loadNodes).toHaveBeenCalledTimes(2)
  expect(preview.nodeIds).toEqual([4, 5, 6])
  expect(preview.items.map((row) => row.nodeId)).toEqual([4, 5, 6])
})

it("passes the caller's abort signal to every read", async () => {
  const controller = new AbortController()
  const listResources = vi.fn<(...args: unknown[]) => Promise<never>>(
    async () => {
      controller.abort()
      throw new NativeClientError("aborted")
    }
  )
  await expect(
    previewFolderShare(
      {
        orgId: 3,
        nodeId: 4,
        userId: ME,
        includeSubfolders: false,
        signal: controller.signal,
      },
      deps({ listResources: listResources as never })
    )
  ).rejects.toThrow()
  expect(listResources.mock.calls[0][5]).toBe(controller.signal)
})

it("shares with the row's own revision and changes nothing else", async () => {
  const row = item({ title: "Exact", authRevision: "7" })
  const d = deps()
  await applyFolderShare(
    {
      orgId: 3,
      userId: ME,
      nodeIds: [4],
      items: [row],
      csrf: "c",
      signal: signal(),
    },
    d
  )
  expect(d.updateResource).toHaveBeenCalledWith(
    3,
    row.id,
    {
      nodeId: 4,
      title: "Exact",
      visibility: "organization",
      archived: false,
      expectedAuthRevision: "7",
    },
    "c",
    expect.anything()
  )
})

it("reports each outcome exactly, and stops at a lost session", async () => {
  const ok = item()
  const nowShared = item()
  const stale = item()
  const trashed = item()
  const broken = item()
  const sessionLost = item()
  const neverTried = item()
  const conflict = () => new NativeClientError("conflict", 409)
  const fresh: Record<string, NativeResource> = {
    [nowShared.id]: { ...nowShared, visibility: "organization" },
    [stale.id]: { ...stale, authRevision: "3" },
    [trashed.id]: { ...trashed, archived: true },
  }
  const attempts: string[] = []
  const updateResource = vi.fn(
    async (
      _org: number,
      id: string,
      input: { expectedAuthRevision: string }
    ) => {
      attempts.push(`${id}@${input.expectedAuthRevision}`)
      if (id === ok.id) return { ...ok, visibility: "organization" as const }
      if (id === stale.id && input.expectedAuthRevision === "3")
        return { ...stale, visibility: "organization" as const }
      if ([nowShared.id, stale.id, trashed.id].includes(id)) throw conflict()
      if (id === broken.id) throw new NativeClientError("unavailable", 503)
      if (id === sessionLost.id)
        throw new NativeClientError("unauthorized", 401)
      throw new Error("must not be attempted")
    }
  )
  const loadResource = vi.fn(async (_org: number, id: string) => fresh[id])
  const progress: number[] = []
  const result = await applyFolderShare(
    {
      orgId: 3,
      userId: ME,
      nodeIds: [4],
      items: [ok, nowShared, stale, trashed, broken, sessionLost, neverTried],
      csrf: "c",
      signal: signal(),
      onProgress: (done) => progress.push(done),
    },
    deps({
      updateResource: updateResource as never,
      loadResource: loadResource as never,
    })
  )
  expect(result.shared.map((row) => row.id)).toEqual([ok.id, stale.id])
  expect(result.already.map((row) => row.id)).toEqual([nowShared.id])
  expect(result.changed.map((row) => row.id)).toEqual([trashed.id])
  expect(result.failed.map((row) => row.resource.id)).toEqual([
    broken.id,
    sessionLost.id,
    neverTried.id,
  ])
  // After the 401 nothing else is sent: the last item is failed, not tried.
  expect(attempts.some((entry) => entry.startsWith(neverTried.id))).toBe(false)
  // A conflict is re-read and retried once against the fresh revision.
  expect(attempts).toContain(`${stale.id}@3`)
  expect(progress[progress.length - 1]).toBe(6)
})

it("leaves alone an item moved out of the folder meanwhile", async () => {
  const row = item()
  const result = await applyFolderShare(
    {
      orgId: 3,
      userId: ME,
      nodeIds: [4],
      items: [row],
      csrf: "c",
      signal: signal(),
    },
    deps({
      updateResource: vi.fn(async () => {
        throw new NativeClientError("conflict", 409)
      }) as never,
      loadResource: vi.fn(async () => ({ ...row, nodeId: 9 })) as never,
    })
  )
  expect(result.changed).toHaveLength(1)
  expect(result.shared).toHaveLength(0)
})
