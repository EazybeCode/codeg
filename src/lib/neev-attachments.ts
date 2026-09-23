// Chat attachments: files a person drags, pastes or picks into a composer, and
// which the agent actually opens.
//
// Two rules shape this file, and both come from the server it talks to:
//
//  - A filename is not a file. Nothing here ever sends a name in place of
//    content: bytes are uploaded first, the server answers with a key, and only
//    keys travel with a message or a turn. A file that failed to upload has no
//    key, so it cannot be silently mentioned as though it were there.
//  - What is accepted is decided by the BYTES, exactly as
//    `neev/content/attachment.rs::sniff_attachment` decides it. The local check
//    mirrors that rule rather than trusting `File.type`, which is supplied by
//    the operating system and is wrong often enough to matter (a `.ts` file is
//    "video/mp2t" to most of them). The server re-sniffs regardless; this is
//    here so a refusal happens before ten megabytes go up the wire, and so the
//    reason can name the file.

import { NativeClientError, nativeRequest } from "./neev-client"

/** `MAX_ATTACHMENTS_PER_MESSAGE` in `content/attachment.rs`. */
export const NEEV_MAX_ATTACHMENTS = 5
/** `MAX_ATTACHMENT_BYTES` in `content/blob.rs`. */
export const NEEV_MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024
/** `MAX_RUN_ATTACHMENT_BYTES` — every attachment on one turn, together. */
export const NEEV_MAX_TURN_ATTACHMENT_BYTES = 25 * 1024 * 1024
/** `MAX_NAME_CHARS` in `content/attachment.rs`. */
const MAX_NAME_CHARS = 200

/**
 * The largest image a composer will build an inline preview for.
 *
 * Two reasons for a bound, and neither is the upload limit — a 10 MB image
 * still uploads and still sends:
 *
 *  - A preview is a `data:` URL, which is base64 and therefore about 1.37× the
 *    bytes, held as a string in React state. Five 10 MB images would be ~68 MB
 *    of string kept alive for as long as the composer is open.
 *  - It has to be produced before the chip can show it, and encoding megabytes
 *    on the main thread is time the person spends looking at nothing.
 *
 * Over this, the chip shows the same file reference with its type icon. The
 * file is staged, uploaded and sent identically; only the thumbnail is absent.
 */
export const NEEV_MAX_PREVIEW_BYTES = 2 * 1024 * 1024

/** The complete set of types the server stores, in its own spelling. */
export const NEEV_ATTACHMENT_TYPES = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "application/pdf",
  "text/plain",
] as const
export type NeevAttachmentType = (typeof NEEV_ATTACHMENT_TYPES)[number]

/** One stored attachment, as the server describes it. */
export interface NeevAttachment {
  key: string
  fileName: string
  contentType: NeevAttachmentType
  byteLength: number
}

/**
 * Why a file was not taken. Each maps to one sentence the composer can show,
 * and every one of them is a rule the server enforces too.
 */
export type NeevAttachmentRejection =
  | "tooMany"
  | "tooLarge"
  | "turnTooLarge"
  | "unsupportedType"
  | "badName"
  | "empty"
  /** The organization is out of attachment storage; only the server knows this. */
  | "quota"

export interface NeevRejectedFile {
  name: string
  reason: NeevAttachmentRejection
}

const KEY = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
/** A session id may be a derived digest, not a real UUID — see the sessions client. */
const SESSION_ID = KEY

function invalid(): never {
  throw new NativeClientError("invalid_response")
}

function invalidRequest(): never {
  throw new NativeClientError("invalid_request")
}

/**
 * Classify by leading magic bytes, falling back to UTF-8 text.
 *
 * A byte-for-byte mirror of the server's `sniff_attachment`, including the
 * reason text has no magic number of its own: it is what is left when nothing
 * else claimed the bytes, which is also what makes source code work without
 * enumerating every language's extension.
 */
export function sniffAttachment(bytes: Uint8Array): NeevAttachmentType | null {
  const starts = (...prefix: number[]) =>
    prefix.length <= bytes.length &&
    prefix.every((byte, index) => bytes[index] === byte)
  if (starts(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)) return "image/png"
  if (starts(0xff, 0xd8, 0xff)) return "image/jpeg"
  if (
    bytes.length >= 12 &&
    starts(0x52, 0x49, 0x46, 0x46) &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x45 &&
    bytes[10] === 0x42 &&
    bytes[11] === 0x50
  )
    return "image/webp"
  if (starts(0x25, 0x50, 0x44, 0x46, 0x2d)) return "application/pdf"
  if (bytes.includes(0)) return null
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(bytes)
    return "text/plain"
  } catch {
    return null
  }
}

/**
 * The name the server will store this under, or `null` if it would refuse it.
 *
 * Mirrors `canonical_name`: one path segment, no climbing out of the staging
 * directory the agent reads from. Refused rather than sanitised — a name
 * rewritten into something plausible is a file the person did not send.
 */
export function canonicalAttachmentName(raw: string): string | null {
  const trimmed = raw.trim()
  const length = [...trimmed].length
  if (length === 0 || length > MAX_NAME_CHARS) return null
  if (trimmed === "." || trimmed === ".." || trimmed.startsWith("."))
    return null
  for (const character of trimmed) {
    const code = character.codePointAt(0) ?? 0
    if (
      // `char::is_control` — the C0 and C1 ranges.
      code <= 0x1f ||
      (code >= 0x7f && code <= 0x9f) ||
      character === "/" ||
      character === "\\" ||
      character === ":"
    )
      return null
  }
  return trimmed
}

/** What has to be true of a file before any of it is uploaded. */
export interface AttachmentCandidate {
  name: string
  size: number
  contentType: NeevAttachmentType
}

/**
 * Read only as much as classification needs.
 *
 * The magic numbers live in the first twelve bytes, so a PNG, a JPEG, a WebP or
 * a PDF is settled without touching the rest. Text is the expensive case — it
 * is defined as "decodes as UTF-8, all of it" — and it is only reached when no
 * magic number matched, which for a real image or PDF is never.
 */
export async function classifyFile(
  file: File
): Promise<NeevAttachmentType | null> {
  if (file.size === 0) return null
  const magic = sniffAttachment(await readBytes(file.slice(0, 12)))
  if (magic !== null && magic !== "text/plain") return magic
  return sniffAttachment(await readBytes(file))
}

/** `Blob.arrayBuffer` where there is one, and the older reader where there is not. */
function readBytes(blob: Blob): Promise<Uint8Array> {
  if (typeof blob.arrayBuffer === "function")
    return blob.arrayBuffer().then((buffer) => new Uint8Array(buffer))
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onerror = () => reject(reader.error ?? new Error("unreadable"))
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer))
    reader.readAsArrayBuffer(blob)
  })
}

/**
 * Decide which of these files the composer may take, and say why for the rest.
 *
 * `existing` is what is already staged, so the count and the per-turn total are
 * judged against the message as it will actually be sent rather than against
 * this drop alone. Files are considered in order, so dropping six at once takes
 * the first five and names the sixth — which is what the person sees happen.
 */
export async function admitFiles(
  files: readonly File[],
  existing: readonly { size: number }[] = []
): Promise<{
  accepted: { file: File; contentType: NeevAttachmentType }[]
  rejected: NeevRejectedFile[]
}> {
  const accepted: { file: File; contentType: NeevAttachmentType }[] = []
  const rejected: NeevRejectedFile[] = []
  let count = existing.length
  let total = existing.reduce((sum, item) => sum + item.size, 0)
  for (const file of files) {
    if (count >= NEEV_MAX_ATTACHMENTS) {
      rejected.push({ name: file.name, reason: "tooMany" })
      continue
    }
    if (file.size === 0) {
      rejected.push({ name: file.name, reason: "empty" })
      continue
    }
    if (file.size > NEEV_MAX_ATTACHMENT_BYTES) {
      rejected.push({ name: file.name, reason: "tooLarge" })
      continue
    }
    if (total + file.size > NEEV_MAX_TURN_ATTACHMENT_BYTES) {
      rejected.push({ name: file.name, reason: "turnTooLarge" })
      continue
    }
    if (canonicalAttachmentName(file.name) === null) {
      rejected.push({ name: file.name, reason: "badName" })
      continue
    }
    const contentType = await classifyFile(file)
    if (contentType === null) {
      rejected.push({ name: file.name, reason: "unsupportedType" })
      continue
    }
    accepted.push({ file, contentType })
    count += 1
    total += file.size
  }
  return { accepted, rejected }
}

/** Whether a drag is carrying files, as opposed to selected text or a link. */
export function dragCarriesFiles(transfer: DataTransfer | null): boolean {
  if (!transfer) return false
  const types = Array.from(transfer.types ?? [])
  return types.includes("Files")
}

/** The files on a drop, ignoring directories the browser cannot hand over. */
export function filesFromDrop(transfer: DataTransfer | null): File[] {
  if (!transfer) return []
  if (transfer.items && transfer.items.length > 0) {
    const out: File[] = []
    for (const item of Array.from(transfer.items)) {
      if (item.kind !== "file") continue
      const file = item.getAsFile()
      if (file) out.push(file)
    }
    if (out.length > 0) return out
  }
  return Array.from(transfer.files ?? [])
}

/** The files on a paste. Empty when the clipboard held only text. */
export function filesFromPaste(clipboard: DataTransfer | null): File[] {
  if (!clipboard) return []
  const out: File[] = []
  for (const item of Array.from(clipboard.items ?? [])) {
    if (item.kind !== "file") continue
    const file = item.getAsFile()
    if (file) out.push(file)
  }
  return out
}

/**
 * Whether taking these pasted files should also swallow the paste.
 *
 * Copying an image out of a design tool puts a file on the clipboard and
 * nothing else; copying a cell out of a spreadsheet puts text on it AND a
 * rendered image. Swallowing the second would silently replace what the person
 * meant to paste, so the text wins and the image is left alone.
 */
export function pasteIsFileOnly(clipboard: DataTransfer | null): boolean {
  if (!clipboard) return false
  return !Array.from(clipboard.types ?? []).includes("text/plain")
}

export function parseAttachment(value: unknown): NeevAttachment {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return invalid()
  const object = value as Record<string, unknown>
  const keys = Object.keys(object).sort()
  const expected = ["byteLength", "contentType", "fileName", "key"]
  if (keys.length !== expected.length || keys.some((k, i) => k !== expected[i]))
    return invalid()
  if (typeof object.key !== "string" || !KEY.test(object.key)) return invalid()
  if (
    typeof object.contentType !== "string" ||
    !(NEEV_ATTACHMENT_TYPES as readonly string[]).includes(object.contentType)
  )
    return invalid()
  if (typeof object.fileName !== "string") return invalid()
  // The name is re-checked against the same rule that stored it: a row that
  // could not have been written by this server is not something to render into
  // a download link.
  if (canonicalAttachmentName(object.fileName) !== object.fileName)
    return invalid()
  if (
    typeof object.byteLength !== "number" ||
    !Number.isInteger(object.byteLength) ||
    object.byteLength <= 0 ||
    object.byteLength > NEEV_MAX_ATTACHMENT_BYTES
  )
    return invalid()
  return {
    key: object.key,
    fileName: object.fileName,
    contentType: object.contentType as NeevAttachmentType,
    byteLength: object.byteLength,
  }
}

/**
 * The attachments on a message or a turn.
 *
 * Absent means none: a server that predates attachments sends no such field,
 * and that is an empty list rather than a protocol error.
 */
export function parseAttachments(value: unknown): NeevAttachment[] {
  if (value === undefined || value === null) return []
  if (!Array.isArray(value) || value.length > NEEV_MAX_ATTACHMENTS)
    return invalid()
  const items = value.map(parseAttachment)
  if (new Set(items.map((item) => item.key)).size !== items.length)
    return invalid()
  return items
}

/** Where the bytes of a stored attachment are read from, for this actor. */
export function attachmentUrl(key: string): string {
  if (!KEY.test(key)) invalidRequest()
  return `/api/neev/attachments/${key}`
}

/**
 * Send one file's bytes and get back the key that stands for them.
 *
 * The body is the raw file — the browser streams it rather than base64-ing it
 * through JSON — and the name travels percent-encoded in a header, because a
 * header is Latin-1 and a filename is not. The declared content type is
 * advisory and the server ignores it; it is sent only so an intermediary sees
 * something honest.
 */
async function uploadTo(
  path: string,
  file: File,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAttachment> {
  if (!csrf) invalidRequest()
  const name = canonicalAttachmentName(file.name)
  if (name === null || file.size === 0 || file.size > NEEV_MAX_ATTACHMENT_BYTES)
    invalidRequest()
  const result = await nativeRequest(
    path,
    {
      method: "POST",
      headers: {
        "x-neev-csrf": csrf,
        "X-Neev-Filename": encodeURIComponent(name),
        "Content-Type": file.type || "application/octet-stream",
      },
      body: file,
    },
    signal
  )
  return parseAttachment(result)
}

/** Upload a file bound to a team chat. It inherits that chat's audience. */
export async function uploadChatAttachment(
  chatId: string,
  file: File,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAttachment> {
  if (!UUID.test(chatId)) invalidRequest()
  return uploadTo(`/api/neev/chats/${chatId}/attachments`, file, csrf, signal)
}

/** Upload a file bound to a private agent session. */
export async function uploadSessionAttachment(
  sessionId: string,
  file: File,
  csrf: string,
  signal?: AbortSignal
): Promise<NeevAttachment> {
  if (!SESSION_ID.test(sessionId)) invalidRequest()
  return uploadTo(
    `/api/neev/agent/sessions/${sessionId}/attachments`,
    file,
    csrf,
    signal
  )
}

/**
 * Why one upload failed, in the same vocabulary as the local refusals.
 *
 * `413` is this file against the per-file limit. `409` is the ORGANIZATION'S
 * storage quota, not this turn's 25 MB — the turn cap is checked locally before
 * anything is uploaded and again when the run resolves, so calling a tenant out
 * of storage "too large for this message" would send them trimming a message
 * that was never the problem. Anything else stays an error for the shared error
 * surface to describe.
 */
export function uploadRejection(
  error: unknown
): NeevAttachmentRejection | null {
  if (!(error instanceof NativeClientError)) return null
  if (error.status === 413) return "tooLarge"
  if (error.status === 409) return "quota"
  if (error.status === 400) return "unsupportedType"
  return null
}

/**
 * A staged image as something an `<img>` in this app may actually load.
 *
 * It returns a `data:` URL and NOT an object URL, and that is a hard
 * requirement rather than a preference. The app is served under a Content
 * Security Policy of `img-src 'self' data:`
 * (`neev/http/static_files.rs`), so a `blob:` source is blocked by the
 * browser: the element loads to `naturalWidth === 0` and draws as a broken
 * image, with the failure visible only in the console. That is exactly what
 * was happening to every staged image here. `data:` is inside the policy, so
 * the preview is a preview again without widening what this origin may load.
 *
 * `null` means "no preview", never "no file": too large to be worth encoding,
 * not an image, or unreadable. The caller shows a file reference instead.
 */
export async function imagePreviewDataUrl(
  file: File,
  contentType: NeevAttachmentType
): Promise<string | null> {
  if (!contentType.startsWith("image/")) return null
  if (file.size === 0 || file.size > NEEV_MAX_PREVIEW_BYTES) return null
  try {
    const bytes = await readBytes(file)
    // Chunked: `String.fromCharCode(...bytes)` spreads one argument per byte,
    // and a couple of hundred thousand of them overflows the call stack.
    let binary = ""
    const CHUNK = 0x8000
    for (let at = 0; at < bytes.length; at += CHUNK)
      binary += String.fromCharCode(...bytes.subarray(at, at + CHUNK))
    return `data:${contentType};base64,${btoa(binary)}`
  } catch {
    // A file that cannot be read locally may still upload — the browser
    // re-reads it for the request — so this is a missing thumbnail and
    // nothing more.
    return null
  }
}

/** A size a person can read, in the units a file manager would use. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}
