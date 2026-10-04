/**
 * Move to Trash and restore for projects and folders.
 *
 * The server hides the folder and everything in it that was not already in
 * the trash, and archives its chats and documents with their own trash flag.
 * Restore undoes exactly that batch, back in the original place. Nothing is
 * deleted, and no item's audience changes either way.
 *
 * Counts here are what the server says the caller can read; another member's
 * private work is only ever a yes/no that explains why the trash is blocked —
 * never how much of it there is.
 */
import { nativeMutation, nativeRequest, NativeClientError } from "./neev-client"

export type NodeTrashPreview = {
  folders: number
  documents: number
  chats: number
  /** The caller's own Agent Chats filed here (sessions are owner-private). */
  agentChats: number
  blocked: { agentActive: boolean; inUse: boolean; privateOthers: boolean }
}

export type NodeTrashBlocker = "agentActive" | "inUse" | "privateOthers"

export type TrashedNode = {
  nodeId: number
  kind: "project" | "folder"
  name: string
  parentId: number | null
  parentName: string | null
  /** The original parent is in the trash too; it must come back first. */
  parentTrashed: boolean
  trashedBy: number
  trashedByName: string
  trashedAt: number
  folders: number
  documents: number
  chats: number
  agentChats: number
}

function invalid(): never {
  throw new NativeClientError("invalid_response")
}
function record(v: unknown): Record<string, unknown> {
  if (!v || typeof v !== "object" || Array.isArray(v)) return invalid()
  return v as Record<string, unknown>
}
function flag(v: unknown): boolean {
  if (typeof v !== "boolean") return invalid()
  return v
}
function count(v: unknown): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > 1e9)
    return invalid()
  return v
}
function timestamp(v: unknown): number {
  if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0)
    return invalid()
  return v
}
function nodeId(v: unknown): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v <= 0 || v > 2147483647)
    return invalid()
  return v
}
function text(v: unknown, allowEmpty = false): string {
  if (typeof v !== "string" || (!allowEmpty && !v.length)) return invalid()
  return v
}

export function parseNodeTrashPreview(value: unknown): NodeTrashPreview {
  const o = record(value),
    b = record(o.blocked)
  return {
    folders: count(o.folders),
    documents: count(o.documents),
    chats: count(o.chats),
    agentChats: count(o.agentChats),
    blocked: {
      agentActive: flag(b.agentActive),
      inUse: flag(b.inUse),
      privateOthers: flag(b.privateOthers),
    },
  }
}

export function parseTrashedNode(value: unknown): TrashedNode {
  const o = record(value)
  if (o.kind !== "project" && o.kind !== "folder") return invalid()
  return {
    nodeId: nodeId(o.nodeId),
    kind: o.kind,
    name: text(o.name),
    parentId: o.parentId === null ? null : nodeId(o.parentId),
    parentName: o.parentName === null ? null : text(o.parentName),
    parentTrashed:
      typeof o.parentTrashed === "boolean" ? o.parentTrashed : invalid(),
    trashedBy: nodeId(o.trashedBy),
    trashedByName: text(o.trashedByName, true),
    trashedAt: timestamp(o.trashedAt),
    folders: count(o.folders),
    documents: count(o.documents),
    chats: count(o.chats),
    agentChats: count(o.agentChats),
  }
}

/** The first thing that stops a Move to Trash, or null when nothing does. */
export function trashBlocker(
  preview: NodeTrashPreview
): NodeTrashBlocker | null {
  const order: NodeTrashBlocker[] = ["agentActive", "inUse", "privateOthers"]
  return order.find((key) => preview.blocked[key]) ?? null
}

export async function previewNodeTrash(
  id: number,
  signal?: AbortSignal
): Promise<NodeTrashPreview> {
  return parseNodeTrashPreview(
    await nativeRequest(
      `/api/neev/nodes/${nodeId(id)}/trash-preview`,
      {},
      signal
    )
  )
}

export async function moveNodeToTrash(
  id: number,
  expectedRevision: string,
  csrf: string,
  signal?: AbortSignal
): Promise<{ nodes: number; resources: number }> {
  const o = record(
    await nativeMutation(
      `/api/neev/nodes/${nodeId(id)}/trash`,
      { expectedRevision },
      csrf,
      signal
    )
  )
  text(o.batchId)
  return { nodes: count(o.nodes), resources: count(o.resources) }
}

export async function restoreNode(
  id: number,
  csrf: string,
  signal?: AbortSignal
): Promise<{ nodes: number; resources: number }> {
  const o = record(
    await nativeMutation(
      `/api/neev/nodes/${nodeId(id)}/restore`,
      {},
      csrf,
      signal
    )
  )
  return { nodes: count(o.nodes), resources: count(o.resources) }
}

export async function listTrashedNodes(
  signal?: AbortSignal
): Promise<TrashedNode[]> {
  const o = record(await nativeRequest("/api/neev/trash/nodes", {}, signal))
  if (!Array.isArray(o.items) || o.items.length > 200) return invalid()
  return o.items.map(parseTrashedNode)
}

/** Every node under `rootId` in an already-loaded tree (not including it). */
export function descendantIds(
  nodes: { id: number; parentId: number | null }[],
  rootId: number
): number[] {
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
