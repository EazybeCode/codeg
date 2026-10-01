/**
 * Org knowledge (saved LLM wiki), admin-only HTTP client. Plan Amendment 1, A1.4.
 *
 * Saved only: nothing here publishes to members or feeds agents. Every
 * response is parsed strictly, and a WITHHELD page or citation is refused if it
 * carries anything derived from its content (slug, title, body) — the server
 * must keep those opaque (spec A3), and a client that rendered them anyway
 * would leak exactly what withholding exists to hide.
 */
import { NativeClientError, nativeMutation, nativeRequest } from "./neev-client"

export type KnowledgeSettings = { captureEnabled: boolean; revision: number }

export const KNOWLEDGE_JOB_STATES = [
  "running",
  "paused",
  "completed",
  "completed_with_gaps",
  "stopped",
] as const
export type KnowledgeJobState = (typeof KNOWLEDGE_JOB_STATES)[number]
export type KnowledgeMode = "incremental" | "rebuild"

export type KnowledgeJob = {
  id: string
  mode: KnowledgeMode
  state: KnowledgeJobState
  provider: string
  model: string
  /** A snake_case code; known ones are translated, others shown generically. */
  pauseReason: string | null
  waitReason: "slot" | null
  sessionId: string | null
  batchesDone: number
  unitsRemaining: number
  startedBy: number
  createdAt: number
  updatedAt: number
}

export type KnowledgeStatus = {
  captureEnabled: boolean
  eligible: number
  captured: number
  /** Eligible sources whose current snapshots are ALL cited. */
  represented: number
  stale: number
  pages: number
  withheldPages: number
  sourceProblems: {
    tooLarge: number
    engineUnavailable: number
    failed: number
  }
  coverage: {
    units: number
    cited: number
    skippedByAgent: number
    uncovered: number
    pending: number
  }
  captureBlocked: "quota_exceeded" | null
  bytesUsed: number
  job: KnowledgeJob | null
}

export type KnowledgeSource = {
  id: string
  title: string
  kind: "document" | "chat"
  engine: boolean
  captureState: "ok" | "too_large" | "engine_unavailable" | "failed" | "pending"
  represented: boolean
  stale: boolean
}

export type KnowledgePageSummary =
  | {
      id: string
      status: "readable"
      slug: string
      title: string
      revision: number
      stale: boolean
      lint: string[]
      updatedAt: number
    }
  | {
      id: string
      status: "withheld"
      revision: number
      unreadableSources: number
      updatedAt: number
    }

export type KnowledgeCitation =
  | {
      key: string
      withheld: false
      sourceId: string
      title: string
      kind: "document" | "chat"
      nodeId: number
      segment: number
    }
  | { key: string; withheld: true }

export type KnowledgePage =
  | {
      id: string
      slug: string
      title: string
      revision: number
      status: "readable"
      markdown: string
      citations: KnowledgeCitation[]
      stale: boolean
      lint: string[]
    }
  | {
      id: string
      revision: number
      status: "withheld"
      unreadableSources: number
    }

export type KnowledgeLogEntry = {
  seq: number
  at: number
  event: string
  actorId: number | null
  pageId: string | null
  /** Null unless that page's head is readable. */
  pageTitle: string | null
  detail: string
}

/** Units per compile batch (MAX_BATCH_UNITS). */
export const KNOWLEDGE_BATCH_UNITS = 12
export const KNOWLEDGE_PAGE_SIZE = 100

// ---------------------------------------------------------------------------
// Strict parsing.

function invalid(): never {
  throw new NativeClientError("invalid_response")
}
function invalidRequest(): never {
  throw new NativeClientError("invalid_request")
}
type Obj = Record<string, unknown>
function record(value: unknown): Obj {
  if (!value || typeof value !== "object" || Array.isArray(value)) invalid()
  return value as Obj
}
/** Exactly these keys: an unexpected field is refused, never ignored. */
function exact(value: unknown, keys: readonly string[]): Obj {
  const object = record(value)
  const found = Object.keys(object).sort().join("\u0000")
  if (found !== [...keys].sort().join("\u0000")) invalid()
  return object
}
function count(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0)
    invalid()
  return value
}
function bool(value: unknown): boolean {
  if (typeof value !== "boolean") invalid()
  return value
}
function text(value: unknown, max = 10_000): string {
  if (typeof value !== "string" || value.length > max) invalid()
  return value
}
function nonEmpty(value: unknown, max = 500): string {
  const s = text(value, max)
  if (!s) invalid()
  return s
}
const ID = /^[A-Za-z0-9_-]{1,128}$/
function id(value: unknown): string {
  const s = text(value, 128)
  if (!ID.test(s)) invalid()
  return s
}
const SLUG = /^[a-z0-9][a-z0-9_-]{0,127}$/
function slug(value: unknown): string {
  const s = text(value, 128)
  if (!SLUG.test(s)) invalid()
  return s
}
const CODE = /^[a-z][a-z0-9_]{0,63}$/
function nullable<T>(value: unknown, parse: (v: unknown) => T): T | null {
  return value === null ? null : parse(value)
}
function oneOf<T extends string>(value: unknown, allowed: readonly T[]): T {
  if (typeof value !== "string" || !allowed.includes(value as T)) invalid()
  return value as T
}
function strings(value: unknown): string[] {
  if (!Array.isArray(value) || value.length > 100) invalid()
  return value.map((item) => nonEmpty(item, 200))
}
function list(value: unknown, max = 1000): unknown[] {
  if (!Array.isArray(value) || value.length > max) invalid()
  return value
}
/** Cite keys are 12 lowercase hex; the `k-` form is canonical here. */
function citeKey(value: unknown): string {
  const s = text(value, 16)
  const match = /^(?:k-)?([0-9a-f]{12})$/.exec(s)
  if (!match) invalid()
  return `k-${match[1]}`
}

export function parseKnowledgeSettings(value: unknown): KnowledgeSettings {
  const o = exact(value, ["captureEnabled", "revision"])
  return { captureEnabled: bool(o.captureEnabled), revision: count(o.revision) }
}

export function parseKnowledgeJob(value: unknown): KnowledgeJob {
  const o = exact(value, [
    "id",
    "mode",
    "state",
    "provider",
    "model",
    "pauseReason",
    "waitReason",
    "sessionId",
    "batchesDone",
    "unitsRemaining",
    "startedBy",
    "createdAt",
    "updatedAt",
  ])
  return {
    id: id(o.id),
    mode: oneOf(o.mode, ["incremental", "rebuild"] as const),
    state: oneOf(o.state, KNOWLEDGE_JOB_STATES),
    provider: nonEmpty(o.provider, 64),
    model: nonEmpty(o.model, 200),
    pauseReason: nullable(o.pauseReason, (v) => {
      const s = text(v, 64)
      if (!CODE.test(s)) invalid()
      return s
    }),
    waitReason: nullable(o.waitReason, (v) => oneOf(v, ["slot"] as const)),
    sessionId: nullable(o.sessionId, id),
    batchesDone: count(o.batchesDone),
    unitsRemaining: count(o.unitsRemaining),
    startedBy: count(o.startedBy),
    createdAt: count(o.createdAt),
    updatedAt: count(o.updatedAt),
  }
}

export function parseKnowledgeStatus(value: unknown): KnowledgeStatus {
  const o = exact(value, [
    "captureEnabled",
    "eligible",
    "captured",
    "represented",
    "stale",
    "pages",
    "withheldPages",
    "sourceProblems",
    "coverage",
    "captureBlocked",
    "bytesUsed",
    "job",
  ])
  const p = exact(o.sourceProblems, ["tooLarge", "engineUnavailable", "failed"])
  const c = exact(o.coverage, [
    "units",
    "cited",
    "skippedByAgent",
    "uncovered",
    "pending",
  ])
  return {
    captureEnabled: bool(o.captureEnabled),
    eligible: count(o.eligible),
    captured: count(o.captured),
    represented: count(o.represented),
    stale: count(o.stale),
    pages: count(o.pages),
    withheldPages: count(o.withheldPages),
    sourceProblems: {
      tooLarge: count(p.tooLarge),
      engineUnavailable: count(p.engineUnavailable),
      failed: count(p.failed),
    },
    coverage: {
      units: count(c.units),
      cited: count(c.cited),
      skippedByAgent: count(c.skippedByAgent),
      uncovered: count(c.uncovered),
      pending: count(c.pending),
    },
    captureBlocked: nullable(o.captureBlocked, (v) =>
      oneOf(v, ["quota_exceeded"] as const)
    ),
    bytesUsed: count(o.bytesUsed),
    job: nullable(o.job, parseKnowledgeJob),
  }
}

export function parseKnowledgeSources(value: unknown): {
  items: KnowledgeSource[]
  nextCursor: string | null
} {
  const o = exact(value, ["items", "nextCursor"])
  return {
    items: list(o.items, KNOWLEDGE_PAGE_SIZE).map((item) => {
      const s = exact(item, [
        "id",
        "title",
        "kind",
        "engine",
        "captureState",
        "represented",
        "stale",
      ])
      return {
        id: id(s.id),
        title: text(s.title, 500),
        kind: oneOf(s.kind, ["document", "chat"] as const),
        engine: bool(s.engine),
        captureState: oneOf(s.captureState, [
          "ok",
          "too_large",
          "engine_unavailable",
          "failed",
          "pending",
        ] as const),
        represented: bool(s.represented),
        stale: bool(s.stale),
      }
    }),
    nextCursor: nullable(o.nextCursor, id),
  }
}

function parseSummary(value: unknown): KnowledgePageSummary {
  const status = record(value).status
  if (status === "withheld") {
    // Opaque: id, revision and a count. Nothing derived from content.
    const o = exact(value, [
      "id",
      "status",
      "revision",
      "unreadableSources",
      "updatedAt",
    ])
    return {
      id: id(o.id),
      status: "withheld",
      revision: count(o.revision),
      unreadableSources: count(o.unreadableSources),
      updatedAt: count(o.updatedAt),
    }
  }
  if (status !== "readable") invalid()
  const o = exact(value, [
    "id",
    "status",
    "slug",
    "title",
    "revision",
    "stale",
    "lint",
    "updatedAt",
  ])
  return {
    id: id(o.id),
    status: "readable",
    slug: slug(o.slug),
    title: nonEmpty(o.title),
    revision: count(o.revision),
    stale: bool(o.stale),
    lint: strings(o.lint),
    updatedAt: count(o.updatedAt),
  }
}

export function parseKnowledgePages(value: unknown): KnowledgePageSummary[] {
  return list(value).map(parseSummary)
}

function parseCitation(value: unknown): KnowledgeCitation {
  const withheld = record(value).withheld
  if (withheld === true) {
    // A citation whose source is no longer readable names nothing.
    const o = exact(value, ["key", "withheld"])
    return { key: citeKey(o.key), withheld: true }
  }
  if (withheld !== false) invalid()
  const o = exact(value, [
    "key",
    "withheld",
    "sourceId",
    "title",
    "kind",
    "nodeId",
    "segment",
  ])
  const nodeId = count(o.nodeId)
  if (nodeId === 0) invalid()
  return {
    key: citeKey(o.key),
    withheld: false,
    sourceId: id(o.sourceId),
    title: nonEmpty(o.title),
    kind: oneOf(o.kind, ["document", "chat"] as const),
    nodeId,
    segment: count(o.segment),
  }
}

export function parseKnowledgePage(value: unknown): KnowledgePage {
  const status = record(value).status
  if (status === "withheld") {
    const o = exact(value, ["id", "revision", "status", "unreadableSources"])
    return {
      id: id(o.id),
      revision: count(o.revision),
      status: "withheld",
      unreadableSources: count(o.unreadableSources),
    }
  }
  if (status !== "readable") invalid()
  const o = exact(value, [
    "id",
    "slug",
    "title",
    "revision",
    "status",
    "markdown",
    "citations",
    "stale",
    "lint",
  ])
  return {
    id: id(o.id),
    slug: slug(o.slug),
    title: nonEmpty(o.title),
    revision: count(o.revision),
    status: "readable",
    // MAX_PAGE_BYTES is 64 KiB; characters can be fewer than bytes, never more.
    markdown: text(o.markdown, 65_536),
    citations: list(o.citations).map(parseCitation),
    stale: bool(o.stale),
    lint: strings(o.lint),
  }
}

export function parseKnowledgeLog(value: unknown): KnowledgeLogEntry[] {
  return list(value, KNOWLEDGE_PAGE_SIZE).map((item) => {
    const o = exact(item, [
      "seq",
      "at",
      "event",
      "actorId",
      "pageId",
      "pageTitle",
      "detail",
    ])
    return {
      seq: count(o.seq),
      at: count(o.at),
      event: nonEmpty(o.event, 64),
      actorId: nullable(o.actorId, count),
      pageId: nullable(o.pageId, id),
      pageTitle: nullable(o.pageTitle, (v) => nonEmpty(v)),
      detail: text(o.detail, 2000),
    }
  })
}

// ---------------------------------------------------------------------------
// Requests.

const BASE = "/api/neev/knowledge"

export async function loadKnowledgeStatus(signal?: AbortSignal) {
  return parseKnowledgeStatus(await nativeRequest(`${BASE}/status`, {}, signal))
}
export async function loadKnowledgeSettings(signal?: AbortSignal) {
  return parseKnowledgeSettings(
    await nativeRequest(`${BASE}/settings`, {}, signal)
  )
}
export async function saveKnowledgeSettings(
  captureEnabled: boolean,
  expectedRevision: number,
  csrf: string,
  signal?: AbortSignal
) {
  return parseKnowledgeSettings(
    await nativeMutation(
      `${BASE}/settings`,
      { captureEnabled, expectedRevision },
      csrf,
      signal,
      "PUT"
    )
  )
}
export async function loadKnowledgeSources(
  cursor: string | null = null,
  signal?: AbortSignal
) {
  if (cursor !== null && !ID.test(cursor)) invalidRequest()
  return parseKnowledgeSources(
    await nativeRequest(
      `${BASE}/sources${cursor === null ? "" : `?cursor=${encodeURIComponent(cursor)}`}`,
      {},
      signal
    )
  )
}
export async function startKnowledgeJob(
  mode: KnowledgeMode,
  provider: string,
  model: string,
  csrf: string,
  signal?: AbortSignal
) {
  if (!provider || !model) invalidRequest()
  return parseKnowledgeJob(
    await nativeMutation(
      `${BASE}/jobs`,
      { mode, provider, model },
      csrf,
      signal
    )
  )
}
export async function controlKnowledgeJob(
  jobId: string,
  action: "pause" | "resume" | "stop",
  csrf: string,
  signal?: AbortSignal
) {
  if (!ID.test(jobId)) invalidRequest()
  return parseKnowledgeJob(
    await nativeMutation(
      `${BASE}/jobs/${encodeURIComponent(jobId)}/${action}`,
      {},
      csrf,
      signal
    )
  )
}
/**
 * "Retry uncovered": uncovered units go back to pending. The response body is
 * not specified by the contract and is not read; callers re-read the status.
 */
export async function resetKnowledgeUncovered(
  csrf: string,
  signal?: AbortSignal
): Promise<void> {
  await nativeMutation(`${BASE}/uncovered/reset`, {}, csrf, signal)
}
export async function loadKnowledgePages(signal?: AbortSignal) {
  return parseKnowledgePages(await nativeRequest(`${BASE}/pages`, {}, signal))
}
export async function loadKnowledgePage(pageId: string, signal?: AbortSignal) {
  // An opaque id, never a path: anything else is refused before it is sent.
  if (!ID.test(pageId)) invalidRequest()
  return parseKnowledgePage(
    await nativeRequest(
      `${BASE}/pages/${encodeURIComponent(pageId)}`,
      {},
      signal
    )
  )
}
export async function loadKnowledgeLog(
  before: number | null = null,
  signal?: AbortSignal
) {
  return parseKnowledgeLog(
    await nativeRequest(
      `${BASE}/log${before === null ? "" : `?before=${before}`}`,
      {},
      signal
    )
  )
}
export function knowledgeExportUrl() {
  return `${BASE}/export`
}

/**
 * Fetch the export, so a refusal is reported instead of being saved as a file.
 *
 * A plain `<a download>` writes whatever the server answers — a 403 or 500
 * body included — to disk under the export's name, and says nothing.
 */
export async function downloadKnowledgeExport(
  signal?: AbortSignal
): Promise<{ blob: Blob; filename: string }> {
  let response: Response
  try {
    response = await fetch(knowledgeExportUrl(), {
      credentials: "same-origin",
      signal,
    })
  } catch {
    throw new NativeClientError(signal?.aborted ? "aborted" : "unavailable")
  }
  if (!response.ok)
    throw new NativeClientError(
      response.status === 401
        ? "unauthorized"
        : response.status === 403
          ? "forbidden"
          : "unavailable",
      response.status
    )
  const named = /filename="?([A-Za-z0-9._-]{1,200})"?/.exec(
    response.headers.get("Content-Disposition") ?? ""
  )
  return {
    blob: await response.blob(),
    filename: named?.[1] ?? "knowledge-export.zip",
  }
}

// ---------------------------------------------------------------------------
// Presentation helpers.

/**
 * Providers the knowledge runner compiles with (spec §5/A1: Claude and Codex).
 * Other connected agents are not offered until Task 9 says it accepts them.
 */
export const KNOWLEDGE_PROVIDERS: readonly string[] = ["claude", "codex"]

/**
 * Whether the wiki is complete — only when NOTHING is outstanding.
 *
 * `represented` alone is not enough: a source with no snapshot yet can count
 * as "all snapshots cited" vacuously, so capture, coverage, source problems
 * and quota all have to be clear as well.
 */
export function knowledgeComplete(status: KnowledgeStatus): boolean {
  const problems = status.sourceProblems
  return (
    status.eligible > 0 &&
    status.represented === status.eligible &&
    status.captured === status.eligible &&
    status.coverage.pending === 0 &&
    status.coverage.uncovered === 0 &&
    problems.tooLarge + problems.engineUnavailable + problems.failed === 0 &&
    status.captureBlocked === null
  )
}

/** Batches needed for `units` remaining units, at most 12 units per batch. */
export function batchesFor(units: number) {
  return Math.ceil(units / KNOWLEDGE_BATCH_UNITS)
}

export const CITE_ANCHOR = "#neev-cite-"
export const PAGE_ANCHOR = "#neev-page-"

/** Link text cannot contain anything that would end or restructure the link. */
function linkText(value: string) {
  return value.replace(/[[\]\\\r\n]/g, " ").trim() || "?"
}

/**
 * Prepare a stored page body for `NativeMarkdown`.
 *
 * `[^k-…]` becomes an in-page anchor labelled with the cited source's title,
 * and `[[slug]]` an anchor to another READABLE page. The preview handles those
 * anchors itself; `NativeMarkdown` only ever sees `#…` hrefs it already
 * allows. A citation whose source is no longer readable — or one this page
 * does not resolve — becomes a plain `[withheld]` that names nothing, and a
 * link to a page that is not readable is just its text.
 */
export function prepareKnowledgeMarkdown(
  markdown: string,
  citations: readonly KnowledgeCitation[],
  readableSlugs: ReadonlySet<string>,
  withheldLabel = "withheld"
): string {
  const byKey = new Map(citations.map((item) => [item.key, item]))
  return markdown
    .replace(/\[\^(k-[0-9a-f]{12}|u[0-9]{2})\]/g, (_all, key: string) => {
      const cite = byKey.get(key)
      // Escaped: a `[withheld]: url` definition in the body cannot link it.
      if (!cite || cite.withheld) return `\\[${linkText(withheldLabel)}\\]`
      return `[${linkText(cite.title)}](${CITE_ANCHOR}${key})`
    })
    .replace(/\[\[([a-z0-9][a-z0-9_-]{0,127})\]\]/g, (_all, target: string) =>
      readableSlugs.has(target)
        ? `[${target}](${PAGE_ANCHOR}${target})`
        : target
    )
}
