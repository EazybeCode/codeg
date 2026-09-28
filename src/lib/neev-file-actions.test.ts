import { beforeEach, expect, it, vi } from "vitest"
import {
  archiveChatFile,
  listTrashedChatFiles,
  previewKind,
  readTextPreview,
} from "./neev-file-actions"
const key = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
const file = {
  key,
  fileName: "notes.txt",
  contentType: "text/plain" as const,
  byteLength: 8,
}
beforeEach(() => vi.stubGlobal("fetch", vi.fn()))
it("never requests HTML bytes through a preview", async () => {
  expect(previewKind({ ...file, fileName: "page.HTML" })).toBe("unsupported")
  await expect(
    readTextPreview({ ...file, fileName: "page.html" })
  ).rejects.toThrow()
  expect(fetch).not.toHaveBeenCalled()
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
