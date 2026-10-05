/**
 * Member directory, teams, and sharing with members and teams
 * (`.context/sharing-api-contract.md`, v1).
 *
 * The server decides every audience; these helpers only ask and describe.
 * Responses are parsed strictly, exact keys, like every other Neev client: a
 * shape this client does not understand is refused rather than guessed at,
 * because the one thing it must never do is show someone access they do not
 * have, or hide access they do.
 */
import { NativeClientError, nativeMutation, nativeRequest } from "./neev-client"
import {
  parseAgentSession,
  type NeevAgentSession,
} from "./neev-agent-sessions-client"
import { parseResource, type NativeResource } from "./neev-content-client"

export type Level = "view" | "edit"
export type PrincipalInput =
  | { kind: "user"; userId: number }
  | { kind: "team"; teamId: string }
export type Principal =
  | {
      kind: "user"
      userId: number
      displayName: string
      avatarUrl: string | null
    }
  | { kind: "team"; teamId: string; name: string; memberCount: number }
export type Grant = { principal: Principal; level: Level }
export type InheritedGrant = {
  nodeId: number
  nodeName: string
  principal: Principal
  level: Level
}

export type DirectoryMember = {
  userId: number
  displayName: string
  avatarUrl: string | null
  role: "admin" | "member"
}
export type DirectoryPage = {
  members: DirectoryMember[]
  nextAfter: string | null
}

export type Team = {
  id: string
  name: string
  memberCount: number
  revision: string
  canManage: boolean
}
export type TeamMembers = {
  teamId: string
  revision: string
  members: { userId: number; displayName: string; avatarUrl: string | null }[]
}

export type ResourceAccess = {
  resourceId: string
  visibility: "private" | "shared" | "organization"
  authRevision: string
  /** The caller's own level. */
  level: "owner" | "edit" | "view"
  /** The creator only. */
  canManage: boolean
  /** The origin chat when access follows a chat (a bound document). */
  boundTo: string | null
  direct: Grant[]
  /** Grants on the containing folders; they apply only while `shared`. */
  inherited: InheritedGrant[]
}
export type NodeAccess = {
  nodeId: number
  revision: string
  /** May change grants here. Restricted region: the outermost restricted root's creator only; open: creator or admin. */
  canManage: boolean
  /** May restrict or reopen this node. Open region: its creator; inside a restriction: the outermost restricted root's creator. Never admins by role. */
  canRestrict: boolean
  /** This folder/project is restricted (hidden from everyone but its creator and recipients). */
  restricted: boolean
  /** The caller's folder level here. */
  level: "owner" | "edit" | "view"
  direct: Grant[]
  /** Visible ancestors only, stopping at the nearest restricted root. */
  inherited: InheritedGrant[]
}
export type SessionAccess = {
  sessionId: string
  revision: string
  level: "owner" | "view"
  canManage: boolean
  direct: Grant[]
}
export type SharedSession = {
  session: NeevAgentSession
  owner: { userId: number; displayName: string; avatarUrl: string | null }
}

/** Why a sharing write was refused; `NativeClientError.code` carries one. */
export type SharingRefusal = "team_name_taken" | "resource_not_shared"

/** The refusal behind an error from a helper here, or `null` for any other. */
export function sharingRefusal(error: unknown): SharingRefusal | null {
  if (!(error instanceof NativeClientError) || error.kind !== "conflict")
    return null
  return error.code === "team_name_taken" ||
    error.code === "resource_not_shared"
    ? error.code
    : null
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
const REVISION = /^[1-9][0-9]{0,18}$/

function invalid(): never {
  throw new NativeClientError("invalid_response")
}
function invalidRequest(): never {
  throw new NativeClientError("invalid_request")
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
const isId = (value: unknown): value is number =>
  typeof value === "number" &&
  Number.isInteger(value) &&
  value > 0 &&
  value <= 2_147_483_647
const id = (value: unknown): number => (isId(value) ? value : invalid())
const uuid = (value: unknown): string =>
  typeof value === "string" && UUID.test(value) ? value : invalid()
const revision = (value: unknown): string =>
  typeof value === "string" && REVISION.test(value) ? value : invalid()
const flag = (value: unknown): boolean =>
  typeof value === "boolean" ? value : invalid()
const count = (value: unknown): number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : invalid()
const name = (value: unknown): string =>
  typeof value === "string" && value.length > 0 ? value : invalid()
const avatar = (value: unknown): string | null =>
  value === null || typeof value === "string" ? value : invalid()
const list = (value: unknown): unknown[] =>
  Array.isArray(value) ? value : invalid()
const level = (value: unknown): Level =>
  value === "view" || value === "edit" ? value : invalid()

function parsePrincipal(value: unknown): Principal {
  const kind =
    value && typeof value === "object"
      ? (value as Record<string, unknown>).kind
      : undefined
  if (kind === "user") {
    const o = exact(value, ["kind", "userId", "displayName", "avatarUrl"])
    return {
      kind: "user",
      userId: id(o.userId),
      displayName: name(o.displayName),
      avatarUrl: avatar(o.avatarUrl),
    }
  }
  if (kind === "team") {
    const o = exact(value, ["kind", "teamId", "name", "memberCount"])
    return {
      kind: "team",
      teamId: uuid(o.teamId),
      name: name(o.name),
      memberCount: count(o.memberCount),
    }
  }
  return invalid()
}

function parseGrant(value: unknown): Grant {
  const o = exact(value, ["principal", "level"])
  return { principal: parsePrincipal(o.principal), level: level(o.level) }
}

function parseInherited(value: unknown): InheritedGrant {
  const o = exact(value, ["nodeId", "nodeName", "principal", "level"])
  return {
    nodeId: id(o.nodeId),
    nodeName: name(o.nodeName),
    principal: parsePrincipal(o.principal),
    level: level(o.level),
  }
}

export function parseDirectoryPage(value: unknown): DirectoryPage {
  const o = exact(value, ["members", "nextAfter"])
  const members = list(o.members).map((row): DirectoryMember => {
    const m = exact(row, ["userId", "displayName", "avatarUrl", "role"])
    if (m.role !== "admin" && m.role !== "member") return invalid()
    return {
      userId: id(m.userId),
      displayName: name(m.displayName),
      avatarUrl: avatar(m.avatarUrl),
      role: m.role,
    }
  })
  const nextAfter =
    o.nextAfter === null
      ? null
      : typeof o.nextAfter === "string" && o.nextAfter.length > 0
        ? o.nextAfter
        : invalid()
  return { members, nextAfter }
}

export function parseTeam(value: unknown): Team {
  const o = exact(value, ["id", "name", "memberCount", "revision", "canManage"])
  return {
    id: uuid(o.id),
    name: name(o.name),
    memberCount: count(o.memberCount),
    revision: revision(o.revision),
    canManage: flag(o.canManage),
  }
}

export function parseTeamMembers(value: unknown, teamId: string): TeamMembers {
  const o = exact(value, ["teamId", "revision", "members"])
  if (o.teamId !== teamId) return invalid()
  return {
    teamId,
    revision: revision(o.revision),
    members: list(o.members).map((row) => {
      const m = exact(row, ["userId", "displayName", "avatarUrl"])
      return {
        userId: id(m.userId),
        displayName: name(m.displayName),
        avatarUrl: avatar(m.avatarUrl),
      }
    }),
  }
}

export function parseResourceAccess(
  value: unknown,
  resourceId: string
): ResourceAccess {
  const o = exact(value, [
    "resourceId",
    "visibility",
    "authRevision",
    "level",
    "canManage",
    "boundTo",
    "direct",
    "inherited",
  ])
  if (o.resourceId !== resourceId) return invalid()
  if (
    o.visibility !== "private" &&
    o.visibility !== "shared" &&
    o.visibility !== "organization"
  )
    return invalid()
  if (o.level !== "owner" && o.level !== "edit" && o.level !== "view")
    return invalid()
  const canManage = flag(o.canManage)
  const boundTo = o.boundTo === null ? null : uuid(o.boundTo)
  const direct = list(o.direct).map(parseGrant)
  // Internally consistent, or refused: only the owner manages, a bound
  // document is managed through its chat, and direct grants exist only on a
  // shared item.
  if (canManage && (o.level !== "owner" || boundTo !== null)) return invalid()
  if (direct.length > 0 && o.visibility !== "shared") return invalid()
  return {
    resourceId,
    visibility: o.visibility,
    authRevision: revision(o.authRevision),
    level: o.level,
    canManage,
    boundTo,
    direct,
    inherited: list(o.inherited).map(parseInherited),
  }
}

export function parseNodeAccess(value: unknown, nodeId: number): NodeAccess {
  const o = exact(value, [
    "nodeId",
    "revision",
    "canManage",
    "canRestrict",
    "restricted",
    "level",
    "direct",
    "inherited",
  ])
  if (o.nodeId !== nodeId) return invalid()
  if (o.level !== "owner" && o.level !== "edit" && o.level !== "view")
    return invalid()
  const canManage = flag(o.canManage)
  const canRestrict = flag(o.canRestrict)
  // Managing or restricting needs at least Edit here; a View recipient does neither.
  if ((canManage || canRestrict) && o.level === "view") return invalid()
  return {
    nodeId,
    revision: revision(o.revision),
    canManage,
    canRestrict,
    restricted: flag(o.restricted),
    level: o.level,
    direct: list(o.direct).map(parseGrant),
    inherited: list(o.inherited).map(parseInherited),
  }
}

export function parseSessionAccess(
  value: unknown,
  sessionId: string
): SessionAccess {
  const o = exact(value, [
    "sessionId",
    "revision",
    "level",
    "canManage",
    "direct",
  ])
  if (o.sessionId !== sessionId) return invalid()
  if (o.level !== "owner" && o.level !== "view") return invalid()
  const canManage = flag(o.canManage)
  if (canManage !== (o.level === "owner")) return invalid()
  const direct = list(o.direct).map(parseGrant)
  // An Agent Chat is shared for viewing only.
  if (direct.some((grant) => grant.level !== "view")) return invalid()
  return {
    sessionId,
    revision: revision(o.revision),
    level: o.level,
    canManage,
    direct,
  }
}

export function parseSharedSessions(value: unknown): SharedSession[] {
  const o = exact(value, ["sessions"])
  return list(o.sessions).map((row) => {
    const s = exact(row, ["session", "owner"])
    const owner = exact(s.owner, ["userId", "displayName", "avatarUrl"])
    const session = parseAgentSession(s.session)
    const userId = id(owner.userId)
    if (session.ownerId !== userId) return invalid()
    return {
      session,
      owner: {
        userId,
        displayName: name(owner.displayName),
        avatarUrl: avatar(owner.avatarUrl),
      },
    }
  })
}

// ------------------------------------------------------------ arguments

function assertUuid(value: string) {
  if (typeof value !== "string" || !UUID.test(value)) invalidRequest()
}
function assertId(value: number) {
  if (!isId(value)) invalidRequest()
}
function assertRevision(value: string) {
  if (typeof value !== "string" || !REVISION.test(value)) invalidRequest()
}
function principalBody(principal: PrincipalInput): PrincipalInput {
  if (principal?.kind === "user") {
    assertId(principal.userId)
    return { kind: "user", userId: principal.userId }
  }
  if (principal?.kind === "team") {
    assertUuid(principal.teamId)
    return { kind: "team", teamId: principal.teamId }
  }
  return invalidRequest()
}
function assertLevel(value: string, allowed: readonly string[]) {
  if (!allowed.includes(value)) invalidRequest()
}

// ------------------------------------------------------------ directory

export async function searchDirectory(
  params: { q?: string; limit?: number; after?: string | null },
  signal?: AbortSignal
): Promise<DirectoryPage> {
  const query = new URLSearchParams()
  if (params.q !== undefined && params.q.trim() !== "") {
    if ([...params.q].length > 100) invalidRequest()
    query.set("q", params.q)
  }
  if (params.limit !== undefined) {
    if (
      !Number.isInteger(params.limit) ||
      params.limit < 1 ||
      params.limit > 100
    )
      invalidRequest()
    query.set("limit", String(params.limit))
  }
  if (params.after) {
    if (params.after.length > 1024) invalidRequest()
    query.set("after", params.after)
  }
  const suffix = query.toString()
  return parseDirectoryPage(
    await nativeRequest(
      `/api/neev/organization/directory${suffix ? `?${suffix}` : ""}`,
      {},
      signal
    )
  )
}

// ---------------------------------------------------------------- teams

export async function listTeams(signal?: AbortSignal): Promise<Team[]> {
  const o = exact(await nativeRequest("/api/neev/teams", {}, signal), ["teams"])
  return list(o.teams).map(parseTeam)
}

export async function createTeam(
  name: string,
  requestId: string,
  csrf: string,
  signal?: AbortSignal
): Promise<Team> {
  assertUuid(requestId)
  if (typeof name !== "string") invalidRequest()
  return parseTeam(
    await nativeMutation("/api/neev/teams", { name, requestId }, csrf, signal)
  )
}

export async function renameTeam(
  teamId: string,
  name: string,
  expectedRevision: string,
  csrf: string,
  signal?: AbortSignal
): Promise<Team> {
  assertUuid(teamId)
  assertRevision(expectedRevision)
  if (typeof name !== "string") invalidRequest()
  const team = parseTeam(
    await nativeMutation(
      `/api/neev/teams/${teamId}`,
      { name, expectedRevision },
      csrf,
      signal,
      "PATCH"
    )
  )
  if (team.id !== teamId) invalid()
  return team
}

export async function deleteTeam(
  teamId: string,
  expectedRevision: string,
  csrf: string,
  signal?: AbortSignal
): Promise<void> {
  assertUuid(teamId)
  assertRevision(expectedRevision)
  const o = exact(
    await nativeMutation(
      `/api/neev/teams/${teamId}`,
      { expectedRevision },
      csrf,
      signal,
      "DELETE"
    ),
    ["ok"]
  )
  if (o.ok !== true) invalid()
}

export async function loadTeamMembers(
  teamId: string,
  signal?: AbortSignal
): Promise<TeamMembers> {
  assertUuid(teamId)
  return parseTeamMembers(
    await nativeRequest(`/api/neev/teams/${teamId}/members`, {}, signal),
    teamId
  )
}

export async function addTeamMember(
  teamId: string,
  userId: number,
  expectedRevision: string,
  csrf: string,
  signal?: AbortSignal
): Promise<TeamMembers> {
  assertUuid(teamId)
  assertId(userId)
  assertRevision(expectedRevision)
  return parseTeamMembers(
    await nativeMutation(
      `/api/neev/teams/${teamId}/members`,
      { userId, expectedRevision },
      csrf,
      signal
    ),
    teamId
  )
}

export async function removeTeamMember(
  teamId: string,
  userId: number,
  expectedRevision: string,
  csrf: string,
  signal?: AbortSignal
): Promise<TeamMembers> {
  assertUuid(teamId)
  assertId(userId)
  assertRevision(expectedRevision)
  return parseTeamMembers(
    await nativeMutation(
      `/api/neev/teams/${teamId}/members/${userId}`,
      { expectedRevision },
      csrf,
      signal,
      "DELETE"
    ),
    teamId
  )
}

// ------------------------------------------------------------- resource

export async function loadResourceAccess(
  resourceId: string,
  signal?: AbortSignal
): Promise<ResourceAccess> {
  assertUuid(resourceId)
  return parseResourceAccess(
    await nativeRequest(`/api/neev/resources/${resourceId}/access`, {}, signal),
    resourceId
  )
}

export async function setResourceGrant(
  resourceId: string,
  input: {
    expectedAuthRevision: string
    principal: PrincipalInput
    level: Level
  },
  csrf: string,
  signal?: AbortSignal
): Promise<ResourceAccess> {
  assertUuid(resourceId)
  assertRevision(input.expectedAuthRevision)
  assertLevel(input.level, ["view", "edit"])
  return parseResourceAccess(
    await nativeMutation(
      `/api/neev/resources/${resourceId}/grants`,
      {
        expectedAuthRevision: input.expectedAuthRevision,
        principal: principalBody(input.principal),
        level: input.level,
      },
      csrf,
      signal,
      "PUT"
    ),
    resourceId
  )
}

export async function removeResourceGrant(
  resourceId: string,
  input: { expectedAuthRevision: string; principal: PrincipalInput },
  csrf: string,
  signal?: AbortSignal
): Promise<ResourceAccess> {
  assertUuid(resourceId)
  assertRevision(input.expectedAuthRevision)
  return parseResourceAccess(
    await nativeMutation(
      `/api/neev/resources/${resourceId}/grants`,
      {
        expectedAuthRevision: input.expectedAuthRevision,
        principal: principalBody(input.principal),
      },
      csrf,
      signal,
      "DELETE"
    ),
    resourceId
  )
}

// ----------------------------------------------------------------- node

export async function loadNodeAccess(
  nodeId: number,
  signal?: AbortSignal
): Promise<NodeAccess> {
  assertId(nodeId)
  return parseNodeAccess(
    await nativeRequest(`/api/neev/nodes/${nodeId}/access`, {}, signal),
    nodeId
  )
}

export async function setNodeGrant(
  nodeId: number,
  input: { expectedRevision: string; principal: PrincipalInput; level: Level },
  csrf: string,
  signal?: AbortSignal
): Promise<NodeAccess> {
  assertId(nodeId)
  assertRevision(input.expectedRevision)
  assertLevel(input.level, ["view", "edit"])
  return parseNodeAccess(
    await nativeMutation(
      `/api/neev/nodes/${nodeId}/grants`,
      {
        expectedRevision: input.expectedRevision,
        principal: principalBody(input.principal),
        level: input.level,
      },
      csrf,
      signal,
      "PUT"
    ),
    nodeId
  )
}

export async function removeNodeGrant(
  nodeId: number,
  input: { expectedRevision: string; principal: PrincipalInput },
  csrf: string,
  signal?: AbortSignal
): Promise<NodeAccess> {
  assertId(nodeId)
  assertRevision(input.expectedRevision)
  return parseNodeAccess(
    await nativeMutation(
      `/api/neev/nodes/${nodeId}/grants`,
      {
        expectedRevision: input.expectedRevision,
        principal: principalBody(input.principal),
      },
      csrf,
      signal,
      "DELETE"
    ),
    nodeId
  )
}

// -------------------------------------------------------------- session

export async function loadSessionAccess(
  sessionId: string,
  signal?: AbortSignal
): Promise<SessionAccess> {
  assertUuid(sessionId)
  return parseSessionAccess(
    await nativeRequest(
      `/api/neev/agent/sessions/${sessionId}/access`,
      {},
      signal
    ),
    sessionId
  )
}

export async function setSessionGrant(
  sessionId: string,
  input: {
    expectedRevision: string
    principal: PrincipalInput
    level: "view"
  },
  csrf: string,
  signal?: AbortSignal
): Promise<SessionAccess> {
  assertUuid(sessionId)
  assertRevision(input.expectedRevision)
  // View only: continuing someone else's Agent Chat is a fork.
  assertLevel(input.level, ["view"])
  return parseSessionAccess(
    await nativeMutation(
      `/api/neev/agent/sessions/${sessionId}/grants`,
      {
        expectedRevision: input.expectedRevision,
        principal: principalBody(input.principal),
        level: "view",
      },
      csrf,
      signal,
      "PUT"
    ),
    sessionId
  )
}

export async function removeSessionGrant(
  sessionId: string,
  input: { expectedRevision: string; principal: PrincipalInput },
  csrf: string,
  signal?: AbortSignal
): Promise<SessionAccess> {
  assertUuid(sessionId)
  assertRevision(input.expectedRevision)
  return parseSessionAccess(
    await nativeMutation(
      `/api/neev/agent/sessions/${sessionId}/grants`,
      {
        expectedRevision: input.expectedRevision,
        principal: principalBody(input.principal),
      },
      csrf,
      signal,
      "DELETE"
    ),
    sessionId
  )
}

export async function listSharedSessions(
  signal?: AbortSignal
): Promise<SharedSession[]> {
  return parseSharedSessions(
    await nativeRequest("/api/neev/agent/shared-sessions", {}, signal)
  )
}

// ------------------------------------------------- restricted folders (v2)

/** Restrict or reopen a project/folder. The node's creator only. */
export async function setNodeRestriction(
  nodeId: number,
  input: { expectedRevision: string; restricted: boolean },
  csrf: string,
  signal?: AbortSignal
): Promise<NodeAccess> {
  assertId(nodeId)
  assertRevision(input.expectedRevision)
  if (typeof input.restricted !== "boolean") invalidRequest()
  const access = parseNodeAccess(
    await nativeMutation(
      `/api/neev/nodes/${nodeId}/restriction`,
      {
        expectedRevision: input.expectedRevision,
        restricted: input.restricted,
      },
      csrf,
      signal,
      "PUT"
    ),
    nodeId
  )
  if (access.restricted !== input.restricted) invalid()
  return access
}

/**
 * Chats and documents the caller can open whose folder is hidden from them
 * (shared directly, or their own). Open each by id; its folder is a 404.
 */
export async function listSharedResources(
  signal?: AbortSignal
): Promise<NativeResource[]> {
  const o = exact(
    await nativeRequest("/api/neev/shared/resources", {}, signal),
    ["resources"]
  )
  return list(o.resources).map(parseResource)
}
