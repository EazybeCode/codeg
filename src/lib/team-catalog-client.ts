import { TeamClientError, teamRequest, teamResponseJson } from "./team-client"

const MAX_I64_DECIMAL = "9223372036854775807"
const MAX_I32 = 2_147_483_647

export type TeamNodeKind = "project" | "folder" | "working_directory"
export type TeamChatVisibility = "private" | "organization"

export interface TeamRepositoryDefault {
  repositoryId: number
  startingBranch: string
}

export interface TeamCatalogNode {
  id: number
  parentId: number | null
  kind: TeamNodeKind
  name: string
  repositoryDefault: TeamRepositoryDefault | null
  revision: string
}

export interface TeamCatalogRepository {
  id: number
  githubRepositoryId: string
  fullName: string
}

export interface TeamCatalogChat {
  id: number
  nodeId: number
  title: string | null
  repositoryId: number
  githubRepositoryId: string
  fullName: string
  startingBranch: string
  visibility: TeamChatVisibility
  accessRevision: string
  canShare: boolean
  executionState: "unprovisioned"
}

export interface CreateTeamNodeInput {
  parentId: number | null
  kind: TeamNodeKind
  name: string
  repositoryDefault: TeamRepositoryDefault | null
}

export interface UpdateTeamNodeInput {
  expectedRevision: string
  parentId: number | null
  name: string
  repositoryDefault: TeamRepositoryDefault | null
}

export interface CreateTeamChatInput {
  nodeId: number
  repositoryOverride: TeamRepositoryDefault | null
  title: string | null
  requestId: string
  visibility: TeamChatVisibility
}

export interface ChangeTeamChatVisibilityInput {
  expectedRevision: string
  visibility: TeamChatVisibility
}

type RecordValue = Record<string, unknown>

function invalidResponse(): never {
  throw new TeamClientError("invalid_response")
}

function invalidRequest(): never {
  throw new TeamClientError("invalid_request")
}

function isRecord(value: unknown): value is RecordValue {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function exactKeys(value: RecordValue, keys: readonly string[]): boolean {
  const actual = Object.keys(value).sort()
  const expected = [...keys].sort()
  return (
    actual.length === expected.length &&
    actual.every((key, index) => key === expected[index])
  )
}

function isPositiveId(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value > 0 &&
    value <= MAX_I32
  )
}

function isRevision(value: unknown, positive = false): value is string {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) return false
  if (positive && value === "0") return false
  return (
    value.length < MAX_I64_DECIMAL.length ||
    (value.length === MAX_I64_DECIMAL.length && value <= MAX_I64_DECIMAL)
  )
}

function isNodeKind(value: unknown): value is TeamNodeKind {
  return (
    value === "project" || value === "folder" || value === "working_directory"
  )
}

function isVisibility(value: unknown): value is TeamChatVisibility {
  return value === "private" || value === "organization"
}

function parseRepositoryDefault(value: unknown): TeamRepositoryDefault {
  if (
    !isRecord(value) ||
    !exactKeys(value, ["repositoryId", "startingBranch"]) ||
    !isPositiveId(value.repositoryId) ||
    typeof value.startingBranch !== "string"
  ) {
    invalidResponse()
  }
  return value as unknown as TeamRepositoryDefault
}

export function parseTeamNode(value: unknown): TeamCatalogNode {
  if (
    !isRecord(value) ||
    !exactKeys(value, [
      "id",
      "parentId",
      "kind",
      "name",
      "repositoryDefault",
      "revision",
    ]) ||
    !isPositiveId(value.id) ||
    !(value.parentId === null || isPositiveId(value.parentId)) ||
    !isNodeKind(value.kind) ||
    typeof value.name !== "string" ||
    !isRevision(value.revision)
  ) {
    invalidResponse()
  }
  if (value.repositoryDefault !== null) {
    parseRepositoryDefault(value.repositoryDefault)
  }
  return value as unknown as TeamCatalogNode
}

function parseTeamRepository(value: unknown): TeamCatalogRepository {
  if (
    !isRecord(value) ||
    !exactKeys(value, ["id", "githubRepositoryId", "fullName"]) ||
    !isPositiveId(value.id) ||
    !isRevision(value.githubRepositoryId, true) ||
    typeof value.fullName !== "string"
  ) {
    invalidResponse()
  }
  return value as unknown as TeamCatalogRepository
}

export function parseTeamChat(value: unknown): TeamCatalogChat {
  if (
    !isRecord(value) ||
    !exactKeys(value, [
      "id",
      "nodeId",
      "title",
      "repositoryId",
      "githubRepositoryId",
      "fullName",
      "startingBranch",
      "visibility",
      "accessRevision",
      "canShare",
      "executionState",
    ]) ||
    !isPositiveId(value.id) ||
    !isPositiveId(value.nodeId) ||
    !(value.title === null || typeof value.title === "string") ||
    !isPositiveId(value.repositoryId) ||
    !isRevision(value.githubRepositoryId, true) ||
    typeof value.fullName !== "string" ||
    typeof value.startingBranch !== "string" ||
    !isVisibility(value.visibility) ||
    !isRevision(value.accessRevision) ||
    typeof value.canShare !== "boolean" ||
    value.executionState !== "unprovisioned"
  ) {
    invalidResponse()
  }
  return value as unknown as TeamCatalogChat
}

function parseList<T>(payload: unknown, parse: (value: unknown) => T): T[] {
  if (
    !isRecord(payload) ||
    !exactKeys(payload, ["items"]) ||
    !Array.isArray(payload.items)
  ) {
    invalidResponse()
  }
  return payload.items.map(parse)
}

function assertCsrf(csrf: string): void {
  if (!csrf) invalidRequest()
}

function assertExactInput(
  value: unknown,
  keys: readonly string[]
): RecordValue {
  if (!isRecord(value) || !exactKeys(value, keys)) invalidRequest()
  return value
}

function assertRepositoryInput(value: unknown, nullable = false): void {
  if (nullable && value === null) return
  const input = assertExactInput(value, ["repositoryId", "startingBranch"])
  if (
    !isPositiveId(input.repositoryId) ||
    typeof input.startingBranch !== "string"
  )
    invalidRequest()
}

function mutationInit(
  body: unknown,
  csrf: string,
  method = "POST"
): RequestInit {
  assertCsrf(csrf)
  return {
    method,
    headers: {
      "Content-Type": "application/json",
      "X-WorkOS-CSRF": csrf,
    },
    body: JSON.stringify(body),
  }
}

async function json(path: string, init: RequestInit, signal?: AbortSignal) {
  return teamRequest(path, init, signal, teamResponseJson)
}

export async function listTeamNodes(
  signal?: AbortSignal
): Promise<TeamCatalogNode[]> {
  return parseList(
    await json("/api/team/catalog/nodes", {}, signal),
    parseTeamNode
  )
}

export async function listTeamRepositories(
  signal?: AbortSignal
): Promise<TeamCatalogRepository[]> {
  return parseList(
    await json("/api/team/catalog/repositories", {}, signal),
    parseTeamRepository
  )
}

export async function connectTeamRepository(
  input: { fullName: string; startingBranch: string },
  csrf: string,
  signal?: AbortSignal
): Promise<TeamCatalogRepository> {
  const value = assertExactInput(input, ["fullName", "startingBranch"])
  if (
    typeof value.fullName !== "string" ||
    typeof value.startingBranch !== "string"
  )
    invalidRequest()
  return parseTeamRepository(
    await json(
      "/api/team/catalog/repositories",
      mutationInit(input, csrf),
      signal
    )
  )
}

export async function createTeamNode(
  input: CreateTeamNodeInput,
  csrf: string,
  signal?: AbortSignal
): Promise<TeamCatalogNode> {
  const value = assertExactInput(input, [
    "parentId",
    "kind",
    "name",
    "repositoryDefault",
  ])
  if (
    !(value.parentId === null || isPositiveId(value.parentId)) ||
    !isNodeKind(value.kind) ||
    typeof value.name !== "string"
  )
    invalidRequest()
  assertRepositoryInput(value.repositoryDefault, true)
  return parseTeamNode(
    await json("/api/team/catalog/nodes", mutationInit(input, csrf), signal)
  )
}

export async function updateTeamNode(
  id: number,
  input: UpdateTeamNodeInput,
  csrf: string,
  signal?: AbortSignal
): Promise<TeamCatalogNode> {
  if (!isPositiveId(id)) invalidRequest()
  const value = assertExactInput(input, [
    "expectedRevision",
    "parentId",
    "name",
    "repositoryDefault",
  ])
  if (
    !isRevision(value.expectedRevision) ||
    !(value.parentId === null || isPositiveId(value.parentId)) ||
    typeof value.name !== "string"
  )
    invalidRequest()
  assertRepositoryInput(value.repositoryDefault, true)
  return parseTeamNode(
    await json(
      `/api/team/catalog/nodes/${id}`,
      mutationInit(input, csrf, "PATCH"),
      signal
    )
  )
}

export async function listTeamChats(
  query: { nodeId?: number; afterId?: number } = {},
  signal?: AbortSignal
): Promise<TeamCatalogChat[]> {
  const value = assertExactInput(query, Object.keys(query))
  if (
    Object.keys(value).some((key) => key !== "nodeId" && key !== "afterId") ||
    (value.nodeId !== undefined && !isPositiveId(value.nodeId)) ||
    (value.afterId !== undefined && !isPositiveId(value.afterId))
  )
    invalidRequest()
  const search = new URLSearchParams()
  if (value.nodeId !== undefined) search.set("nodeId", String(value.nodeId))
  if (value.afterId !== undefined) search.set("afterId", String(value.afterId))
  const suffix = search.size ? `?${search}` : ""
  return parseList(
    await json(`/api/team/chats${suffix}`, {}, signal),
    parseTeamChat
  )
}

export async function loadTeamChat(
  id: number,
  signal?: AbortSignal
): Promise<TeamCatalogChat> {
  if (!isPositiveId(id)) invalidRequest()
  return parseTeamChat(await json(`/api/team/chats/${id}`, {}, signal))
}

export async function createTeamChat(
  input: CreateTeamChatInput,
  csrf: string,
  signal?: AbortSignal
): Promise<TeamCatalogChat> {
  const value = assertExactInput(input, [
    "nodeId",
    "repositoryOverride",
    "title",
    "requestId",
    "visibility",
  ])
  if (
    !isPositiveId(value.nodeId) ||
    !(value.title === null || typeof value.title === "string") ||
    typeof value.requestId !== "string" ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(
      value.requestId
    ) ||
    !isVisibility(value.visibility)
  )
    invalidRequest()
  assertRepositoryInput(value.repositoryOverride, true)
  return parseTeamChat(
    await json("/api/team/chats", mutationInit(input, csrf), signal)
  )
}

export async function changeTeamChatVisibility(
  id: number,
  input: ChangeTeamChatVisibilityInput,
  csrf: string,
  signal?: AbortSignal
): Promise<TeamCatalogChat> {
  if (!isPositiveId(id)) invalidRequest()
  const value = assertExactInput(input, ["expectedRevision", "visibility"])
  if (!isRevision(value.expectedRevision) || !isVisibility(value.visibility))
    invalidRequest()
  return parseTeamChat(
    await json(
      `/api/team/chats/${id}/visibility`,
      mutationInit(input, csrf, "PATCH"),
      signal
    )
  )
}
