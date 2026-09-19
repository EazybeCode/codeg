const TIMEOUT_MS = 65_000
export type NativeRole = "admin" | "member"
export type NativeProvider = "google" | "email" | "github"
export interface NativeUser {
  id: number
  displayName: string
  avatarUrl: string | null
}
export interface NativeOrganization {
  id: number
  name: string
  avatarUrl: string | null
}
export type NativeSession = { user: NativeUser; csrf: string } & (
  | { scope: "identity"; organization: null; role: null }
  | {
      scope: "organization"
      organization: NativeOrganization
      role: NativeRole
    }
)
export interface NativeCapabilities {
  version: 1
  identityMode: "native"
  runtimeEnabled: boolean
  providers: Record<NativeProvider, boolean>
  catalog: {
    nodeKinds: readonly ["project", "folder"]
    repositoryDefaults: false
    chats: boolean
    documents: boolean
  }
}
export type ServerCapabilities =
  | (NativeCapabilities & { mode: "native" })
  | { version: 1; mode: "operator" | "team"; runtimeEnabled: boolean }
export interface NativeOrganizationDetails extends NativeOrganization {
  ownerUserId: number
  brandingRevision: string
}
export interface NativeMembership {
  id: number
  name: string
  role: NativeRole
}
export interface NativeMember {
  id: number
  user: NativeUser
  role: NativeRole
}
export interface NativeNode {
  id: number
  orgId: number
  parentId: number | null
  kind: "project" | "folder"
  name: string
  revision: string
}
export type NativeErrorKind =
  | "invalid_request"
  | "invalid_response"
  | "unauthorized"
  | "forbidden"
  | "conflict"
  | "limited"
  | "unavailable"
  | "aborted"
export class NativeClientError extends Error {
  constructor(
    public readonly kind: NativeErrorKind,
    public readonly status?: number
  ) {
    super("Native request failed")
    this.name = "NativeClientError"
  }
}
function invalid(): never {
  throw new NativeClientError("invalid_response")
}
function record(v: unknown): Record<string, unknown> {
  if (!v || typeof v !== "object" || Array.isArray(v)) return invalid()
  return v as Record<string, unknown>
}
function id(v: unknown): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v <= 0 || v > 2147483647)
    return invalid()
  return v
}
function string(v: unknown): string {
  if (typeof v !== "string" || !v.length) return invalid()
  return v
}
function avatar(v: unknown): string | null {
  if (v === null) return null
  const value = string(v)
  if (
    !/^https:\/\//.test(value) &&
    !/^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(value)
  )
    return invalid()
  return value
}
function role(v: unknown): NativeRole {
  if (v !== "admin" && v !== "member") return invalid()
  return v
}
function revision(v: unknown): string {
  const value = string(v)
  if (
    !/^[1-9]\d{0,18}$/.test(value) ||
    (value.length === 19 && value > "9223372036854775807")
  )
    return invalid()
  return value
}
function user(v: unknown): NativeUser {
  const o = record(v)
  return {
    id: id(o.id),
    displayName: string(o.displayName),
    avatarUrl: avatar(o.avatarUrl),
  }
}
function organization(v: unknown): NativeOrganization {
  const o = record(v)
  return { id: id(o.id), name: string(o.name), avatarUrl: avatar(o.avatarUrl) }
}
function list<T>(v: unknown, parse: (v: unknown) => T, max: number): T[] {
  if (!Array.isArray(v) || v.length > max) return invalid()
  return v.map(parse)
}
function offset(v: unknown): number | null {
  if (v === null) return null
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > 100000)
    return invalid()
  return v
}
export function parseSession(value: unknown): NativeSession {
  const o = record(value),
    person = user(o.user),
    csrf = string(o.csrf)
  if (o.scope === "identity" && o.organization === null && o.role === null)
    return {
      user: person,
      csrf,
      organization: null,
      role: null,
      scope: "identity",
    }
  if (o.scope === "organization")
    return {
      user: person,
      csrf,
      organization: organization(o.organization),
      role: role(o.role),
      scope: "organization",
    }
  return invalid()
}
export function parseOrganization(value: unknown): NativeOrganizationDetails {
  const o = record(value)
  return {
    ...organization(o),
    ownerUserId: id(o.ownerUserId),
    brandingRevision: revision(o.brandingRevision),
  }
}
export function parseNode(value: unknown): NativeNode {
  const o = record(value)
  if (o.kind !== "project" && o.kind !== "folder") return invalid()
  const parentId = o.parentId === null ? null : id(o.parentId)
  if (parentId === null && o.kind !== "project") return invalid()
  return {
    id: id(o.id),
    orgId: id(o.orgId),
    parentId,
    kind: o.kind,
    name: string(o.name),
    revision: revision(o.revision),
  }
}
export function parseCapabilities(value: unknown): ServerCapabilities {
  const o = record(value)
  if (o.version !== 1) return invalid()
  if ("identityMode" in o) {
    if (
      o.identityMode !== "native" ||
      typeof o.runtimeEnabled !== "boolean" ||
      "mode" in o
    )
      return invalid()
    const p = record(o.providers),
      c = record(o.catalog)
    if (
      ![p.google, p.email, p.github].every((v) => typeof v === "boolean") ||
      c.repositoryDefaults !== false ||
      typeof c.chats !== "boolean" ||
      (c.documents !== undefined && typeof c.documents !== "boolean") ||
      !Array.isArray(c.nodeKinds) ||
      c.nodeKinds.length !== 2 ||
      c.nodeKinds[0] !== "project" ||
      c.nodeKinds[1] !== "folder"
    )
      return invalid()
    return {
      version: 1,
      mode: "native",
      identityMode: "native",
      runtimeEnabled: o.runtimeEnabled as boolean,
      providers: {
        google: p.google as boolean,
        email: p.email as boolean,
        github: p.github as boolean,
      },
      catalog: {
        nodeKinds: ["project", "folder"],
        repositoryDefaults: false,
        chats: c.chats,
        documents: c.documents === true,
      },
    }
  }
  if (
    (o.mode !== "operator" && o.mode !== "team") ||
    typeof o.runtimeEnabled !== "boolean"
  )
    return invalid()
  return { version: 1, mode: o.mode, runtimeEnabled: o.runtimeEnabled }
}
export async function nativeRequest(
  path: string,
  init: RequestInit = {},
  signal?: AbortSignal
): Promise<unknown> {
  if (
    (!path.startsWith("/api/neev/") && path !== "/api/workos/capabilities") ||
    /[\\#]/.test(path) ||
    path.includes("..")
  )
    throw new NativeClientError("invalid_request")
  if (signal?.aborted) throw new NativeClientError("aborted")
  const controller = new AbortController(),
    abort = () => controller.abort()
  signal?.addEventListener("abort", abort, { once: true })
  const timer = setTimeout(abort, TIMEOUT_MS)
  try {
    const response = await fetch(path, {
      ...init,
      credentials: "same-origin",
      cache: "no-store",
      referrerPolicy: "no-referrer",
      redirect: "error",
      signal: controller.signal,
    })
    if (controller.signal.aborted)
      throw new NativeClientError(signal?.aborted ? "aborted" : "unavailable")
    if (!response.ok) {
      const kinds: Record<number, NativeErrorKind> = {
        400: "invalid_request",
        401: "unauthorized",
        403: "forbidden",
        404: "unavailable",
        409: "conflict",
        429: "limited",
        503: "unavailable",
      }
      throw new NativeClientError(
        kinds[response.status] ?? "unavailable",
        response.status
      )
    }
    if (response.status === 204) return null
    if (!response.headers.get("Content-Type")?.includes("application/json"))
      return invalid()
    try {
      return await response.json()
    } catch {
      return invalid()
    }
  } catch (error) {
    if (controller.signal.aborted)
      throw new NativeClientError(signal?.aborted ? "aborted" : "unavailable")
    if (error instanceof NativeClientError) throw error
    throw new NativeClientError("unavailable")
  } finally {
    clearTimeout(timer)
    signal?.removeEventListener("abort", abort)
  }
}
export async function loadServerCapabilities(signal?: AbortSignal) {
  return parseCapabilities(
    await nativeRequest("/api/workos/capabilities", {}, signal)
  )
}
export async function loadNativeSession(signal?: AbortSignal) {
  return parseSession(await nativeRequest("/api/neev/session", {}, signal))
}
export function nativeMutation(
  path: string,
  body: unknown,
  csrf: string,
  signal?: AbortSignal,
  method = "POST"
) {
  if (!csrf) throw new NativeClientError("invalid_request")
  return nativeRequest(
    path,
    {
      method,
      headers: { "Content-Type": "application/json", "x-neev-csrf": csrf },
      body: JSON.stringify(body),
    },
    signal
  )
}
export async function beginNativeSignIn(
  provider: "google" | "email",
  email: string,
  sessionCsrf?: string,
  signal?: AbortSignal
): Promise<string | null> {
  // Refresh browser binding even for a still-valid authenticated link session.
  const preauth = record(await nativeRequest("/api/neev/preauth", {}, signal))
  const csrf = sessionCsrf ?? string(preauth.csrf),
    intent = sessionCsrf ? "link" : "login"
  const result = record(
    await nativeMutation(
      provider === "email"
        ? `/api/neev/email/${intent}`
        : `/api/neev/auth/google/${intent}`,
      provider === "email" ? { email } : {},
      csrf,
      signal
    )
  )
  if (provider === "email") {
    if (result.status !== "accepted" || typeof result.message !== "string")
      return invalid()
    return null
  }
  const url = new URL(string(result.authorizationUrl))
  if (
    url.origin !== "https://accounts.google.com" ||
    url.pathname !== "/o/oauth2/v2/auth" ||
    url.username ||
    url.password ||
    url.hash
  )
    return invalid()
  return url.href
}
export async function loadMemberships(page: number, signal?: AbortSignal) {
  const o = record(
    await nativeRequest(
      `/api/neev/organizations?limit=100&offset=${page}`,
      {},
      signal
    )
  )
  return {
    organizations: list(
      o.organizations,
      (v) => {
        const row = record(v)
        return { id: id(row.id), name: string(row.name), role: role(row.role) }
      },
      100
    ),
    nextOffset: offset(o.nextOffset),
  }
}
export async function createOrganization(
  name: string,
  requestId: string,
  csrf: string,
  signal?: AbortSignal
) {
  const o = record(
    await nativeMutation(
      "/api/neev/organizations",
      { name, requestId },
      csrf,
      signal
    )
  )
  if (role(o.role) !== "admin") return invalid()
  revision(o.brandingRevision)
  return organization(o.organization)
}
export async function selectOrganization(
  organizationId: number,
  csrf: string,
  signal?: AbortSignal
) {
  const o = record(
    await nativeMutation(
      "/api/neev/organizations/select",
      { organizationId },
      csrf,
      signal
    )
  )
  role(o.role)
  return organization(o.organization)
}
export async function loadOrganization(signal?: AbortSignal) {
  return parseOrganization(
    await nativeRequest("/api/neev/organization", {}, signal)
  )
}
export async function loadMembers(page: number, signal?: AbortSignal) {
  const o = record(
    await nativeRequest(
      `/api/neev/organization/members?limit=100&offset=${page}`,
      {},
      signal
    )
  )
  return list(
    o.members,
    (v) => {
      const row = record(v)
      return { id: id(row.id), user: user(row.user), role: role(row.role) }
    },
    100
  )
}
export async function loadMethods(
  signal?: AbortSignal
): Promise<NativeProvider[]> {
  const o = record(await nativeRequest("/api/neev/auth/methods", {}, signal))
  const methods = list(
    o.methods,
    (v) => {
      if (v !== "google" && v !== "email" && v !== "github") return invalid()
      return v
    },
    3
  )
  if (new Set(methods).size !== methods.length) return invalid()
  return methods
}
export async function loadNodes(
  orgId: number,
  after: number | null,
  signal?: AbortSignal
) {
  const o = record(
    await nativeRequest(
      `/api/neev/nodes${after === null ? "" : `?after=${after}`}`,
      {},
      signal
    )
  )
  const nodes = list(o.nodes, parseNode, 200),
    nextAfter = o.nextAfter === null ? null : id(o.nextAfter)
  if (
    nodes.some(
      (node, index) =>
        node.orgId !== orgId ||
        node.id <= (index ? nodes[index - 1].id : (after ?? 0))
    ) ||
    (nextAfter !== null &&
      (nodes.length === 0 || nextAfter !== nodes[nodes.length - 1].id))
  )
    return invalid()
  return { nodes, nextAfter }
}
