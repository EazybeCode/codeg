/**
 * Sharing a folder's EXISTING items, one resource at a time.
 *
 * There is no folder permission in Neev. A folder and its name are already
 * visible to everyone in the organization; what is private or shared is each
 * item in it, and only its creator may change that. So "share this folder" is
 * exactly "share each of my private items that are in it now", through the
 * same per-resource PATCH an item's own header sends — no new ACL, no default
 * for things made later (they stay private), and nothing of anyone else's.
 *
 * What is counted and shown is only what was actually collected: every page of
 * every list is read before anything is offered, so a preview never claims "all"
 * of a folder from a first page.
 */
import {
  listNativeResources,
  loadNativeResource,
  updateNativeResource,
  type NativeResource,
} from "./neev-content-client"
import { loadNodes, NativeClientError } from "./neev-client"

/** A bound on pages read for one count, against a server that never ends. */
const MAX_PAGES = 1000

/** Mine, private, live, and not a chat's generated file (that follows its chat). */
export function eligible(resource: NativeResource, userId: number): boolean {
  return (
    resource.creatorId === userId &&
    resource.visibility === "private" &&
    !resource.archived &&
    !resource.originChatId
  )
}

export type FolderShareDeps = {
  listResources: typeof listNativeResources
  loadNodes: typeof loadNodes
  loadResource: typeof loadNativeResource
  updateResource: typeof updateNativeResource
}

const realDeps: FolderShareDeps = {
  listResources: listNativeResources,
  loadNodes,
  loadResource: loadNativeResource,
  updateResource: updateNativeResource,
}

export type FolderSharePreview = {
  /** The folders read: the folder itself, then any subfolders. */
  nodeIds: number[]
  /** What would be shared: the caller's own private items, nothing else. */
  items: NativeResource[]
}

/** Every subfolder under `rootId`, from a COMPLETE read of the tree. */
async function descendants(
  orgId: number,
  rootId: number,
  deps: FolderShareDeps,
  signal: AbortSignal
): Promise<number[]> {
  const nodes: { id: number; parentId: number | null }[] = []
  let after: number | null = null
  for (let page = 0; ; page += 1) {
    if (page >= MAX_PAGES) throw new NativeClientError("invalid_response")
    const result = await deps.loadNodes(orgId, after, signal)
    nodes.push(...result.nodes)
    if (result.nextAfter === null) break
    after = result.nextAfter
  }
  const found: number[] = []
  const queue = [rootId]
  const seen = new Set(queue)
  while (queue.length) {
    const parent = queue.shift() as number
    for (const node of nodes)
      if (node.parentId === parent && !seen.has(node.id)) {
        seen.add(node.id)
        found.push(node.id)
        queue.push(node.id)
      }
  }
  return found
}

/** Read every page of one folder's documents and chats. */
async function everything(
  orgId: number,
  nodeId: number,
  deps: FolderShareDeps,
  signal: AbortSignal
): Promise<NativeResource[]> {
  const out: NativeResource[] = []
  for (const kind of ["document", "chat"] as const) {
    let after: string | null = null
    for (let page = 0; ; page += 1) {
      if (page >= MAX_PAGES) throw new NativeClientError("invalid_response")
      const result = await deps.listResources(
        orgId,
        nodeId,
        kind,
        false,
        after,
        signal
      )
      out.push(...result.items)
      if (result.nextAfter === null) break
      after = result.nextAfter
    }
  }
  return out
}

export async function previewFolderShare(
  input: {
    orgId: number
    nodeId: number
    userId: number
    includeSubfolders: boolean
    signal: AbortSignal
  },
  deps: FolderShareDeps = realDeps
): Promise<FolderSharePreview> {
  const nodeIds = [
    input.nodeId,
    ...(input.includeSubfolders
      ? await descendants(input.orgId, input.nodeId, deps, input.signal)
      : []),
  ]
  const items: NativeResource[] = []
  for (const nodeId of nodeIds) {
    const listed = await everything(input.orgId, nodeId, deps, input.signal)
    // Filtered here, and only the result kept: nothing about anyone else's
    // items — not even how many there are — leaves this function.
    items.push(...listed.filter((item) => eligible(item, input.userId)))
  }
  return { nodeIds, items }
}

export type FolderShareResult = {
  shared: NativeResource[]
  /** Already shared by the time we got there: nothing to do, not a failure. */
  already: NativeResource[]
  /** Moved out, trashed or otherwise no longer eligible meanwhile: left alone. */
  changed: NativeResource[]
  failed: { resource: NativeResource; error: unknown }[]
}

/**
 * A lost session (401) or membership (403) ends the run; it is not one item's
 * failure. The server answers a non-creator with 404, never 403, so a 403 here
 * is about the caller, not the item.
 */
export function isAuthority(error: unknown): boolean {
  return (
    error instanceof NativeClientError &&
    (error.kind === "unauthorized" || error.kind === "forbidden")
  )
}

function isConflict(error: unknown): boolean {
  return error instanceof NativeClientError && error.kind === "conflict"
}

const patchOf = (resource: NativeResource) => ({
  nodeId: resource.nodeId,
  title: resource.title,
  visibility: "organization" as const,
  archived: resource.archived,
  expectedAuthRevision: resource.authRevision,
})

/**
 * Share each item, one request at a time, and say exactly what happened.
 *
 * A conflict means the row moved since it was read. It is re-read and judged
 * again — already shared, no longer eligible, or still private and tried once
 * more against its fresh revision — rather than reported as a failure it may
 * not be. Anything else is that item's failure, kept for a retry of just those.
 * An authority error stops the run: everything after it is reported as failed
 * with that error, never as shared.
 */
export async function applyFolderShare(
  input: {
    orgId: number
    userId: number
    nodeIds: readonly number[]
    items: readonly NativeResource[]
    csrf: string
    signal: AbortSignal
    onProgress?: (done: number) => void
  },
  deps: FolderShareDeps = realDeps
): Promise<FolderShareResult> {
  const result: FolderShareResult = {
    shared: [],
    already: [],
    changed: [],
    failed: [],
  }
  const inScope = new Set(input.nodeIds)
  let stop: unknown = null
  for (const [index, item] of input.items.entries()) {
    if (input.signal.aborted) break
    if (stop !== null) {
      result.failed.push({ resource: item, error: stop })
      continue
    }
    try {
      try {
        result.shared.push(
          await deps.updateResource(
            input.orgId,
            item.id,
            patchOf(item),
            input.csrf,
            input.signal
          )
        )
      } catch (error) {
        if (!isConflict(error)) throw error
        const fresh = await deps.loadResource(
          input.orgId,
          item.id,
          input.signal
        )
        if (fresh.visibility === "organization") result.already.push(fresh)
        else if (!eligible(fresh, input.userId) || !inScope.has(fresh.nodeId))
          result.changed.push(fresh)
        else
          result.shared.push(
            await deps.updateResource(
              input.orgId,
              item.id,
              patchOf(fresh),
              input.csrf,
              input.signal
            )
          )
      }
    } catch (error) {
      if (input.signal.aborted) break
      result.failed.push({ resource: item, error })
      if (isAuthority(error)) stop = error
    }
    input.onProgress?.(index + 1)
  }
  return result
}
