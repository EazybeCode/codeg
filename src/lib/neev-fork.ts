/**
 * "Fork to new tab": copy a conversation up through one reply into a NEW
 * private conversation the caller can continue.
 *
 * The server copies, never links: messages (or completed Agent Chat turns)
 * up through the selected reply, with their original authors and times, and
 * every file they carry copied to the new conversation. The original is never
 * changed, and the fork is private to the caller whatever folder it sits in.
 *
 * These helpers only start a fork and describe the result; the conversation
 * itself is read through the existing chat / Agent Chat clients.
 */
import { nativeMutation, NativeClientError } from "./neev-client"

export interface NeevChatFork {
  chatId: string
  sourceChatId: string
  throughMessageId: string
  nodeId: number
  copiedMessages: number
  copiedAttachments: number
  /** Files the source had already deleted (archived): not visible there, so not copied. */
  archivedAttachmentsNotCopied: number
  /** `true` when this request id had already produced this fork. */
  replayed: boolean
}

export interface NeevAgentChatFork {
  sessionId: string
  sourceSessionId: string
  throughTurnId: string
  copiedTurns: number
  copiedAttachments: number
  archivedAttachmentsNotCopied: number
  /** Failed or canceled turns before the selection, left out (never part of a run's context). */
  skippedIncompleteTurns: number
  replayed: boolean
}

/** Why a fork was refused; `NativeClientError.code` carries one of these. */
export type NeevForkRefusal =
  | "fork_not_complete"
  | "fork_too_large"
  | "fork_attachments_too_large"
  | "fork_source_trashed"
  | "fork_bound_chat_unavailable"

const REFUSALS: readonly NeevForkRefusal[] = [
  "fork_not_complete",
  "fork_too_large",
  "fork_attachments_too_large",
  "fork_source_trashed",
  "fork_bound_chat_unavailable",
]

/** The refusal behind an error from a fork helper, or `null` for any other failure. */
export function forkRefusal(error: unknown): NeevForkRefusal | null {
  if (!(error instanceof NativeClientError) || error.kind !== "conflict")
    return null
  return REFUSALS.find((code) => code === error.code) ?? null
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/

function invalid(): never {
  throw new NativeClientError("invalid_response")
}

function exact(
  value: unknown,
  keys: readonly string[]
): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return invalid()
  const object = value as Record<string, unknown>
  const found = Object.keys(object)
  if (found.length !== keys.length || !keys.every((key) => key in object))
    return invalid()
  return object
}

function id(value: unknown): string {
  return typeof value === "string" && UUID.test(value) ? value : invalid()
}

function count(value: unknown): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : invalid()
}

function flag(value: unknown): boolean {
  return typeof value === "boolean" ? value : invalid()
}

/** A caller-supplied id, refused before any request is made. */
function requestArgument(value: string): string {
  if (!UUID.test(value)) throw new NativeClientError("invalid_request")
  return value
}

export function parseChatFork(value: unknown): NeevChatFork {
  const o = exact(value, [
    "chatId",
    "sourceChatId",
    "throughMessageId",
    "nodeId",
    "copiedMessages",
    "copiedAttachments",
    "archivedAttachmentsNotCopied",
    "replayed",
  ])
  const nodeId = o.nodeId
  if (
    typeof nodeId !== "number" ||
    !Number.isInteger(nodeId) ||
    nodeId <= 0 ||
    nodeId > 2147483647
  )
    return invalid()
  const fork: NeevChatFork = {
    chatId: id(o.chatId),
    sourceChatId: id(o.sourceChatId),
    throughMessageId: id(o.throughMessageId),
    nodeId,
    copiedMessages: count(o.copiedMessages),
    copiedAttachments: count(o.copiedAttachments),
    archivedAttachmentsNotCopied: count(o.archivedAttachmentsNotCopied),
    replayed: flag(o.replayed),
  }
  // A fork is a NEW conversation that holds at least the selected reply.
  if (fork.chatId === fork.sourceChatId || fork.copiedMessages < 1)
    return invalid()
  return fork
}

export function parseAgentChatFork(value: unknown): NeevAgentChatFork {
  const o = exact(value, [
    "sessionId",
    "sourceSessionId",
    "throughTurnId",
    "copiedTurns",
    "copiedAttachments",
    "archivedAttachmentsNotCopied",
    "skippedIncompleteTurns",
    "replayed",
  ])
  const fork: NeevAgentChatFork = {
    sessionId: id(o.sessionId),
    sourceSessionId: id(o.sourceSessionId),
    throughTurnId: id(o.throughTurnId),
    copiedTurns: count(o.copiedTurns),
    copiedAttachments: count(o.copiedAttachments),
    archivedAttachmentsNotCopied: count(o.archivedAttachmentsNotCopied),
    skippedIncompleteTurns: count(o.skippedIncompleteTurns),
    replayed: flag(o.replayed),
  }
  if (fork.sessionId === fork.sourceSessionId || fork.copiedTurns < 1)
    return invalid()
  return fork
}

/**
 * Fork a resource chat up through the agent reply `throughMessageId`.
 * `requestId` makes it idempotent: keep the same id when retrying a fork
 * whose response was lost, so it resolves to the fork already made.
 */
export async function forkChat(
  chatId: string,
  throughMessageId: string,
  requestId: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevChatFork> {
  const chat = requestArgument(chatId)
  return parseChatFork(
    await nativeMutation(
      `/api/neev/chats/${chat}/fork`,
      {
        throughMessageId: requestArgument(throughMessageId),
        requestId: requestArgument(requestId),
      },
      csrf,
      signal
    )
  )
}

/** Fork the caller's Agent Chat up through the completed turn `throughTurnId`. */
export async function forkAgentChat(
  sessionId: string,
  throughTurnId: string,
  requestId: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAgentChatFork> {
  const session = requestArgument(sessionId)
  return parseAgentChatFork(
    await nativeMutation(
      `/api/neev/agent/sessions/${session}/fork`,
      {
        throughTurnId: requestArgument(throughTurnId),
        requestId: requestArgument(requestId),
      },
      csrf,
      signal
    )
  )
}
