// Client for Conductor-style saved Agent Chats (spec §5).
//
// Every shape here mirrors a DTO the server actually serialises
// (`neev/http/agent_sessions.rs`); nothing is invented. Responses are validated
// strictly — an unexpected field or an out-of-domain status is a protocol error,
// not something to render — because this surface shows private agent output and
// honest connection state, and a lenient parser would let a wrong server make
// the UI claim things that are not true.

import { NativeClientError } from "./neev-client"
import {
  contentGet,
  contentMutation,
  NativeContentNotFoundError,
  type NativePage,
} from "./neev-content-client"

/** Providers the server will accept for a session (`agents/sessions.rs`). */
export const NEEV_AGENT_PROVIDERS = ["claude", "codex"] as const
export type NeevAgentProvider = (typeof NEEV_AGENT_PROVIDERS)[number]

/**
 * Models offered per provider. The server validates only shape (1..=80 chars),
 * so this list is the UI's own promise about what it will send; it must not
 * grow past what the runtime can actually dispatch.
 */
export const NEEV_AGENT_MODELS: Record<NeevAgentProvider, readonly string[]> = {
  claude: ["default"],
  codex: ["default"],
}

export type NeevTurnStatus = "running" | "completed" | "failed" | "canceled"

/**
 * Connection states the server reports for the caller's own per-user provider
 * connection. `disabled` means no runtime is bound at all, so no connection can
 * be exercised — it is never shown as "Connected".
 */
export type NeevConnectionState =
  | "not_signed_in"
  | "connecting"
  | "connected"
  | "reconnect_required"
  | "disconnecting"
  | "unavailable"
  | "disabled"

export interface NeevAgentSession {
  id: string
  orgId: number
  ownerId: number
  title: string
  provider: string
  model: string
  boundChatId: string | null
  archived: boolean
  revision: string
  createdAt: number
  updatedAt: number
}

export interface NeevAgentTurn {
  id: string
  sessionId: string
  sequence: string
  runId: string
  prompt: string
  response: string
  status: NeevTurnStatus
  errorCode: string | null
  provider: string
  model: string
  createdAt: number
  updatedAt: number
  completedAt: number | null
}

export interface NeevStartedTurn {
  turn: NeevAgentTurn
  runId: string
}

export interface NeevAgentConnection {
  provider: NeevAgentProvider
  state: NeevConnectionState
  accountLabel: string | null
  checkedAt: number | null
  errorCode: string | null
}

export interface NeevAgentConnections {
  runtimeEnabled: boolean
  connections: NeevAgentConnection[]
}

export interface CreateNeevAgentSessionInput {
  title: string
  provider: NeevAgentProvider
  model: string
  boundChatId?: string | null
  requestId: string
}

/** The server accepts exactly one of `title` / `archived` per request. */
export type UpdateNeevAgentSessionInput = { expectedRevision: string } & (
  | { title: string; archived?: never }
  | { archived: boolean; title?: never }
)

export type NeevRunEvent =
  | { type: "started"; runId: string; turnId: string; sequence: string | null }
  | { type: "chunk"; text: string }
  | { type: "finished"; status: NeevTurnStatus; errorCode: string | null }

const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
/** Run ids are the server's dash-less uuid: 32 lowercase hex digits. */
const RUN_ID = /^[0-9a-f]{32}$/
const TURN_STATUSES: readonly NeevTurnStatus[] = [
  "running",
  "completed",
  "failed",
  "canceled",
]
const CONNECTION_STATES: readonly NeevConnectionState[] = [
  "not_signed_in",
  "connecting",
  "connected",
  "reconnect_required",
  "disconnecting",
  "unavailable",
  "disabled",
]
/** `MAX_PROMPT_CHARS` in `agents/sessions.rs`. */
export const NEEV_MAX_PROMPT_CHARS = 32_768
/** `MAX_SESSIONS_PER_OWNER` — the list endpoint cannot return more. */
const MAX_SESSIONS = 200

function invalid(): never {
  throw new NativeClientError("invalid_response")
}

function invalidRequest(): never {
  throw new NativeClientError("invalid_request")
}

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return invalid()
  return value as Record<string, unknown>
}

function exact(
  value: unknown,
  keys: readonly string[]
): Record<string, unknown> {
  const object = record(value)
  const actual = Object.keys(object).sort()
  const expected = [...keys].sort()
  if (
    actual.length !== expected.length ||
    actual.some((key, index) => key !== expected[index])
  )
    return invalid()
  return object
}

function text(value: unknown): string {
  if (typeof value !== "string") return invalid()
  return value
}

function positiveId(value: unknown): number {
  if (
    typeof value !== "number" ||
    !Number.isInteger(value) ||
    value <= 0 ||
    value > 2_147_483_647
  )
    return invalid()
  return value
}

function timestamp(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0)
    return invalid()
  return value
}

function nullableTimestamp(value: unknown): number | null {
  return value === null ? null : timestamp(value)
}

function integerString(value: unknown): string {
  const parsed = text(value)
  if (
    !/^[1-9]\d{0,18}$/.test(parsed) ||
    (parsed.length === 19 && parsed > "9223372036854775807")
  )
    return invalid()
  return parsed
}

function uuid(value: unknown): string {
  const parsed = text(value)
  if (!UUID.test(parsed)) return invalid()
  return parsed
}

function runId(value: unknown): string {
  const parsed = text(value)
  if (!RUN_ID.test(parsed)) return invalid()
  return parsed
}

function nullableCode(value: unknown): string | null {
  if (value === null) return null
  const parsed = text(value)
  // Codes are short machine tokens; anything else is not a code we can map to
  // an honest message, so refuse rather than render a server string verbatim.
  if (!/^[a-z_]{1,40}$/.test(parsed)) return invalid()
  return parsed
}

export function parseAgentSession(value: unknown): NeevAgentSession {
  const object = exact(value, [
    "id",
    "orgId",
    "ownerId",
    "title",
    "provider",
    "model",
    "boundChatId",
    "archived",
    "revision",
    "createdAt",
    "updatedAt",
  ])
  if (typeof object.archived !== "boolean") return invalid()
  const title = text(object.title)
  if (
    title.length === 0 ||
    [...title].length > 100 ||
    [...title].some((character) => /\p{Cc}/u.test(character))
  )
    return invalid()
  const model = text(object.model)
  if (model.length === 0 || [...model].length > 80) return invalid()
  return {
    id: uuid(object.id),
    orgId: positiveId(object.orgId),
    ownerId: positiveId(object.ownerId),
    title,
    provider: text(object.provider),
    model,
    boundChatId: object.boundChatId === null ? null : uuid(object.boundChatId),
    archived: object.archived,
    revision: integerString(object.revision),
    createdAt: timestamp(object.createdAt),
    updatedAt: timestamp(object.updatedAt),
  }
}

export function parseAgentTurn(value: unknown): NeevAgentTurn {
  const object = exact(value, [
    "id",
    "sessionId",
    "sequence",
    "runId",
    "prompt",
    "response",
    "status",
    "errorCode",
    "provider",
    "model",
    "createdAt",
    "updatedAt",
    "completedAt",
  ])
  const status = text(object.status)
  if (!TURN_STATUSES.includes(status as NeevTurnStatus)) return invalid()
  const completedAt = nullableTimestamp(object.completedAt)
  // A settled turn has a completion time and a running one does not; the pair
  // disagreeing means the row is not something we can display truthfully.
  if ((status === "running") === (completedAt !== null)) return invalid()
  return {
    id: uuid(object.id),
    sessionId: uuid(object.sessionId),
    sequence: integerString(object.sequence),
    runId: runId(object.runId),
    prompt: text(object.prompt),
    response: text(object.response),
    status: status as NeevTurnStatus,
    errorCode: nullableCode(object.errorCode),
    provider: text(object.provider),
    model: text(object.model),
    createdAt: timestamp(object.createdAt),
    updatedAt: timestamp(object.updatedAt),
    completedAt,
  }
}

export function parseAgentConnections(value: unknown): NeevAgentConnections {
  const object = exact(value, ["runtimeEnabled", "connections"])
  if (
    typeof object.runtimeEnabled !== "boolean" ||
    !Array.isArray(object.connections) ||
    object.connections.length > NEEV_AGENT_PROVIDERS.length
  )
    return invalid()
  const connections = object.connections.map((item) => {
    const entry = exact(item, [
      "provider",
      "state",
      "accountLabel",
      "checkedAt",
      "errorCode",
    ])
    const provider = text(entry.provider)
    const state = text(entry.state)
    if (
      !NEEV_AGENT_PROVIDERS.includes(provider as NeevAgentProvider) ||
      !CONNECTION_STATES.includes(state as NeevConnectionState)
    )
      return invalid()
    return {
      provider: provider as NeevAgentProvider,
      state: state as NeevConnectionState,
      accountLabel:
        entry.accountLabel === null ? null : text(entry.accountLabel),
      checkedAt: nullableTimestamp(entry.checkedAt),
      errorCode: nullableCode(entry.errorCode),
    }
  })
  // With no runtime bound nothing can be exercised, so the server must not have
  // reported a usable connection. Refuse rather than render a false "Connected".
  if (
    !object.runtimeEnabled &&
    connections.some((connection) => connection.state !== "disabled")
  )
    return invalid()
  return { runtimeEnabled: object.runtimeEnabled, connections }
}

export function parseAgentSessions(value: unknown): NeevAgentSession[] {
  const object = exact(value, ["sessions"])
  if (!Array.isArray(object.sessions) || object.sessions.length > MAX_SESSIONS)
    return invalid()
  return object.sessions.map(parseAgentSession)
}

export function parseAgentTurnPage(value: unknown): NativePage<NeevAgentTurn> {
  const object = exact(value, ["items", "nextAfter"])
  if (!Array.isArray(object.items) || object.items.length > 100)
    return invalid()
  const items = object.items.map(parseAgentTurn)
  const nextAfter =
    object.nextAfter === null ? null : integerString(object.nextAfter)
  if (
    nextAfter !== null &&
    (items.length !== 100 || items[items.length - 1].sequence !== nextAfter)
  )
    return invalid()
  return { items, nextAfter }
}

export function parseStartedTurn(value: unknown): NeevStartedTurn {
  const object = exact(value, ["turn", "runId"])
  const turn = parseAgentTurn(object.turn)
  const id = runId(object.runId)
  // The envelope's run id must be the turn's own, or a reattach would follow a
  // different run than the one just started.
  if (id !== turn.runId) return invalid()
  return { turn, runId: id }
}

function assertUuid(value: string): void {
  if (!UUID.test(value)) invalidRequest()
}

function assertRunId(value: string): void {
  if (!RUN_ID.test(value)) invalidRequest()
}

export async function listAgentSessions(
  includeArchived = false,
  signal?: AbortSignal
): Promise<NeevAgentSession[]> {
  return parseAgentSessions(
    await contentGet(
      `/api/neev/agent/sessions${includeArchived ? "?archived=1" : ""}`,
      signal
    )
  )
}

export async function createAgentSession(
  input: CreateNeevAgentSessionInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAgentSession> {
  assertUuid(input.requestId)
  if (
    !NEEV_AGENT_PROVIDERS.includes(input.provider) ||
    typeof input.title !== "string" ||
    typeof input.model !== "string" ||
    input.model.length === 0 ||
    [...input.model].length > 80
  )
    invalidRequest()
  if (input.boundChatId) assertUuid(input.boundChatId)
  const body: Record<string, unknown> = {
    title: input.title,
    provider: input.provider,
    model: input.model,
    requestId: input.requestId,
  }
  // `deny_unknown_fields` accepts an absent optional but not an unexpected one,
  // so only send the binding when there is one.
  if (input.boundChatId) body.boundChatId = input.boundChatId
  return parseAgentSession(
    await contentMutation("/api/neev/agent/sessions", body, csrf, signal)
  )
}

export async function loadAgentSession(
  sessionId: string,
  signal?: AbortSignal
): Promise<NeevAgentSession> {
  assertUuid(sessionId)
  const session = parseAgentSession(
    await contentGet(`/api/neev/agent/sessions/${sessionId}`, signal)
  )
  if (session.id !== sessionId) invalid()
  return session
}

export async function updateAgentSession(
  sessionId: string,
  input: UpdateNeevAgentSessionInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAgentSession> {
  assertUuid(sessionId)
  try {
    integerString(input.expectedRevision)
  } catch {
    invalidRequest()
  }
  const renaming = Object.prototype.hasOwnProperty.call(input, "title")
  const archiving = Object.prototype.hasOwnProperty.call(input, "archived")
  // Exactly one field per request: the server refuses a combined change because
  // it would need two revisions and could half-apply.
  if (renaming === archiving) invalidRequest()
  const body = renaming
    ? { title: input.title, expectedRevision: input.expectedRevision }
    : { archived: input.archived, expectedRevision: input.expectedRevision }
  return parseAgentSession(
    await contentMutation(
      `/api/neev/agent/sessions/${sessionId}`,
      body,
      csrf,
      signal,
      "PATCH"
    )
  )
}

export async function listAgentTurns(
  sessionId: string,
  after: string | null = null,
  signal?: AbortSignal
): Promise<NativePage<NeevAgentTurn>> {
  assertUuid(sessionId)
  if (after !== null) {
    try {
      integerString(after)
    } catch {
      invalidRequest()
    }
  }
  const page = parseAgentTurnPage(
    await contentGet(
      `/api/neev/agent/sessions/${sessionId}/turns${
        after === null ? "" : `?after=${after}`
      }`,
      signal
    )
  )
  if (page.items.some((turn) => turn.sessionId !== sessionId)) invalid()
  return page
}

/**
 * Start a turn. The server persists the prompt BEFORE launching, so a refusal
 * comes back as a settled failed turn rather than a request that vanished —
 * the caller renders whatever `status`/`errorCode` says and never retries on
 * its own.
 */
export async function startAgentTurn(
  sessionId: string,
  prompt: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevStartedTurn> {
  assertUuid(sessionId)
  if (
    typeof prompt !== "string" ||
    prompt.length === 0 ||
    [...prompt].length > NEEV_MAX_PROMPT_CHARS
  )
    invalidRequest()
  const started = parseStartedTurn(
    await contentMutation(
      `/api/neev/agent/sessions/${sessionId}/turns`,
      { prompt },
      csrf,
      signal
    )
  )
  if (started.turn.sessionId !== sessionId) invalid()
  return started
}

export async function cancelAgentRun(
  runIdValue: string,
  csrf: string,
  signal?: AbortSignal
): Promise<{ canceled: boolean; status: NeevTurnStatus }> {
  assertRunId(runIdValue)
  const object = exact(
    await contentMutation(
      "/api/neev/agent/runs/cancel",
      { runId: runIdValue },
      csrf,
      signal
    ),
    ["canceled", "status"]
  )
  const status = text(object.status)
  if (
    typeof object.canceled !== "boolean" ||
    !TURN_STATUSES.includes(status as NeevTurnStatus)
  )
    return invalid()
  return { canceled: object.canceled, status: status as NeevTurnStatus }
}

export async function loadAgentConnections(
  signal?: AbortSignal
): Promise<NeevAgentConnections> {
  return parseAgentConnections(
    await contentGet("/api/neev/agent/connections", signal)
  )
}

export function parseRunEvent(json: string): NeevRunEvent | null {
  let value: unknown
  try {
    value = JSON.parse(json)
  } catch {
    return null
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return null
  const object = value as Record<string, unknown>
  try {
    switch (object.type) {
      case "started":
        return {
          type: "started",
          runId: runId(object.runId),
          turnId: uuid(object.turnId),
          // A live reattach omits the sequence; a replayed settled turn has it.
          sequence:
            object.sequence === undefined
              ? null
              : integerString(object.sequence),
        }
      case "chunk":
        return { type: "chunk", text: text(object.text) }
      case "finished": {
        const status = text(object.status)
        if (!TURN_STATUSES.includes(status as NeevTurnStatus)) return null
        return {
          type: "finished",
          status: status as NeevTurnStatus,
          errorCode:
            object.errorCode === undefined
              ? null
              : nullableCode(object.errorCode),
        }
      }
      default:
        return null
    }
  } catch {
    return null
  }
}

/**
 * Attach to a run and replay it. This is the ONLY stream: the first view of a
 * run and a view after a dropped connection take the same path, so reconnecting
 * can never re-send a prompt or re-execute a turn (§5.2).
 *
 * Resolves when the server ends the stream. Rejects when the run cannot be
 * followed, so the caller can fall back to reading the stored turn.
 */
export type NeevRunPhase = "streaming" | "reconnecting" | "lost"

/** Backoff between reattach attempts; exhausting it reports the run as lost. */
const RUN_RETRY_DELAYS = [500, 1000, 2000, 4000] as const
/**
 * How long an attachment must survive before ending it counts as routine (the
 * server caps a stream at 900s and the client simply reattaches). A shorter
 * one is a fault — without this floor a server that accepts and hangs up would
 * be reattached in a tight loop.
 */
const RUN_HEALTHY_AFTER_MS = 5_000

/**
 * Follow a run to completion, reattaching across drops.
 *
 * Every attempt is a GET on the run, so a reconnect can never re-send a prompt
 * or re-execute a turn (§5.2). When the run can no longer be followed this
 * reports `lost` and stops: recovering is an explicit new turn by the user,
 * never an automatic re-run.
 */
export async function followAgentRun({
  runId: runIdValue,
  signal,
  onEvent,
  onPhase,
  retryDelays = RUN_RETRY_DELAYS,
  healthyAfterMs = RUN_HEALTHY_AFTER_MS,
  open = openAgentRunStream,
}: {
  runId: string
  signal: AbortSignal
  onEvent: (event: NeevRunEvent) => void
  onPhase: (phase: NeevRunPhase) => void
  retryDelays?: readonly number[]
  healthyAfterMs?: number
  open?: typeof openAgentRunStream
}): Promise<void> {
  let failures = 0
  let finished = false
  const done = () => finished || signal.aborted
  const sleep = (ms: number) =>
    new Promise<void>((resolve) => {
      if (signal.aborted) return resolve()
      const settleSleep = () => {
        clearTimeout(handle)
        signal.removeEventListener("abort", settleSleep)
        resolve()
      }
      const handle = setTimeout(settleSleep, ms)
      signal.addEventListener("abort", settleSleep)
    })
  for (;;) {
    if (done()) return
    let delivered = false
    const startedAt = Date.now()
    onPhase("streaming")
    try {
      await open(
        runIdValue,
        (event) => {
          if (done()) return
          delivered = true
          if (event.type === "finished") finished = true
          onEvent(event)
        },
        signal
      )
    } catch (error) {
      if (done()) return
      // The run is gone, or was never ours to read: stop rather than retry a
      // request that cannot start succeeding.
      if (
        error instanceof NativeContentNotFoundError ||
        (error instanceof NativeClientError &&
          (error.kind === "unauthorized" || error.kind === "forbidden"))
      ) {
        onPhase("lost")
        return
      }
    }
    if (done()) return
    // The stream ended without a terminal event. Only a long-lived attachment
    // ending that way is routine (the server's lifetime cap); anything shorter
    // is a fault and must back off.
    const healthy = delivered && Date.now() - startedAt >= healthyAfterMs
    failures = healthy ? 0 : failures + 1
    if (failures > retryDelays.length) {
      onPhase("lost")
      return
    }
    if (failures > 0) {
      onPhase("reconnecting")
      await sleep(retryDelays[failures - 1])
    }
  }
}

export async function openAgentRunStream(
  runIdValue: string,
  onEvent: (event: NeevRunEvent) => void,
  signal?: AbortSignal
): Promise<void> {
  assertRunId(runIdValue)
  const response = await fetch(`/api/neev/agent/runs/${runIdValue}/events`, {
    credentials: "same-origin",
    cache: "no-store",
    referrerPolicy: "no-referrer",
    redirect: "error",
    headers: { Accept: "text/event-stream" },
    signal,
  })
  if (response.status === 401) throw new NativeClientError("unauthorized")
  if (response.status === 403) throw new NativeClientError("forbidden")
  if (response.status === 404) throw new NativeContentNotFoundError()
  if (!response.ok || !response.body)
    throw new NativeClientError("unavailable", response.status)
  if (!response.headers.get("Content-Type")?.includes("text/event-stream"))
    throw new NativeClientError("unavailable", response.status)
  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (value) buffer += decoder.decode(value, { stream: true })
    let boundary = buffer.indexOf("\n\n")
    while (boundary >= 0) {
      const frame = buffer.slice(0, boundary)
      buffer = buffer.slice(boundary + 2)
      for (const line of frame.split("\n"))
        if (line.startsWith("data:")) {
          const event = parseRunEvent(line.slice(5).trim())
          if (event) onEvent(event)
        }
      boundary = buffer.indexOf("\n\n")
    }
    if (done) break
  }
}
