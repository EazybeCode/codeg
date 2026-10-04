import {
  NativeClientError,
  nativeMutation,
  nativeRequest,
  type NativeErrorKind,
} from "./neev-client"

export interface LibraryItem {
  id: string
  name: string
  description: string
  kind: "skill" | "mcp"
  enabled: boolean
  revision: string
  files: string[]
  setupNote: string | null
  integration?: LibraryIntegration | null
}
export interface LibraryIntegrationStatus {
  state: string
  checkedAt: string
  toolCount: number | null
  detail: string | null
  revision: string
}
export interface LibraryIntegration {
  transport: "remote" | "package"
  hosts: string[]
  secretNames: string[]
  source: string
  notes: string[]
  status: LibraryIntegrationStatus | null
}
export interface LibrarySkipped {
  name: string
  reason: string
}
const STATUS_MESSAGES: Record<string, string> = {
  ready: "Ready",
  auth_failed: "Credentials were rejected (401). Update the token.",
  forbidden: "Credentials were accepted but access is not allowed (403).",
  session_expired: "The service session expired. Run the test again.",
  connection_lost:
    "The connection dropped after the request was sent; the call may have run and was not retried.",
  egress_not_configured:
    "This deployment has no network path for integrations. Ask an operator.",
  invalid: "The integration's stored configuration is invalid. Re-import it.",
  busy: "The test runner is busy. Try again in a moment.",
  broker_unavailable: "The credential broker is unavailable; try again later.",
  not_found: "Endpoint not found (404). Check the server URL.",
  egress_denied: "Network policy blocked this host.",
  unreachable: "Could not reach the service.",
  upstream_error: "The service returned an error. Try again later.",
  protocol_error: "The service did not respond as an MCP server.",
  package_unavailable: "Package is not installed in this deployment.",
  vault_unavailable: "Stored credentials cannot be decrypted; re-enter them.",
  runtime_unavailable: "Agent runtime is unavailable; try again later.",
}
export function integrationStatusMessage(
  status: LibraryIntegrationStatus | null | undefined
) {
  if (!status) return "Not tested yet"
  return STATUS_MESSAGES[status.state] ?? status.state
}
const SKIP_MESSAGES: Record<string, string> = {
  unsupported_package: "This package is not supported.",
  unresolved_placeholder:
    "It references a placeholder value that could not be resolved.",
  invalid_url: "The server URL is not valid or not allowed.",
  invalid_name: "The server name is not valid.",
  duplicate_name: "A server with this name already exists.",
  invalid_secret: "A credential name or value is not valid.",
  invalid_config: "The configuration is not valid.",
  unsupported_field:
    "It uses a setting that shared integrations do not support.",
  unsupported_env:
    "It uses an environment setting that shared integrations do not support.",
}
export function skippedReasonMessage(reason: string) {
  return SKIP_MESSAGES[reason] ?? reason
}
function strings(v: unknown): v is string[] {
  return Array.isArray(v) && v.every((x) => typeof x === "string")
}
function parseIntegration(value: unknown): LibraryIntegration | null {
  if (value === undefined || value === null) return null
  if (typeof value !== "object" || Array.isArray(value)) return invalid()
  const o = value as Record<string, unknown>
  if (
    (o.transport !== "remote" && o.transport !== "package") ||
    !strings(o.hosts) ||
    !strings(o.secretNames) ||
    typeof o.source !== "string" ||
    !strings(o.notes)
  )
    return invalid()
  let status: LibraryIntegrationStatus | null = null
  if (o.status !== undefined && o.status !== null) {
    if (typeof o.status !== "object" || Array.isArray(o.status))
      return invalid()
    const s = o.status as Record<string, unknown>
    if (
      typeof s.state !== "string" ||
      !s.state ||
      typeof s.checkedAt !== "string" ||
      !/^\d{1,20}$/.test(s.checkedAt) ||
      !(
        s.toolCount === null ||
        s.toolCount === undefined ||
        (typeof s.toolCount === "number" && Number.isInteger(s.toolCount))
      ) ||
      !(
        s.detail === null ||
        s.detail === undefined ||
        typeof s.detail === "string"
      ) ||
      typeof s.revision !== "string" ||
      !/^[1-9]\d{0,18}$/.test(s.revision)
    )
      return invalid()
    status = {
      state: s.state,
      checkedAt: s.checkedAt,
      toolCount: s.toolCount ?? null,
      detail: s.detail ?? null,
      revision: s.revision,
    }
  }
  return {
    transport: o.transport,
    hosts: o.hosts,
    secretNames: o.secretNames,
    source: o.source,
    notes: o.notes,
    status,
  }
}
function invalid(): never {
  throw new NativeClientError("invalid_response")
}
export function parseLibraryItem(value: unknown): LibraryItem {
  if (!value || typeof value !== "object") return invalid()
  const o = value as Record<string, unknown>
  if (
    typeof o.id !== "string" ||
    !/^(?:[a-f0-9]{32}|[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12})$/i.test(
      o.id
    ) ||
    typeof o.name !== "string" ||
    !o.name ||
    typeof o.description !== "string" ||
    (o.kind !== "skill" && o.kind !== "mcp") ||
    typeof o.enabled !== "boolean" ||
    typeof o.revision !== "string" ||
    !/^[1-9]\d{0,18}$/.test(o.revision) ||
    !Array.isArray(o.files) ||
    !o.files.every((f) => typeof f === "string") ||
    !(o.setupNote === null || typeof o.setupNote === "string")
  )
    return invalid()
  const integration = parseIntegration(o.integration)
  return {
    id: o.id,
    name: o.name,
    description: o.description,
    kind: o.kind,
    enabled: o.enabled,
    revision: o.revision,
    files: o.files,
    setupNote: o.setupNote,
    ...(integration ? { integration } : {}),
  }
}
export function parseLibraryItems(value: unknown): LibraryItem[] {
  if (
    !value ||
    typeof value !== "object" ||
    !("items" in value) ||
    !Array.isArray(value.items)
  )
    return invalid()
  return value.items.map(parseLibraryItem)
}
export async function loadLibrary(signal?: AbortSignal) {
  return parseLibraryItems(await nativeRequest("/api/neev/library", {}, signal))
}
function fileBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onerror = () => reject(new NativeClientError("invalid_request"))
    reader.onload = () => {
      const value = reader.result
      if (typeof value !== "string" || !value.includes(","))
        reject(new NativeClientError("invalid_request"))
      else resolve(value.slice(value.indexOf(",") + 1))
    }
    reader.readAsDataURL(file)
  })
}
const IMPORT_KINDS: Record<number, NativeErrorKind> = {
  400: "invalid_request",
  401: "unauthorized",
  403: "forbidden",
  404: "unavailable",
  409: "conflict",
  429: "limited",
  503: "unavailable",
}
// Import is the one call whose non-2xx body matters: a 422 carries the
// skipped-server reasons, which nativeRequest would reduce to a bare status.
async function importRequest(
  body: unknown,
  csrf: string,
  signal?: AbortSignal
): Promise<unknown> {
  if (!csrf) throw new NativeClientError("invalid_request")
  if (signal?.aborted) throw new NativeClientError("aborted")
  let response: Response
  try {
    response = await fetch("/api/neev/library/import", {
      method: "POST",
      headers: { "Content-Type": "application/json", "x-neev-csrf": csrf },
      body: JSON.stringify(body),
      credentials: "same-origin",
      cache: "no-store",
      referrerPolicy: "no-referrer",
      redirect: "error",
      signal,
    })
  } catch {
    throw new NativeClientError(signal?.aborted ? "aborted" : "unavailable")
  }
  const parsed = await response.json().catch(() => null)
  if (response.status === 422) {
    // Nothing stored; the body says why. Anything else stays an error.
    if (
      parsed &&
      Array.isArray(parsed.items) &&
      parsed.items.length === 0 &&
      Array.isArray(parsed.skipped) &&
      parsed.skipped.length > 0
    )
      return parsed
    throw new NativeClientError("invalid_request", 422)
  }
  if (!response.ok)
    throw new NativeClientError(
      IMPORT_KINDS[response.status] ?? "unavailable",
      response.status
    )
  if (!parsed) return invalid()
  return parsed
}
/** GitHub's official remote MCP server (token as a Bearer header). */
export const GITHUB_MCP_URL = "https://api.githubcopilot.com/mcp/"
const MAX_GITHUB_TOKEN_CHARS = 8000

/** The trimmed token if it is one printable-ASCII word, else null. Nothing
 * about the token's permissions is assumed: the admin's Test tells. */
export function validGithubToken(value: string): string | null {
  const token = value.trim()
  if (!token || token.length > MAX_GITHUB_TOKEN_CHARS) return null
  return /^[\x21-\x7e]+$/.test(token) ? token : null
}

/** An MCP JSON import for GitHub's remote server. It goes through the same
 * admin import as any file: sealed server-side, never listed or returned. */
export function githubConnectionFile(token: string): File {
  const config = {
    mcpServers: {
      github: {
        type: "http",
        url: GITHUB_MCP_URL,
        headers: { Authorization: `Bearer ${token}` },
      },
    },
  }
  return new File([JSON.stringify(config)], "github.json", {
    type: "application/json",
  })
}

export async function importLibrary(
  file: File,
  csrf: string,
  signal?: AbortSignal
) {
  const data = await fileBase64(file)
  const body = await importRequest({ fileName: file.name, data }, csrf, signal)
  const items = parseLibraryItems(body)
  const raw = (body as { skipped?: unknown }).skipped
  if (raw === undefined || raw === null) return { items, skipped: [] }
  if (!Array.isArray(raw)) return invalid()
  const skipped = raw.map((entry): LibrarySkipped => {
    if (
      !entry ||
      typeof entry !== "object" ||
      typeof (entry as LibrarySkipped).name !== "string" ||
      typeof (entry as LibrarySkipped).reason !== "string"
    )
      return invalid()
    const e = entry as LibrarySkipped
    return { name: e.name, reason: e.reason }
  })
  return { items, skipped }
}
export async function updateLibrarySecrets(
  item: LibraryItem,
  env: Record<string, string>,
  headers: Record<string, string>,
  csrf: string,
  signal?: AbortSignal
) {
  return parseLibraryItem(
    await nativeMutation(
      `/api/neev/library/${item.id}/secrets`,
      { expectedRevision: item.revision, env, headers },
      csrf,
      signal,
      "PUT"
    )
  )
}
export async function testLibraryItem(
  item: LibraryItem,
  csrf: string,
  signal?: AbortSignal
) {
  return parseLibraryItem(
    await nativeMutation(
      `/api/neev/library/${item.id}/test`,
      { expectedRevision: item.revision },
      csrf,
      signal
    )
  )
}
export async function enableLibraryItem(
  item: LibraryItem,
  enabled: boolean,
  csrf: string,
  signal?: AbortSignal
) {
  return parseLibraryItem(
    await nativeMutation(
      `/api/neev/library/${item.id}`,
      { expectedRevision: item.revision, enabled },
      csrf,
      signal,
      "PATCH"
    )
  )
}
export async function deleteLibraryItem(
  item: LibraryItem,
  csrf: string,
  signal?: AbortSignal
) {
  await nativeMutation(
    `/api/neev/library/${item.id}`,
    { expectedRevision: item.revision },
    csrf,
    signal,
    "DELETE"
  )
}
