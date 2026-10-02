/**
 * "Test knowledge": an admin-only question against the saved wiki.
 * Contract: `.context/org-knowledge/implementation/test-knowledge-contract.md`.
 *
 * Nothing here is stored anywhere. The server keeps a running ask in memory
 * for at most 15 minutes after it finishes; the follow-up context lives in the
 * component that sent it and is gone when that unmounts. An ask reads only the
 * saved pages the admin can read right now, and never writes to the wiki.
 *
 * Parsed strictly: an answer is accepted only in the one state that may carry
 * it, so a failed or revoked ask can never be rendered with text.
 */
import { NativeClientError, nativeMutation, nativeRequest } from "./neev-client"
import { prepareKnowledgeMarkdown } from "./neev-knowledge"

export const ASK_STATES = [
  "running",
  "answered",
  "insufficient",
  "failed",
  "canceled",
] as const
export type KnowledgeAskState = (typeof ASK_STATES)[number]
export const ASK_INSUFFICIENT_REASONS = [
  "agent",
  "no_readable_pages",
  "ungrounded",
] as const

export type KnowledgeAskCitation = {
  key: string
  sourceId: string
  title: string
  kind: "document" | "chat"
  nodeId: number
  segment: number
  pageIds: string[]
}
export type KnowledgeAskPage = {
  id: string
  slug: string
  title: string
  revision: number
}
export type KnowledgeAsk = {
  id: string
  state: KnowledgeAskState
  provider: string
  model: string
  question: string
  createdAt: number
  finishedAt: number | null
  pagesConsidered: number
  answer: string | null
  citations: KnowledgeAskCitation[]
  pages: KnowledgeAskPage[]
  insufficient: {
    reason: (typeof ASK_INSUFFICIENT_REASONS)[number]
    note: string | null
  } | null
  error: {
    code: string
    category: string | null
    message: string | null
  } | null
}
export type AskTurn = { question: string; answer: string }
export type AskInput = {
  provider: string
  model: string
  question: string
  history: AskTurn[]
}

/** The contract's bounds on follow-up history. */
export const ASK_HISTORY_LIMITS = {
  turns: 6,
  question: 2000,
  answer: 6000,
  total: 24_000,
} as const
export const ASK_QUESTION_MAX = 2000
/** How often a running ask is polled (contract: about 1.5 s). */
export const ASK_POLL_MS = 1_500

function invalid(): never {
  throw new NativeClientError("invalid_response")
}
function invalidRequest(): never {
  throw new NativeClientError("invalid_request")
}
type Obj = Record<string, unknown>
function exact(value: unknown, keys: readonly string[]): Obj {
  if (!value || typeof value !== "object" || Array.isArray(value)) invalid()
  const object = value as Obj
  if (
    Object.keys(object).sort().join("\u0000") !==
    [...keys].sort().join("\u0000")
  )
    invalid()
  return object
}
function count(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0)
    invalid()
  return value
}
/**
 * Length in Unicode scalar values, as the server (Rust `chars()`) counts it —
 * not UTF-16 units, where an emoji counts twice.
 */
export function charCount(value: string) {
  return Array.from(value).length
}
/** The first `max` scalar values: never splits a surrogate pair. */
export function charSlice(value: string, max: number) {
  return charCount(value) <= max
    ? value
    : Array.from(value).slice(0, max).join("")
}

function text(value: unknown, max: number, allowEmpty = true): string {
  // UTF-16 length is an upper bound on the scalar count: count only if needed.
  if (
    typeof value !== "string" ||
    (value.length > max && charCount(value) > max)
  )
    invalid()
  if (!allowEmpty && !value) invalid()
  return value
}
function oneOf<T extends string>(value: unknown, allowed: readonly T[]): T {
  if (typeof value !== "string" || !allowed.includes(value as T)) invalid()
  return value as T
}
function list(value: unknown, max: number): unknown[] {
  if (!Array.isArray(value) || value.length > max) invalid()
  return value
}
const ID = /^[A-Za-z0-9_-]{1,128}$/
function id(value: unknown): string {
  const s = text(value, 128, false)
  if (!ID.test(s)) invalid()
  return s
}
const CODE = /^[a-z][a-z0-9_]{0,63}$/

function parseCitation(value: unknown): KnowledgeAskCitation {
  const o = exact(value, [
    "key",
    "sourceId",
    "title",
    "kind",
    "nodeId",
    "segment",
    "pageIds",
  ])
  const key = text(o.key, 16)
  if (!/^k-[0-9a-f]{12}$/.test(key)) invalid()
  const nodeId = count(o.nodeId)
  if (nodeId === 0) invalid()
  return {
    key,
    sourceId: id(o.sourceId),
    title: text(o.title, 500, false),
    kind: oneOf(o.kind, ["document", "chat"] as const),
    nodeId,
    segment: count(o.segment),
    pageIds: list(o.pageIds, 1000).map(id),
  }
}

function parsePage(value: unknown): KnowledgeAskPage {
  const o = exact(value, ["id", "slug", "title", "revision"])
  const slug = text(o.slug, 128, false)
  if (!/^[a-z0-9][a-z0-9_-]{0,127}$/.test(slug)) invalid()
  return {
    id: id(o.id),
    slug,
    title: text(o.title, 500, false),
    revision: count(o.revision),
  }
}

export function parseKnowledgeAsk(value: unknown): KnowledgeAsk {
  const o = exact(value, [
    "id",
    "state",
    "provider",
    "model",
    "question",
    "createdAt",
    "finishedAt",
    "pagesConsidered",
    "answer",
    "citations",
    "pages",
    "insufficient",
    "error",
  ])
  const state = oneOf(o.state, ASK_STATES)
  // 32 KiB of answer is at most 32768 UTF-16 units.
  const answer = o.answer === null ? null : text(o.answer, 32_768)
  const citations = list(o.citations, 1000).map(parseCitation)
  const pages = list(o.pages, 1000).map(parsePage)
  const insufficient =
    o.insufficient === null
      ? null
      : (() => {
          const i = exact(o.insufficient, ["reason", "note"])
          return {
            reason: oneOf(i.reason, ASK_INSUFFICIENT_REASONS),
            note: i.note === null ? null : text(i.note, 300),
          }
        })()
  const error =
    o.error === null
      ? null
      : (() => {
          const e = exact(o.error, ["code", "category", "message"])
          const code = text(e.code, 64, false)
          if (!CODE.test(code)) invalid()
          return {
            code,
            category: e.category === null ? null : text(e.category, 64),
            message: e.message === null ? null : text(e.message, 300),
          }
        })()
  // The states and what each may carry, exactly.
  const answeredShape =
    answer !== null && insufficient === null && error === null
  const bare = answer === null && citations.length === 0 && pages.length === 0
  if (state === "answered" && !answeredShape) invalid()
  if (state === "insufficient" && !(bare && insufficient && error === null))
    invalid()
  if (state === "failed" && !(bare && insufficient === null && error)) invalid()
  if (
    (state === "running" || state === "canceled") &&
    !(bare && insufficient === null && error === null)
  )
    invalid()
  const finishedAt = o.finishedAt === null ? null : count(o.finishedAt)
  if ((state === "running") !== (finishedAt === null)) invalid()
  return {
    id: id(o.id),
    state,
    provider: text(o.provider, 64, false),
    model: text(o.model, 80, false),
    question: text(o.question, ASK_QUESTION_MAX, false),
    createdAt: count(o.createdAt),
    finishedAt,
    pagesConsidered: count(o.pagesConsidered),
    answer,
    citations,
    pages,
    insufficient,
    error,
  }
}

const BASE = "/api/neev/knowledge/ask"

export async function startKnowledgeAsk(
  input: AskInput,
  csrf: string,
  signal?: AbortSignal
) {
  const question = input.question.trim()
  if (!question || charCount(question) > ASK_QUESTION_MAX) invalidRequest()
  if (
    !input.provider ||
    !/^[A-Za-z0-9._][A-Za-z0-9._-]{0,79}$/.test(input.model)
  )
    invalidRequest()
  return parseKnowledgeAsk(
    await nativeMutation(
      BASE,
      {
        provider: input.provider,
        model: input.model,
        question,
        history: input.history,
      },
      csrf,
      signal
    )
  )
}
export async function loadKnowledgeAsk(askId: string, signal?: AbortSignal) {
  if (!ID.test(askId)) invalidRequest()
  return parseKnowledgeAsk(
    await nativeRequest(`${BASE}/${encodeURIComponent(askId)}`, {}, signal)
  )
}
export async function stopKnowledgeAsk(
  askId: string,
  csrf: string,
  signal?: AbortSignal
) {
  if (!ID.test(askId)) invalidRequest()
  return parseKnowledgeAsk(
    await nativeMutation(
      `${BASE}/${encodeURIComponent(askId)}/stop`,
      {},
      csrf,
      signal
    )
  )
}

/**
 * Follow-up context for the next question, within the contract's bounds.
 *
 * Only answered and insufficient turns, with the server's own text (the
 * answer, or the insufficient note). Newest kept first when something has to
 * go: at most 6 turns, each answer cut to 6000 characters, 24000 in all —
 * characters as the server counts them (Unicode scalar values).
 */
export function askHistory(turns: readonly KnowledgeAsk[]): AskTurn[] {
  const usable = turns
    .filter(
      (turn) => turn.state === "answered" || turn.state === "insufficient"
    )
    .map((turn) => ({
      question: charSlice(turn.question, ASK_HISTORY_LIMITS.question),
      answer: charSlice(
        turn.state === "answered"
          ? (turn.answer ?? "")
          : (turn.insufficient?.note ?? ""),
        ASK_HISTORY_LIMITS.answer
      ),
    }))
    .slice(-ASK_HISTORY_LIMITS.turns)
  const size = (item: AskTurn) =>
    charCount(item.question) + charCount(item.answer)
  let total = usable.reduce((sum, item) => sum + size(item), 0)
  while (usable.length && total > ASK_HISTORY_LIMITS.total) {
    total -= size(usable.shift()!)
  }
  return usable
}

/** The answer, with citations and page links as in-app anchors. */
export function prepareAskMarkdown(ask: KnowledgeAsk, withheldLabel?: string) {
  return prepareKnowledgeMarkdown(
    ask.answer ?? "",
    ask.citations.map((item) => ({
      key: item.key,
      withheld: false as const,
      sourceId: item.sourceId,
      title: item.title,
      kind: item.kind,
      nodeId: item.nodeId,
      segment: item.segment,
    })),
    new Set(ask.pages.map((item) => item.slug)),
    withheldLabel
  )
}
