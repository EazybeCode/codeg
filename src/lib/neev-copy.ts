/**
 * Copying what a reply SAYS, not what its UI draws.
 *
 * Two sources only: a fenced code block's raw text (no line numbers, no
 * language badge, no button labels) and a reply's own Markdown source. Both
 * come from data — the parsed tree or the stored text — never from the DOM,
 * so nothing the interface adds around them can leak into the clipboard.
 */
import { installClipboardFallback } from "./utils"

/** A hast-like node as Streamdown hands it to a component. */
export interface CopyNode {
  type?: string
  tagName?: string
  value?: unknown
  children?: unknown
}

/** The most text one copy collects from a tree. A reply is bounded already;
 * this only stops a pathological tree from being walked without end. */
export const MAX_COPY_CHARS = 1_000_000

function isNode(value: unknown): value is CopyNode {
  return typeof value === "object" && value !== null
}

/** The concatenated text of a node, in document order, bounded. */
export function nodeText(node: unknown, limit = MAX_COPY_CHARS): string {
  let out = ""
  const stack: unknown[] = [node]
  while (stack.length && out.length < limit) {
    const next = stack.pop()
    if (!isNode(next)) continue
    if (next.type === "text" && typeof next.value === "string") {
      out += next.value
      continue
    }
    if (Array.isArray(next.children))
      for (let i = next.children.length - 1; i >= 0; i -= 1)
        stack.push(next.children[i])
  }
  return out.length > limit ? out.slice(0, limit) : out
}

/**
 * The raw code of a fenced block from its `pre` node: the `code` child's text
 * exactly as written, minus the one newline the fence itself contributes.
 */
export function codeFromPre(node: unknown): string {
  if (!isNode(node)) return ""
  const children = Array.isArray(node.children) ? node.children : []
  const code = children.find(
    (child) =>
      isNode(child) && child.type === "element" && child.tagName === "code"
  )
  const text = nodeText(code ?? node)
  return text.endsWith("\n") ? text.slice(0, -1) : text
}

/** A reply's Markdown source as the reader would paste it: as stored, with
 * only surrounding blank lines removed. */
export function messageSource(markdown: string): string {
  return markdown.replace(/^\s*\n/, "").replace(/\s+$/, "")
}

/**
 * Write `text` to the clipboard. `true` once it is there; `false` when the
 * browser refused (permission, no user gesture, no clipboard at all) — the
 * caller says so rather than claiming it copied.
 */
export async function copyToClipboard(text: string): Promise<boolean> {
  if (typeof navigator === "undefined") return false
  if (typeof navigator.clipboard?.writeText !== "function")
    installClipboardFallback()
  if (typeof navigator.clipboard?.writeText !== "function") return false
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    return false
  }
}
