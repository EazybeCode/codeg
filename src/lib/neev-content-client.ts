import { NativeClientError, nativeMutation, nativeRequest } from "./neev-client"
import { parseRichDocument, type RichDocument } from "./neev-rich-document"
import {
  NEEV_MAX_ATTACHMENTS,
  parseAttachments,
  type NeevAttachment,
} from "./neev-attachments"
import {
  MAX_MENTIONS_PER_MESSAGE,
  countCodePoints,
  type MentionSpan,
} from "./neev-mentions"

export type NativeResourceKind = "chat" | "document"
export type NativeVisibility = "private" | "organization"

/**
 * Which editor owns a document's content.
 *
 * `native` is Neev's own editor and is what every existing document is; those
 * are not migrated and keep it forever. `lasuite` is the embedded La Suite Docs
 * engine, which owns its own content, title and saving.
 */
export type NativeEngine = "native" | "lasuite"

export interface NativeResource {
  id: string
  orgId: number
  nodeId: number
  kind: NativeResourceKind
  creatorId: number
  title: string
  visibility: NativeVisibility
  originChatId: string | null
  authRevision: string
  contentRevision: string
  archived: boolean
  createdAt: number
  updatedAt: number
  /**
   * Absent from a server that predates the engine split, and from every chat.
   * Absent means `native`, so an older server keeps working unchanged.
   */
  engine?: NativeEngine
}

/**
 * Who wrote a team message (spec §5.3). An `agent` message was produced by a
 * provider answering an explicit invocation; `authorId`/`authorName` still
 * identify the human who invoked it, so the agent never impersonates anyone.
 */
export type NativeAuthorKind = "human" | "agent"

export interface NativeMessage {
  id: string
  sequence: string
  authorId: number
  authorName: string
  body: string
  createdAt: number
  authorKind: NativeAuthorKind
  /** The provider that produced an agent message; `null` for a human one. */
  agentLabel: string | null
  /**
   * The files this message was sent with, as the server stored them.
   *
   * Persisted metadata, not the staged list the composer held: it is what makes
   * a reopened conversation still show what was attached, and the only thing a
   * reader other than the sender could ever see. Empty for a message with none,
   * and for a server that predates attachments.
   */
  attachments: NeevAttachment[]
  /**
   * The coworkers this message names, as stored spans over `body`.
   *
   * Absent — not `[]` — for the overwhelming majority of messages: everything
   * written before mentions existed, everything an agent wrote, and everything
   * that simply names nobody. Absent and empty mean the same thing to a reader,
   * and leaving it off is what lets every message this client already knew how
   * to describe keep its exact shape.
   *
   * Nothing here is derived from the text: a chip is drawn only where a stored
   * span says one belongs, which is why an agent writing the characters `@Ana`
   * renders those characters and notifies no one.
   */
  mentions?: MentionSpan[]
}

export interface NativeRevision {
  format: "markdown" | "blocks"
  formatVersion: 1
  revision: string
  authorId: number
  authorName: string
  byteLength: string
  sha256: string
  createdAt: number
  parentRevision: string | null
  restoredFrom: string | null
}

export interface NativeDocumentContent {
  revision: NativeRevision
  markdown: string
  document: RichDocument | null
}

export interface NativePage<T> {
  items: T[]
  nextAfter: string | null
}

export interface CreateNativeResourceInput {
  title: string
  requestId: string
  originChatId?: string | null
  /**
   * An explicit audience, overriding the folder's default for new items.
   * Omitted means the server applies that default (schema 21). Never sent with
   * `originChatId`: a chat's generated file always follows its chat.
   */
  visibility?: NativeVisibility
}

export interface UpdateNativeResourceInput {
  expectedAuthRevision: string
  title: string
  nodeId: number
  visibility: NativeVisibility
  archived: boolean
}

export interface AppendNativeMessageInput {
  requestId: string
  expectedAuthRevision: string
  body: string
  /**
   * Keys of attachments already uploaded to THIS chat, in the order they should
   * be shown. Omitted from the request when empty, so a server that has not
   * learned about attachments sees exactly the payload it always did.
   */
  attachmentKeys?: readonly string[]
  /**
   * Who this message names, and where in `body` it says so.
   *
   * Omitted from the request when empty, for the same reason the keys are. The
   * server proves every span against the body it stores and against the
   * sender's own organization before a single row is written; this side checks
   * what it can so an impossible send is refused here rather than by a round
   * trip.
   */
  mentions?: readonly MentionSpan[]
}

export type SaveNativeDocumentInput = {
  requestId: string
  expectedContentRevision: string
  expectedAuthRevision: string
} & (
  | { markdown: string; document?: never }
  | { document: RichDocument; markdown?: never }
)

export interface RestoreNativeDocumentInput {
  requestId: string
  expectedContentRevision: string
  expectedAuthRevision: string
  sourceRevision: string
}

export class NativeContentNotFoundError extends Error {
  readonly kind = "not_found" as const

  constructor() {
    super("Native content was not found")
    this.name = "NativeContentNotFoundError"
  }
}

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

/**
 * Every required key present, no key that is neither required nor optional.
 *
 * The strictness is deliberate and stays: an unexpected key means the response
 * is not the shape this client was written against, and guessing is how a
 * rename becomes silent data loss. Optional keys are the one concession, and
 * only for fields a server may genuinely not send yet — adding a field to a
 * response must not break every client that has not learned about it, which
 * without this would be exactly what happens.
 */
function exact(
  value: unknown,
  keys: readonly string[],
  optional: readonly string[] = []
): Record<string, unknown> {
  const object = record(value)
  const actual = Object.keys(object).sort()
  const required = [...keys].sort()
  if (
    actual.length < required.length ||
    actual.length > required.length + optional.length
  )
    return invalid()
  const seen = new Set(actual)
  if (required.some((key) => !seen.has(key))) return invalid()
  const allowed = new Set([...required, ...optional])
  if (actual.some((key) => !allowed.has(key))) return invalid()
  return object
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

function text(value: unknown): string {
  if (typeof value !== "string") return invalid()
  return value
}

const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
function uuid(value: unknown): string {
  const parsed = text(value)
  if (!UUID.test(parsed)) return invalid()
  return parsed
}

function integerString(value: unknown, allowZero: boolean): string {
  const parsed = text(value)
  const pattern = allowZero ? /^(0|[1-9]\d{0,18})$/ : /^[1-9]\d{0,18}$/
  if (
    !pattern.test(parsed) ||
    (parsed.length === 19 && parsed > "9223372036854775807")
  )
    return invalid()
  return parsed
}

function nullableRevision(value: unknown): string | null {
  return value === null ? null : integerString(value, false)
}

function resourceCursor(value: unknown): string {
  const parsed = text(value)
  if (parsed.length > 512 || !/^[A-Za-z0-9_-]+$/.test(parsed)) return invalid()
  return parsed
}

function page<T>(
  value: unknown,
  parser: (item: unknown) => T,
  cursor: (value: unknown) => string
): NativePage<T> {
  const object = exact(value, ["items", "nextAfter"])
  if (!Array.isArray(object.items) || object.items.length > 100)
    return invalid()
  return {
    items: object.items.map(parser),
    nextAfter: object.nextAfter === null ? null : cursor(object.nextAfter),
  }
}

export function parseResource(value: unknown): NativeResource {
  const object = exact(
    value,
    [
      "id",
      "orgId",
      "nodeId",
      "kind",
      "creatorId",
      "title",
      "visibility",
      "originChatId",
      "authRevision",
      "contentRevision",
      "archived",
      "createdAt",
      "updatedAt",
    ],
    ["engine"]
  )
  if (object.kind !== "chat" && object.kind !== "document") return invalid()
  /**
   * Rejected rather than coerced when it is a value this client does not know.
   * Falling back to `native` for an unrecognised engine would render Neev's
   * editor against content it cannot read — a blank document where one exists,
   * which reads as data loss and invites saving over it. Failing loudly is the
   * safe direction, and matches how `kind` and `visibility` are handled.
   */
  const engine =
    object.engine === undefined
      ? undefined
      : object.engine === "native" || object.engine === "lasuite"
        ? object.engine
        : invalid()
  if (object.visibility !== "private" && object.visibility !== "organization")
    return invalid()
  if (typeof object.archived !== "boolean") return invalid()
  const originChatId =
    object.originChatId === null ? null : uuid(object.originChatId)
  if (object.kind === "chat" && originChatId !== null) return invalid()
  const title = text(object.title)
  if (
    title.length === 0 ||
    [...title].length > 100 ||
    [...title].some((character) => /\p{Cc}/u.test(character))
  )
    return invalid()
  return {
    id: uuid(object.id),
    orgId: positiveId(object.orgId),
    nodeId: positiveId(object.nodeId),
    kind: object.kind,
    creatorId: positiveId(object.creatorId),
    title,
    visibility: object.visibility,
    originChatId,
    authRevision: integerString(object.authRevision, false),
    contentRevision: integerString(object.contentRevision, true),
    archived: object.archived,
    createdAt: timestamp(object.createdAt),
    updatedAt: timestamp(object.updatedAt),
    ...(engine === undefined ? {} : { engine }),
  }
}

const MESSAGE_KEYS = [
  "id",
  "sequence",
  "authorId",
  "authorName",
  "body",
  "createdAt",
] as const

export function parseMessage(value: unknown): NativeMessage {
  // Authorship arrived with §5.3. During the rollout a server may still send
  // the older six-key shape; that is unambiguously a human message. Once the
  // fields are present they are validated strictly — a half-present pair, or a
  // label that does not match the kind, means the row cannot be labelled
  // truthfully and must not be rendered.
  const labelled =
    record(value).authorKind !== undefined ||
    record(value).agentLabel !== undefined
  const object = exact(
    value,
    labelled ? [...MESSAGE_KEYS, "authorKind", "agentLabel"] : MESSAGE_KEYS,
    // Attachments arrived after authorship did, and a message without files
    // carries no such field at all — so it is optional in both directions.
    // Mentions arrived after those, and are optional for the same reason: every
    // message stored before they existed names nobody.
    ["attachments", "mentions"]
  )
  const body = text(object.body)
  const attachments = parseAttachments(object.attachments)
  const mentions = parseMentions(object.mentions, body)
  // Files with no words is a real message; words with nothing in them is not.
  if (
    (body.length === 0 && attachments.length === 0) ||
    new TextEncoder().encode(body).length > 32768
  )
    return invalid()
  let authorKind: NativeAuthorKind = "human"
  let agentLabel: string | null = null
  if (labelled) {
    if (object.authorKind !== "human" && object.authorKind !== "agent")
      return invalid()
    authorKind = object.authorKind
    agentLabel = object.agentLabel === null ? null : text(object.agentLabel)
    // The server writes the pair together and enforces it in the schema.
    if ((authorKind === "agent") !== (agentLabel !== null)) return invalid()
    if (agentLabel !== null && !/^[a-z][a-z0-9_-]{0,31}$/.test(agentLabel))
      return invalid()
  }
  // Nothing an agent writes can become a mention, so a reply that claims one
  // is not a message this client will render.
  if (authorKind === "agent" && mentions.length > 0) return invalid()
  return {
    id: uuid(object.id),
    sequence: integerString(object.sequence, false),
    authorId: positiveId(object.authorId),
    authorName: text(object.authorName),
    body,
    createdAt: timestamp(object.createdAt),
    authorKind,
    agentLabel,
    attachments,
    // An agent-authored message carries no mentions by construction: the server
    // path that writes one records no spans. Refused here too rather than
    // rendered, so a reply can never be made to look like a provider pinging a
    // teammate.
    ...(mentions.length === 0 ? {} : { mentions }),
  }
}

/**
 * The spans a stored message carries, checked against the body they describe.
 *
 * Strict, and strict in one direction only: the server proves each span against
 * the body inside the transaction that writes it, so a response that disagrees
 * with itself is not a message this client can render truthfully. Absent is the
 * normal case — every message written before mentions existed, and every one
 * that names nobody.
 */
function parseMentions(value: unknown, body: string): MentionSpan[] {
  if (value === undefined) return []
  if (!Array.isArray(value) || value.length > MAX_MENTIONS_PER_MESSAGE)
    return invalid()
  const total = countCodePoints(body)
  const spans: MentionSpan[] = []
  let reach = 0
  for (const item of value) {
    const object = exact(item, ["userId", "start", "length"])
    const start = object.start,
      length = object.length
    if (
      typeof start !== "number" ||
      !Number.isSafeInteger(start) ||
      typeof length !== "number" ||
      !Number.isSafeInteger(length) ||
      // `@` plus at least one character: a span shorter than that cannot be a
      // name, and a zero-length one would be a chip standing on nothing.
      length < 2 ||
      start < reach ||
      start + length > total
    )
      return invalid()
    spans.push({ userId: positiveId(object.userId), start, length })
    reach = start + length
  }
  return spans
}

/**
 * The spans a send may carry: no more than the server takes, each inside the
 * body, in order, and never two claiming the same characters.
 *
 * Checked here so a composer bug becomes a refused request rather than a
 * message whose chips point at the wrong words. It is not a substitute for the
 * server's own proof — only that one can say whether a user id is a coworker.
 */
export function mentionSpans(
  value: readonly MentionSpan[] | undefined,
  body: string
): MentionSpan[] {
  if (value === undefined) return []
  if (!Array.isArray(value) || value.length > MAX_MENTIONS_PER_MESSAGE)
    invalidRequest()
  const total = countCodePoints(body)
  let reach = 0
  for (const span of value) {
    if (
      !span ||
      !Number.isInteger(span.userId) ||
      span.userId <= 0 ||
      !Number.isSafeInteger(span.start) ||
      !Number.isSafeInteger(span.length) ||
      span.length < 2 ||
      span.start < reach ||
      span.start + span.length > total
    )
      invalidRequest()
    reach = span.start + span.length
  }
  return value.map((span) => ({
    userId: span.userId,
    start: span.start,
    length: span.length,
  }))
}

export function parseRevision(value: unknown): NativeRevision {
  const object = exact(value, [
    "format",
    "formatVersion",
    "revision",
    "authorId",
    "authorName",
    "byteLength",
    "sha256",
    "createdAt",
    "parentRevision",
    "restoredFrom",
  ])
  const sha256 = text(object.sha256)
  if (!/^[0-9a-f]{64}$/.test(sha256)) return invalid()
  const byteLength = integerString(object.byteLength, true)
  if (BigInt(byteLength) > 1048576n) return invalid()
  if (
    (object.format !== "markdown" && object.format !== "blocks") ||
    object.formatVersion !== 1
  )
    return invalid()
  return {
    format: object.format,
    formatVersion: 1,
    revision: integerString(object.revision, false),
    authorId: positiveId(object.authorId),
    authorName: text(object.authorName),
    byteLength,
    sha256,
    createdAt: timestamp(object.createdAt),
    parentRevision: nullableRevision(object.parentRevision),
    restoredFrom: nullableRevision(object.restoredFrom),
  }
}

export function parseResourcePage(value: unknown): NativePage<NativeResource> {
  const parsed = page(value, parseResource, resourceCursor)
  if (parsed.nextAfter !== null && parsed.items.length !== 100) return invalid()
  return parsed
}

export function parseMessagePage(value: unknown): NativePage<NativeMessage> {
  const parsed = page(value, parseMessage, (cursor) =>
    integerString(cursor, false)
  )
  if (
    parsed.nextAfter !== null &&
    (parsed.items.length !== 100 ||
      parsed.items[parsed.items.length - 1].sequence !== parsed.nextAfter)
  )
    return invalid()
  return parsed
}

export function parseRevisionPage(value: unknown): NativePage<NativeRevision> {
  const parsed = page(value, parseRevision, (cursor) =>
    integerString(cursor, false)
  )
  if (
    parsed.nextAfter !== null &&
    (parsed.items.length !== 100 ||
      parsed.items[parsed.items.length - 1].revision !== parsed.nextAfter)
  )
    return invalid()
  return parsed
}

export function parseDocumentContent(value: unknown): NativeDocumentContent {
  const object = exact(value, ["revision", "markdown", "document"])
  const revision = parseRevision(object.revision)
  const markdown = text(object.markdown)
  if (new TextEncoder().encode(markdown).length > 1048576) return invalid()
  const document =
    revision.format === "blocks" ? parseRichDocument(object.document) : null
  if (revision.format === "markdown" && object.document !== null)
    return invalid()
  const stored = document === null ? markdown : JSON.stringify(document)
  if (String(new TextEncoder().encode(stored).length) !== revision.byteLength)
    return invalid()
  return { revision, markdown, document }
}

export function contentGet(
  path: string,
  signal?: AbortSignal
): Promise<unknown> {
  return nativeRequest(path, {}, signal).catch(classifyContentError)
}

export function contentMutation(
  path: string,
  body: unknown,
  csrf: string,
  signal?: AbortSignal,
  method = "POST"
): Promise<unknown> {
  return nativeMutation(path, body, csrf, signal, method).catch(
    classifyContentError
  )
}

function classifyContentError(error: unknown): never {
  if (error instanceof NativeClientError && error.status === 404)
    throw new NativeContentNotFoundError()
  throw error
}

function assertUuid(value: string): void {
  if (!UUID.test(value)) invalidRequest()
}

function assertPositiveRevision(value: string): void {
  try {
    integerString(value, false)
  } catch {
    invalidRequest()
  }
}

function assertNonnegativeRevision(value: string): void {
  try {
    integerString(value, true)
  } catch {
    invalidRequest()
  }
}

function assertId(value: number): void {
  if (!Number.isInteger(value) || value <= 0 || value > 2_147_483_647)
    invalidRequest()
}

function assertScope(
  resource: NativeResource,
  orgId: number,
  nodeId?: number
): void {
  if (
    resource.orgId !== orgId ||
    (nodeId !== undefined && resource.nodeId !== nodeId)
  )
    invalid()
}

export async function listNativeResources(
  orgId: number,
  nodeId: number,
  kind: NativeResourceKind,
  archived = false,
  after: string | null = null,
  signal?: AbortSignal
): Promise<NativePage<NativeResource>> {
  assertId(orgId)
  assertId(nodeId)
  if (after !== null && typeof after !== "string") invalidRequest()
  const query = new URLSearchParams()
  if (archived) query.set("archived", "true")
  if (after !== null) query.set("after", after)
  const result = parseResourcePage(
    await contentGet(
      `/api/neev/nodes/${nodeId}/${kind}s${query.size ? `?${query}` : ""}`,
      signal
    )
  )
  if (result.items.some((resource) => resource.kind !== kind)) invalid()
  result.items.forEach((resource) => assertScope(resource, orgId, nodeId))
  return result
}

export async function loadNativeResource(
  orgId: number,
  resourceId: string,
  signal?: AbortSignal
): Promise<NativeResource> {
  assertId(orgId)
  assertUuid(resourceId)
  const resource = parseResource(
    await contentGet(`/api/neev/resources/${resourceId}`, signal)
  )
  assertScope(resource, orgId)
  if (resource.id !== resourceId) invalid()
  return resource
}

export async function createNativeResource(
  orgId: number,
  nodeId: number,
  kind: NativeResourceKind,
  input: CreateNativeResourceInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NativeResource> {
  assertId(orgId)
  assertId(nodeId)
  assertUuid(input.requestId)
  if (
    typeof input.title !== "string" ||
    (kind === "chat" &&
      Object.prototype.hasOwnProperty.call(input, "originChatId"))
  )
    invalidRequest()
  if (input.originChatId) assertUuid(input.originChatId)
  const explicit = input.visibility !== undefined
  if (
    explicit &&
    ((input.visibility !== "private" && input.visibility !== "organization") ||
      Object.prototype.hasOwnProperty.call(input, "originChatId"))
  )
    invalidRequest()
  const body: Record<string, unknown> =
    kind === "document" &&
    Object.prototype.hasOwnProperty.call(input, "originChatId")
      ? {
          title: input.title,
          requestId: input.requestId,
          originChatId: input.originChatId,
        }
      : { title: input.title, requestId: input.requestId }
  // Only when chosen: an omitted field is what lets the folder decide, and
  // keeps the request (and its idempotency hash) exactly what it was before.
  if (explicit) body.visibility = input.visibility
  const resource = parseResource(
    await contentMutation(
      `/api/neev/nodes/${nodeId}/${kind}s`,
      body,
      csrf,
      signal
    )
  )
  assertScope(resource, orgId, nodeId)
  if (resource.kind !== kind) invalid()
  return resource
}

export async function updateNativeResource(
  orgId: number,
  resourceId: string,
  input: UpdateNativeResourceInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NativeResource> {
  assertId(orgId)
  assertUuid(resourceId)
  assertId(input.nodeId)
  assertPositiveRevision(input.expectedAuthRevision)
  if (
    typeof input.title !== "string" ||
    (input.visibility !== "private" && input.visibility !== "organization") ||
    typeof input.archived !== "boolean"
  )
    invalidRequest()
  const resource = parseResource(
    await contentMutation(
      `/api/neev/resources/${resourceId}`,
      input,
      csrf,
      signal,
      "PATCH"
    )
  )
  assertScope(resource, orgId, input.nodeId)
  if (resource.id !== resourceId) invalid()
  return resource
}

export async function listNativeMessages(
  chatId: string,
  after: string | null = null,
  signal?: AbortSignal
): Promise<NativePage<NativeMessage>> {
  assertUuid(chatId)
  if (after !== null) assertPositiveRevision(after)
  return parseMessagePage(
    await contentGet(
      `/api/neev/chats/${chatId}/messages${after === null ? "" : `?after=${after}`}`,
      signal
    )
  )
}

export async function appendNativeMessage(
  chatId: string,
  input: AppendNativeMessageInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NativeMessage> {
  assertUuid(chatId)
  assertUuid(input.requestId)
  assertPositiveRevision(input.expectedAuthRevision)
  if (typeof input.body !== "string") invalidRequest()
  const keys = attachmentKeys(input.attachmentKeys)
  const named = mentionSpans(input.mentions, input.body)
  // An empty body is only a message when files carry it; the server accepts it
  // on exactly that condition, and refusing here keeps an empty send local.
  if (input.body.length === 0 && keys.length === 0) invalidRequest()
  return parseMessage(
    await contentMutation(
      `/api/neev/chats/${chatId}/messages`,
      {
        requestId: input.requestId,
        expectedAuthRevision: input.expectedAuthRevision,
        body: input.body,
        ...(keys.length ? { attachmentKeys: keys } : {}),
        ...(named.length ? { mentions: named } : {}),
      },
      csrf,
      signal
    )
  )
}

/**
 * The keys a send may carry: each a real key, no repeats, no more than the
 * server takes. Checked here so a bug in a composer becomes a refused request
 * rather than a message whose files quietly did not make it.
 */
export function attachmentKeys(value: readonly string[] | undefined): string[] {
  if (value === undefined) return []
  if (!Array.isArray(value) || value.length > NEEV_MAX_ATTACHMENTS)
    invalidRequest()
  for (const key of value)
    if (
      typeof key !== "string" ||
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
        key
      )
    )
      invalidRequest()
  if (new Set(value).size !== value.length) invalidRequest()
  return [...value]
}

/**
 * One conversation the reader was named in.
 *
 * A pointer, not a copy: enough to recognise what is waiting and to open it.
 * The excerpt is the server's own single bounded line of the message.
 */
export interface NeevMentionNotice {
  messageId: string
  chatId: string
  chatTitle: string
  authorName: string
  excerpt: string
  createdAt: number
  read: boolean
}

export interface NeevMentionInbox {
  items: NeevMentionNotice[]
  /** How many listed conversations still hold something unread. */
  unread: number
}

/** As many notices as the server will list at once. */
export const NEEV_MENTION_INBOX_LIMIT = 50
/** As many messages as one mark-read call may name. */
export const NEEV_MENTION_READ_LIMIT = 100

export function parseMentionInbox(value: unknown): NeevMentionInbox {
  const object = exact(value, ["items", "unread"])
  if (
    !Array.isArray(object.items) ||
    object.items.length > NEEV_MENTION_INBOX_LIMIT ||
    typeof object.unread !== "number" ||
    !Number.isSafeInteger(object.unread) ||
    object.unread < 0
  )
    return invalid()
  return {
    items: object.items.map((item) => {
      const notice = exact(item, [
        "messageId",
        "chatId",
        "chatTitle",
        "authorName",
        "excerpt",
        "createdAt",
        "read",
      ])
      if (typeof notice.read !== "boolean") return invalid()
      return {
        messageId: uuid(notice.messageId),
        chatId: uuid(notice.chatId),
        chatTitle: text(notice.chatTitle),
        authorName: text(notice.authorName),
        excerpt: text(notice.excerpt),
        createdAt: timestamp(notice.createdAt),
        read: notice.read,
      }
    }),
    unread: object.unread,
  }
}

/**
 * The reader's own mentions. The whole of the notification: nothing is mailed,
 * pushed or sent anywhere, and every notice is re-proved against the
 * conversation's CURRENT audience by the server on each read — so losing access
 * to a chat removes its notices with it.
 */
export async function loadNativeMentions(
  signal?: AbortSignal
): Promise<NeevMentionInbox> {
  return parseMentionInbox(await contentGet("/api/neev/mentions", signal))
}

/** Mark notices read. Idempotent, and only ever the caller's own rows. */
export async function markNativeMentionsRead(
  messageIds: readonly string[],
  csrf: string,
  signal?: AbortSignal
): Promise<NeevMentionInbox> {
  if (messageIds.length > NEEV_MENTION_READ_LIMIT) invalidRequest()
  for (const id of messageIds) assertUuid(id)
  return parseMentionInbox(
    await contentMutation(
      "/api/neev/mentions/read",
      { messageIds: [...messageIds] },
      csrf,
      signal
    )
  )
}

export async function listNativeDocumentRevisions(
  documentId: string,
  after: string | null = null,
  signal?: AbortSignal
): Promise<NativePage<NativeRevision>> {
  assertUuid(documentId)
  if (after !== null) assertPositiveRevision(after)
  return parseRevisionPage(
    await contentGet(
      `/api/neev/documents/${documentId}/revisions${after === null ? "" : `?after=${after}`}`,
      signal
    )
  )
}

export async function saveNativeDocument(
  documentId: string,
  input: SaveNativeDocumentInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NativeRevision> {
  assertUuid(documentId)
  assertUuid(input.requestId)
  assertNonnegativeRevision(input.expectedContentRevision)
  assertPositiveRevision(input.expectedAuthRevision)
  let body: SaveNativeDocumentInput
  try {
    const isMarkdown = Object.prototype.hasOwnProperty.call(input, "markdown")
    exact(input, [
      "requestId",
      "expectedContentRevision",
      "expectedAuthRevision",
      isMarkdown ? "markdown" : "document",
    ])
    if (isMarkdown) {
      if (
        typeof input.markdown !== "string" ||
        new TextEncoder().encode(input.markdown).length > 1048576
      )
        return invalidRequest()
      body = input
    } else {
      body = {
        requestId: input.requestId,
        expectedContentRevision: input.expectedContentRevision,
        expectedAuthRevision: input.expectedAuthRevision,
        document: parseRichDocument(input.document),
      }
    }
  } catch {
    return invalidRequest()
  }
  return parseRevision(
    await contentMutation(
      `/api/neev/documents/${documentId}/revisions`,
      body,
      csrf,
      signal
    )
  )
}

export async function restoreNativeDocument(
  documentId: string,
  input: RestoreNativeDocumentInput,
  csrf: string,
  signal?: AbortSignal
): Promise<NativeRevision> {
  assertUuid(documentId)
  assertUuid(input.requestId)
  assertNonnegativeRevision(input.expectedContentRevision)
  assertPositiveRevision(input.expectedAuthRevision)
  assertPositiveRevision(input.sourceRevision)
  return parseRevision(
    await contentMutation(
      `/api/neev/documents/${documentId}/restore`,
      input,
      csrf,
      signal
    )
  )
}

export async function loadNativeDocumentContent(
  orgId: number,
  documentId: string,
  revision: string | null = null,
  signal?: AbortSignal
): Promise<NativeDocumentContent> {
  assertId(orgId)
  assertUuid(documentId)
  if (revision !== null) integerString(revision, false)
  const suffix = revision === null ? "" : `?revision=${revision}`
  return parseDocumentContent(
    await contentGet(
      `/api/neev/documents/${documentId}/content${suffix}`,
      signal
    )
  )
}

export interface RunNativeAgentInput {
  chatId: string
  prompt: string
  provider: string
  model: string
}

export type NativeAgentEvent =
  | { type: "started"; runId: string }
  | { type: "chunk"; text: string }
  | { type: "completed" }
  | { type: "failed"; reason: string }

function parseAgentEvent(json: string): NativeAgentEvent | null {
  let value: unknown
  try {
    value = JSON.parse(json)
  } catch {
    return null
  }
  const object = record(value)
  switch (object.type) {
    case "started":
      return { type: "started", runId: text(object.run_id) }
    case "chunk":
      return { type: "chunk", text: text(object.text) }
    case "completed":
      return { type: "completed" }
    case "failed":
      return { type: "failed", reason: text(object.reason) }
    default:
      return null
  }
}

// Runs a personal agent for a chat, invoking onEvent for each streamed event.
// Rejects with NativeClientError("unavailable") when the runtime is disabled.
export async function runNativeAgent(
  input: RunNativeAgentInput,
  csrf: string,
  onEvent: (event: NativeAgentEvent) => void,
  signal?: AbortSignal
): Promise<void> {
  if (!csrf) throw new NativeClientError("invalid_request")
  assertUuid(input.chatId)
  if (typeof input.prompt !== "string" || !input.prompt.trim()) invalidRequest()
  const response = await fetch("/api/neev/agent/run", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", "x-neev-csrf": csrf },
    body: JSON.stringify({
      chat_id: input.chatId,
      prompt: input.prompt,
      provider: input.provider,
      model: input.model,
    }),
    signal,
  })
  if (response.status === 503) throw new NativeClientError("unavailable")
  if (response.status === 401) throw new NativeClientError("unauthorized")
  if (response.status === 403) throw new NativeClientError("forbidden")
  await readEventStream(response, (data) => {
    const event = parseAgentEvent(data)
    if (event) onEvent(event)
  })
}

// Reads an SSE body frame by frame, handing each `data:` payload to `onData`.
// Shared by the agent run stream and the team-message stream.
async function readEventStream(
  response: Response,
  onData: (data: string) => void
): Promise<void> {
  if (!response.body) throw new NativeClientError("invalid_response")
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
        if (line.startsWith("data:")) onData(line.slice(5).trim())
      boundary = buffer.indexOf("\n\n")
    }
    if (done) break
  }
}

// A team-message push notice. Deliberately content-free: the server sends a
// cursor, the client re-reads the conversation through the authorized list
// endpoint. Never trust a body that arrives on this channel — there isn't one.
export type NativeMessageStreamEvent =
  | { type: "sync"; sequence: string }
  | { type: "message"; sequence: string }
  | { type: "resync" }
  | { type: "revoked" }

export function parseMessageStreamEvent(
  json: string
): NativeMessageStreamEvent | null {
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
      case "sync":
        // An empty conversation legitimately reports sequence "0".
        return { type: "sync", sequence: integerString(object.sequence, true) }
      case "message":
        return {
          type: "message",
          sequence: integerString(object.sequence, false),
        }
      case "resync":
        return { type: "resync" }
      case "revoked":
        return { type: "revoked" }
      default:
        return null
    }
  } catch {
    return null
  }
}

// Opens the team-message push stream for a chat. Resolves when the server ends
// the stream (its bounded lifetime, or access revoked); rejects when push is
// unavailable, so the caller can fall back to polling with an honest status.
export async function openNativeMessageStream(
  chatId: string,
  onEvent: (event: NativeMessageStreamEvent) => void,
  signal?: AbortSignal
): Promise<void> {
  assertUuid(chatId)
  const response = await fetch(`/api/neev/chats/${chatId}/stream`, {
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
  if (!response.ok) throw new NativeClientError("unavailable", response.status)
  // A rolled-back deployment can serve the SPA shell from the fallback route;
  // treat anything that is not an event stream as "push unavailable".
  if (!response.headers.get("Content-Type")?.includes("text/event-stream"))
    throw new NativeClientError("unavailable", response.status)
  await readEventStream(response, (data) => {
    const event = parseMessageStreamEvent(data)
    if (event) onEvent(event)
  })
}

export async function cancelNativeAgent(
  runId: string,
  csrf: string,
  signal?: AbortSignal
): Promise<boolean> {
  const object = record(
    await contentMutation(
      "/api/neev/agent/cancel",
      { run_id: runId },
      csrf,
      signal
    )
  )
  return object.canceled === true
}

/** Upload an image bound to a document; resolves to its internal serve URL. The
 *  raw file is the body (the server sniffs the bytes; the declared type is
 *  advisory). CSRF-guarded. The returned URL is re-checked to be internal. */
export async function uploadNativeImage(
  documentId: string,
  file: File,
  csrf: string,
  signal?: AbortSignal
): Promise<string> {
  if (!csrf) throw new NativeClientError("invalid_request")
  if (
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
      documentId
    )
  )
    throw new NativeClientError("invalid_request")
  const result = await nativeRequest(
    `/api/neev/documents/${documentId}/images`,
    {
      method: "POST",
      headers: {
        "x-neev-csrf": csrf,
        "Content-Type": file.type || "application/octet-stream",
      },
      body: file,
    },
    signal
  )
  const url = (result as { url?: unknown } | null)?.url
  if (
    typeof url !== "string" ||
    !/^\/api\/neev\/images\/[A-Za-z0-9_-]{1,64}$/.test(url)
  )
    throw new NativeClientError("invalid_response")
  return url
}
