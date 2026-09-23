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

export type PromptSegment =
  | { kind: "text"; text: string }
  | { kind: "attachment"; id: string }

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

/** Everything the person actually typed, with no markers in it. */
export function promptText(segments: readonly PromptSegment[]): string {
  return segments
    .map((segment) => (segment.kind === "text" ? segment.text : ""))
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
    return segment.kind === "text"
      ? segment.text === (other as { text: string }).text
      : segment.id === (other as { id: string }).id
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
): { prompt: string; ids: string[] } {
  const ids: string[] = []
  let prompt = ""
  for (const segment of segments) {
    if (segment.kind === "text") {
      prompt += segment.text
      continue
    }
    const name = names.get(segment.id)
    if (name === undefined) continue
    ids.push(segment.id)
    prompt += attachmentMarker(ids.length, name)
  }
  // Trimmed at the ends only: the space a chip leaves behind it, or a stray
  // newline, is not part of what anyone wrote.
  return { prompt: prompt.trim(), ids }
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
