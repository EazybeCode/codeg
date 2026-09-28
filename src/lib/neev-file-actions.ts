import { staticHtmlDocument } from "./neev-html-sandbox"
import { NativeClientError, nativeRequest } from "./neev-client"
import {
  attachmentUrl,
  parseAttachment,
  type NeevAttachment,
} from "./neev-attachments"

export type PreviewKind = "text" | "image" | "pdf" | "html" | "unsupported"
export function previewKind(
  file: Pick<NeevAttachment, "fileName" | "contentType">
): PreviewKind {
  // HTML files as text/plain: sandboxed static preview, no scripts/active content
  if (
    /\.(?:html?|xhtml)$/i.test(file.fileName) &&
    file.contentType === "text/plain"
  )
    return "html"
  // SVG deliberately unsupported: vector images can embed scripts
  if (/\.svg$/i.test(file.fileName)) return "unsupported"
  if (file.contentType.startsWith("image/")) return "image"
  if (file.contentType === "application/pdf") return "pdf"
  if (file.contentType === "text/plain") return "text"
  return "unsupported"
}
export async function archiveChatFile(
  key: string,
  archived: boolean,
  csrf: string,
  signal?: AbortSignal
) {
  if (!csrf) throw new NativeClientError("invalid_request")
  const result = await nativeRequest(
    attachmentUrl(key),
    {
      method: "PATCH",
      headers: { "x-neev-csrf": csrf, "Content-Type": "application/json" },
      body: JSON.stringify({ archived }),
    },
    signal
  )
  if (
    !result ||
    typeof result !== "object" ||
    !("ok" in result) ||
    result.ok !== true
  )
    throw new NativeClientError("invalid_response")
  window.dispatchEvent(
    new CustomEvent("neev-files-changed", { detail: { key, archived } })
  )
}
export async function listTrashedChatFiles(
  kind: "chat" | "agent",
  id: string,
  signal?: AbortSignal
): Promise<NeevAttachment[]> {
  attachmentUrl(id) // Same canonical identifier validation.
  const base =
    kind === "chat" ? `/api/neev/chats/${id}` : `/api/neev/agent/sessions/${id}`
  const files = new Map<string, NeevAttachment>()
  const cursors = new Set<string>()
  let after: string | null = null
  do {
    const page = (await nativeRequest(
      `${base}/attachments?archived=true${after ? `&after=${encodeURIComponent(after)}` : ""}`,
      {},
      signal
    )) as { items?: unknown; nextAfter?: unknown }
    if (
      !page ||
      !Array.isArray(page.items) ||
      page.items.length > 100 ||
      !(page.nextAfter === null || typeof page.nextAfter === "string")
    )
      throw new NativeClientError("invalid_response")
    for (const raw of page.items) {
      const file = parseAttachment(raw)
      files.set(file.key, file)
    }
    after = page.nextAfter
    if (after !== null) {
      attachmentUrl(after)
      if (cursors.has(after)) throw new NativeClientError("invalid_response")
      cursors.add(after)
    }
  } while (after !== null)
  return [...files.values()]
}

export interface OrgArchivedAttachment extends NeevAttachment {
  /** Parent chat/session ID for trash listing */
  parentId: string
  /** Chat or agent session */
  parentKind: "chat" | "agent"
}

/** List all archived attachments across the org, respecting parent ACL. */
export async function listOrgArchivedAttachments(
  signal?: AbortSignal
): Promise<OrgArchivedAttachment[]> {
  const files = new Map<string, OrgArchivedAttachment>()
  const cursors = new Set<string>()
  let after: string | null = null
  do {
    const page = (await nativeRequest(
      `/api/neev/attachments?archived=true${after ? `&after=${encodeURIComponent(after)}` : ""}`,
      {},
      signal
    )) as { items?: unknown; nextAfter?: unknown }
    if (
      !page ||
      !Array.isArray(page.items) ||
      page.items.length > 100 ||
      !(page.nextAfter === null || typeof page.nextAfter === "string")
    )
      throw new NativeClientError("invalid_response")
    for (const raw of page.items) {
      if (
        !raw ||
        typeof raw !== "object" ||
        !("parentId" in raw) ||
        typeof raw.parentId !== "string" ||
        !("parentKind" in raw) ||
        (raw.parentKind !== "chat" && raw.parentKind !== "agent")
      )
        throw new NativeClientError("invalid_response")
      attachmentUrl(raw.parentId)
      const { parentId, parentKind, ...attachment } = raw
      const file: OrgArchivedAttachment = {
        ...parseAttachment(attachment),
        parentId,
        parentKind,
      }
      files.set(file.key, file)
    }
    after = page.nextAfter
    if (after !== null) {
      attachmentUrl(after)
      if (cursors.has(after)) throw new NativeClientError("invalid_response")
      cursors.add(after)
    }
  } while (after !== null)
  return [...files.values()]
}

/** Bounded text rendering is a preview limit, not an upload/storage limit. */
export async function readTextPreview(
  file: NeevAttachment,
  signal?: AbortSignal
): Promise<{ text: string; truncated: boolean }> {
  if (previewKind(file) !== "text")
    throw new NativeClientError("invalid_request")
  const response = await fetch(attachmentUrl(file.key), {
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    referrerPolicy: "no-referrer",
    signal,
  })
  if (!response.ok || !response.body) throw new NativeClientError("unavailable")
  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  const limit = 2 * 1024 * 1024
  let bytes = 0,
    text = "",
    truncated = false
  try {
    for (;;) {
      const part = await reader.read()
      if (part.done) break
      const available = limit - bytes
      text += decoder.decode(part.value.subarray(0, available), {
        stream: true,
      })
      bytes += part.value.byteLength
      if (bytes > limit) {
        truncated = true
        await reader.cancel()
        break
      }
    }
    text += decoder.decode()
  } finally {
    reader.releaseLock()
  }
  return { text, truncated }
}

/** Read HTML attachment bytes for sandboxed static preview. */
export async function readHtmlPreview(
  file: NeevAttachment,
  signal?: AbortSignal
): Promise<{ html: string; truncated: boolean }> {
  if (previewKind(file) !== "html")
    throw new NativeClientError("invalid_request")
  const response = await fetch(attachmentUrl(file.key), {
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    referrerPolicy: "no-referrer",
    signal,
  })
  if (!response.ok || !response.body) throw new NativeClientError("unavailable")
  const html = await response.text()
  if (signal?.aborted) throw new DOMException("Aborted", "AbortError")
  return { html: staticHtmlDocument(html), truncated: false }
}
