/**
 * Mentioning a coworker, as an identity rather than as a name.
 *
 * A mention is picked from the organization's own directory and travels as a
 * `userId` with a SPAN — the run of characters in the stored body that stands
 * for that person. Nothing downstream ever matches a name back to an account,
 * which is the whole difference between "@Ana" meaning Ana and "@Ana" meaning
 * whichever Ana the reader happens to guess at.
 *
 * Two consequences worth being explicit about:
 *
 *  - **Nothing parses `@` out of text.** The composer knows which node is a
 *    mention because somebody chose it from a list. So an `@` inside a code
 *    fence, inside a link, or simply typed by hand is text and stays text —
 *    there is no scanner to mislead, here or on the server.
 *  - **An agent cannot mint one.** Agent replies are written by a server path
 *    that records no spans, and every chip a reader sees is drawn from a stored
 *    span. A reply containing the literal characters `@Ana` renders as those
 *    characters and notifies nobody.
 *
 * Spans are measured in Unicode code points. Byte offsets would need a
 * `TextEncoder` on one side, UTF-16 indices would need surrogate care on the
 * other; code points are exact in both `char_indices()` and `Array.from`.
 */

import type { NativeMember } from "@/lib/neev-client"

/** As many people as one message may name. Mirrored by the server. */
export const MAX_MENTIONS_PER_MESSAGE = 10

/** How many candidates the picker shows at once. */
export const MENTION_SUGGESTION_LIMIT = 8

export interface MentionSpan {
  userId: number
  /** Code points into the stored body, from its first character. */
  start: number
  /** Code points the mention occupies, `@` included. */
  length: number
}

/** A person the picker may offer. */
export interface MentionCandidate {
  userId: number
  name: string
}

export function countCodePoints(text: string): number {
  let count = 0
  for (const _ of text) count += 1
  return count
}

/**
 * Where a code-point offset lands in a JavaScript string.
 *
 * Clamped rather than thrown on: a span that does not fit the body it arrived
 * with is a span the renderer drops, and it finds that out by comparing the
 * text it sliced — not by trusting the number.
 */
export function indexOfCodePoint(text: string, offset: number): number {
  if (offset <= 0) return 0
  let index = 0
  let seen = 0
  while (index < text.length && seen < offset) {
    index += text.codePointAt(index)! > 0xffff ? 2 : 1
    seen += 1
  }
  return index
}

export type BodyPart =
  | { kind: "text"; text: string }
  | { kind: "mention"; text: string; userId: number }

/**
 * Split a run of stored text into what to draw.
 *
 * `offset` is where this run starts in the whole body, so a message whose text
 * was already split by attachment markers can be decorated a piece at a time
 * without re-deriving anything.
 *
 * Spans are taken in order and only while they stay inside this run and do not
 * overlap the previous one. Anything else — a span from a body that has since
 * been read differently, two spans claiming the same characters — is ignored
 * and its characters render as the text they are. A mention is decoration; it
 * must never be able to swallow or reorder what somebody wrote.
 */
export function splitBodyParts(
  text: string,
  spans: readonly MentionSpan[],
  offset = 0
): BodyPart[] {
  const parts: BodyPart[] = []
  const end = offset + countCodePoints(text)
  let cursor = 0
  let reach = offset
  const push = (value: string) => {
    if (!value) return
    const last = parts[parts.length - 1]
    if (last?.kind === "text") last.text += value
    else parts.push({ kind: "text", text: value })
  }
  for (const span of spans) {
    if (span.length <= 0 || span.start < reach) continue
    if (span.start < offset || span.start + span.length > end) continue
    const from = indexOfCodePoint(text, span.start - offset)
    const to = indexOfCodePoint(text, span.start + span.length - offset)
    push(text.slice(cursor, from))
    parts.push({ kind: "mention", text: text.slice(from, to), userId: span.userId })
    cursor = to
    reach = span.start + span.length
  }
  push(text.slice(cursor))
  return parts
}

/** Whether the body actually reads as a mention of that person there. */
export function spanIsIntact(
  text: string,
  span: MentionSpan,
  name: string
): boolean {
  const from = indexOfCodePoint(text, span.start)
  const to = indexOfCodePoint(text, span.start + span.length)
  return text.slice(from, to) === `@${name}`
}

/**
 * The coworkers a query offers, in the order a picker should show them.
 *
 * Name prefix first, then word starts, then anything containing the query —
 * typing "an" should reach Ana before Susan. Case-insensitive, accent-naive on
 * purpose: this narrows a list somebody is looking at, it is not a search
 * engine, and a wrong ranking costs an arrow key.
 *
 * `exclude` is the reader themself. You are not a coworker you need to notify,
 * and offering yourself is the fastest way to send a ping into your own inbox.
 */
export function matchCandidates(
  members: readonly NativeMember[],
  query: string,
  exclude: number | null,
  limit = MENTION_SUGGESTION_LIMIT
): MentionCandidate[] {
  const needle = query.trim().toLocaleLowerCase()
  const scored: { candidate: MentionCandidate; rank: number }[] = []
  for (const member of members) {
    const name = member.user.displayName
    if (!name || member.user.id === exclude) continue
    const hay = name.toLocaleLowerCase()
    let rank: number
    if (!needle) rank = 0
    else if (hay.startsWith(needle)) rank = 0
    else if (new RegExp(`(^|\\s)${escapeRegExp(needle)}`).test(hay)) rank = 1
    else if (hay.includes(needle)) rank = 2
    else continue
    scored.push({ candidate: { userId: member.user.id, name }, rank })
  }
  return scored
    .sort(
      (a, b) =>
        a.rank - b.rank ||
        a.candidate.name.localeCompare(b.candidate.name) ||
        a.candidate.userId - b.candidate.userId
    )
    .slice(0, limit)
    .map((entry) => entry.candidate)
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}
