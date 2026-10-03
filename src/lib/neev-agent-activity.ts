/**
 * A turn's tool activity, as the UI holds it.
 *
 * The shapes and their parsing live with the API types. What is here is only
 * what the VIEW needs: how items are merged across a replay, where they sit in
 * the prose, and how a collapsed row describes itself.
 */

import {
  sliceByScalars,
  type NeevActivityItem,
  type NeevSupervisionVerdict,
} from "./neev-agent-sessions-client"

/**
 * Merge new items into what we already have.
 *
 * `seq` is the identity, NOT the position, and last-write-wins under it is
 * load-bearing twice over. Reattaching to a live run replays the stored prefix
 * before continuing, so the same item arrives twice — append and the trace
 * doubles, upsert and it does not. And a tool call is announced the moment it
 * starts, then sent again under the same `seq` once its arguments are complete:
 * the later copy is the fuller one, and it replaces the placeholder in place
 * rather than appearing beneath it.
 */
export function mergeActivity(
  current: readonly NeevActivityItem[],
  incoming: readonly NeevActivityItem[]
): NeevActivityItem[] {
  if (incoming.length === 0) return current as NeevActivityItem[]
  const bySeq = new Map<number, NeevActivityItem>()
  for (const item of current) bySeq.set(item.seq, item)
  for (const item of incoming) bySeq.set(item.seq, item)
  return [...bySeq.values()].sort((left, right) => left.seq - right.seq)
}

/**
 * Split a reply around the activity that happened while it was being written.
 *
 * `offset` counts Unicode scalar values, which is what Rust's `chars()` yields
 * and NOT what JavaScript's `String.slice` counts. An emoji is one scalar on
 * the server and two UTF-16 units here, so slicing by the raw offset drifts one
 * character left per astral character in the text. Splitting the string into
 * scalars first is the whole fix.
 */
export function splitByActivity(
  text: string,
  items: readonly NeevActivityItem[]
): { text: string; items: NeevActivityItem[] }[] {
  const length = [...text].length
  const ordered = [...items].sort((left, right) => left.seq - right.seq)
  const segments: { text: string; items: NeevActivityItem[] }[] = []
  let cursor = 0
  let pending: NeevActivityItem[] = []
  const between = (from: number, to: number) =>
    sliceByScalars(text, to).slice(sliceByScalars(text, from).length)
  for (const item of ordered) {
    const at = Math.max(cursor, Math.min(item.offset, length))
    if (at > cursor) {
      segments.push({ text: between(cursor, at), items: pending })
      pending = []
      cursor = at
    }
    pending.push(item)
  }
  if (cursor < length || pending.length > 0)
    segments.push({ text: between(cursor, length), items: pending })
  return segments
}

/** A tool result, narrowed out of the item union. */
export type NeevToolResult = Extract<NeevActivityItem, { kind: "tool_result" }>

/**
 * Fold each tool result into the call it answers.
 *
 * One invocation is one row. The server records a call and its result as two
 * separate items — it must, because the result can be minutes later — but a
 * reader looking at a trace sees one thing happen: `Bash` ran, and it either
 * worked or it did not. Drawn as two rows that is the same fact twice, and the
 * second row has no name on it.
 *
 * Pairing is by `callId`, which is the server's own link, never by position.
 * Two things are deliberately NOT hidden:
 *  - a result whose `callId` matches no call in this trace (the type says it
 *    may match none), and
 *  - a SECOND result for a call that already has one.
 * Both keep their own row. Absorbing them would drop recorded output on the
 * floor to make the list shorter, which is the one thing a trace may not do.
 *
 * The result is attached at the CALL's position. That is where the invocation
 * happened, and it is why a running row can become a finished one in place
 * rather than a new row appearing further down.
 */
export function pairResults(items: readonly NeevActivityItem[]): {
  /** The result for a call, keyed by the CALL's `seq`. */
  resultOf: Map<number, NeevToolResult>
  /** `seq` of every result now drawn inside a call, and so not on its own. */
  absorbed: Set<number>
} {
  const resultOf = new Map<number, NeevToolResult>()
  const absorbed = new Set<number>()
  const callSeqById = new Map<string, number>()
  for (const item of [...items].sort((left, right) => left.seq - right.seq)) {
    if (item.kind === "tool_call") {
      // First call wins the id. Ids are the server's and are not reused within
      // a turn; if one ever were, the later call keeps its own row.
      if (!callSeqById.has(item.id)) callSeqById.set(item.id, item.seq)
      continue
    }
    if (item.kind !== "tool_result") continue
    const seq = callSeqById.get(item.callId)
    if (seq === undefined || resultOf.has(seq)) continue
    resultOf.set(seq, item)
    absorbed.add(item.seq)
  }
  return { resultOf, absorbed }
}

/** What a collapsed group says about itself, without opening it. */
export function summariseActivity(items: readonly NeevActivityItem[]): {
  calls: number
  results: number
  errors: number
  truncated: boolean
  unsupported: boolean
} {
  let calls = 0
  let results = 0
  let errors = 0
  let truncated = false
  let unsupported = false
  for (const item of items) {
    if (item.kind === "tool_call") calls += 1
    else if (item.kind === "tool_result") {
      results += 1
      if (item.isError) errors += 1
    } else if (item.state === "truncated") truncated = true
    else if (item.state === "unsupported") unsupported = true
  }
  return { calls, results, errors, truncated, unsupported }
}

/**
 * A one-line gist of a tool call, for the collapsed row.
 *
 * The input is a STRING of already-serialised JSON — a tool input streamed in
 * fragments is not always valid JSON by the time the block ends — so it is
 * parsed only inside a try, and shown as-is when that fails. Everything here is
 * model-authored text and is only ever rendered, never executed or re-sent.
 */
export function describeToolInput(input: string): string {
  let value: unknown
  try {
    value = JSON.parse(input)
  } catch {
    return oneLine(input)
  }
  if (!value || typeof value !== "object") return oneLine(input)
  const fields = value as Record<string, unknown>
  for (const key of [
    "command",
    "file_path",
    "path",
    "pattern",
    "url",
    "query",
    "prompt",
  ]) {
    const found = fields[key]
    if (typeof found === "string" && found.trim()) return oneLine(found)
  }
  return oneLine(input)
}

function oneLine(value: string): string {
  const flat = value.replace(/\s+/g, " ").trim()
  const characters = [...flat]
  return characters.length <= 80
    ? flat
    : `${characters.slice(0, 79).join("").trimEnd()}…`
}

/**
 * The statuses worth putting on screen.
 *
 * `model`, `stop` and `result` are the provider's own bookkeeping — the end of
 * a turn is already visible from the turn ending, and repeating it as a row
 * called `end_turn` or `success` is noise between the reader and the answer.
 * What survives is what changes the meaning of the trace: a cap was hit, or the
 * provider's stream is one we cannot read.
 */
export function isNoise(item: NeevActivityItem): boolean {
  return (
    item.kind === "status" &&
    (item.state === "model" ||
      item.state === "stop" ||
      item.state === "result" ||
      // The provider's failure is the turn's OUTCOME, shown once by the
      // failure card with its actions — not a second time as a trace row.
      item.state === "error" ||
      // Supervision steps have their own compact summary under the reply.
      isSupervisionState(item.state))
  )
}

function isSupervisionState(state: string): boolean {
  return (
    state === "plan_pending" ||
    state === "plan" ||
    state === "review_pending" ||
    state === "review"
  )
}

export interface NeevSupervisionStep {
  verdict: NeevSupervisionVerdict
  detail: string | null
  pending: boolean
}

/**
 * The supervision of one turn, from its trace: the latest plan and review
 * steps (each is revised in place, so the highest `seq` wins), and the phase a
 * still-running turn is in. `phase` is `null` when no supervisor is working —
 * which is also when steering can reach the agent.
 */
export function supervisionOf(items: readonly NeevActivityItem[]): {
  plan: NeevSupervisionStep | null
  review: NeevSupervisionStep | null
  phase: "planning" | "reviewing" | null
} {
  let plan: (NeevSupervisionStep & { seq: number }) | null = null
  let review: (NeevSupervisionStep & { seq: number }) | null = null
  for (const item of items) {
    if (item.kind !== "status" || !isSupervisionState(item.state)) continue
    const step = {
      seq: item.seq,
      verdict: item.verdict ?? "unavailable",
      detail: item.detail,
      pending: item.state.endsWith("_pending"),
    }
    if (item.state.startsWith("plan")) {
      if (!plan || step.seq >= plan.seq) plan = step
    } else if (!review || step.seq >= review.seq) review = step
  }
  const phase = review?.pending
    ? "reviewing"
    : plan?.pending && !review
      ? "planning"
      : null
  const strip = (step: typeof plan): NeevSupervisionStep | null =>
    step
      ? { verdict: step.verdict, detail: step.detail, pending: step.pending }
      : null
  return { plan: strip(plan), review: strip(review), phase }
}

/** Everything the reader should see, in order. */
export function visibleActivity(
  items: readonly NeevActivityItem[]
): NeevActivityItem[] {
  return items.filter((item) => !isNoise(item))
}

/**
 * What the agent is doing right now, if anything.
 *
 * The last tool call with no result yet: that is the one still running. A call
 * whose result has arrived is finished, however recent it is.
 */
export function runningTool(
  items: readonly NeevActivityItem[]
): { name: string; input: string } | null {
  const answered = new Set(
    items
      .filter((item) => item.kind === "tool_result")
      .map((item) => (item.kind === "tool_result" ? item.callId : ""))
  )
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index]
    if (item.kind !== "tool_call") continue
    if (answered.has(item.id)) return null
    return { name: item.name, input: item.input }
  }
  return null
}

/** Milliseconds since `startedAt`, never negative. */
export function elapsedMs(startedAt: number, now: number): number {
  return Math.max(0, now - startedAt)
}

/** One tick per tenth: the resolution the running clock is shown at. */
export const ELAPSED_TICK_MS = 100

/**
 * The seconds an elapsed reading is shown as.
 *
 * `precise` says whether the input was actually measured to better than a
 * second. It is true for a live clock, which reads one, and FALSE for a
 * duration computed from stored timestamps: those are whole seconds, so a
 * tenth on one would be a digit nobody recorded.
 *
 * The tenth stays for the whole run, however long it gets. A long run is
 * exactly when a still number reads as a hung one.
 */
export function elapsedDisplay(ms: number, precise: boolean): number {
  const value = Math.max(0, ms)
  if (!precise) return Math.floor(value / 1000)
  return Math.floor(value / 100) / 10
}

/**
 * Elapsed time parts for localized display.
 */
export type ElapsedParts =
  | { hasMinutes: false; seconds: string }
  | { hasMinutes: true; minutes: string; seconds: string }

/**
 * Compute elapsed time components for display.
 *
 * Returns minutes and seconds as formatted strings for use with i18n.
 * The `precise` flag controls whether tenths are shown (live clock) or not
 * (stored whole-second timestamps).
 */
export function elapsedParts(ms: number, precise: boolean): ElapsedParts {
  const value = Math.max(0, ms)
  const totalSeconds = precise
    ? Math.floor(value / 100) / 10
    : Math.floor(value / 1000)

  if (totalSeconds < 60) {
    return {
      hasMinutes: false,
      seconds: precise
        ? totalSeconds.toFixed(1)
        : String(Math.floor(totalSeconds)),
    }
  }

  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds - minutes * 60

  return {
    hasMinutes: true,
    minutes: String(minutes),
    seconds: precise ? seconds.toFixed(1) : String(Math.floor(seconds)),
  }
}

/**
 * Format an elapsed time as a readable string with minutes when >= 60 seconds.
 *
 * Returns "37.2s" for durations under a minute, and "2m 13.2s" for longer ones.
 * The `precise` flag controls whether tenths are shown (live clock) or not
 * (stored whole-second timestamps).
 *
 * NOTE: This function returns English literals. For localized display, use
 * `elapsedParts()` with i18n translation keys instead.
 */
export function formatElapsed(ms: number, precise: boolean): string {
  const parts = elapsedParts(ms, precise)
  if (!parts.hasMinutes) {
    return `${parts.seconds}s`
  }
  return `${parts.minutes}m ${parts.seconds}s`
}
