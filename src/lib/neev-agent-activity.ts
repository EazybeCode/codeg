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
    (item.state === "model" || item.state === "stop" || item.state === "result")
  )
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

/** Whole seconds since `startedAt`, for a running turn. */
export function elapsedSeconds(startedAt: number, now: number): number {
  return Math.max(0, Math.floor((now - startedAt) / 1000))
}
