import { NativeClientError, nativeMutation, nativeRequest } from "./neev-client"

export type NativeResourceKind = "chat" | "document"
export type NativeVisibility = "private" | "organization"

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
}

export interface NativeMessage {
  id: string
  sequence: string
  authorId: number
  authorName: string
  body: string
  createdAt: number
}

export interface NativeRevision {
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
}

export interface NativePage<T> {
  items: T[]
  nextAfter: string | null
}

export interface CreateNativeResourceInput {
  title: string
  requestId: string
  originChatId?: string | null
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
}

export interface SaveNativeDocumentInput {
  requestId: string
  expectedContentRevision: string
  expectedAuthRevision: string
  markdown: string
}

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
  const object = exact(value, [
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
  ])
  if (object.kind !== "chat" && object.kind !== "document") return invalid()
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
  }
}

export function parseMessage(value: unknown): NativeMessage {
  const object = exact(value, [
    "id",
    "sequence",
    "authorId",
    "authorName",
    "body",
    "createdAt",
  ])
  const body = text(object.body)
  if (body.length === 0 || new TextEncoder().encode(body).length > 32768)
    return invalid()
  return {
    id: uuid(object.id),
    sequence: integerString(object.sequence, false),
    authorId: positiveId(object.authorId),
    authorName: text(object.authorName),
    body,
    createdAt: timestamp(object.createdAt),
  }
}

export function parseRevision(value: unknown): NativeRevision {
  const object = exact(value, [
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
  return {
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
  const object = exact(value, ["revision", "markdown"])
  const revision = parseRevision(object.revision)
  const markdown = text(object.markdown)
  if (String(new TextEncoder().encode(markdown).length) !== revision.byteLength)
    return invalid()
  return { revision, markdown }
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
  const body =
    kind === "document" &&
    Object.prototype.hasOwnProperty.call(input, "originChatId")
      ? {
          title: input.title,
          requestId: input.requestId,
          originChatId: input.originChatId,
        }
      : { title: input.title, requestId: input.requestId }
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
  return parseMessage(
    await contentMutation(
      `/api/neev/chats/${chatId}/messages`,
      input,
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
  if (typeof input.markdown !== "string") invalidRequest()
  return parseRevision(
    await contentMutation(
      `/api/neev/documents/${documentId}/revisions`,
      input,
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
