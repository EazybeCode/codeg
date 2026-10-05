/**
 * A folder's audience for NEW items (schema 21): `/api/neev/nodes/{id}/sharing`.
 *
 * Creation-time only. The server resolves it — the folder's own setting, else
 * the nearest folder above that has one, else private — when a document or
 * chat is created there with no explicit visibility, and stores the result on
 * that item. Changing it never rewrites an existing item: a private exception
 * stays private, and an item moved in keeps its audience. The folder itself,
 * and its name, are visible to the whole organization either way.
 *
 * Parsed strictly, like every other Neev response: a shape this client does
 * not understand is refused rather than guessed at, because the one thing it
 * must never do is show "private" for something the server will share.
 */
import { NativeClientError } from "./neev-client"
import { contentGet, contentMutation } from "./neev-content-client"

export type FolderDefault = "inherit" | "private" | "shared" | "organization"
export type FolderAudience = "private" | "shared" | "organization"

export interface FolderSharing {
  nodeId: number
  /** The node's own revision; a change here moves it, like a rename does. */
  revision: string
  /** What is set on THIS folder. `inherit` means nothing is. */
  defaultVisibility: FolderDefault
  /** What a new item created here right now would get. */
  effectiveVisibility: FolderAudience
  /** The folder it comes from when inherited; null when set here or nowhere. */
  inheritedFrom: number | null
  /** The folder's creator or an organization admin. */
  canManage: boolean
}

const KEYS = [
  "nodeId",
  "revision",
  "defaultVisibility",
  "effectiveVisibility",
  "inheritedFrom",
  "canManage",
] as const

function invalid(): never {
  throw new NativeClientError("invalid_response")
}
function invalidRequest(): never {
  throw new NativeClientError("invalid_request")
}
const isId = (value: unknown): value is number =>
  typeof value === "number" &&
  Number.isInteger(value) &&
  value > 0 &&
  value <= 2_147_483_647
const isRevision = (value: unknown): value is string =>
  typeof value === "string" && /^[1-9][0-9]{0,18}$/.test(value)

export function parseFolderSharing(
  value: unknown,
  nodeId: number
): FolderSharing {
  if (!value || typeof value !== "object" || Array.isArray(value)) invalid()
  const object = value as Record<string, unknown>
  const keys = Object.keys(object).sort()
  if (keys.join() !== [...KEYS].sort().join()) invalid()
  const {
    nodeId: id,
    revision,
    defaultVisibility,
    effectiveVisibility,
    inheritedFrom,
    canManage,
  } = object
  if (id !== nodeId || !isRevision(revision) || typeof canManage !== "boolean")
    invalid()
  if (
    defaultVisibility !== "inherit" &&
    defaultVisibility !== "private" &&
    defaultVisibility !== "shared" &&
    defaultVisibility !== "organization"
  )
    invalid()
  if (
    effectiveVisibility !== "private" &&
    effectiveVisibility !== "shared" &&
    effectiveVisibility !== "organization"
  )
    invalid()
  if (inheritedFrom !== null && !isId(inheritedFrom)) invalid()
  // Internally consistent, or refused: a setting made here IS what applies
  // here, comes from nowhere else, and a folder cannot inherit from itself.
  if (
    defaultVisibility !== "inherit" &&
    (effectiveVisibility !== defaultVisibility || inheritedFrom !== null)
  )
    invalid()
  if (inheritedFrom === nodeId) invalid()
  // Nothing set anywhere above falls back to private, never to shared.
  if (
    defaultVisibility === "inherit" &&
    inheritedFrom === null &&
    effectiveVisibility !== "private"
  )
    invalid()
  return {
    nodeId: id,
    revision,
    defaultVisibility,
    effectiveVisibility,
    inheritedFrom,
    canManage,
  }
}

function assertNode(nodeId: number) {
  if (!isId(nodeId)) invalidRequest()
}

export async function loadFolderSharing(
  nodeId: number,
  signal?: AbortSignal
): Promise<FolderSharing> {
  assertNode(nodeId)
  return parseFolderSharing(
    await contentGet(`/api/neev/nodes/${nodeId}/sharing`, signal),
    nodeId
  )
}

export async function updateFolderSharing(
  nodeId: number,
  input: { expectedRevision: string; defaultVisibility: FolderDefault },
  csrf: string,
  signal?: AbortSignal
): Promise<FolderSharing> {
  assertNode(nodeId)
  if (
    !isRevision(input.expectedRevision) ||
    !["inherit", "private", "shared", "organization"].includes(
      input.defaultVisibility
    )
  )
    invalidRequest()
  return parseFolderSharing(
    await contentMutation(
      `/api/neev/nodes/${nodeId}/sharing`,
      {
        expectedRevision: input.expectedRevision,
        defaultVisibility: input.defaultVisibility,
      },
      csrf,
      signal,
      "PATCH"
    ),
    nodeId
  )
}

/**
 * The later of two decimal revisions of the same node.
 *
 * For keeping the TREE monotonic only: a save's answer may arrive after a
 * rename already moved the tree past it, and taking the older one would make
 * the next rename a conflict nobody caused. Never for choosing what to send —
 * a request carries the revision of what the reader was actually shown.
 */
export function newerRevision(current: string, incoming: string): string {
  try {
    return BigInt(incoming) > BigInt(current) ? incoming : current
  } catch {
    return current
  }
}
