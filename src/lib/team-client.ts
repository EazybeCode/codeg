const TEAM_REQUEST_TIMEOUT_MS = 65_000
const MAX_I64_DECIMAL = "9223372036854775807"

export type BrowserServerMode = "operator" | "team"
export type TeamRole = "admin" | "member"

export interface TeamCapabilities {
  version: 1
  mode: BrowserServerMode
  runtimeEnabled: boolean
}

export interface GithubIdentity {
  githubUserId: string
  login: string
  name: string | null
  avatarUrl: string | null
}

export interface GithubOrganizationChoice {
  githubOrgId: string
  login: string
  avatarUrl: string | null
  role: TeamRole
}

export interface PendingChoices {
  identity: GithubIdentity
  organizations: GithubOrganizationChoice[]
  csrf: string
}

export interface TeamAccount {
  id: number
  login: string
  name: string | null
  avatarUrl: string | null
}

export type TeamOrganization = TeamAccount

export interface TeamSession {
  user: TeamAccount
  organization: TeamOrganization
  role: TeamRole
  scope: string
  csrf: string
}

export type TeamClientErrorKind =
  | "aborted"
  | "conflict"
  | "forbidden"
  | "invalid_request"
  | "invalid_response"
  | "request_failed"
  | "unauthorized"
  | "unavailable"

export class TeamClientError extends Error {
  readonly name = "TeamClientError"

  constructor(
    public readonly kind: TeamClientErrorKind,
    public readonly status?: number
  ) {
    super("Team request failed")
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string"
}

function isRole(value: unknown): value is TeamRole {
  return value === "admin" || value === "member"
}

export function isCanonicalGithubId(value: unknown): value is string {
  if (typeof value !== "string" || !/^[1-9]\d{0,18}$/.test(value)) {
    return false
  }
  return (
    value.length < MAX_I64_DECIMAL.length ||
    (value.length === MAX_I64_DECIMAL.length && value <= MAX_I64_DECIMAL)
  )
}

function isPositiveInternalId(value: unknown): value is number {
  return Number.isInteger(value) && typeof value === "number" && value > 0
}

function isAccount(value: unknown): value is TeamAccount {
  return (
    isRecord(value) &&
    isPositiveInternalId(value.id) &&
    typeof value.login === "string" &&
    isNullableString(value.name) &&
    isNullableString(value.avatarUrl)
  )
}

function isGithubIdentity(value: unknown): value is GithubIdentity {
  return (
    isRecord(value) &&
    isCanonicalGithubId(value.githubUserId) &&
    typeof value.login === "string" &&
    isNullableString(value.name) &&
    isNullableString(value.avatarUrl)
  )
}

function isOrganizationChoice(
  value: unknown
): value is GithubOrganizationChoice {
  return (
    isRecord(value) &&
    isCanonicalGithubId(value.githubOrgId) &&
    typeof value.login === "string" &&
    isNullableString(value.avatarUrl) &&
    isRole(value.role)
  )
}

function errorForStatus(status: number): TeamClientError {
  switch (status) {
    case 400:
      return new TeamClientError("invalid_request", status)
    case 401:
      return new TeamClientError("unauthorized", status)
    case 403:
      return new TeamClientError("forbidden", status)
    case 409:
      return new TeamClientError("conflict", status)
    case 429:
    case 503:
      return new TeamClientError("unavailable", status)
    default:
      return new TeamClientError("request_failed", status)
  }
}

async function request<T>(
  path: string,
  init: RequestInit = {},
  externalSignal: AbortSignal | undefined,
  consume: (response: Response) => Promise<T>
): Promise<T> {
  if (externalSignal?.aborted) {
    throw new TeamClientError("aborted")
  }
  const controller = new AbortController()
  const abort = () => controller.abort()
  externalSignal?.addEventListener("abort", abort, { once: true })
  const timeout = window.setTimeout(abort, TEAM_REQUEST_TIMEOUT_MS)
  try {
    const response = await fetch(path, {
      ...init,
      credentials: "same-origin",
      cache: "no-store",
      signal: controller.signal,
    })
    if (!response.ok) throw errorForStatus(response.status)
    return await consume(response)
  } catch (error) {
    if (controller.signal.aborted) {
      throw new TeamClientError(
        externalSignal?.aborted ? "aborted" : "unavailable"
      )
    }
    if (error instanceof TeamClientError) throw error
    throw new TeamClientError("request_failed")
  } finally {
    window.clearTimeout(timeout)
    externalSignal?.removeEventListener("abort", abort)
  }
}

async function responseJson(response: Response): Promise<unknown> {
  try {
    return await response.json()
  } catch {
    throw new TeamClientError("invalid_response", response.status)
  }
}

export async function loadCapabilities(
  signal?: AbortSignal
): Promise<TeamCapabilities> {
  const payload = await request(
    "/api/workos/capabilities",
    {},
    signal,
    responseJson
  )
  if (
    !isRecord(payload) ||
    payload.version !== 1 ||
    (payload.mode !== "operator" && payload.mode !== "team") ||
    typeof payload.runtimeEnabled !== "boolean"
  ) {
    throw new TeamClientError("invalid_response")
  }
  return payload as unknown as TeamCapabilities
}

export async function loadPendingChoices(
  signal?: AbortSignal
): Promise<PendingChoices> {
  const payload = await request(
    "/api/team/onboarding",
    {},
    signal,
    responseJson
  )
  if (
    !isRecord(payload) ||
    !isGithubIdentity(payload.identity) ||
    !Array.isArray(payload.organizations) ||
    !payload.organizations.every(isOrganizationChoice) ||
    typeof payload.csrf !== "string" ||
    payload.csrf.length === 0
  ) {
    throw new TeamClientError("invalid_response")
  }
  return payload as unknown as PendingChoices
}

export async function selectOrganization(
  githubOrgId: string,
  csrf: string,
  signal?: AbortSignal
): Promise<{ redirect: "/workspace" }> {
  if (!isCanonicalGithubId(githubOrgId) || !csrf) {
    throw new TeamClientError("invalid_request")
  }
  const payload = await request(
    "/api/team/onboarding/select",
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-WorkOS-CSRF": csrf,
      },
      body: JSON.stringify({ githubOrgId }),
    },
    signal,
    responseJson
  )
  if (!isRecord(payload) || payload.redirect !== "/workspace") {
    throw new TeamClientError("invalid_response")
  }
  return { redirect: "/workspace" }
}

export async function loadTeamSession(
  signal?: AbortSignal
): Promise<TeamSession> {
  const payload = await request("/api/team/session", {}, signal, responseJson)
  if (
    !isRecord(payload) ||
    !isAccount(payload.user) ||
    !isAccount(payload.organization) ||
    !isRole(payload.role) ||
    typeof payload.scope !== "string" ||
    payload.scope.length === 0 ||
    typeof payload.csrf !== "string" ||
    payload.csrf.length === 0
  ) {
    throw new TeamClientError("invalid_response")
  }
  return payload as unknown as TeamSession
}

export async function logoutTeamSession(
  csrf: string,
  signal?: AbortSignal
): Promise<void> {
  if (!csrf) throw new TeamClientError("invalid_request")
  await request(
    "/api/team/logout",
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-WorkOS-CSRF": csrf,
      },
      body: "{}",
    },
    signal,
    () => Promise.resolve()
  )
}
