import { NativeClientError } from "./neev-client"

export type RichStyle = "bold" | "italic" | "strike" | "code"
export interface RichInlineRun {
  text: string
  styles: RichStyle[]
  href: string | null
}
type BlockBase = { id: string; children: RichBlock[] }
export type RichBlock = BlockBase &
  (
    | {
        type: "paragraph" | "bulletListItem" | "numberedListItem" | "quote"
        content: RichInlineRun[]
      }
    | { type: "heading"; content: RichInlineRun[]; level: 1 | 2 | 3 }
    | { type: "checkListItem"; content: RichInlineRun[]; checked: boolean }
    | { type: "codeBlock"; text: string; language: string }
    | {
        type: "table"
        rows: RichInlineRun[][][]
        /**
         * One entry per column: a pixel width, or null for "as it comes".
         *
         * ABSENT when no column has been resized, which is what keeps a table
         * nobody has dragged byte-identical to what this model produced before
         * widths existed.
         */
        widths?: (number | null)[]
      }
    | { type: "divider" }
    | { type: "image"; url: string; caption: string }
  )
export interface RichDocument {
  version: 1
  blocks: RichBlock[]
}

const LIMIT = 1_048_576
/** A column narrower than this is not usable; wider than this is not a canvas. */
const MIN_COLUMN_WIDTH = 8
const MAX_COLUMN_WIDTH = 4096

/** Whether an object carries a key at all, absent being different from null. */
function hasKey(value: unknown, key: string): boolean {
  return (
    !!value &&
    typeof value === "object" &&
    (value as Record<string, unknown>)[key] !== undefined
  )
}
const encoder = new TextEncoder()
function invalid(): never {
  throw new NativeClientError("invalid_response")
}
function exact(value: unknown, keys: string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return invalid()
  const object = value as Record<string, unknown>
  if (
    Object.keys(object).length !== keys.length ||
    keys.some((key) => !Object.prototype.hasOwnProperty.call(object, key))
  )
    return invalid()
  return object
}
function string(value: unknown): string {
  // Rust strings cannot contain unpaired UTF-16 surrogates.
  if (
    typeof value !== "string" ||
    value.length > LIMIT ||
    /[\uD800-\uDFFF]/u.test(value) ||
    encoder.encode(value).length > LIMIT
  )
    return invalid()
  return value
}
function text(value: unknown): string {
  const parsed = string(value)
  if (/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F]/u.test(parsed))
    return invalid()
  return parsed
}
function safeLink(value: unknown): string {
  const href = string(value)
  if (!href || href.includes("\\")) return invalid()
  // Match the native linear stack reduction, including nested percent escapes.
  const decoded: number[] = []
  for (const byte of encoder.encode(href)) {
    decoded.push(byte)
    while (decoded.length >= 3 && decoded[decoded.length - 3] === 37) {
      const hex = String.fromCharCode(...decoded.slice(-2))
      if (!/^[0-9a-f]{2}$/i.test(hex)) break
      decoded.splice(-3, 3, parseInt(hex, 16))
    }
  }
  let decodedText: string
  try {
    decodedText = new TextDecoder("utf-8", {
      fatal: true,
      ignoreBOM: true,
    }).decode(new Uint8Array(decoded))
  } catch {
    return invalid()
  }
  if (/[\p{Cc}\p{White_Space}]/u.test(decodedText)) return invalid()
  if (href.startsWith("#")) return href.length > 1 ? href : invalid()
  const match = /^(https?):\/\/([^/?#]*)/i.exec(href)
  if (!match || !match[2] || match[2].includes("@")) return invalid()
  try {
    const url = new URL(href)
    if (!url.hostname || url.username || url.password) return invalid()
  } catch {
    return invalid()
  }
  return href
}

/** An image URL must be an internal, ACL-served reference — never external or
 *  data:. Mirrors the native `image_url_valid`. */
function internalImageUrl(value: unknown): string {
  const url = string(value)
  const prefix = "/api/neev/images/"
  const key = url.startsWith(prefix) ? url.slice(prefix.length) : ""
  if (key.length < 1 || key.length > 64 || !/^[A-Za-z0-9_-]+$/.test(key))
    return invalid()
  return url
}

/** Validate untrusted JSON and rebuild it in the native canonical field order. */
export function parseRichDocument(value: unknown): RichDocument {
  const object = exact(value, ["version", "blocks"])
  if (
    object.version !== 1 ||
    !Array.isArray(object.blocks) ||
    object.blocks.length === 0 ||
    object.blocks.length > 1000
  )
    return invalid()
  const ids = new Set<string>()
  let blocks = 0,
    runs = 0,
    bytes = 0
  const account = (value: string) => {
    bytes += encoder.encode(value).length
    if (bytes > LIMIT) invalid()
  }
  const parseRuns = (value: unknown, table = false): RichInlineRun[] => {
    if (!Array.isArray(value) || value.length > 10000 - runs) return invalid()
    runs += value.length
    return value.map((item) => {
      const run = exact(item, ["text", "styles", "href"])
      const parsedText = text(run.text)
      account(parsedText)
      if (table && /[\n\r\u2028\u2029]/u.test(parsedText)) return invalid()
      if (!Array.isArray(run.styles) || run.styles.length > 3) return invalid()
      let previous = -1
      const styles = run.styles.map((style) => {
        const index = ["bold", "italic", "strike", "code"].indexOf(style)
        if (
          index <= previous ||
          (index === 3 && (run.styles as unknown[]).length !== 1)
        )
          return invalid()
        previous = index
        return style as RichStyle
      })
      const href = run.href === null ? null : safeLink(run.href)
      if (href !== null) account(href)
      return { text: parsedText, styles, href }
    })
  }
  const parseBlock = (value: unknown, depth: number): RichBlock => {
    if (++blocks > 1000 || depth > 8 || !value || typeof value !== "object")
      return invalid()
    const type = (value as Record<string, unknown>).type
    const extras =
      type === "codeBlock"
        ? ["text", "language"]
        : type === "table"
          ? // `widths` is optional: a table nobody resized does not carry one.
            hasKey(value, "widths")
            ? ["rows", "widths"]
            : ["rows"]
          : type === "heading"
            ? ["content", "level"]
            : type === "checkListItem"
              ? ["content", "checked"]
              : type === "divider"
                ? []
                : type === "image"
                  ? ["url", "caption"]
                  : ["content"]
    const block = exact(value, ["type", "id", "children", ...extras])
    const id = string(block.id)
    if (
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
        id
      ) ||
      ids.has(id)
    )
      return invalid()
    ids.add(id)
    if (!Array.isArray(block.children) || block.children.length > 1000 - blocks)
      return invalid()
    if (
      (type === "codeBlock" ||
        type === "table" ||
        type === "divider" ||
        type === "image") &&
      block.children.length
    )
      return invalid()
    const children = block.children.map((child) => parseBlock(child, depth + 1))
    switch (type) {
      case "paragraph":
      case "bulletListItem":
      case "numberedListItem":
      case "quote":
        return { type, id, content: parseRuns(block.content), children }
      case "heading":
        if (block.level !== 1 && block.level !== 2 && block.level !== 3)
          return invalid()
        return {
          type,
          id,
          content: parseRuns(block.content),
          children,
          level: block.level,
        }
      case "checkListItem":
        if (typeof block.checked !== "boolean") return invalid()
        return {
          type,
          id,
          content: parseRuns(block.content),
          children,
          checked: block.checked,
        }
      case "codeBlock": {
        const parsedText = text(block.text),
          language = string(block.language)
        account(parsedText)
        if (!/^[A-Za-z0-9_+\-]{0,32}$/.test(language)) return invalid()
        return { type, id, children, text: parsedText, language }
      }
      case "table": {
        if (
          !Array.isArray(block.rows) ||
          !block.rows.length ||
          block.rows.length > 100
        )
          return invalid()
        const columns = Array.isArray(block.rows[0]) ? block.rows[0].length : 0
        if (columns < 1 || columns > 20) return invalid()
        const rows = block.rows.map((row) => {
          if (!Array.isArray(row) || row.length !== columns) return invalid()
          return row.map((cell) => parseRuns(cell, true))
        })
        if (block.widths === undefined) return { type, id, children, rows }
        // One per column, so a width can always be matched to the column it
        // belongs to, and bounded because a column wider than any canvas is not
        // a measurement this editor produced.
        if (!Array.isArray(block.widths) || block.widths.length !== columns)
          return invalid()
        const widths = block.widths.map((width) => {
          if (width === null) return null
          if (
            typeof width !== "number" ||
            !Number.isInteger(width) ||
            width < MIN_COLUMN_WIDTH ||
            width > MAX_COLUMN_WIDTH
          )
            return invalid()
          return width
        })
        // All-null is the same as never resized, and is stored as such so the
        // two cannot drift apart.
        return widths.every((width) => width === null)
          ? { type, id, children, rows }
          : { type, id, children, rows, widths }
      }
      case "divider":
        return { type, id, children }
      case "image": {
        const url = internalImageUrl(block.url)
        const caption = text(block.caption)
        account(url)
        account(caption)
        if (caption.length > 2000) return invalid()
        return { type, id, children, url, caption }
      }
      default:
        return invalid()
    }
  }
  const document: RichDocument = {
    version: 1,
    blocks: object.blocks.map((block) => parseBlock(block, 1)),
  }
  if (encoder.encode(JSON.stringify(document)).length > LIMIT) return invalid()
  return document
}
