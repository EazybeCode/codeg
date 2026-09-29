/**
 * Drag-and-drop move helpers for resources and agent sessions in the Neev sidebar.
 *
 * Handles:
 * - Serialization/deserialization of drag payloads
 * - Validation of cross-org and stale payloads
 * - MIME type constants for internal drag operations
 */

/** MIME type for internal move operations - not Files, not attachments */
export const NEEV_MOVE_MIME = "application/x-neev-move"

/** Payload carried during a drag operation */
export interface MovePayload {
  kind: "resource" | "session"
  /** Resource id for resources, session id for sessions */
  id: string
  /** Current parent node id */
  nodeId: number
  /** Organization id at drag start */
  orgId: number
  /** Auth/regular revision for optimistic concurrency */
  expectedRevision: string
}

/** Serialize a move payload for dataTransfer */
export function serializeMovePayload(payload: MovePayload): string {
  return JSON.stringify(payload)
}

/** Parse and validate a move payload from dataTransfer */
export function parseMovePayload(data: string): MovePayload | null {
  try {
    const parsed = JSON.parse(data)
    if (
      !parsed ||
      typeof parsed !== "object" ||
      (parsed.kind !== "resource" && parsed.kind !== "session") ||
      typeof parsed.id !== "string" ||
      !parsed.id ||
      !Number.isSafeInteger(parsed.nodeId) ||
      parsed.nodeId <= 0 ||
      !Number.isSafeInteger(parsed.orgId) ||
      parsed.orgId <= 0 ||
      typeof parsed.expectedRevision !== "string" ||
      !/^[1-9][0-9]*$/.test(parsed.expectedRevision)
    ) {
      return null
    }
    return {
      kind: parsed.kind,
      id: parsed.id,
      nodeId: parsed.nodeId,
      orgId: parsed.orgId,
      expectedRevision: parsed.expectedRevision,
    }
  } catch {
    return null
  }
}

/**
 * Check if a drag event contains our internal move payload.
 * Returns false for Files, attachments, or other external drags.
 */
export function isNeevMoveEvent(event: DragEvent): boolean {
  return (
    !isExternalFileDrag(event) &&
    (event.dataTransfer?.types.includes(NEEV_MOVE_MIME) ?? false)
  )
}

/**
 * Check if a drag event contains external files (which we should ignore for moves).
 */
export function isExternalFileDrag(event: DragEvent): boolean {
  const types = event.dataTransfer?.types ?? []
  // External file drops have "Files" in their types
  return types.includes("Files")
}

/**
 * Extract move payload from a drop event after validation.
 * Returns null for invalid, cross-org, or external payloads.
 */
export function extractMovePayload(
  event: DragEvent,
  currentOrgId: number
): MovePayload | null {
  if (!event.dataTransfer) return null
  if (isExternalFileDrag(event)) return null

  const data = event.dataTransfer.getData(NEEV_MOVE_MIME)
  if (!data) return null

  const payload = parseMovePayload(data)
  if (!payload) return null

  // Reject cross-org moves
  if (payload.orgId !== currentOrgId) return null

  return payload
}

/**
 * Determine if a move is valid (different target than source).
 */
export function isValidMove(
  payload: MovePayload,
  targetNodeId: number
): boolean {
  // Same parent = no-op, skip the request
  return payload.nodeId !== targetNodeId
}
