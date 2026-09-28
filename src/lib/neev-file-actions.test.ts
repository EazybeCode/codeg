import { beforeEach, expect, it, vi } from "vitest"
import {
  archiveChatFile,
  listTrashedChatFiles,
  listOrgArchivedAttachments,
  previewKind,
  readTextPreview,
  readHtmlPreview,
} from "./neev-file-actions"
const key = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
const file = {
  key,
  fileName: "notes.txt",
  contentType: "text/plain" as const,
  byteLength: 8,
}
beforeEach(() => vi.stubGlobal("fetch", vi.fn()))
it("classifies HTML files as html preview kind", () => {
  expect(previewKind({ ...file, fileName: "page.HTML" })).toBe("html")
  expect(previewKind({ ...file, fileName: "doc.htm" })).toBe("html")
  expect(previewKind({ ...file, fileName: "index.xhtml" })).toBe("html")
})
it("rejects HTML through text preview", async () => {
  await expect(
    readTextPreview({ ...file, fileName: "page.html" })
  ).rejects.toThrow()
  expect(fetch).not.toHaveBeenCalled()
})
it("rejects SVG files as unsupported", () => {
  expect(previewKind({ ...file, fileName: "image.svg" })).toBe("unsupported")
})
it("loads text with same-origin credentials and rejects redirects", async () => {
  vi.mocked(fetch).mockResolvedValue(new Response("hello"))
  expect(await readTextPreview(file)).toEqual({
    text: "hello",
    truncated: false,
  })
  expect(fetch).toHaveBeenCalledWith(
    `/api/neev/attachments/${key}`,
    expect.objectContaining({
      credentials: "same-origin",
      redirect: "error",
      cache: "no-store",
    })
  )
})
it("archives and restores with CSRF without downloading bytes", async () => {
  vi.mocked(fetch).mockImplementation(
    async () =>
      new Response(JSON.stringify({ ok: true }), {
        headers: { "Content-Type": "application/json" },
      })
  )
  await archiveChatFile(key, true, "csrf")
  expect(fetch).toHaveBeenLastCalledWith(
    `/api/neev/attachments/${key}`,
    expect.objectContaining({
      method: "PATCH",
      body: JSON.stringify({ archived: true }),
      headers: expect.objectContaining({ "x-neev-csrf": "csrf" }),
    })
  )
  await archiveChatFile(key, false, "csrf")
  expect(fetch).toHaveBeenLastCalledWith(
    `/api/neev/attachments/${key}`,
    expect.objectContaining({ body: JSON.stringify({ archived: false }) })
  )
})
it("paginates trash and deduplicates files", async () => {
  vi.mocked(fetch)
    .mockResolvedValueOnce(
      new Response(JSON.stringify({ items: [file], nextAfter: key }), {
        headers: { "Content-Type": "application/json" },
      })
    )
    .mockResolvedValueOnce(
      new Response(JSON.stringify({ items: [file], nextAfter: null }), {
        headers: { "Content-Type": "application/json" },
      })
    )
  expect(await listTrashedChatFiles("chat", key)).toEqual([file])
  expect(fetch).toHaveBeenCalledTimes(2)
})
it("reads HTML preview with same-origin fetch", async () => {
  vi.mocked(fetch).mockResolvedValue(
    new Response("<html><body>content</body></html>")
  )
  const result = await readHtmlPreview({ ...file, fileName: "page.html" })
  expect(result.truncated).toBe(false)
  expect(result.html).toContain("content")
  expect(fetch).toHaveBeenCalledWith(
    `/api/neev/attachments/${key}`,
    expect.objectContaining({
      credentials: "same-origin",
      redirect: "error",
    })
  )
})
it("fetches org-wide archived attachments from correct endpoint", async () => {
  // Server returns base attachment plus parent context
  // Note: parseAttachment validates only 4 keys, so response must separate them
  const serverResponse = {
    byteLength: 8,
    contentType: "text/plain" as const,
    fileName: "notes.txt",
    key,
    parentId: "33333333-3333-4333-8333-333333333333",
    parentKind: "chat" as const,
  }
  vi.mocked(fetch).mockResolvedValueOnce(
    new Response(JSON.stringify({ items: [serverResponse], nextAfter: null }), {
      headers: { "Content-Type": "application/json" },
    })
  )
  await expect(listOrgArchivedAttachments()).resolves.toEqual([serverResponse])
  expect(fetch).toHaveBeenCalledWith(
    `/api/neev/attachments?archived=true`,
    expect.objectContaining({ credentials: "same-origin" })
  )
})

it("reports HTML bytes while the response is still streaming", async () => {
  let stream!: ReadableStreamDefaultController<Uint8Array>
  vi.mocked(fetch).mockResolvedValue(
    new Response(
      new ReadableStream({
        start(c) {
          stream = c
        },
      })
    )
  )
  const progress = vi.fn()
  const pending = readHtmlPreview(
    { ...file, fileName: "page.html" },
    undefined,
    progress
  )
  stream.enqueue(new TextEncoder().encode("<p>Hello"))
  await vi.waitFor(() => expect(progress).toHaveBeenCalledWith(8))
  stream.enqueue(new TextEncoder().encode("</p>"))
  stream.close()
  expect((await pending).html).toContain("Hello")
  expect(progress).toHaveBeenLastCalledWith(12)
})

it("aborts a transfer that makes no progress for 30 seconds", async () => {
  vi.useFakeTimers()
  try {
    vi.mocked(fetch).mockImplementation(
      (_url, init) =>
        new Promise((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () =>
            reject(new DOMException("Aborted", "AbortError"))
          )
        })
    )
    const pending = readHtmlPreview({ ...file, fileName: "page.html" })
    const rejected = expect(pending).rejects.toThrow("Aborted")
    await vi.advanceTimersByTimeAsync(30_000)
    await rejected
  } finally {
    vi.useRealTimers()
  }
})
