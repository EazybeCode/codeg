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
  parseMessage,
  type NativeMessage,
  type NativePage,
} from "./neev-content-client"

/** Providers the server will accept for a session (`agents/sessions.rs`). */
export const NEEV_AGENT_PROVIDERS = ["claude", "codex"] as const
export type NeevAgentProvider = (typeof NEEV_AGENT_PROVIDERS)[number]

/**
 * `"default"` is the provider's own default: the runtime expresses it by passing
 * no `--model` at all rather than by naming a model here. Any other value is
 * passed through to the provider verbatim, so the UI must not invent names — it
 * offers `default` and lets the operator type an identifier their provider
 * actually has.
 */
export const NEEV_DEFAULT_MODEL = "default"

/**
 * Mirrors `safe_model` in `agents/execution.rs`: bounded, and free of anything
 * that could be read as a flag or a separator. Checking it here means a model
 * the runtime would refuse is caught before a turn is ever persisted.
 */
export function isSupportedModel(value: string): boolean {
  return (
    value === NEEV_DEFAULT_MODEL ||
    (value.length >= 1 &&
      value.length <= 80 &&
      !value.startsWith("-") &&
      /^[A-Za-z0-9._-]+$/.test(value))
  )
}

/**
 * A reasoning level, as the runner spells it.
 *
 * Deliberately a bare `string` and not a union: the levels differ per provider
 * (`max` is Claude's, `minimal` is Codex's) and codex-cli types its own as "a
 * non-empty reasoning effort value advertised by the model", so a closed list
 * here would refuse a level the server had just been taught. The shape is
 * checked — a short lowercase token, nothing that could read as a flag — and
 * membership is decided by the catalogue, which is the only thing that knows.
 */
export type NeevEffort = string

/** `null` is not a level: it means "pass no flag", i.e. the provider default. */
export function isSupportedEffort(value: unknown): value is NeevEffort {
  return typeof value === "string" && /^[a-z][a-z0-9_-]{0,30}$/.test(value)
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
  /**
   * The reasoning level this session runs at, or `null` for the provider's own
   * default (no flag passed). This — not local component state — is the live
   * run configuration, which is what makes it still true after a reload.
   */
  effort: NeevEffort | null
  /** Whether this session runs in fast mode. Defaults false and never inherited. */
  fast: boolean
  boundChatId: string | null
  archived: boolean
  /**
   * The project this session is filed under, or null for one that is not.
   *
   * Filing is presentation, never permission: a session is private to its owner
   * whatever it points at, and the server's list remains the only thing that
   * decides which sessions the caller can see.
   */
  nodeId: number | null
  revision: string
  createdAt: number
  updatedAt: number
}

/** Server-emitted trace states. `truncated` and `unsupported` are ours. */
export type NeevActivityState =
  | "model"
  | "stop"
  | "result"
  | "truncated"
  | "unsupported"

interface NeevActivityBase {
  /**
   * Monotonic within a turn and the DEDUPE KEY. A reattach replays the stored
   * prefix before streaming live items, so the same item can arrive twice —
   * upsert on this, never append.
   */
  seq: number
  /**
   * Where in the reply text this happened, counted in **Unicode scalar values**
   * (Rust `chars()`), NOT UTF-16 code units. `text.slice(0, offset)` is wrong
   * past the first astral character; use {@link sliceByScalars}.
   */
  offset: number
  id: string
}

export type NeevActivityItem =
  | (NeevActivityBase & {
      kind: "tool_call"
      name: string
      /**
       * Already-serialised JSON, as a STRING: a tool input streamed in
       * fragments is not always valid JSON when its block ends, so the server
       * forwards what arrived rather than guessing. `JSON.parse` inside a try,
       * and treat it as untrusted display text.
       */
      input: string
      inputTruncated: boolean
    })
  | (NeevActivityBase & {
      kind: "tool_result"
      /** The `id` of the `tool_call` this answers; may match none. */
      callId: string
      output: string
      outputTruncated: boolean
      isError: boolean
    })
  | (NeevActivityBase & {
      kind: "status"
      state: NeevActivityState
      detail: string | null
    })

/**
 * Slice `text` at a server `offset`.
 *
 * The server counts Unicode scalars and a JavaScript string is UTF-16, so a
 * plain `slice` drifts left by one per astral character (an emoji is 1 scalar
 * and 2 UTF-16 units). Spreading into an array iterates by code point, which is
 * the unit the server promised.
 */
export function sliceByScalars(text: string, offset: number): string {
  if (offset <= 0) return ""
  return [...text].slice(0, offset).join("")
}

export interface NeevAgentTurn {
  id: string
  sessionId: string
  sequence: string
  runId: string
  prompt: string
  response: string
  /** The turn's tool trace; `[]` when it used none. Seed the view from this. */
  activity: NeevActivityItem[]
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

/**
 * The whole run configuration, which is the only way it may be changed.
 *
 * All three together, never one at a time: the server validates them as a
 * COMBINATION, so patching model and speed in sequence would put the session
 * through a pairing no run could honour (Fast still on after moving to a model
 * that cannot do it) and burn two revisions that a second editor can interleave.
 */
export interface NeevAgentSettings {
  model: string
  effort: NeevEffort | null
  fast: boolean
}

export interface CreateNeevAgentSessionInput {
  title: string
  provider: NeevAgentProvider
  model: string
  /** Omitted when null, which is what asks for the provider's own default. */
  effort?: NeevEffort | null
  /** Omitted when false, which is the default everywhere and never inherited. */
  fast?: boolean
  boundChatId?: string | null
  /** File it under a project on creation, so it opens where it was started. */
  nodeId?: number | null
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
/**
 * An agent session id, which is NOT always a UUID.
 *
 * A team session's id is DERIVED — `team_session_id` in `agents/sessions.rs`
 * hashes `(org, owner, chat, provider)` and lays the digest out in the
 * canonical 8-4-4-4-12 shape so it looks like one. The version and variant
 * nibbles a real UUID reserves are, in a digest, just more hash output: roughly
 * fifteen in sixteen team ids fail the version check and eleven in sixteen fail
 * the variant check, so most of them were refused as `invalid_response` — which
 * is what broke live invocation, because a turn carries its session id.
 *
 * So a session id is validated as what it actually is: an opaque, canonically
 * shaped, lowercase hex token. Every real UUID still matches, so a personal
 * session is unaffected. This is used for session ids ONLY — chat, resource,
 * turn, message and request ids stay strict UUIDs, because those really are
 * generated as UUIDs and loosening them would buy nothing.
 */
const SESSION_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
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

function sessionId(value: unknown): string {
  const parsed = text(value)
  if (!SESSION_ID.test(parsed)) return invalid()
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
  // `nodeId` is OPTIONAL: a server that predates schema 12 does not send it and
  // must keep working, and one that does must not be refused for saying more
  // than we used to expect. Absent reads as unfiled.
  //
  // `effort` and `fast` are optional for the same reason and only that reason:
  // during a rolling deployment one replica answers with them and another does
  // not, and refusing the older one would black out every saved chat mid-roll.
  // Absent is read as the documented default — no level, not fast — never as
  // "unknown", so nothing on screen claims a configuration the server did not
  // send. Once every replica is new these become required.
  const FIELDS = [
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
  ]
  const present = (["nodeId", "effort", "fast"] as const).filter(
    (key) => record(value)[key] !== undefined
  )
  const object = exact(value, [...FIELDS, ...present])
  if (typeof object.archived !== "boolean") return invalid()
  if (object.fast !== undefined && typeof object.fast !== "boolean")
    return invalid()
  if (
    object.effort !== undefined &&
    object.effort !== null &&
    !isSupportedEffort(object.effort)
  )
    return invalid()
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
    // Opaque: a team session's id is a digest, not a UUID. See `SESSION_ID`.
    id: sessionId(object.id),
    orgId: positiveId(object.orgId),
    ownerId: positiveId(object.ownerId),
    title,
    provider: text(object.provider),
    model,
    effort:
      object.effort === undefined ? null : (object.effort as string | null),
    fast: object.fast === true,
    boundChatId: object.boundChatId === null ? null : uuid(object.boundChatId),
    archived: object.archived,
    nodeId:
      object.nodeId === undefined || object.nodeId === null
        ? null
        : positiveId(object.nodeId),
    revision: integerString(object.revision),
    createdAt: timestamp(object.createdAt),
    updatedAt: timestamp(object.updatedAt),
  }
}

/**
 * One model the runner has been shown to accept.
 *
 * `supportsSpeed` is per MODEL, not per provider: Claude's fast mode runs on
 * Opus and nowhere else, so the chip's availability follows the selection.
 */
export interface NeevAgentModel {
  id: string
  /** The server's own display name. Never invented here, never translated. */
  label: string
  /** The one used when the session says `default`. */
  isDefault: boolean
  supportsSpeed: boolean
  /**
   * The levels THIS model accepts, or `null` when the provider's list applies.
   *
   * `null` is "the provider list applies", not "every level is certainly fine":
   * Claude's CLI reports one set for the whole provider and nothing per model,
   * so narrowing it here would be a guess. Codex's app-server does report a set
   * per model, so discovered entries carry a real one.
   */
  efforts: readonly NeevEffort[] | null
}

export interface NeevAgentProviderCatalog {
  provider: NeevAgentProvider
  label: string
  models: readonly NeevAgentModel[]
  efforts: readonly NeevEffort[]
  /** `null` means the CLI's own default, which is a choice and not an absence. */
  defaultEffort: NeevEffort | null
  /** Whether the provider has the speed control at all. */
  supportsSpeed: boolean
}

export interface NeevAgentCapabilities {
  providers: readonly NeevAgentProviderCatalog[]
}

/** A catalogue big enough for any real runner and small enough to render. */
const MAX_MODELS = 50
const MAX_EFFORTS = 20

function effortList(value: unknown): NeevEffort[] {
  if (!Array.isArray(value) || value.length > MAX_EFFORTS) return invalid()
  const levels = value.map((level) => {
    if (!isSupportedEffort(level)) return invalid()
    return level
  })
  if (new Set(levels).size !== levels.length) return invalid()
  return levels
}

function agentModel(value: unknown): NeevAgentModel {
  // `efforts` is optional AND nullable: the catalogue omits it for a model
  // whose provider list applies, and sends an explicit null for the same.
  const graded = record(value).efforts !== undefined
  const keys = ["id", "label", "default", "supportsSpeed"]
  const object = exact(value, graded ? [...keys, "efforts"] : keys)
  const id = text(object.id)
  const label = text(object.label)
  if (
    !isSupportedModel(id) ||
    label.length === 0 ||
    [...label].length > 80 ||
    [...label].some((character) => /\p{Cc}/u.test(character)) ||
    typeof object.default !== "boolean" ||
    typeof object.supportsSpeed !== "boolean"
  )
    return invalid()
  return {
    id,
    label,
    isDefault: object.default,
    supportsSpeed: object.supportsSpeed,
    efforts:
      !graded || object.efforts === null ? null : effortList(object.efforts),
  }
}

function providerCatalog(value: unknown): NeevAgentProviderCatalog {
  // `speedUnavailableReason` is parsed so the payload is accepted, and then
  // deliberately NOT rendered: it is English prose from the server, and this
  // app ships ten locales. The reason shown beside a disabled Fast chip is
  // derived from the catalogue itself and translated like every other label.
  const object = exact(value, [
    "provider",
    "label",
    "models",
    "efforts",
    "defaultEffort",
    "supportsSpeed",
    "speedUnavailableReason",
  ])
  const provider = text(object.provider)
  const label = text(object.label)
  if (
    !NEEV_AGENT_PROVIDERS.includes(provider as NeevAgentProvider) ||
    label.length === 0 ||
    [...label].length > 80 ||
    typeof object.supportsSpeed !== "boolean" ||
    !Array.isArray(object.models) ||
    // A provider with no models could not start a chat; an empty menu with a
    // heading over it would be worse than saying nothing.
    object.models.length === 0 ||
    object.models.length > MAX_MODELS
  )
    return invalid()
  const models = object.models.map(agentModel)
  if (new Set(models.map((model) => model.id)).size !== models.length)
    return invalid()
  const efforts = effortList(object.efforts)
  if (object.defaultEffort !== null && !isSupportedEffort(object.defaultEffort))
    return invalid()
  // A default outside the advertised set is not a level anyone could pick.
  if (
    object.defaultEffort !== null &&
    !efforts.includes(object.defaultEffort as string)
  )
    return invalid()
  // A model narrowing to a level its provider never advertised is a catalogue
  // that disagrees with itself; rendering it would offer a level the server
  // would then refuse.
  if (
    models.some((model) =>
      model.efforts?.some((level) => !efforts.includes(level))
    )
  )
    return invalid()
  return {
    provider: provider as NeevAgentProvider,
    label,
    models,
    efforts,
    defaultEffort: (object.defaultEffort as string | null) ?? null,
    supportsSpeed: object.supportsSpeed,
  }
}

/**
 * The menu, as the server says it is.
 *
 * Nothing in the controls is hardcoded: a model or a level absent from this
 * response is one the runner has not been shown to accept, and offering it
 * would be offering a failed run.
 */
export function parseAgentCapabilities(value: unknown): NeevAgentCapabilities {
  const object = exact(value, ["providers"])
  if (
    !Array.isArray(object.providers) ||
    object.providers.length > NEEV_AGENT_PROVIDERS.length
  )
    return invalid()
  const providers = object.providers.map(providerCatalog)
  if (
    new Set(providers.map((entry) => entry.provider)).size !== providers.length
  )
    return invalid()
  return { providers }
}

export async function loadAgentCapabilities(
  signal?: AbortSignal
): Promise<NeevAgentCapabilities> {
  return parseAgentCapabilities(
    await contentGet("/api/neev/agent/capabilities", signal)
  )
}

const ACTIVITY_STATES: readonly NeevActivityState[] = [
  "model",
  "stop",
  "result",
  "truncated",
  "unsupported",
]

function activityItem(value: unknown): NeevActivityItem | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null
  const item = value as Record<string, unknown>
  const seq = item.seq
  const offset = item.offset
  if (
    typeof seq !== "number" ||
    !Number.isSafeInteger(seq) ||
    seq < 1 ||
    typeof offset !== "number" ||
    !Number.isSafeInteger(offset) ||
    offset < 0 ||
    typeof item.id !== "string"
  )
    return null
  const base = { seq, offset, id: item.id }
  if (
    item.kind === "tool_call" &&
    typeof item.name === "string" &&
    typeof item.input === "string"
  )
    return {
      ...base,
      kind: "tool_call",
      name: item.name,
      input: item.input,
      inputTruncated: item.inputTruncated === true,
    }
  if (
    item.kind === "tool_result" &&
    typeof item.callId === "string" &&
    typeof item.output === "string"
  )
    return {
      ...base,
      kind: "tool_result",
      callId: item.callId,
      output: item.output,
      outputTruncated: item.outputTruncated === true,
      isError: item.isError === true,
    }
  if (
    item.kind === "status" &&
    typeof item.state === "string" &&
    ACTIVITY_STATES.includes(item.state as NeevActivityState)
  )
    return {
      ...base,
      kind: "status",
      state: item.state as NeevActivityState,
      detail: typeof item.detail === "string" ? item.detail : null,
    }
  return null
}

/**
 * Parse a turn's trace.
 *
 * An unreadable ITEM is dropped, not fatal: a turn's reply must still display
 * when its decoration does not, and a future server may send a kind this build
 * has never heard of. Sorted by `seq` so the caller can rely on the order.
 */
export function parseActivity(value: unknown): NeevActivityItem[] {
  if (!Array.isArray(value)) return []
  const items: NeevActivityItem[] = []
  const seen = new Set<number>()
  for (const entry of value) {
    const item = activityItem(entry)
    if (!item || seen.has(item.seq)) continue
    seen.add(item.seq)
    items.push(item)
  }
  return items.sort((left, right) => left.seq - right.seq)
}

/** One `activity` frame off the run stream, or `null` if it is not one. */
export function parseActivityEvent(value: unknown): NeevActivityItem | null {
  if (!value || typeof value !== "object") return null
  if ((value as Record<string, unknown>).type !== "activity") return null
  return activityItem(value)
}

export function parseAgentTurn(value: unknown): NeevAgentTurn {
  const object = exact(value, [
    "id",
    "sessionId",
    "sequence",
    "runId",
    "prompt",
    "response",
    "activity",
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
    // The TURN is a real UUID; the session it belongs to may be a digest.
    id: uuid(object.id),
    sessionId: sessionId(object.sessionId),
    sequence: integerString(object.sequence),
    runId: runId(object.runId),
    prompt: text(object.prompt),
    response: text(object.response),
    activity: parseActivity(object.activity),
    status: status as NeevTurnStatus,
    errorCode: nullableCode(object.errorCode),
    provider: text(object.provider),
    model: text(object.model),
    createdAt: timestamp(object.createdAt),
    updatedAt: timestamp(object.updatedAt),
    completedAt,
  }
}

export function parseConnection(value: unknown): NeevAgentConnection {
  const entry = exact(value, [
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
    accountLabel: entry.accountLabel === null ? null : text(entry.accountLabel),
    checkedAt: nullableTimestamp(entry.checkedAt),
    errorCode: nullableCode(entry.errorCode),
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
  const connections = object.connections.map(parseConnection)
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

/**
 * A session id on its way into a URL.
 *
 * Same rule as {@link SESSION_ID}: canonical shape, lowercase hex, opaque. It
 * is still a strict allowlist — nothing that could carry a path segment, a
 * query or an escape can pass — it simply does not insist on UUID version and
 * variant bits that a derived team id never had.
 */
function assertSessionId(value: string): void {
  if (!SESSION_ID.test(value)) invalidRequest()
}

function assertRunId(value: string): void {
  if (!RUN_ID.test(value)) invalidRequest()
}

/**
 * Every session the caller owns. There is deliberately no server-side project
 * filter: the unfiled organizer needs the whole list anyway, so a project view
 * filters what it already has rather than asking twice. `ListSessionsInput`
 * accepts `archived` only, and it denies unknown fields.
 */
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

/**
 * File a session under a project, or unfile it with null.
 *
 * `expectedRevision` is the decimal string every other revision on this DTO is,
 * and it is what makes this safe against a concurrent edit: the server refuses
 * the move if the session has changed underneath.
 */
export async function setAgentSessionNode(
  sessionId: string,
  nodeId: number | null,
  expectedRevision: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAgentSession> {
  assertSessionId(sessionId)
  if (nodeId !== null && (!Number.isSafeInteger(nodeId) || nodeId <= 0))
    invalidRequest()
  if (!/^(0|[1-9]\d{0,18})$/.test(expectedRevision)) invalidRequest()
  return parseAgentSession(
    await contentMutation(
      `/api/neev/agent/sessions/${sessionId}/node`,
      { nodeId, expectedRevision },
      csrf,
      signal,
      "PATCH"
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
    !isSupportedModel(input.model)
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
  // so only send these when there is something to send.
  if (input.boundChatId) body.boundChatId = input.boundChatId
  if (input.effort !== undefined && input.effort !== null) {
    if (!isSupportedEffort(input.effort)) invalidRequest()
    body.effort = input.effort
  }
  // Sent only when it is ON. A server that predates the field must refuse a
  // request that asked for fast mode rather than quietly run without it, and
  // must not be refused for a `false` that changes nothing.
  if (input.fast === true) body.fast = true
  if (input.nodeId !== undefined && input.nodeId !== null) {
    if (!Number.isSafeInteger(input.nodeId) || input.nodeId <= 0)
      invalidRequest()
    body.nodeId = input.nodeId
  }
  return parseAgentSession(
    await contentMutation("/api/neev/agent/sessions", body, csrf, signal)
  )
}

export async function loadAgentSession(
  sessionId: string,
  signal?: AbortSignal
): Promise<NeevAgentSession> {
  assertSessionId(sessionId)
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
  assertSessionId(sessionId)
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

/**
 * Change what this session runs as, in one revision.
 *
 * The full desired state goes up every time — `model`, `effort` and `fast` are
 * all required — because an omission would have to be guessed at, and because
 * the server checks the triple as a combination. A run reads the SESSION, so a
 * change made here applies to every later turn and survives a reload without the
 * composer resending anything.
 *
 * `provider` is deliberately not a parameter: it is immutable. A turn may not
 * contradict its session's provider and a team holder's id is derived from it,
 * so changing it would rewrite history that already ran. A different provider
 * is a different chat.
 */
export async function updateAgentSessionSettings(
  sessionId: string,
  settings: NeevAgentSettings,
  expectedRevision: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAgentSession> {
  assertSessionId(sessionId)
  try {
    integerString(expectedRevision)
  } catch {
    invalidRequest()
  }
  if (
    typeof settings.model !== "string" ||
    !isSupportedModel(settings.model) ||
    typeof settings.fast !== "boolean" ||
    (settings.effort !== null && !isSupportedEffort(settings.effort))
  )
    invalidRequest()
  return parseAgentSession(
    await contentMutation(
      `/api/neev/agent/sessions/${sessionId}/settings`,
      {
        model: settings.model,
        effort: settings.effort,
        fast: settings.fast,
        expectedRevision,
      },
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
  assertSessionId(sessionId)
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
  assertSessionId(sessionId)
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
export interface NeevTeamInvocation {
  /** The invoker's prompt, already persisted as an ordinary human message. */
  message: NativeMessage
  turn: NeevAgentTurn
  runId: string
}

export interface InvokeTeamAgentInput {
  prompt: string
  provider: NeevAgentProvider
  model: string
  /**
   * The level and speed this ask should run at.
   *
   * They travel WITH the invocation, not after it, because the holder for
   * (chat, provider) is created by the first ask: a control that could only be
   * used once a holder existed would leave the first — and paid — turn the one
   * turn nobody could configure. Omitted when they are the defaults, so a
   * server that predates them is not refused over a value that changes nothing.
   */
  effort?: NeevEffort | null
  fast?: boolean
  requestId: string
  expectedAuthRevision: string
}

export function parseTeamInvocation(value: unknown): NeevTeamInvocation {
  const object = exact(value, ["message", "turn", "runId"])
  const turn = parseAgentTurn(object.turn)
  const id = runId(object.runId)
  if (id !== turn.runId) return invalid()
  return { message: parseMessage(object.message), turn, runId: id }
}

/**
 * Explicit team invocation (spec §5.3).
 *
 * The prompt is posted as an ordinary human message first — idempotent on
 * `requestId` and guarded by `expectedAuthRevision` exactly like posting — so
 * the team can see what was asked even before a reply lands. The reply is
 * written by the run driver as an agent-authored message; the caller re-reads
 * the thread once the run settles rather than rendering it from the stream.
 *
 * The run is sent only this prompt: no private Agent Chat history travels with
 * it, and it executes under the invoker's own per-user credentials.
 */
export async function invokeTeamAgent(
  chatId: string,
  input: InvokeTeamAgentInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevTeamInvocation> {
  assertUuid(chatId)
  assertUuid(input.requestId)
  if (
    !NEEV_AGENT_PROVIDERS.includes(input.provider) ||
    !isSupportedModel(input.model) ||
    typeof input.prompt !== "string" ||
    input.prompt.length === 0 ||
    [...input.prompt].length > NEEV_MAX_PROMPT_CHARS
  )
    invalidRequest()
  try {
    integerString(input.expectedAuthRevision)
  } catch {
    invalidRequest()
  }
  const body: Record<string, unknown> = {
    prompt: input.prompt,
    provider: input.provider,
    model: input.model,
    requestId: input.requestId,
    expectedAuthRevision: input.expectedAuthRevision,
  }
  // Same omission rule as creating a session: a level only when one was picked,
  // and fast mode only when it was actually asked for.
  if (input.effort !== undefined && input.effort !== null) {
    if (!isSupportedEffort(input.effort)) invalidRequest()
    body.effort = input.effort
  }
  if (input.fast === true) body.fast = true
  const invocation = parseTeamInvocation(
    await contentMutation(`/api/neev/chats/${chatId}/agent`, body, csrf, signal)
  )
  // The reply is posted by the server under the invoker's account; a prompt
  // echoed back as agent-authored would mean the wrong row came back.
  if (invocation.message.authorKind !== "human") invalid()
  return invocation
}

/**
 * The CALLER'S OWN holder for one chat and provider, or `null` if they have
 * never invoked here.
 *
 * This is what makes a reopened conversation show the trace it showed before:
 * the session's turns carry the persisted activity, so the view replays what
 * actually happened rather than starting blank. `404` means no invocation, not
 * a failure — an empty history is a normal history.
 *
 * It is private by construction. The server scopes the lookup to the current
 * actor and organization, and this checks the two things a wrong row would get
 * wrong anyway: the holder must be bound to the chat that was asked about, and
 * it must be for the provider that was asked about. The caller additionally
 * refuses a holder owned by anyone else, so no one else's private trace can be
 * attached to a message in a shared thread.
 */
export async function loadTeamAgentSession(
  chatId: string,
  provider: NeevAgentProvider,
  signal?: AbortSignal
): Promise<NeevAgentSession | null> {
  assertUuid(chatId)
  if (!NEEV_AGENT_PROVIDERS.includes(provider)) invalidRequest()
  let session: NeevAgentSession
  try {
    session = parseAgentSession(
      await contentGet(
        `/api/neev/chats/${chatId}/agent?provider=${provider}`,
        signal
      )
    )
  } catch (error) {
    if (error instanceof NativeContentNotFoundError) return null
    throw error
  }
  if (session.boundChatId !== chatId || session.provider !== provider)
    return invalid()
  return session
}

/**
 * Per-user provider sign-in (spec §5.4).
 *
 * `connect` records the intent and hands back an attempt to poll; completion is
 * performed server-side by the supervisor, so nothing here may report
 * "connected" on its own — the attempt's own state, and then the connection
 * itself, are the only sources of truth.
 */
export type NeevAttemptState =
  | "pending"
  | "completed"
  | "canceled"
  | "expired"
  | "failed"

const ATTEMPT_STATES: readonly NeevAttemptState[] = [
  "pending",
  "completed",
  "canceled",
  "expired",
  "failed",
]

export type NeevConnectOutcome =
  | { outcome: "pending"; attemptId: string; expiresAt: number }
  | { outcome: "settled"; connection: NeevAgentConnection }

/**
 * What the user must do to approve a sign-in, once the supervisor has produced
 * it. Display data ONLY — never a client secret, token, PKCE verifier or
 * cookie; those stay supervisor-side, keyed by attempt.
 *
 * `device` (Codex): show the code, the user approves in their own browser and
 * the supervisor polls — Neev needs nothing further.
 * `authorization_code` (Claude): the user approves in their own browser and
 * pastes the code they are given back into Neev.
 *
 * These are the INTERNAL names. The server sends `authorizationCode`; see
 * `approvalKind` for the mapping.
 */
export type NeevApproval =
  | { kind: "device"; verificationUrl: string; userCode: string }
  | {
      kind: "authorization_code"
      verificationUrl: string
      needsCode: boolean
    }

export interface NeevConnectAttempt {
  state: NeevAttemptState
  expiresAt: number
  approval: NeevApproval | null
}

/** A code the user pasted back: bounded, and never stored by the server. */
export const NEEV_MAX_AUTH_CODE_CHARS = 512

/**
 * Where each provider's approval page really lives.
 *
 * Claude's is `claude.com` — confirmed by an offline probe of the pinned
 * 2.1.267 binary, NOT the legacy `claude.ai`. Codex's is the device endpoint
 * already pinned server-side in `control_dto.rs`.
 *
 * This is an allowlist, not a format check, because the verification URL is a
 * link we actively tell the user to open and approve on. Accepting any https
 * host would mean a buggy or compromised supervisor could aim that instruction
 * at a phishing page. Keep in sync with the server-side allowlist.
 */
const APPROVAL_ORIGINS: Record<NeevAgentProvider, readonly string[]> = {
  claude: ["claude.com"],
  codex: ["auth.openai.com"],
}

/**
 * The verification URL must be an absolute `https:` URL on the provider's own
 * approval host. A relative, `http:` or `javascript:` value — or a valid https
 * URL somewhere else entirely — is refused rather than rendered.
 */
function verificationUrl(value: unknown, provider: NeevAgentProvider): string {
  const raw = text(value)
  let parsed: URL
  try {
    parsed = new URL(raw)
  } catch {
    return invalid()
  }
  if (
    parsed.protocol !== "https:" ||
    !APPROVAL_ORIGINS[provider].includes(parsed.hostname) ||
    parsed.username !== "" ||
    parsed.password !== ""
  )
    return invalid()
  return raw
}

/**
 * The wire's `kind`, mapped to ours.
 *
 * The server sends `authorizationCode` — a literal in `approval_dto`, matching
 * the camelCase of every other field in the payload. This client originally
 * read `authorization_code`, so the whole approval failed to parse and the
 * panel sat on "waiting for sign-in" with no link to open: the one screen where
 * the user cannot proceed without it.
 *
 * The snake_case spelling is still accepted. It costs one line, and rejecting a
 * payload over its spelling is precisely what caused this.
 */
function approvalKind(value: unknown): "device" | "authorization_code" | null {
  if (value === "device") return "device"
  if (value === "authorizationCode" || value === "authorization_code")
    return "authorization_code"
  return null
}

export function parseApproval(
  value: unknown,
  provider: NeevAgentProvider
): NeevApproval {
  const object = exact(value, [
    "kind",
    "verificationUrl",
    "userCode",
    "needsCode",
  ])
  const kind = approvalKind(object.kind)
  const url = verificationUrl(object.verificationUrl, provider)
  if (typeof object.needsCode !== "boolean") return invalid()
  if (kind === "device") {
    // A device flow is only actionable with a code to type; and it never asks
    // for one back, so a `needsCode` device payload is self-contradictory.
    const userCode = text(object.userCode)
    if (userCode.length === 0 || userCode.length > 64 || object.needsCode)
      return invalid()
    return { kind: "device", verificationUrl: url, userCode }
  }
  if (kind === "authorization_code") {
    if (object.userCode !== null) return invalid()
    return {
      kind: "authorization_code",
      verificationUrl: url,
      needsCode: object.needsCode,
    }
  }
  return invalid()
}

export interface NeevOperationReceipt {
  operationId: string
  generation: string
  /**
   * The connection AFTER the request, which is the point of reading it: a 200
   * means the teardown was accepted, not that the credential is already gone.
   * If the supervisor could not be reached the row is still `disconnecting`.
   */
  connection: NeevAgentConnection
}

/**
 * The server's idempotency-key rule. Replaying a key returns the SAME attempt
 * rather than starting a second sign-in, so a retry after a lost response can
 * never leave two logins in flight.
 */
export function isValidRequestKey(value: string): boolean {
  return (
    value.length >= 1 &&
    value.length <= 64 &&
    /^[A-Za-z0-9_-]+$/.test(value) &&
    !value.startsWith("__neev_")
  )
}

/** A request key that satisfies the server's rule and is safe to retry with. */
export function newRequestKey(): string {
  return crypto.randomUUID()
}

function assertProvider(value: string): asserts value is NeevAgentProvider {
  if (!NEEV_AGENT_PROVIDERS.includes(value as NeevAgentProvider))
    invalidRequest()
}

function assertRequestKey(value: string): void {
  if (!isValidRequestKey(value)) invalidRequest()
}

/** Server-side ids are 32 lowercase hex, the same shape as a run id. */
function hexId(value: unknown): string {
  const parsed = text(value)
  if (!RUN_ID.test(parsed)) return invalid()
  return parsed
}

function nonNegativeInteger(value: unknown): string {
  const parsed = text(value)
  if (!/^(0|[1-9]\d{0,18})$/.test(parsed)) return invalid()
  return parsed
}

export function parseConnectOutcome(value: unknown): NeevConnectOutcome {
  const object = exact(value, [
    "outcome",
    "attemptId",
    "expiresAt",
    "connection",
  ])
  if (object.outcome === "pending") {
    // A pending attempt must identify itself and say when it lapses, or the UI
    // could show an indefinite "waiting" it can never resolve or cancel.
    if (object.connection !== null) return invalid()
    return {
      outcome: "pending",
      attemptId: hexId(object.attemptId),
      expiresAt: timestamp(object.expiresAt),
    }
  }
  if (object.outcome === "settled") {
    if (object.attemptId !== null || object.expiresAt !== null) return invalid()
    return {
      outcome: "settled",
      connection: parseConnection(object.connection),
    }
  }
  return invalid()
}

export function parseConnectAttempt(
  value: unknown,
  provider: NeevAgentProvider
): NeevConnectAttempt {
  // `approval` is designed and lands with the supervisor binding; a server that
  // does not send it yet simply has no approval to show. Once present it is
  // validated strictly.
  const labelled = record(value).approval !== undefined
  const object = exact(
    value,
    labelled ? ["state", "expiresAt", "approval"] : ["state", "expiresAt"]
  )
  const state = text(object.state)
  if (!ATTEMPT_STATES.includes(state as NeevAttemptState)) return invalid()
  const approval =
    !labelled || object.approval === null
      ? null
      : parseApproval(object.approval, provider)
  // Only a live attempt can be approved; approval data on a settled one would
  // invite the user to complete something that can no longer succeed.
  if (approval !== null && state !== "pending") return invalid()
  return {
    state: state as NeevAttemptState,
    expiresAt: timestamp(object.expiresAt),
    approval,
  }
}

/**
 * Forward the authorization code the user pasted. It is handed straight to the
 * supervisor and never stored by the server, so this client keeps no copy
 * either — the caller clears its field on success.
 */
export async function submitConnectCode(
  provider: string,
  attemptId: string,
  code: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevConnectAttempt> {
  assertProvider(provider)
  if (!RUN_ID.test(attemptId)) invalidRequest()
  if (
    typeof code !== "string" ||
    code.length === 0 ||
    code.length > NEEV_MAX_AUTH_CODE_CHARS
  )
    invalidRequest()
  // The server answers 202 with the whole attempt, not a bare state — the same
  // shape the poll returns, so it is read with the same parser rather than a
  // second opinion about the contract. Expecting `{state}` alone made a
  // perfectly good submission look like an unexpected response, and the user
  // was told their code had failed after it had in fact been accepted.
  return parseConnectAttempt(
    await contentMutation(
      `/api/neev/agent/connections/${provider}/attempts/${attemptId}/code`,
      { code },
      csrf,
      signal
    ),
    provider
  )
}

/**
 * `ReceiptDto` from both `cancel_connect` and `disconnect_provider` — the same
 * shape from both, checked against each.
 *
 * It carries the connection as it stands after the request, and this parser
 * used to demand only the two ids: a disconnect the server had performed came
 * back looking like an unexpected response. `generation` is a decimal string,
 * not a number — the server stringifies an i64 that would not survive JSON.
 */
export function parseOperationReceipt(value: unknown): NeevOperationReceipt {
  const object = exact(value, ["operationId", "generation", "connection"])
  return {
    operationId: hexId(object.operationId),
    generation: nonNegativeInteger(object.generation),
    connection: parseConnection(object.connection),
  }
}

/** Begin (or replay) a sign-in for one provider. */
export async function startProviderConnect(
  provider: string,
  requestKey: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevConnectOutcome> {
  assertProvider(provider)
  assertRequestKey(requestKey)
  return parseConnectOutcome(
    await contentMutation(
      `/api/neev/agent/connections/${provider}/connect`,
      { requestKey },
      csrf,
      signal
    )
  )
}

/** Poll a sign-in. Side-effect free, and visible only to its own session. */
export async function loadConnectAttempt(
  provider: string,
  attemptId: string,
  signal?: AbortSignal
): Promise<NeevConnectAttempt> {
  assertProvider(provider)
  if (!RUN_ID.test(attemptId)) invalidRequest()
  return parseConnectAttempt(
    await contentGet(
      `/api/neev/agent/connections/${provider}/attempts/${attemptId}`,
      signal
    ),
    provider
  )
}

/** Abandon an in-flight sign-in; the server queues the same cleanup a crash would. */
export async function cancelProviderConnect(
  provider: string,
  attemptId: string,
  requestKey: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevOperationReceipt> {
  assertProvider(provider)
  assertRequestKey(requestKey)
  if (!RUN_ID.test(attemptId)) invalidRequest()
  return parseOperationReceipt(
    await contentMutation(
      `/api/neev/agent/connections/${provider}/cancel`,
      { attemptId, requestKey },
      csrf,
      signal
    )
  )
}

/** Sign this user out of this provider; credential teardown is a queued cleanup. */
export async function disconnectProvider(
  provider: string,
  requestKey: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevOperationReceipt> {
  assertProvider(provider)
  assertRequestKey(requestKey)
  return parseOperationReceipt(
    await contentMutation(
      `/api/neev/agent/connections/${provider}/disconnect`,
      { requestKey },
      csrf,
      signal
    )
  )
}

/**
 * Run a bounded, side-effect-free check of an existing connection and return
 * what the server decided.
 *
 * The result is the server's own connection row after the check settled — the
 * caller replaces its view with it rather than inferring anything. There is no
 * optimistic "connected": if the server cannot settle the check it says so, and
 * `409` (busy or stale), `503` (unavailable) and `429` (rate limited) all
 * surface as themselves.
 */
export async function testProviderConnection(
  provider: string,
  requestKey: string,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAgentConnection> {
  assertProvider(provider)
  assertRequestKey(requestKey)
  const object = exact(
    await contentMutation(
      `/api/neev/agent/connections/${provider}/test`,
      { requestKey },
      csrf,
      signal
    ),
    ["connection"]
  )
  const connection = parseConnection(object.connection)
  // A check settles the provider it was asked about; anything else means the
  // wrong row came back and the panel would mislabel it.
  if (connection.provider !== provider) return invalid()
  return connection
}

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
  onActivity,
  onPhase,
  retryDelays = RUN_RETRY_DELAYS,
  healthyAfterMs = RUN_HEALTHY_AFTER_MS,
  open = openAgentRunStream,
}: {
  runId: string
  signal: AbortSignal
  onEvent: (event: NeevRunEvent) => void
  /**
   * The turn's tool trace, as it happens.
   *
   * Separate from `onEvent` because it is a separate track: text and activity
   * interleave by `offset`, and a reattach replays the stored prefix of BOTH,
   * so the receiver upserts on `seq` rather than appending.
   */
  onActivity?: (item: NeevActivityItem) => void
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
        signal,
        (item) => {
          if (done()) return
          delivered = true
          onActivity?.(item)
        }
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
  signal?: AbortSignal,
  onActivity?: (item: NeevActivityItem) => void
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
          const raw = line.slice(5).trim()
          const event = parseRunEvent(raw)
          if (event) onEvent(event)
          else if (onActivity) {
            // `parseRunEvent` returns null for anything it does not know,
            // which includes the activity frame and any future one. Only a
            // readable activity item is forwarded; the rest stays ignored.
            let value: unknown
            try {
              value = JSON.parse(raw)
            } catch {
              value = null
            }
            const item = value === null ? null : parseActivityEvent(value)
            if (item) onActivity(item)
          }
        }
      boundary = buffer.indexOf("\n\n")
    }
    if (done) break
  }
}
