/**
 * A prompt that has files IN it, not beside it.
 *
 * The composer is one editable document: words, then a file, then more words.
 * That ordering is the whole point — "compare this screenshot with this log"
 * means nothing if both files arrive in a tray at the bottom — so it has to
 * survive the trip to the server and back without a schema change.
 *
 * It survives as two things that are already sent today: the prompt text, and
 * the ordered list of attachment keys. Each file leaves a bounded marker where
 * it sat in the sentence, and the marker's number is its index in that list.
 * The server stores the keys in the order given (`ORDER BY l.position`), so
 * reading the message back and re-pairing marker N with attachment N is exact.
 *
 * A marker names the file and nothing else. It is built from the `File`'s own
 * name with every path separator removed, so nothing a person types in the
 * composer can turn into a path an agent might try to open.
 */

import { countCodePoints, type MentionSpan } from "@/lib/neev-mentions"

/** What a send carries for each person it names: who, and where they are said. */
export type WireMention = MentionSpan

export type PromptSegment =
  | { kind: "text"; text: string }
  | { kind: "attachment"; id: string }
  /**
   * A coworker, picked from the directory rather than typed.
   *
   * `userId` is the whole point: the message carries WHO was meant, so nothing
   * downstream has to match a name back to a person. The text is what the
   * provider and every older reader see — a plain, readable `@Name`.
   */
  | { kind: "mention"; userId: number; name: string }

/** Long enough to recognise a file by, short enough not to be a payload. */
export const MARKER_NAME_LIMIT = 80

const MARKER = /\[attachment (\d{1,2}): ([^\][\n]{1,80})\]/g

/**
 * The name a marker may carry.
 *
 * Basename only, single-line, no brackets: a marker must be unambiguous to
 * parse back and must never read as a location on anyone's disk.
 */
export function markerName(name: string): string {
  const base = name.split(/[/\\]/).pop() ?? ""
  const clean = base
    .replace(/[\u0000-\u001f\u007f]/g, " ")
    .replace(/[[\]]/g, "")
    .replace(/\s+/g, " ")
    .trim()
  if (!clean) return "file"
  return clean.length > MARKER_NAME_LIMIT
    ? clean.slice(0, MARKER_NAME_LIMIT).trimEnd()
    : clean
}

/** The reference that stands where the file sat. `index` is 1-based. */
export function attachmentMarker(index: number, name: string): string {
  return `[attachment ${index}: ${markerName(name)}]`
}

/** The text a mention stands for, in the sentence and in the prompt. */
export function mentionText(name: string): string {
  return `@${name}`
}

/**
 * Everything the person actually typed, with no markers in it.
 *
 * A mention counts as typed: it is a word in the sentence, and a draft holding
 * nothing but "@Ana" is not an empty draft.
 */
export function promptText(segments: readonly PromptSegment[]): string {
  return segments
    .map((segment) => {
      if (segment.kind === "text") return segment.text
      return segment.kind === "mention" ? mentionText(segment.name) : ""
    })
    .join("")
}

/** Nothing typed and nothing attached: there is no message here. */
export function promptIsEmpty(segments: readonly PromptSegment[]): boolean {
  return (
    !segments.some((segment) => segment.kind === "attachment") &&
    promptText(segments).trim() === ""
  )
}

export function attachmentIds(
  segments: readonly PromptSegment[]
): readonly string[] {
  return segments.flatMap((segment) =>
    segment.kind === "attachment" ? [segment.id] : []
  )
}

/** Whether two drafts are the same draft, files included. */
export function sameSegments(
  a: readonly PromptSegment[],
  b: readonly PromptSegment[]
): boolean {
  if (a.length !== b.length) return false
  return a.every((segment, index) => {
    const other = b[index]
    if (segment.kind !== other.kind) return false
    if (segment.kind === "text")
      return segment.text === (other as { text: string }).text
    if (segment.kind === "mention") {
      const mention = other as { userId: number; name: string }
      return segment.userId === mention.userId && segment.name === mention.name
    }
    return segment.id === (other as { id: string }).id
  })
}

export const EMPTY_PROMPT: readonly PromptSegment[] = []

/**
 * What is actually sent: the text with a numbered marker at each file, and the
 * local ids in the order those markers were numbered.
 *
 * `names` is consulted for the marker text. A segment whose file is no longer
 * staged is dropped rather than numbered — the alternative is a marker
 * pointing at a key that will not be in the list.
 */
export function composeWirePrompt(
  segments: readonly PromptSegment[],
  names: ReadonlyMap<string, string>
): { prompt: string; ids: string[]; mentions: WireMention[] } {
  const ids: string[] = []
  const spans: WireMention[] = []
  let prompt = ""
  // Code points written so far. The span is what tells the server, and later
  // the reader, WHICH run of characters is a person — so it is measured in the
  // one unit Rust and JavaScript agree on without conversion.
  let offset = 0
  for (const segment of segments) {
    if (segment.kind === "text") {
      prompt += segment.text
      offset += countCodePoints(segment.text)
      continue
    }
    if (segment.kind === "mention") {
      const text = mentionText(segment.name)
      const length = countCodePoints(text)
      spans.push({ userId: segment.userId, start: offset, length })
      prompt += text
      offset += length
      continue
    }
    const name = names.get(segment.id)
    if (name === undefined) continue
    ids.push(segment.id)
    const marker = attachmentMarker(ids.length, name)
    prompt += marker
    offset += countCodePoints(marker)
  }
  // Trimmed at the ends only: the space a chip leaves behind it, or a stray
  // newline, is not part of what anyone wrote.
  const trimmed = prompt.trim()
  const lead = countCodePoints(
    prompt.slice(0, prompt.length - prompt.trimStart().length)
  )
  const total = countCodePoints(trimmed)
  // Trimming moves every span left by whatever the front lost. A span that no
  // longer fits the body it describes is dropped rather than sent: a mention
  // the server would refuse must not take the whole message down with it.
  const mentions = spans
    .map((span) => ({ ...span, start: span.start - lead }))
    .filter((span) => span.start >= 0 && span.start + span.length <= total)
  return { prompt: trimmed, ids, mentions }
}

export type SentPromptPart =
  | { kind: "text"; text: string }
  | { kind: "attachment"; index: number }

/**
 * Read a stored message back into the order it was written in.
 *
 * Deliberately strict: a marker is only honoured when its number is the next
 * attachment in the list AND the name matches that attachment's own. Anything
 * else — a message from before this existed, or someone who simply typed
 * square brackets — stays the literal text they wrote.
 *
 * `trailing` holds the attachments no marker claimed, so a message can never
 * lose a file to a parsing decision.
 */
export function parseSentPrompt(
  body: string,
  attachments: readonly { fileName: string }[]
): { parts: SentPromptPart[]; trailing: number[] } {
  const parts: SentPromptPart[] = []
  let next = 0
  let cursor = 0
  const push = (text: string) => {
    if (!text) return
    const last = parts[parts.length - 1]
    if (last?.kind === "text") last.text += text
    else parts.push({ kind: "text", text })
  }
  // A fresh regex per call: `lastIndex` on a module-level global is shared
  // state, and a transcript renders many messages.
  const scan = new RegExp(MARKER.source, "g")
  for (let match = scan.exec(body); match !== null; match = scan.exec(body)) {
    const claimed = attachments[next]
    const matches =
      claimed !== undefined &&
      Number(match[1]) === next + 1 &&
      match[2] === markerName(claimed.fileName)
    if (!matches) continue
    push(body.slice(cursor, match.index))
    parts.push({ kind: "attachment", index: next })
    next += 1
    cursor = match.index + match[0].length
  }
  push(body.slice(cursor))
  const trailing = attachments.map((_, index) => index).slice(next)
  return { parts, trailing }
}
